//! The tools the agent calls, and the state they operate on.
//!
//! Handlers are dispatched by name rather than through a table of closures: the state they touch —
//! the ledger, the book, the lexicon — is owned by one struct, and a `match` keeps every borrow of
//! it plain enough to read.

use std::collections::HashMap;
use std::sync::Arc;

use crate::draft::{ApplyContext, DraftBook, DraftOperation, Take, apply_draft_operations};
use crate::error::{Result, RiffError};
use crate::grounding::GroundingChecker;
use crate::host::{
    LookupTermRequest, RecallPromptsRequest, ResolveReferenceRequest, RiffHost, RiffStore,
    SubmitOptions,
};
use crate::json::{Json, JsonObject};
use crate::json_object;
use crate::ledger::UtteranceLedger;
use crate::lexicon::{Lexicon, is_plausible_mishearing, term_key};
use crate::render::{
    BuildArtifactOptions, RenderOptions, build_artifact, render_prompt, summarize_draft,
};
use crate::runtime::{Clock, with_deadline};
use crate::schema::validate;
use crate::text::tidy_whitespace;
use crate::types::{
    AgentBundle, ContextItem, GroundingKind, LexiconTerm, Line, LineGrounding, Motif,
    PromptArtifact, Provenance, Section, TakeStatus,
};

/// Something the session has to act on once a tool has run.
///
/// Handlers report effects rather than calling back into the session, so the state a tool mutates
/// and the events a session emits stay on opposite sides of one clean boundary.
#[derive(Debug, Clone)]
pub enum ToolEffect {
    /// The vocabulary changed, so the provider's transcription biasing is stale.
    LexiconChanged,
    /// A take was edited.
    DraftChanged(String),
    /// A different take is active, or none is.
    TakeChanged(Option<String>),
    /// A prompt was sent.
    Submitted(Box<PromptArtifact>),
}

/// What one tool call produced.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    /// Whether the call succeeded. A refusal the model can act on is still a success.
    pub ok: bool,
    /// What to hand back to the model.
    pub result: Json,
    /// How long it took.
    pub duration_ms: u64,
    /// What the session has to do about it.
    pub effects: Vec<ToolEffect>,
}

impl ToolOutcome {
    fn error(message: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            ok: false,
            result: Json::Object(json_object! { "error" => message.into() }),
            duration_ms,
            effects: Vec::new(),
        }
    }
}

/// Everything the tools read and write.
pub struct ToolRuntime {
    /// The agent definition every threshold and label comes from.
    pub bundle: Arc<AgentBundle>,
    /// Everything the speaker said.
    pub ledger: UtteranceLedger,
    /// Every take in the session.
    pub book: DraftBook,
    /// The active vocabulary.
    pub lexicon: Lexicon,
    /// References resolved this session, so the agent can attach one by id later.
    pub references: HashMap<String, ContextItem>,
    /// Standing instructions, retired ones included.
    pub motifs: HashMap<String, Motif>,
    /// Render profile the session was configured with, so a preview and a submission agree.
    pub render_profile: Option<String>,
    /// Where a prompt came from, refreshed by the session before every artifact is built.
    pub provenance: Provenance,

    checker: GroundingChecker,
    host: Arc<dyn RiffHost>,
    store: Arc<dyn RiffStore>,
    clock: Arc<dyn Clock>,
    /// Effects recorded so far by the running handler. See [`ToolRuntime::record`].
    effects: Vec<ToolEffect>,
    /// A result that is already true. See [`ToolRuntime::commit`].
    committed: Option<Json>,
}

impl ToolRuntime {
    /// Builds the runtime a session drives.
    pub fn new(
        bundle: Arc<AgentBundle>,
        host: Arc<dyn RiffHost>,
        store: Arc<dyn RiffStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let lexicon = Lexicon::new(bundle.lexicon.terms.clone());
        let ledger = UtteranceLedger::new(
            bundle.grounding.window_size,
            bundle.policy.redact_secrets_from_transcript,
        );
        let book = DraftBook::new(&bundle.policy);
        let checker = GroundingChecker::new(&bundle.grounding);

        Self {
            bundle,
            ledger,
            book,
            lexicon,
            references: HashMap::new(),
            motifs: HashMap::new(),
            render_profile: None,
            provenance: Provenance::default(),
            checker,
            host,
            store,
            clock,
            effects: Vec::new(),
            committed: None,
        }
    }

    /// Records something the session has to act on.
    ///
    /// Effects are recorded where the state actually changes rather than returned when the handler
    /// finishes, because a handler is dropped when its deadline expires. An effect for something
    /// that has already happened — a prompt the destination has taken, a term the lexicon now
    /// holds — has to survive that, or Riff loses its own record of it.
    fn record(&mut self, effect: ToolEffect) {
        self.effects.push(effect);
    }

    /// Records a result that is already true, whether or not the handler gets to finish.
    ///
    /// The deadline exists so a host that never answers cannot stall a turn, but past the point
    /// where the world has changed there is nothing left to give up on. Reporting a timeout there
    /// would tell the speaker their prompt did not go when it did.
    fn commit(&mut self, result: Json) {
        self.committed = Some(result);
    }

    /// The fidelity gate this runtime checks lines against.
    pub fn checker(&self) -> &GroundingChecker {
        &self.checker
    }

    /// Canonical spellings to bias transcription toward, capped the way the bundle says.
    pub fn vocabulary(&self) -> Vec<String> {
        match &self.bundle.session.transcription.biasing {
            Some(biasing) if biasing.enabled => self.lexicon.keywords(biasing.max_keywords),
            _ => Vec::new(),
        }
    }

    /// A reference id no resolved reference is using.
    ///
    /// Counting is not enough: the count grows as a batch is stored, so offsetting it by the batch
    /// index skips ids and then collides with a skipped one on the next call. An id that changes
    /// what it points at between being reported and being attached puts the wrong thing in the
    /// prompt's context.
    fn next_reference_id(&self) -> String {
        (1..)
            .map(|n| format!("r{n}"))
            .find(|id| !self.references.contains_key(id))
            .expect("the range is unbounded")
    }

    fn render(&self) -> RenderOptions<'_> {
        RenderOptions {
            config: &self.bundle.render,
            profile: self.render_profile.as_deref(),
        }
    }

    /// Builds the artifact for a take as it stands.
    pub fn artifact_for(&self, take: &Take) -> Result<PromptArtifact> {
        build_artifact(
            take,
            BuildArtifactOptions {
                render: self.render(),
                lexicon: &self.lexicon,
                utterance_count: self.ledger.len(),
                now: &self.clock.now(),
                provenance: self.provenance.clone(),
            },
        )
    }

    /// The artifact for whichever take is active, if any.
    pub fn active_artifact(&self) -> Option<PromptArtifact> {
        let take = self.book.get(self.book.active_id()?)?;
        self.artifact_for(take).ok()
    }

    /// Runs one tool call, bounded by the deadline the bundle sets.
    pub async fn dispatch(&mut self, name: &str, arguments_json: &str) -> ToolOutcome {
        let started = self.clock.monotonic_ms();
        let elapsed = |runtime: &Self| runtime.clock.monotonic_ms().saturating_sub(started);

        let Some(definition) = self.bundle.tool(name) else {
            return ToolOutcome::error(format!("unknown tool \"{name}\""), elapsed(self));
        };

        let parsed = match Json::parse(arguments_json) {
            Ok(parsed) => parsed,
            Err(error) => {
                return ToolOutcome::error(
                    format!("arguments were not valid JSON: {error}"),
                    elapsed(self),
                );
            }
        };

        let validated = validate(&definition.parameters, &parsed);
        if !validated.valid {
            return ToolOutcome {
                ok: false,
                result: Json::Object(json_object! {
                    "error" => "invalid arguments",
                    "details" => validated.errors,
                }),
                duration_ms: elapsed(self),
                effects: Vec::new(),
            };
        }

        let timeout_ms = self.bundle.session.limits.tool_timeout_ms;
        let clock = Arc::clone(&self.clock);
        let outcome = with_deadline(
            timeout_ms,
            clock.as_ref(),
            Box::pin(self.run(name, &validated.value)),
        )
        .await;

        // Drained whatever the outcome: a handler abandoned at its deadline may already have
        // changed state the session has to hear about.
        let effects = std::mem::take(&mut self.effects);
        let committed = self.committed.take();
        let duration_ms = elapsed(self);

        match outcome {
            Some(Ok(result)) => ToolOutcome {
                ok: true,
                result,
                duration_ms,
                effects,
            },
            Some(Err(error)) => ToolOutcome {
                effects,
                ..ToolOutcome::error(error.to_string(), duration_ms)
            },
            // A handler that committed a result before running out of time reports it. Saying the
            // tool never answered would tell the model a prompt the destination has already taken
            // did not go.
            None => match committed {
                Some(result) => ToolOutcome {
                    ok: true,
                    result,
                    duration_ms,
                    effects,
                },
                None => ToolOutcome {
                    effects,
                    ..ToolOutcome::error(
                        format!(
                            "{name} did not answer within {timeout_ms}ms; tell them it is not responding"
                        ),
                        duration_ms,
                    )
                },
            },
        }
    }

    async fn run(&mut self, name: &str, args: &Json) -> Result<Json> {
        match name {
            "draft_update" => self.draft_update(args),
            "read_draft" => self.read_draft(args),
            "resolve_reference" => self.resolve_reference(args).await,
            "lookup_term" => self.lookup_term(args).await,
            "record_term" => self.record_term(args).await,
            "recall_prompts" => self.recall_prompts(args).await,
            "motifs" => self.motifs(args).await,
            "takes" => self.takes(args),
            "submit_prompt" => self.submit_prompt(args).await,
            other => Err(RiffError::tool(format!(
                "tool \"{other}\" has no implementation in this build"
            ))),
        }
    }

    // MARK: - Draft

    fn draft_update(&mut self, args: &Json) -> Result<Json> {
        let take_id = self.resolve_take_id(args.get_str("take_id"))?;
        self.require_open(&take_id)?;

        let operations: Vec<DraftOperation> = args
            .get("operations")
            .map(Json::array_or_empty)
            .unwrap_or(&[])
            .iter()
            .map(DraftOperation::from_json)
            .collect();

        let now = self.clock.now();
        let spans = self.ledger.spans(&self.lexicon).to_vec();
        let mut take = self
            .book
            .get(&take_id)
            .cloned()
            .ok_or_else(|| RiffError::tool(format!("no take \"{take_id}\"")))?;

        let result = apply_draft_operations(
            &mut take,
            &operations,
            &ApplyContext {
                checker: &self.checker,
                spans: &spans,
                lexicon: &self.lexicon,
                grounding: &self.bundle.grounding,
                references: &self.references,
                motifs: &self.motifs,
                now: &now,
            },
        );

        let accepted_any = !result.accepted.is_empty();
        *self.book.get_mut(&take_id).expect("take exists") = take;

        let take = self.book.get(&take_id).expect("take exists");
        let fidelity = self.artifact_for(take)?.provenance.fidelity;
        let view = self.draft_view(take, false)?;

        if accepted_any {
            self.record(ToolEffect::DraftChanged(take_id));
        }

        Ok(Json::Object(json_object! {
            "accepted" => Json::Array(result.accepted.iter().map(|outcome| outcome.to_json()).collect()),
            "rejected" => Json::Array(result.rejected.iter().map(|outcome| outcome.to_json()).collect()),
            "fidelity" => fidelity,
            "draft" => view,
        }))
    }

    fn read_draft(&mut self, args: &Json) -> Result<Json> {
        let take_id = self.resolve_take_id(args.get_str("take_id"))?;
        let include_rendered = args.get("include_rendered").and_then(Json::as_bool) == Some(true);

        let take = self
            .book
            .get(&take_id)
            .ok_or_else(|| RiffError::tool(format!("no take \"{take_id}\"")))?;
        let fidelity = self.artifact_for(take)?.provenance.fidelity;
        let gist = summarize_draft(take);
        let view = self.draft_view(take, include_rendered)?;

        let mut object = view.as_object().cloned().unwrap_or_default();
        object.insert("gist", Json::from(gist));
        object.insert("fidelity", Json::from(fidelity));
        Ok(Json::Object(object))
    }

    /// Compact view of the draft returned after every mutation, so the agent always knows line ids.
    fn draft_view(&self, take: &Take, include_rendered: bool) -> Result<Json> {
        let mut sections = JsonObject::new();
        for line in take.lines() {
            let entry = Json::Object(json_object! {
                "id" => line.id.clone(),
                "text" => line.text.clone(),
                "grounding" => line.grounding.kind.as_str(),
            });
            match sections.get(line.section.as_str()) {
                Some(Json::Array(existing)) => {
                    let mut existing = existing.clone();
                    existing.push(entry);
                    sections.insert(line.section.as_str(), Json::Array(existing));
                }
                _ => sections.insert(line.section.as_str(), Json::Array(vec![entry])),
            }
        }

        let mut object = json_object! { "take_id" => take.id.clone() };
        object.insert_some("label", take.label.clone().map(Json::from));
        object.insert(
            "title",
            take.title
                .as_ref()
                .map_or(Json::Null, |title| Json::from(title.text.clone())),
        );
        object.insert("sections", Json::Object(sections));
        object.insert(
            "context",
            Json::Array(
                take.context()
                    .iter()
                    .map(|item| {
                        let mut entry =
                            json_object! { "reference_id" => item.reference_id.clone() };
                        entry.insert(
                            "identifier",
                            Json::from(
                                item.identifier
                                    .clone()
                                    .unwrap_or_else(|| item.title.clone()),
                            ),
                        );
                        entry.insert_some("url", item.url.clone().map(Json::from));
                        Json::Object(entry)
                    })
                    .collect(),
            ),
        );
        object.insert("ready", Json::from(take.is_ready(&self.bundle.policy)));
        if include_rendered {
            object.insert("rendered", Json::from(render_prompt(take, self.render())?));
        }
        Ok(Json::Object(object))
    }

    // MARK: - Host-backed tools

    async fn resolve_reference(&mut self, args: &Json) -> Result<Json> {
        let phrase = args.get_str("phrase").unwrap_or_default().to_owned();
        let candidates = self
            .host
            .resolve_reference(ResolveReferenceRequest {
                phrase: phrase.clone(),
                kind: args
                    .get_str("kind")
                    .filter(|kind| *kind != "unknown")
                    .map(str::to_owned),
                recency: args.get_str("recency").map(str::to_owned),
                actor: args.get_str("actor").map(str::to_owned),
                limit: args.get("limit").and_then(Json::as_usize),
                transcript: Some(self.ledger.recent_text(8)),
            })
            .await?;

        let mut reported = Vec::new();
        for candidate in candidates {
            let reference_id = if candidate.reference_id.is_empty() {
                self.next_reference_id()
            } else {
                candidate.reference_id.clone()
            };
            let item = ContextItem {
                reference_id: reference_id.clone(),
                resolved_from: Some(phrase.clone()),
                ..candidate
            };

            let mut entry = json_object! {
                "reference_id" => reference_id.clone(),
                "kind" => item.kind.clone(),
                "title" => item.title.clone(),
            };
            entry.insert_some("identifier", item.identifier.clone().map(Json::from));
            entry.insert_some("url", item.url.clone().map(Json::from));
            entry.insert_some("actor", item.actor.clone().map(Json::from));
            entry.insert_some("timestamp", item.timestamp.clone().map(Json::from));
            entry.insert_some("state", item.state.clone().map(Json::from));
            entry.insert_some("confidence", item.confidence.map(Json::from));
            reported.push(Json::Object(entry));

            self.references.insert(reference_id, item);
        }

        let mut object = json_object! { "candidates" => Json::Array(reported.clone()) };
        if reported.is_empty() {
            object.insert(
                "note",
                Json::from("nothing matched; ask them which one they mean rather than guessing"),
            );
        }
        Ok(Json::Object(object))
    }

    async fn lookup_term(&mut self, args: &Json) -> Result<Json> {
        let heard = args.get_str("heard").unwrap_or_default().to_owned();
        let local = self.lexicon.lookup(&heard);
        let remote = self
            .host
            .lookup_term(LookupTermRequest {
                heard,
                context: args.get_str("context").map(str::to_owned),
                kind: args
                    .get_str("kind")
                    .filter(|kind| *kind != "unknown")
                    .map(str::to_owned),
            })
            .await?;

        let mut matches: Vec<Json> = local
            .iter()
            .map(|term| {
                let mut entry = term.to_json().as_object().cloned().unwrap_or_default();
                entry.insert("confidence", Json::from(1.0));
                entry.insert("source", Json::from("lexicon"));
                Json::Object(entry)
            })
            .collect();

        let seen: Vec<String> = local
            .iter()
            .map(|term| term.canonical.to_lowercase())
            .collect();
        for candidate in remote {
            if seen.contains(&candidate.term.canonical.to_lowercase()) {
                continue;
            }
            let mut entry = candidate
                .term
                .to_json()
                .as_object()
                .cloned()
                .unwrap_or_default();
            entry.insert_some("confidence", candidate.confidence.map(Json::from));
            entry.insert("source", Json::from("host"));
            matches.push(Json::Object(entry));
        }

        let mut object = json_object! { "matches" => Json::Array(matches.clone()) };
        if matches.is_empty() {
            object.insert(
                "note",
                Json::from("unknown here; if it matters, ask them what it is"),
            );
        }
        Ok(Json::Object(object))
    }

    async fn record_term(&mut self, args: &Json) -> Result<Json> {
        let canonical = tidy_whitespace(args.get_str("canonical").unwrap_or_default());
        if canonical.is_empty() {
            return Err(RiffError::tool("record_term needs a canonical spelling"));
        }

        // Aliases are applied to both sides of every grounding comparison, so one for a word that is
        // not really the same word lets an invented line match different spoken words. Similarity
        // does not establish sameness — "cache" and "cash" are one edit apart — so an alias only
        // becomes grounding-active when something outside this conversation confirms the term exists.
        let proposed: Vec<String> = args
            .get("heard_as")
            .map(Json::array_or_empty)
            .unwrap_or(&[])
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_owned))
            .collect();
        let (plausible, refused): (Vec<String>, Vec<String>) = proposed
            .into_iter()
            .partition(|heard| is_plausible_mishearing(heard, &canonical));

        // Deliberately not `lookup`: an uncorroborated term is still recorded, because it still
        // biases transcription, so asking `lookup` would let this call find what an identical
        // earlier call stored and treat the agent's own assertion as corroboration.
        let known = self.lexicon.is_corroborated(&canonical);
        let confirmed = known
            || self
                .host
                .lookup_term(LookupTermRequest {
                    heard: canonical.clone(),
                    ..LookupTermRequest::default()
                })
                .await?
                .iter()
                .any(|candidate| term_key(&candidate.term.canonical) == term_key(&canonical));

        let term = LexiconTerm {
            canonical: canonical.clone(),
            kind: args.get_str("kind").unwrap_or("other").to_owned(),
            heard_as: plausible.clone(),
            definition: args.get_str("definition").map(str::to_owned),
            scope: Some(args.get_str("scope").unwrap_or("user").to_owned()),
        };

        self.lexicon.add_with(term.clone(), confirmed);
        self.ledger.invalidate();
        // Recorded before the save: the vocabulary has already changed, so the provider has to be
        // told even if the save is what the deadline lands on.
        self.record(ToolEffect::LexiconChanged);
        if term.scope.as_deref() != Some("session") {
            self.store.save_term(term).await?;
        }

        let mut object = json_object! {
            "recorded" => canonical,
            "corrections" => if confirmed { plausible.len() } else { 0 },
        };
        if !refused.is_empty() {
            object.insert("refused", Json::from(refused));
            object.insert(
                "reason",
                Json::from(
                    "a correction has to be a mishearing of the same word; those are different words, so record the term without them",
                ),
            );
        }
        if !confirmed {
            object.insert(
                "note",
                Json::from(
                    "nothing here knows that term, so it will help transcription but cannot be used as a spelling correction; write what they actually said",
                ),
            );
        }

        Ok(Json::Object(object))
    }

    async fn recall_prompts(&mut self, args: &Json) -> Result<Json> {
        let prompts = self
            .host
            .recall_prompts(RecallPromptsRequest {
                query: args.get_str("query").unwrap_or_default().to_owned(),
                recency: args.get_str("recency").map(str::to_owned),
                status: args.get_str("status").map(str::to_owned),
                limit: args.get("limit").and_then(Json::as_usize),
            })
            .await?;

        Ok(Json::Object(json_object! {
            "prompts" => Json::Array(prompts.iter().map(|prompt| {
                    // Snake case because these are the keys the tool contract in
                    // `core/agent/tools/recall_prompts.json` names, and the model reads them.
                    let mut entry = json_object! {
                        "prompt_id" => prompt.prompt_id.clone(),
                        "title" => prompt.title.clone(),
                        "excerpt" => prompt.excerpt.clone(),
                    };
                    entry.insert_some("submitted_at", prompt.submitted_at.clone().map(Json::from));
                    entry.insert_some("status", prompt.status.clone().map(Json::from));
                    entry.insert_some("outcome", prompt.outcome.clone().map(Json::from));
                    entry.insert_some("url", prompt.url.clone().map(Json::from));
                Json::Object(entry)
            }).collect()),
        }))
    }

    // MARK: - Motifs

    async fn motifs(&mut self, args: &Json) -> Result<Json> {
        let action = args.get_str("action").unwrap_or_default();
        let motif_id = args.get_str("motif_id").map(str::to_owned);

        match action {
            "list" => {
                let mut active: Vec<&Motif> = self
                    .motifs
                    .values()
                    .filter(|motif| motif.retired_at.is_none())
                    .collect();
                active.sort_by(|a, b| a.id.cmp(&b.id));
                Ok(Json::Object(json_object! {
                    "motifs" => Json::Array(active.iter().map(|motif| {
                        let mut entry = json_object! {
                            "motif_id" => motif.id.clone(),
                            "text" => motif.text.clone(),
                            "scope" => motif.scope.clone(),
                        };
                        entry.insert_some("applies_when", motif.applies_when.clone().map(Json::from));
                        Json::Object(entry)
                    }).collect()),
                }))
            }

            "save" => {
                let text = tidy_whitespace(args.get_str("text").unwrap_or_default());
                if text.is_empty() {
                    return Err(RiffError::tool(
                        "save needs the text of the standing instruction",
                    ));
                }

                let spans = self.ledger.spans(&self.lexicon).to_vec();
                let grounding = self.checker.check(&text, &spans, &self.lexicon);
                if !grounding.ok {
                    return Ok(Json::Object(json_object! {
                        "saved" => false,
                        "reason" => format!(
                            "a motif has to be their wording; they did not say {}",
                            grounding
                                .unmatched_tokens
                                .iter()
                                .take(6)
                                .map(|token| format!("\"{token}\""))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    }));
                }

                let motif = Motif {
                    // Counting active motifs reuses an id after one is retired: retire m1, reload,
                    // and the next save is called m2 and silently replaces the existing m2.
                    id: next_motif_id(&self.motifs),
                    text,
                    scope: args.get_str("scope").unwrap_or("user").to_owned(),
                    applies_when: args.get_str("applies_when").map(str::to_owned),
                    created_at: self.clock.now(),
                    retired_at: None,
                };
                // Persisted before the session believes it. A store that fails, or a deadline that
                // drops this handler, would otherwise leave a motif that `list` and `attach` treat
                // as saved and that no later session has ever heard of.
                let id = motif.id.clone();
                self.store.save_motif(motif.clone()).await?;
                self.motifs.insert(id.clone(), motif);
                Ok(Json::Object(
                    json_object! { "saved" => true, "motif_id" => id },
                ))
            }

            "attach" => {
                let motif = motif_id
                    .as_ref()
                    .and_then(|id| self.motifs.get(id))
                    .cloned()
                    .ok_or_else(|| {
                        RiffError::tool(format!(
                            "no motif \"{}\"",
                            motif_id.as_deref().unwrap_or("(missing motif_id)")
                        ))
                    })?;
                if motif.retired_at.is_some() {
                    return Err(RiffError::tool(format!(
                        "motif \"{}\" was retired; they asked to stop using it",
                        motif.id
                    )));
                }

                let now = self.clock.now();
                let take_id = self.book.active(&now)?.id.clone();
                self.require_open(&take_id)?;

                let line_id;
                {
                    let take = self.book.get_mut(&take_id).expect("take exists");
                    if take
                        .lines()
                        .iter()
                        .any(|line| line.motif_id.as_deref() == Some(motif.id.as_str()))
                    {
                        return Ok(Json::Object(json_object! {
                            "attached" => false,
                            "reason" => "already on this take",
                        }));
                    }

                    let id = take.next_line_id();
                    let order = take.order_after(Section::Constraint, &crate::draft::Anchor::End);
                    take.set_line(Line {
                        id: id.clone(),
                        section: Section::Constraint,
                        text: motif.text.clone(),
                        order,
                        source_utterance_ids: Vec::new(),
                        motif_id: Some(motif.id.clone()),
                        supersedes: Vec::new(),
                        grounding: LineGrounding::full(GroundingKind::Motif),
                    });
                    take.updated_at = now;
                    line_id = id;
                }

                let take = self.book.get(&take_id).expect("take exists");
                let view = self.draft_view(take, false)?;
                self.record(ToolEffect::DraftChanged(take_id));
                Ok(Json::Object(json_object! {
                    "attached" => true,
                    "line_id" => line_id,
                    "draft" => view,
                }))
            }

            "detach" => {
                let now = self.clock.now();
                let take_id = self.book.active(&now)?.id.clone();
                self.require_open(&take_id)?;

                let take = self.book.get_mut(&take_id).expect("take exists");
                // A line carries a motif id only when it came from one, so a call with no
                // `motif_id` matches nothing rather than detaching an ordinary line.
                let line = take
                    .lines()
                    .into_iter()
                    .find(|line| line.motif_id.is_some() && line.motif_id == motif_id);
                let Some(line) = line else {
                    return Ok(Json::Object(
                        json_object! { "detached" => false, "reason" => "not on this take" },
                    ));
                };
                take.remove_line(&line.id);
                take.updated_at = now;
                self.record(ToolEffect::DraftChanged(take_id));
                Ok(Json::Object(json_object! { "detached" => true }))
            }

            "retire" => {
                let id = motif_id.ok_or_else(|| RiffError::tool("retire needs motif_id"))?;
                let at = self.clock.now();
                if !self.motifs.contains_key(&id) {
                    return Err(RiffError::tool(format!("no motif \"{id}\"")));
                }
                // Same ordering as `save`, for the same reason: a motif that vanished from this
                // session while the store still has it would come back on the next one, having
                // been reported as retired.
                self.store.retire_motif(id.clone(), at.clone()).await?;
                if let Some(motif) = self.motifs.get_mut(&id) {
                    motif.retired_at = Some(at);
                }
                Ok(Json::Object(json_object! { "retired" => true }))
            }

            other => Err(RiffError::tool(format!(
                "unknown motifs action \"{other}\""
            ))),
        }
    }

    // MARK: - Takes

    fn takes(&mut self, args: &Json) -> Result<Json> {
        let action = args.get_str("action").unwrap_or_default();
        let take_id = args.get_str("take_id").map(str::to_owned);
        let now = self.clock.now();

        match action {
            "new" => {
                let carry = args.get("carry_context").and_then(Json::as_bool) == Some(true);
                let previous = if carry {
                    self.book.active_id().map(str::to_owned)
                } else {
                    None
                };
                let label = args.get_str("label").map(str::to_owned);
                let id = self.book.create(label, previous.as_deref(), &now)?;
                let take = self.book.get(&id).expect("just created");
                let result = Json::Object(json_object! {
                    "take_id" => id.clone(),
                    "label" => take.label.clone().map_or(Json::Null, Json::from),
                    "active" => true,
                });
                self.record(ToolEffect::TakeChanged(Some(id)));
                Ok(result)
            }

            "switch" => {
                let id = take_id.ok_or_else(|| RiffError::tool("switch needs take_id"))?;
                if self.book.switch_to(&id)?.is_none() {
                    return Err(RiffError::tool(format!("no take \"{id}\"")));
                }
                let take = self.book.get(&id).expect("just switched");
                let view = self.draft_view(take, false)?;
                let result =
                    Json::Object(json_object! { "take_id" => id.clone(), "draft" => view });
                self.record(ToolEffect::TakeChanged(Some(id)));
                Ok(result)
            }

            "park" => {
                let id = take_id
                    .or_else(|| self.book.active_id().map(str::to_owned))
                    .ok_or_else(|| RiffError::tool("there is no take to park"))?;
                if !self.book.park(&id)? {
                    return Err(RiffError::tool(format!("no take \"{id}\"")));
                }
                let active = self.book.active_id().map(str::to_owned);
                self.record(ToolEffect::TakeChanged(active));
                Ok(Json::Object(json_object! { "parked" => id }))
            }

            "list" => Ok(Json::Object(json_object! {
                "takes" => Json::Array(self.book.takes().iter().map(|take| {
                    Json::Object(json_object! {
                        "take_id" => take.id.clone(),
                        "label" => take.label.clone().map_or(Json::Null, Json::from),
                        "status" => take.status.as_str(),
                        "active" => self.book.active_id() == Some(take.id.as_str()),
                        "title" => take.title.as_ref().map_or(Json::Null, |title| Json::from(title.text.clone())),
                        "lines" => take.lines().len(),
                        "updated_at" => take.updated_at.clone(),
                    })
                }).collect()),
            })),

            "discard" => {
                let id = take_id
                    .or_else(|| self.book.active_id().map(str::to_owned))
                    .ok_or_else(|| RiffError::tool("there is no take to discard"))?;
                if !self.book.discard(&id)? {
                    return Err(RiffError::tool(format!("no take \"{id}\"")));
                }
                let active = self.book.active_id().map(str::to_owned);
                self.record(ToolEffect::TakeChanged(active));
                Ok(Json::Object(json_object! { "discarded" => id }))
            }

            other => Err(RiffError::tool(format!("unknown takes action \"{other}\""))),
        }
    }

    // MARK: - Submission

    async fn submit_prompt(&mut self, args: &Json) -> Result<Json> {
        let take_id = self.resolve_take_id(args.get_str("take_id"))?;
        self.require_open(&take_id)?;

        let target = args.get_str("target").map(str::to_owned);
        let keep_open = args.get("keep_open").and_then(Json::as_bool) == Some(true);

        let missing: Vec<&str> = {
            let take = self.book.get(&take_id).expect("take exists");
            self.bundle
                .policy
                .readiness_requires
                .iter()
                .filter(|section| take.lines_in(**section).is_empty())
                .map(|section| section.as_str())
                .collect()
        };
        if !missing.is_empty() {
            return Ok(Json::Object(json_object! {
                "submitted" => false,
                "reason" => format!(
                    "nothing to send yet: no {} captured. Ask them what they want done.",
                    missing.join(" or ")
                ),
            }));
        }

        {
            let take = self.book.get_mut(&take_id).expect("take exists");
            if let Some(target) = &target {
                take.target = Some(target.clone());
            }
        }

        // The take stays `drafting` across the await, and only the artifact the host receives is
        // marked `ready`. Moving the take first would strand it there when this handler is dropped
        // at its deadline: nothing was sent, but the take would read as though it had been.
        let mut artifact = self.artifact_for(self.book.get(&take_id).expect("take exists"))?;
        artifact.status = Some(TakeStatus::Ready);

        let result = self
            .host
            .submit_prompt(
                artifact.clone(),
                SubmitOptions {
                    target: target.clone(),
                    keep_open,
                },
            )
            .await;

        let result = match result {
            Ok(result) => result,
            // No rollback needed: the take was never moved, so a failure leaves it exactly as the
            // speaker left it — including parked, which a failed send has no business changing.
            Err(error) => return Err(error.into()),
        };

        if !result.submitted {
            return Ok(submit_report(&result, &artifact.id, None));
        }

        // The destination now has the prompt, so everything that records that fact happens before
        // the next await. A handler dropped at its deadline part way through this would leave a
        // sent take active, and the next thing spoken would land inside a prompt already gone out.
        let status = if keep_open {
            TakeStatus::Drafting
        } else {
            TakeStatus::Submitted
        };
        self.book.get_mut(&take_id).expect("take exists").status = status;

        let stored = PromptArtifact {
            status: Some(status),
            submitted_at: Some(self.clock.now()),
            ..artifact.clone()
        };
        self.record(ToolEffect::Submitted(Box::new(stored.clone())));

        if !keep_open {
            self.book.clear_active();
            self.record(ToolEffect::TakeChanged(None));
        }

        // Committed before the save for the same reason the state changes are: whether the local
        // copy is kept has no bearing on whether the prompt went, and a deadline landing on the save
        // must not turn a delivered prompt into a reported failure the speaker would act on.
        self.commit(submit_report(&result, &artifact.id, None));

        // Reporting a failed save as a failed submission would invite a retry that sends it twice,
        // so the send is reported as what it is.
        let store_warning = match self.store.save_artifact(stored).await {
            Ok(()) => None,
            Err(error) => Some(format!("it was sent, but saving a copy failed: {error}")),
        };

        Ok(submit_report(&result, &artifact.id, store_warning))
    }

    // MARK: - Internals

    fn resolve_take_id(&mut self, take_id: Option<&str>) -> Result<String> {
        match take_id {
            Some(id) => self
                .book
                .get(id)
                .map(|take| take.id.clone())
                .ok_or_else(|| RiffError::tool(format!("no take \"{id}\""))),
            None => {
                let now = self.clock.now();
                Ok(self.book.active(&now)?.id.clone())
            }
        }
    }

    /// A take that has been sent or thrown away is finished, and naming it explicitly does not
    /// reopen it. Without this, later speech could still be written into a prompt that has already
    /// gone out.
    fn require_open(&self, take_id: &str) -> Result<()> {
        let take = self
            .book
            .get(take_id)
            .ok_or_else(|| RiffError::tool(format!("no take \"{take_id}\"")))?;
        if take.is_terminal() {
            return Err(RiffError::tool(format!(
                "take \"{take_id}\" was already {}; start a new one for anything further",
                take.status.as_str()
            )));
        }
        Ok(())
    }
}

fn submit_report(
    result: &crate::host::SubmitResult,
    fallback_id: &str,
    store_warning: Option<String>,
) -> Json {
    let mut object = json_object! {
        "submitted" => result.submitted,
        "prompt_id" => result.prompt_id.clone().unwrap_or_else(|| fallback_id.to_owned()),
    };
    object.insert_some("destination", result.destination.clone().map(Json::from));
    object.insert_some("url", result.url.clone().map(Json::from));
    object.insert_some("message", result.message.clone().map(Json::from));
    object.insert_some("warning", store_warning.map(Json::from));
    Json::Object(object)
}

/// A motif id that no existing or retired motif is using.
fn next_motif_id(motifs: &HashMap<String, Motif>) -> String {
    let highest = motifs
        .keys()
        .filter_map(|id| id.strip_prefix('m'))
        .filter_map(|digits| digits.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("m{}", highest + 1)
}
