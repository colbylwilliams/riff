//! Takes: the draft prompts a session holds, and the operations the agent applies to them.

use std::collections::HashMap;

use crate::error::{Result, RiffError};
use crate::grounding::{GroundingChecker, SourceSpan};
use crate::json::Json;
use crate::lexicon::Lexicon;
use crate::text::tidy_whitespace;
use crate::types::{
    ContextItem, GroundingConfig, GroundingKind, GroundingMode, Line, LineGrounding, Motif,
    PolicyConfig, SECTIONS, Section, TakeStatus,
};

/// Where a new line goes within its section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Anchor {
    /// Append to the end of the section. This is what an absent `after_line_id` means.
    #[default]
    End,
    /// Place first. This is what an explicit null `after_line_id` means.
    Start,
    /// Place immediately after this line.
    After(String),
}

impl Anchor {
    /// Reads the `after_line_id` field, which distinguishes absent from an explicit null.
    pub fn from_field(value: Option<&Json>) -> Anchor {
        match value {
            None => Anchor::End,
            Some(Json::Null) => Anchor::Start,
            Some(Json::String(id)) => Anchor::After(id.clone()),
            Some(_) => Anchor::End,
        }
    }
}

/// One operation the agent applies to a take.
#[derive(Debug, Clone)]
pub enum DraftOperation {
    /// Name the prompt.
    SetTitle {
        /// The proposed title.
        text: String,
    },
    /// Add or replace a line.
    UpsertLine {
        /// The line to replace, or none to add a new one.
        line_id: Option<String>,
        /// Which part of the prompt it belongs to, as the model spelled it.
        section: Option<String>,
        /// The proposed text.
        text: String,
        /// Where it goes within the section.
        after: Anchor,
        /// Lines it replaces because the speaker corrected themselves.
        supersedes: Vec<String>,
    },
    /// Take a line out.
    RemoveLine {
        /// Which line.
        line_id: Option<String>,
    },
    /// Move a line within its section.
    MoveLine {
        /// Which line.
        line_id: Option<String>,
        /// Where it goes.
        after: Anchor,
    },
    /// Attach a resolved reference.
    AttachContext {
        /// Which reference, as handed out by `resolve_reference`.
        reference_id: Option<String>,
    },
    /// Detach a reference.
    DetachContext {
        /// Which reference.
        reference_id: Option<String>,
    },
    /// An operation this build does not know.
    Unknown {
        /// What the model called it.
        op: String,
    },
}

impl DraftOperation {
    /// The wire name, which the outcome reports back.
    pub fn name(&self) -> &str {
        match self {
            DraftOperation::SetTitle { .. } => "set_title",
            DraftOperation::UpsertLine { .. } => "upsert_line",
            DraftOperation::RemoveLine { .. } => "remove_line",
            DraftOperation::MoveLine { .. } => "move_line",
            DraftOperation::AttachContext { .. } => "attach_context",
            DraftOperation::DetachContext { .. } => "detach_context",
            DraftOperation::Unknown { op } => op,
        }
    }

    /// Reads one entry of `draft_update`'s `operations` array.
    pub fn from_json(value: &Json) -> DraftOperation {
        let line_id = value.get_str("line_id").map(str::to_owned);
        let reference_id = value.get_str("reference_id").map(str::to_owned);
        let text = value.get_str("text").unwrap_or_default().to_owned();
        let after = Anchor::from_field(value.get("after_line_id"));

        match value.get_str("op").unwrap_or_default() {
            "set_title" => DraftOperation::SetTitle { text },
            "upsert_line" => DraftOperation::UpsertLine {
                line_id,
                section: value.get_str("section").map(str::to_owned),
                text,
                after,
                supersedes: value
                    .get("supersedes")
                    .map(|ids| {
                        ids.array_or_empty()
                            .iter()
                            .filter_map(|id| id.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            "remove_line" => DraftOperation::RemoveLine { line_id },
            "move_line" => DraftOperation::MoveLine { line_id, after },
            "attach_context" => DraftOperation::AttachContext { reference_id },
            "detach_context" => DraftOperation::DetachContext { reference_id },
            other => DraftOperation::Unknown {
                op: other.to_owned(),
            },
        }
    }
}

/// What happened to one operation.
#[derive(Debug, Clone)]
pub struct DraftOperationOutcome {
    /// Which operation.
    pub op: String,
    /// Which line it touched, when it named one.
    pub line_id: Option<String>,
    /// Whether it was applied.
    pub accepted: bool,
    /// Written for the model to act on: it says what to do, not just what went wrong.
    pub reason: Option<String>,
    /// How much of the line was provably theirs.
    pub ratio: Option<f64>,
    /// How the line relates to what they said.
    pub kind: Option<GroundingKind>,
    /// The words the agent invented.
    pub unmatched_tokens: Vec<String>,
    /// The nearest thing they actually said, so the model can put their words back.
    pub closest_source: Option<String>,
}

impl DraftOperationOutcome {
    fn accepted(op: &str, line_id: Option<String>) -> Self {
        Self {
            op: op.to_owned(),
            line_id,
            accepted: true,
            reason: None,
            ratio: None,
            kind: None,
            unmatched_tokens: Vec::new(),
            closest_source: None,
        }
    }

    fn rejected(op: &str, line_id: Option<String>, reason: impl Into<String>) -> Self {
        Self {
            op: op.to_owned(),
            line_id,
            accepted: false,
            reason: Some(reason.into()),
            ratio: None,
            kind: None,
            unmatched_tokens: Vec::new(),
            closest_source: None,
        }
    }

    /// The outcome as the model reads it.
    pub fn to_json(&self) -> Json {
        let mut object = crate::json_object! { "op" => self.op.clone() };
        object.insert_some("lineId", self.line_id.clone().map(Json::from));
        object.insert(
            "status",
            Json::from(if self.accepted {
                "accepted"
            } else {
                "rejected"
            }),
        );
        object.insert_some("reason", self.reason.clone().map(Json::from));
        object.insert_some("ratio", self.ratio.map(Json::from));
        object.insert_some("kind", self.kind.map(|kind| Json::from(kind.as_str())));
        if !self.unmatched_tokens.is_empty() {
            object.insert("unmatchedTokens", Json::from(self.unmatched_tokens.clone()));
        }
        object.insert_some("closestSource", self.closest_source.clone().map(Json::from));
        Json::Object(object)
    }
}

/// Gap left between lines so a later insertion has somewhere to go without renumbering.
const ORDER_STEP: f64 = 1000.0;

/// One draft prompt. A session can hold several, so a change of subject does not destroy the last one.
#[derive(Debug, Clone)]
pub struct Take {
    /// Identity within the session.
    pub id: String,
    /// What the speaker called this line of thinking.
    pub label: Option<String>,
    /// When it was started.
    pub created_at: String,
    /// When it was last edited.
    pub updated_at: String,
    /// Where it is in its life.
    pub status: TakeStatus,
    /// What the prompt is called.
    pub title: Option<crate::types::Title>,
    /// Where it is going.
    pub target: Option<String>,

    lines: Vec<Line>,
    context: Vec<ContextItem>,
    history: Vec<Line>,
    sequence: usize,
}

impl Take {
    /// A new, empty take.
    pub fn new(
        id: impl Into<String>,
        created_at: impl Into<String>,
        label: Option<String>,
    ) -> Self {
        let created_at = created_at.into();
        Self {
            id: id.into(),
            label,
            updated_at: created_at.clone(),
            created_at,
            status: TakeStatus::Drafting,
            title: None,
            target: None,
            lines: Vec::new(),
            context: Vec::new(),
            history: Vec::new(),
            sequence: 0,
        }
    }

    /// A line id no line in this take is using.
    pub fn next_line_id(&mut self) -> String {
        self.sequence += 1;
        format!("{}-l{}", self.id, self.sequence)
    }

    /// Lines in reading order: section order first, then position within the section.
    pub fn lines(&self) -> Vec<Line> {
        let mut lines = self.lines.clone();
        lines.sort_by(|a, b| {
            a.section
                .order()
                .cmp(&b.section.order())
                .then_with(|| a.order.total_cmp(&b.order))
        });
        lines
    }

    /// The lines of one section, in order.
    pub fn lines_in(&self, section: Section) -> Vec<Line> {
        self.lines()
            .into_iter()
            .filter(|line| line.section == section)
            .collect()
    }

    /// One line by id.
    pub fn line(&self, id: &str) -> Option<&Line> {
        self.lines.iter().find(|line| line.id == id)
    }

    /// Everything the prompt refers to, in the order the speaker raised it.
    pub fn context(&self) -> &[ContextItem] {
        &self.context
    }

    /// Lines replaced by a correction. Kept so a change of mind can be walked back.
    pub fn history(&self) -> &[Line] {
        &self.history
    }

    /// Adds or replaces a line.
    pub fn set_line(&mut self, line: Line) {
        match self
            .lines
            .iter()
            .position(|existing| existing.id == line.id)
        {
            Some(index) => {
                self.history.push(self.lines[index].clone());
                self.lines[index] = line;
            }
            None => self.lines.push(line),
        }
    }

    /// Takes a line out, keeping it in history.
    pub fn remove_line(&mut self, id: &str) -> bool {
        match self.lines.iter().position(|line| line.id == id) {
            Some(index) => {
                self.history.push(self.lines.remove(index));
                true
            }
            None => false,
        }
    }

    /// Attaches a resolved reference, replacing one already attached under the same id.
    pub fn attach_context(&mut self, item: ContextItem) {
        match self
            .context
            .iter()
            .position(|existing| existing.reference_id == item.reference_id)
        {
            Some(index) => self.context[index] = item,
            None => self.context.push(item),
        }
    }

    /// Detaches a reference.
    pub fn detach_context(&mut self, reference_id: &str) -> bool {
        match self
            .context
            .iter()
            .position(|item| item.reference_id == reference_id)
        {
            Some(index) => {
                self.context.remove(index);
                true
            }
            None => false,
        }
    }

    /// Whether the take holds nothing at all.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.context.is_empty()
    }

    /// Order value that places a new line where `anchor` says within its section.
    pub fn order_after(&self, section: Section, anchor: &Anchor) -> f64 {
        let siblings = self.lines_in(section);
        let last = || {
            siblings
                .last()
                .map_or(ORDER_STEP, |line| line.order + ORDER_STEP)
        };

        match anchor {
            Anchor::Start => siblings
                .first()
                .map_or(ORDER_STEP, |line| line.order - ORDER_STEP),
            Anchor::End => last(),
            Anchor::After(id) => match siblings.iter().position(|line| line.id == *id) {
                None => last(),
                Some(index) => {
                    let anchor_order = siblings[index].order;
                    siblings
                        .get(index + 1)
                        .map_or(anchor_order + ORDER_STEP, |next| {
                            (anchor_order + next.order) / 2.0
                        })
                }
            },
        }
    }

    /// Whether the take has everything the policy says a sendable prompt needs.
    pub fn is_ready(&self, policy: &PolicyConfig) -> bool {
        policy
            .readiness_requires
            .iter()
            .all(|section| !self.lines_in(*section).is_empty())
    }

    /// A take that has been sent or thrown away. Nothing may reopen or alter it.
    pub fn is_terminal(&self) -> bool {
        matches!(self.status, TakeStatus::Submitted | TakeStatus::Discarded)
    }
}

/// What the checker needs to decide whether an operation may be applied.
pub struct ApplyContext<'a> {
    /// The fidelity gate.
    pub checker: &'a GroundingChecker,
    /// Everything the speaker said, windowed.
    pub spans: &'a [SourceSpan],
    /// The vocabulary corrections are drawn from.
    pub lexicon: &'a Lexicon,
    /// How strictly each section is held to their words.
    pub grounding: &'a GroundingConfig,
    /// Resolved references the agent may attach, keyed by reference id.
    pub references: &'a HashMap<String, ContextItem>,
    /// Standing instructions, keyed by motif id.
    pub motifs: &'a HashMap<String, Motif>,
    /// The timestamp to stamp an edit with.
    pub now: &'a str,
}

/// What happened to a batch of operations.
#[derive(Debug, Clone, Default)]
pub struct ApplyResult {
    /// Operations that were applied.
    pub accepted: Vec<DraftOperationOutcome>,
    /// Operations that were refused, each with what to do about it.
    pub rejected: Vec<DraftOperationOutcome>,
}

/// Applies the agent's draft operations, refusing any line that is not made of the speaker's words.
///
/// Rejection is the mechanism that keeps the prompt in their voice: the model proposes text, this
/// function decides whether it survives, and the rejection message tells the model exactly which
/// words it invented so it can put theirs back.
pub fn apply_draft_operations(
    take: &mut Take,
    operations: &[DraftOperation],
    ctx: &ApplyContext<'_>,
) -> ApplyResult {
    let mut result = ApplyResult::default();

    for operation in operations {
        let outcome = match operation {
            DraftOperation::SetTitle { text } => apply_set_title(take, text, ctx),
            DraftOperation::UpsertLine { .. } => apply_upsert_line(take, operation, ctx),
            DraftOperation::RemoveLine { line_id } => {
                if take.remove_line(line_id.as_deref().unwrap_or_default()) {
                    DraftOperationOutcome::accepted("remove_line", line_id.clone())
                } else {
                    DraftOperationOutcome::rejected(
                        "remove_line",
                        line_id.clone(),
                        format!(
                            "no line {} in this take",
                            line_id.as_deref().unwrap_or("(missing line_id)")
                        ),
                    )
                }
            }
            DraftOperation::MoveLine { line_id, after } => apply_move_line(take, line_id, after),
            DraftOperation::AttachContext { reference_id } => {
                match reference_id.as_ref().and_then(|id| ctx.references.get(id)) {
                    Some(reference) => {
                        take.attach_context(reference.clone());
                        DraftOperationOutcome::accepted("attach_context", None)
                    }
                    None => DraftOperationOutcome::rejected(
                        "attach_context",
                        None,
                        format!(
                            "unknown reference_id {}; call resolve_reference first and use an id it returned",
                            reference_id.as_deref().unwrap_or("(missing)")
                        ),
                    ),
                }
            }
            DraftOperation::DetachContext { reference_id } => {
                if take.detach_context(reference_id.as_deref().unwrap_or_default()) {
                    DraftOperationOutcome::accepted("detach_context", None)
                } else {
                    DraftOperationOutcome::rejected(
                        "detach_context",
                        None,
                        "that reference is not attached",
                    )
                }
            }
            DraftOperation::Unknown { op } => {
                DraftOperationOutcome::rejected(op, None, format!("unknown operation \"{op}\""))
            }
        };

        if outcome.accepted {
            result.accepted.push(outcome);
        } else {
            result.rejected.push(outcome);
        }
    }

    if !result.accepted.is_empty() {
        take.updated_at = ctx.now.to_owned();
    }
    result
}

fn apply_set_title(take: &mut Take, text: &str, ctx: &ApplyContext<'_>) -> DraftOperationOutcome {
    let text = tidy_whitespace(text);
    if text.is_empty() {
        return DraftOperationOutcome::rejected("set_title", None, "set_title needs text");
    }

    let result = ctx.checker.check_title(&text, ctx.spans, ctx.lexicon);
    if !result.ok {
        // A title is held to a looser bar than a body line, but it may still only use words they
        // used. Accepting it as `derived` here would make the looser bar no bar at all.
        let mut outcome = DraftOperationOutcome::rejected(
            "set_title",
            None,
            result
                .reason
                .clone()
                .unwrap_or_else(|| rejection_reason(&result.unmatched_tokens, ctx.spans)),
        );
        outcome.ratio = Some(result.ratio);
        outcome.unmatched_tokens = result.unmatched_tokens;
        return outcome;
    }

    take.title = Some(crate::types::Title {
        text,
        origin: crate::types::TitleOrigin::Spoken,
    });
    let mut outcome = DraftOperationOutcome::accepted("set_title", None);
    outcome.ratio = Some(result.ratio);
    outcome.kind = Some(result.kind);
    outcome
}

fn apply_upsert_line(
    take: &mut Take,
    operation: &DraftOperation,
    ctx: &ApplyContext<'_>,
) -> DraftOperationOutcome {
    let DraftOperation::UpsertLine {
        line_id,
        section,
        text,
        after,
        supersedes,
    } = operation
    else {
        unreachable!("caller matched upsert_line")
    };

    let text = tidy_whitespace(text);
    let Some(section) = section.as_deref().and_then(Section::parse) else {
        return DraftOperationOutcome::rejected(
            "upsert_line",
            None,
            format!(
                "section must be one of {}",
                SECTIONS
                    .iter()
                    .map(|section| section.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    };
    if text.is_empty() {
        return DraftOperationOutcome::rejected("upsert_line", None, "upsert_line needs text");
    }

    let mode = ctx.grounding.mode_for(section);
    let motif = find_motif(&text, ctx.motifs);

    let grounding;
    let mut source_utterance_ids = Vec::new();
    let mut motif_id = None;

    if mode == GroundingMode::MotifOrStrict && motif.is_some() {
        grounding = LineGrounding::full(GroundingKind::Motif);
        motif_id = motif.map(|motif| motif.id.clone());
    } else {
        let result = ctx.checker.check(&text, ctx.spans, ctx.lexicon);
        if !result.ok {
            let mut outcome = DraftOperationOutcome::rejected(
                "upsert_line",
                line_id.clone(),
                result
                    .reason
                    .clone()
                    .unwrap_or_else(|| rejection_reason(&result.unmatched_tokens, ctx.spans)),
            );
            outcome.ratio = Some(result.ratio);
            outcome.closest_source = closest_source_text(ctx.spans, &result.source_utterance_ids);
            outcome.unmatched_tokens = result.unmatched_tokens;
            return outcome;
        }
        grounding = LineGrounding {
            ratio: result.ratio,
            kind: result.kind,
            unmatched_tokens: result.unmatched_tokens,
        };
        source_utterance_ids = result.source_utterance_ids;
    }

    let existing = line_id.as_deref().and_then(|id| take.line(id)).cloned();
    if line_id.is_some() && existing.is_none() {
        // Accepting an unknown id would create a line outside the generated sequence, and the next
        // ordinary insert would reuse that id and silently overwrite this line.
        return DraftOperationOutcome::rejected(
            "upsert_line",
            line_id.clone(),
            format!(
                "no line {} in this take; omit line_id to add a new one",
                line_id.as_deref().unwrap_or_default()
            ),
        );
    }

    let id = match &existing {
        Some(line) => line.id.clone(),
        None => take.next_line_id(),
    };
    let order = match (&existing, after) {
        (Some(line), Anchor::End) => line.order,
        _ => take.order_after(section, after),
    };

    let mut deduplicated: Vec<String> = Vec::new();
    for target in supersedes {
        if *target != id && !deduplicated.contains(target) {
            deduplicated.push(target.clone());
        }
    }
    for target in &deduplicated {
        take.remove_line(target);
    }

    let ratio = grounding.ratio;
    let kind = grounding.kind;
    take.set_line(Line {
        id: id.clone(),
        section,
        text,
        order,
        source_utterance_ids,
        motif_id,
        supersedes: deduplicated,
        grounding,
    });

    let mut outcome = DraftOperationOutcome::accepted("upsert_line", Some(id));
    outcome.ratio = Some(ratio);
    outcome.kind = Some(kind);
    outcome
}

fn apply_move_line(
    take: &mut Take,
    line_id: &Option<String>,
    after: &Anchor,
) -> DraftOperationOutcome {
    let Some(mut line) = line_id.as_deref().and_then(|id| take.line(id)).cloned() else {
        return DraftOperationOutcome::rejected(
            "move_line",
            line_id.clone(),
            format!(
                "no line {} in this take",
                line_id.as_deref().unwrap_or("(missing line_id)")
            ),
        );
    };
    line.order = take.order_after(line.section, after);
    let id = line.id.clone();
    take.set_line(line);
    DraftOperationOutcome::accepted("move_line", Some(id))
}

fn find_motif<'a>(text: &str, motifs: &'a HashMap<String, Motif>) -> Option<&'a Motif> {
    let normalized = text.to_lowercase();
    let mut matches: Vec<&Motif> = motifs
        .values()
        .filter(|motif| motif.retired_at.is_none() && motif.text.to_lowercase() == normalized)
        .collect();
    // Iteration order of a hash map is not stable, so the lowest id wins rather than whichever the
    // map happened to hand back first.
    matches.sort_by(|a, b| a.id.cmp(&b.id));
    matches.into_iter().next()
}

fn rejection_reason(unmatched: &[String], spans: &[SourceSpan]) -> String {
    if spans.is_empty() {
        return "nothing has been said yet, so there is nothing to draw on".to_owned();
    }
    if unmatched.is_empty() {
        return "the words are theirs but the order is not; keep their phrasing intact".to_owned();
    }
    let shown = unmatched
        .iter()
        .take(6)
        .map(|token| format!("\"{token}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let more = if unmatched.len() > 6 {
        format!(" and {} more", unmatched.len() - 6)
    } else {
        String::new()
    };
    format!("they did not say {shown}{more}; use their words or ask them")
}

fn closest_source_text(spans: &[SourceSpan], utterance_ids: &[String]) -> Option<String> {
    if utterance_ids.is_empty() {
        return None;
    }
    spans
        .iter()
        .find(|span| {
            span.utterance_ids
                .iter()
                .any(|id| utterance_ids.contains(id))
        })
        .map(|span| span.tokens.join(" "))
}

/// Holds every take in the session and tracks which one is being spoken into.
#[derive(Debug)]
pub struct DraftBook {
    takes: Vec<Take>,
    active_id: Option<String>,
    sequence: usize,
    max_takes: usize,
}

impl DraftBook {
    /// A book that holds at most `policy.max_takes` open takes.
    pub fn new(policy: &PolicyConfig) -> Self {
        Self {
            takes: Vec::new(),
            active_id: None,
            sequence: 0,
            max_takes: policy.max_takes,
        }
    }

    /// Which take is being spoken into.
    pub fn active_id(&self) -> Option<&str> {
        self.active_id.as_deref()
    }

    /// Every take, in the order they were started.
    pub fn takes(&self) -> &[Take] {
        &self.takes
    }

    /// One take by id.
    pub fn get(&self, id: &str) -> Option<&Take> {
        self.takes.iter().find(|take| take.id == id)
    }

    /// One take by id, for editing.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Take> {
        self.takes.iter_mut().find(|take| take.id == id)
    }

    /// The take being spoken into, creating the first one on demand.
    pub fn active(&mut self, now: &str) -> Result<&mut Take> {
        if let Some(id) = self.active_id.clone()
            && self.get(&id).is_some()
        {
            return Ok(self.get_mut(&id).expect("just checked"));
        }
        let id = self.create(None, None, now)?;
        Ok(self.get_mut(&id).expect("just created"))
    }

    /// Starts a new take and makes it active, optionally carrying context over from another.
    pub fn create(
        &mut self,
        label: Option<String>,
        carry_context_from: Option<&str>,
        now: &str,
    ) -> Result<String> {
        let live: Vec<usize> = self
            .takes
            .iter()
            .enumerate()
            .filter(|(_, take)| !take.is_terminal())
            .map(|(index, _)| index)
            .collect();

        if live.len() >= self.max_takes {
            let oldest = live
                .iter()
                .min_by(|a, b| self.takes[**a].updated_at.cmp(&self.takes[**b].updated_at))
                .copied();
            match oldest {
                Some(index) if self.takes[index].is_empty() => {
                    self.takes.remove(index);
                }
                _ => {
                    return Err(RiffError::tool(format!(
                        "too many open takes (limit {}); park or send one first",
                        self.max_takes
                    )));
                }
            }
        }

        let carried: Vec<ContextItem> = carry_context_from
            .and_then(|id| self.get(id))
            .map(|take| take.context().to_vec())
            .unwrap_or_default();

        self.sequence += 1;
        let mut take = Take::new(format!("t{}", self.sequence), now, label);
        for item in carried {
            take.attach_context(item);
        }
        let id = take.id.clone();
        self.takes.push(take);
        // Which also parks whatever was being spoken into, so `60-corrections` holds: they can come
        // back to the one they were on.
        self.set_active(Some(&id));
        Ok(id)
    }

    /// Makes one take the one being spoken into, or stands them all down when `None`.
    ///
    /// Status is derived here rather than maintained alongside: the active take is the one being
    /// drafted, and every take that has not finished is otherwise parked. Deriving it in one place
    /// is what keeps the two from disagreeing — "drafting but not active" is the state in which a
    /// second prompt quietly collects what was said for the first, and every caller that set a
    /// status by hand was one more chance to produce it.
    fn set_active(&mut self, id: Option<&str>) {
        self.active_id = id.map(str::to_owned);
        for take in &mut self.takes {
            if take.is_terminal() {
                continue;
            }
            take.status = if Some(take.id.as_str()) == id {
                TakeStatus::Drafting
            } else {
                TakeStatus::Parked
            };
        }
    }

    /// Makes another take the one being spoken into.
    pub fn switch_to(&mut self, id: &str) -> Result<Option<&Take>> {
        let Some(index) = self.takes.iter().position(|take| take.id == id) else {
            return Ok(None);
        };
        // Switching to a finished take would make it the target of the next thing spoken.
        if self.takes[index].is_terminal() {
            return Err(RiffError::tool(format!(
                "take \"{id}\" was already {}; it cannot be reopened",
                self.takes[index].status.as_str()
            )));
        }

        self.set_active(Some(id));
        Ok(Some(&self.takes[index]))
    }

    /// Sets a take aside so it can be picked up again.
    pub fn park(&mut self, id: &str) -> Result<bool> {
        let Some(index) = self.takes.iter().position(|take| take.id == id) else {
            return Ok(false);
        };
        // Parking a finished take would move it out of a terminal state, and switching back would
        // then promote it to drafting — which is how every guard downstream gets bypassed.
        if self.takes[index].is_terminal() {
            return Err(RiffError::tool(format!(
                "take \"{id}\" was already {}; it cannot be parked",
                self.takes[index].status.as_str()
            )));
        }
        // Any take that is not the active one is already parked; only standing down the active one
        // changes anything, and it leaves nothing active so the next thing said starts fresh.
        if self.active_id.as_deref() == Some(id) {
            self.set_active(None);
        }
        Ok(true)
    }

    /// Records that a take was delivered.
    ///
    /// `keep_open` leaves it exactly where the speaker had it, because a prompt they are still
    /// working on has not finished. Otherwise it is terminal, and if it was the one being spoken
    /// into then nothing is: the next line lands in a new prompt rather than one already gone out.
    ///
    /// Returns whether this stood the active take down, so a caller can report that it did. A take
    /// that was no longer active — another became active while the host was answering — leaves that
    /// newer one alone.
    pub fn mark_submitted(&mut self, id: &str, keep_open: bool) -> bool {
        if keep_open {
            return false;
        }
        let Some(index) = self.takes.iter().position(|take| take.id == id) else {
            return false;
        };
        self.takes[index].status = TakeStatus::Submitted;
        if self.active_id.as_deref() != Some(id) {
            return false;
        }
        self.set_active(None);
        true
    }

    /// Throws a take away.
    pub fn discard(&mut self, id: &str) -> Result<bool> {
        let Some(index) = self.takes.iter().position(|take| take.id == id) else {
            return Ok(false);
        };
        if self.takes[index].status == TakeStatus::Submitted {
            return Err(RiffError::tool(format!(
                "take \"{id}\" was already submitted"
            )));
        }
        self.takes[index].status = TakeStatus::Discarded;
        if self.active_id.as_deref() == Some(id) {
            self.set_active(None);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PolicyConfig {
        PolicyConfig {
            max_takes: 2,
            readiness_requires: vec![Section::Intent],
            auto_submit: false,
            readback_default: "gist".into(),
            persist_audio: false,
            redact_secrets_from_transcript: true,
        }
    }

    #[test]
    fn will_not_reopen_a_finished_take() {
        let mut book = DraftBook::new(&policy());
        let id = book.create(None, None, "t0").unwrap();
        book.discard(&id).unwrap();
        assert!(book.switch_to(&id).is_err());
        assert!(book.park(&id).is_err());
    }

    #[test]
    fn reclaims_an_empty_take_rather_than_refusing_a_new_one() {
        let mut book = DraftBook::new(&policy());
        book.create(None, None, "t0").unwrap();
        book.create(None, None, "t1").unwrap();
        // Both are empty, so the oldest is recycled instead of the limit being hit.
        assert!(book.create(None, None, "t2").is_ok());
        assert_eq!(book.takes().len(), 2);
    }

    #[test]
    fn inserts_between_neighbours_without_renumbering() {
        let mut take = Take::new("t1", "t0", None);
        let first = take.order_after(Section::Intent, &Anchor::End);
        take.set_line(Line {
            id: "t1-l1".into(),
            section: Section::Intent,
            text: "one".into(),
            order: first,
            source_utterance_ids: vec![],
            motif_id: None,
            supersedes: vec![],
            grounding: LineGrounding::full(GroundingKind::Verbatim),
        });
        let second = take.order_after(Section::Intent, &Anchor::End);
        take.set_line(Line {
            id: "t1-l2".into(),
            section: Section::Intent,
            text: "two".into(),
            order: second,
            source_utterance_ids: vec![],
            motif_id: None,
            supersedes: vec![],
            grounding: LineGrounding::full(GroundingKind::Verbatim),
        });

        let between = take.order_after(Section::Intent, &Anchor::After("t1-l1".into()));
        assert!(between > first && between < second);
        assert!(take.order_after(Section::Intent, &Anchor::Start) < first);
    }
}
