//! The vocabulary the whole engine is written in: sections, grounding results, lines, takes,
//! artifacts, and the configuration the agent bundle carries.

use crate::json::Json;
use crate::json_object;

/// Sections of a prompt, ordered by how much they matter to the downstream agent.
pub const SECTIONS: [Section; 5] = [
    Section::Intent,
    Section::Detail,
    Section::Constraint,
    Section::Acceptance,
    Section::OpenQuestion,
];

/// One part of a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    /// What they want done.
    Intent,
    /// What else matters about it.
    Detail,
    /// What must not change.
    Constraint,
    /// How they will know it worked.
    Acceptance,
    /// What is still undecided.
    OpenQuestion,
}

impl Section {
    /// Parses the wire name, which is what `core/agent` and the conformance cases use.
    pub fn parse(value: &str) -> Option<Section> {
        match value {
            "intent" => Some(Section::Intent),
            "detail" => Some(Section::Detail),
            "constraint" => Some(Section::Constraint),
            "acceptance" => Some(Section::Acceptance),
            "open_question" => Some(Section::OpenQuestion),
            _ => None,
        }
    }

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Section::Intent => "intent",
            Section::Detail => "detail",
            Section::Constraint => "constraint",
            Section::Acceptance => "acceptance",
            Section::OpenQuestion => "open_question",
        }
    }

    /// Reading order, which is the order sections appear in a rendered prompt.
    pub fn order(self) -> usize {
        SECTIONS
            .iter()
            .position(|section| *section == self)
            .unwrap_or(SECTIONS.len())
    }
}

/// How a line in the prompt relates to what the speaker actually said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundingKind {
    /// Word for word what they said.
    Verbatim,
    /// Their words with filler, false starts, or whole sentences deleted.
    Trimmed,
    /// Their words with a transcription mistake fixed from the lexicon.
    Corrected,
    /// A standing instruction they set up earlier, not something said in this session.
    Motif,
    /// Not held to their words. Only ever a title, never a line of the body.
    Derived,
}

impl GroundingKind {
    /// The wire name, as it appears in the artifact.
    pub fn as_str(self) -> &'static str {
        match self {
            GroundingKind::Verbatim => "verbatim",
            GroundingKind::Trimmed => "trimmed",
            GroundingKind::Corrected => "corrected",
            GroundingKind::Motif => "motif",
            GroundingKind::Derived => "derived",
        }
    }
}

/// How strictly a section is held to the speaker's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundingMode {
    /// Only their words.
    Strict,
    /// Their words, or a motif they set up earlier.
    MotifOrStrict,
    /// Not held to their words.
    Derived,
}

impl GroundingMode {
    /// Parses the wire name, defaulting to the strictest reading of an unknown one.
    pub fn parse(value: &str) -> GroundingMode {
        match value {
            "motif-or-strict" => GroundingMode::MotifOrStrict,
            "derived" => GroundingMode::Derived,
            _ => GroundingMode::Strict,
        }
    }
}

/// Where an utterance came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtteranceSource {
    /// Transcribed speech.
    Speech,
    /// Typed input, which counts as something they said.
    Typed,
}

impl UtteranceSource {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            UtteranceSource::Speech => "speech",
            UtteranceSource::Typed => "typed",
        }
    }
}

/// One thing the speaker said, as the transcriber produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    /// Identity within the session, referenced by every line grounded in it.
    pub id: String,
    /// Exactly what the transcriber produced, before any correction.
    pub text: String,
    /// When it was captured.
    pub at: String,
    /// Whether it was spoken or typed.
    pub source: UtteranceSource,
    /// Transcriber confidence, when the provider reports one.
    pub confidence: Option<f64>,
}

/// Vocabulary the transcriber gets wrong, and how it is really spelled.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LexiconTerm {
    /// The correct spelling.
    pub canonical: String,
    /// What sort of thing it is: a product, a person, an acronym, and so on.
    pub kind: String,
    /// Ways transcription renders this term incorrectly.
    pub heard_as: Vec<String>,
    /// What it means here, for the agent rather than for the prompt.
    pub definition: Option<String>,
    /// How long the term should live: `session`, `user`, or `workspace`.
    pub scope: Option<String>,
}

impl LexiconTerm {
    /// A term with just the two fields every term needs.
    pub fn new(canonical: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            canonical: canonical.into(),
            kind: kind.into(),
            ..Self::default()
        }
    }

    /// Reads a term as `core/agent`, a host, and the conformance cases write one.
    pub fn from_json(value: &Json) -> Self {
        Self {
            canonical: value.get_str("canonical").unwrap_or_default().to_owned(),
            kind: value.get_str("kind").unwrap_or("other").to_owned(),
            heard_as: value
                .get("heardAs")
                .map(|heard| {
                    heard
                        .array_or_empty()
                        .iter()
                        .filter_map(|entry| entry.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            definition: value.get_str("definition").map(str::to_owned),
            scope: value.get_str("scope").map(str::to_owned),
        }
    }

    /// Writes the term the way a tool result reports it.
    pub fn to_json(&self) -> Json {
        let mut object = json_object! {
            "canonical" => self.canonical.clone(),
            "kind" => self.kind.clone(),
        };
        if !self.heard_as.is_empty() {
            object.insert("heardAs", Json::from(self.heard_as.clone()));
        }
        object.insert_some("definition", self.definition.clone().map(Json::from));
        object.insert_some("scope", self.scope.clone().map(Json::from));
        Json::Object(object)
    }
}

/// What the grounding check decided about one candidate line.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundingResult {
    /// Whether the line may go into the prompt.
    pub ok: bool,
    /// Share of the candidate's meaningful tokens recoverable from a span of what the speaker said.
    pub ratio: f64,
    /// How the line relates to what they said.
    pub kind: GroundingKind,
    /// Which utterances the match drew on.
    pub source_utterance_ids: Vec<String>,
    /// Meaningful candidate tokens with no source. These are the words the agent invented.
    pub unmatched_tokens: Vec<String>,
    /// Set when the line failed for a reason the token comparison cannot express.
    pub reason: Option<String>,
}

/// How a stored line relates to what the speaker said.
#[derive(Debug, Clone, PartialEq)]
pub struct LineGrounding {
    /// Share of the line that is provably theirs.
    pub ratio: f64,
    /// How the line relates to what they said.
    pub kind: GroundingKind,
    /// Meaningful tokens with no source, when there were any.
    pub unmatched_tokens: Vec<String>,
}

impl LineGrounding {
    /// A line that is theirs outright, used for motifs and for rendering fixtures.
    pub fn full(kind: GroundingKind) -> Self {
        Self {
            ratio: 1.0,
            kind,
            unmatched_tokens: Vec::new(),
        }
    }

    fn to_json(&self) -> Json {
        let mut object = json_object! {
            "ratio" => self.ratio,
            "kind" => self.kind.as_str(),
        };
        if !self.unmatched_tokens.is_empty() {
            object.insert("unmatchedTokens", Json::from(self.unmatched_tokens.clone()));
        }
        Json::Object(object)
    }
}

/// One line of the prompt body.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Identity within the take, which the agent uses to revise or move the line.
    pub id: String,
    /// Which part of the prompt it belongs to.
    pub section: Section,
    /// The text as it will appear.
    pub text: String,
    /// Position within the section.
    pub order: f64,
    /// Which utterances it was grounded in.
    pub source_utterance_ids: Vec<String>,
    /// The standing instruction it came from, when it is one.
    pub motif_id: Option<String>,
    /// Lines this one replaced.
    pub supersedes: Vec<String>,
    /// How it relates to what they said.
    pub grounding: LineGrounding,
}

impl Line {
    fn to_json(&self) -> Json {
        let mut object = json_object! {
            "id" => self.id.clone(),
            "section" => self.section.as_str(),
            "text" => self.text.clone(),
            "order" => self.order,
            "sourceUtteranceIds" => Json::from(self.source_utterance_ids.clone()),
        };
        object.insert_some("motifId", self.motif_id.clone().map(Json::from));
        if !self.supersedes.is_empty() {
            object.insert("supersedes", Json::from(self.supersedes.clone()));
        }
        object.insert("grounding", self.grounding.to_json());
        Json::Object(object)
    }
}

/// Something in the speaker's world that the prompt refers to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextItem {
    /// Identity the agent attaches by, handed out when the reference was resolved.
    pub reference_id: String,
    /// What sort of thing it is: a pull request, a commit, a URL, and so on.
    pub kind: String,
    /// What it is called.
    pub title: String,
    /// How the speaker's world names it, such as `acme/web#412`.
    pub identifier: Option<String>,
    /// Where to find it.
    pub url: Option<String>,
    /// Who it belongs to.
    pub actor: Option<String>,
    /// When it happened.
    pub timestamp: Option<String>,
    /// What state it is in, such as `open`.
    pub state: Option<String>,
    /// A short account of it, for the agent rather than for the prompt.
    pub summary: Option<String>,
    /// The phrase the speaker used, so the prompt and the resolved thing stay connected.
    pub resolved_from: Option<String>,
    /// How sure the host is that this is the thing they meant.
    pub confidence: Option<f64>,
}

impl ContextItem {
    fn to_json(&self) -> Json {
        let mut object = json_object! {
            "referenceId" => self.reference_id.clone(),
            "kind" => self.kind.clone(),
            "title" => self.title.clone(),
        };
        object.insert_some("identifier", self.identifier.clone().map(Json::from));
        object.insert_some("url", self.url.clone().map(Json::from));
        object.insert_some("actor", self.actor.clone().map(Json::from));
        object.insert_some("timestamp", self.timestamp.clone().map(Json::from));
        object.insert_some("state", self.state.clone().map(Json::from));
        object.insert_some("summary", self.summary.clone().map(Json::from));
        object.insert_some("resolvedFrom", self.resolved_from.clone().map(Json::from));
        object.insert_some("confidence", self.confidence.map(Json::from));
        Json::Object(object)
    }
}

/// A standing instruction the speaker set up once and wants applied from then on.
#[derive(Debug, Clone, PartialEq)]
pub struct Motif {
    /// Identity, never reused even after the motif is retired.
    pub id: String,
    /// Their wording of the instruction.
    pub text: String,
    /// `user` or `workspace`.
    pub scope: String,
    /// When it applies, in their words.
    pub applies_when: Option<String>,
    /// When it was set up.
    pub created_at: String,
    /// When they asked to stop using it.
    pub retired_at: Option<String>,
}

/// Where a take is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeStatus {
    /// Being spoken into.
    Drafting,
    /// Has everything the policy requires.
    Ready,
    /// Sent. Terminal.
    Submitted,
    /// Set aside, and can be come back to.
    Parked,
    /// Thrown away. Terminal.
    Discarded,
}

impl TakeStatus {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            TakeStatus::Drafting => "drafting",
            TakeStatus::Ready => "ready",
            TakeStatus::Submitted => "submitted",
            TakeStatus::Parked => "parked",
            TakeStatus::Discarded => "discarded",
        }
    }
}

/// One tool call, recorded for the artifact's provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    /// Which tool.
    pub name: String,
    /// When it was called.
    pub at: String,
    /// How long it took.
    pub duration_ms: Option<u64>,
    /// Whether it succeeded.
    pub ok: Option<bool>,
}

/// Where a prompt came from, so a consumer can tell at a glance whether it was captured or composed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Provenance {
    /// Token-weighted share of the body that is provably the speaker's.
    pub fidelity: f64,
    /// How much was said in the session.
    pub utterance_count: usize,
    /// How many tokens the body holds.
    pub body_tokens: usize,
    /// How many of them the agent, rather than the speaker, is responsible for.
    pub agent_authored_tokens: i64,
    /// Which version of the agent captured it.
    pub agent_version: Option<String>,
    /// Which build of the agent bundle.
    pub bundle_revision: Option<String>,
    /// Which speech provider.
    pub provider_id: Option<String>,
    /// Which model the provider negotiated.
    pub model: Option<String>,
    /// The provider's session identity.
    pub session_id: Option<String>,
    /// How long the session had been running.
    pub duration_ms: Option<u64>,
    /// Every tool the agent called.
    pub tool_calls: Vec<ToolCallRecord>,
}

/// The origin of a prompt's title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleOrigin {
    /// Built from the first line when the agent never set one.
    Derived,
    /// Set by the agent from words the speaker used.
    Spoken,
}

impl TitleOrigin {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            TitleOrigin::Derived => "derived",
            TitleOrigin::Spoken => "spoken",
        }
    }
}

/// What a prompt is called.
#[derive(Debug, Clone, PartialEq)]
pub struct Title {
    /// The text.
    pub text: String,
    /// Whether it came from their words or was built from the first line.
    pub origin: TitleOrigin,
}

/// A term as the artifact reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactTerm {
    /// The correct spelling.
    pub canonical: String,
    /// What sort of thing it is.
    pub kind: String,
    /// How transcription renders it incorrectly.
    pub heard_as: Vec<String>,
    /// What it means here.
    pub definition: Option<String>,
}

/// The finished prompt, and everything a consumer needs to trust it.
///
/// This is the output contract in [`core/schema/prompt-artifact.schema.json`]; every field here has
/// a counterpart there.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptArtifact {
    /// Identity of this build of the take.
    pub id: String,
    /// Which take it was built from.
    pub take_id: String,
    /// What the speaker called this line of thinking.
    pub label: Option<String>,
    /// When the take was started.
    pub created_at: String,
    /// When it was last edited.
    pub updated_at: String,
    /// What the prompt is called.
    pub title: Title,
    /// The body, in reading order.
    pub lines: Vec<Line>,
    /// What it refers to.
    pub context: Vec<ContextItem>,
    /// Vocabulary that appears in the body, so a consumer can read it the way it was meant.
    pub terms: Vec<ArtifactTerm>,
    /// Where it came from.
    pub provenance: Provenance,
    /// The Markdown the downstream agent receives.
    pub rendered: String,
    /// Where it is going.
    pub target: Option<String>,
    /// Where the take stood when this was built.
    pub status: Option<TakeStatus>,
    /// When the host confirmed the send. Distinct from `updated_at`, which tracks edits.
    pub submitted_at: Option<String>,
}

impl PromptArtifact {
    /// Writes the artifact in the shape [`core/schema/prompt-artifact.schema.json`] specifies, which
    /// is what a host stores and what a consumer reads.
    pub fn to_json(&self) -> Json {
        let mut object = json_object! { "id" => self.id.clone(), "takeId" => self.take_id.clone() };
        object.insert_some("label", self.label.clone().map(Json::from));
        object.insert("createdAt", Json::from(self.created_at.clone()));
        object.insert("updatedAt", Json::from(self.updated_at.clone()));
        object.insert(
            "title",
            Json::Object(json_object! {
                "text" => self.title.text.clone(),
                "origin" => self.title.origin.as_str(),
            }),
        );
        object.insert(
            "lines",
            Json::Array(self.lines.iter().map(Line::to_json).collect()),
        );
        object.insert(
            "context",
            Json::Array(self.context.iter().map(ContextItem::to_json).collect()),
        );
        if !self.terms.is_empty() {
            object.insert(
                "terms",
                Json::Array(
                    self.terms
                        .iter()
                        .map(|term| {
                            let mut entry = json_object! {
                                "canonical" => term.canonical.clone(),
                                "kind" => term.kind.clone(),
                            };
                            if !term.heard_as.is_empty() {
                                entry.insert("heardAs", Json::from(term.heard_as.clone()));
                            }
                            entry
                                .insert_some("definition", term.definition.clone().map(Json::from));
                            Json::Object(entry)
                        })
                        .collect(),
                ),
            );
        }
        object.insert("provenance", self.provenance.to_json());
        object.insert("rendered", Json::from(self.rendered.clone()));
        object.insert_some("target", self.target.clone().map(Json::from));
        object.insert_some(
            "status",
            self.status.map(|status| Json::from(status.as_str())),
        );
        object.insert_some("submittedAt", self.submitted_at.clone().map(Json::from));
        Json::Object(object)
    }
}

impl Provenance {
    fn to_json(&self) -> Json {
        let mut object = json_object! {
            "fidelity" => self.fidelity,
            "utteranceCount" => self.utterance_count,
            "bodyTokens" => self.body_tokens,
            "agentAuthoredTokens" => self.agent_authored_tokens,
        };
        object.insert_some("agentVersion", self.agent_version.clone().map(Json::from));
        object.insert_some(
            "bundleRevision",
            self.bundle_revision.clone().map(Json::from),
        );
        object.insert_some("providerId", self.provider_id.clone().map(Json::from));
        object.insert_some("model", self.model.clone().map(Json::from));
        object.insert_some("sessionId", self.session_id.clone().map(Json::from));
        object.insert_some("durationMs", self.duration_ms.map(Json::from));
        if !self.tool_calls.is_empty() {
            object.insert(
                "toolCalls",
                Json::Array(
                    self.tool_calls
                        .iter()
                        .map(|call| {
                            let mut entry = json_object! {
                                "name" => call.name.clone(),
                                "at" => call.at.clone(),
                            };
                            entry.insert_some("durationMs", call.duration_ms.map(Json::from));
                            entry.insert_some("ok", call.ok.map(Json::from));
                            Json::Object(entry)
                        })
                        .collect(),
                ),
            );
        }
        Json::Object(object)
    }
}

/// The thresholds and vocabulary the grounding check runs on.
#[derive(Debug, Clone)]
pub struct GroundingConfig {
    /// Share of a body line that must be recoverable from what they said.
    pub threshold: f64,
    /// The looser bar a title is held to, because a title is a label rather than part of the request.
    pub title_threshold: Option<f64>,
    /// How many consecutive utterances one span may cover.
    pub window_size: usize,
    /// How strictly each section is held to their words.
    pub sections: Vec<(Section, GroundingMode)>,
    /// Connective words that may be added, because they carry no meaning of their own.
    pub free_tokens: Vec<String>,
    /// Words that may be dropped, because dropping them is a deletion rather than a rewrite.
    pub filler: Vec<String>,
}

impl GroundingConfig {
    /// How strictly `section` is held to the speaker's words.
    pub fn mode_for(&self, section: Section) -> GroundingMode {
        self.sections
            .iter()
            .find(|(candidate, _)| *candidate == section)
            .map_or(GroundingMode::Strict, |(_, mode)| *mode)
    }
}

/// How one section becomes Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderDisposition {
    /// A top-level heading.
    H1,
    /// Sentences joined into a paragraph.
    Paragraphs,
    /// A bold label above a bulleted list.
    LabeledList,
    /// A second-level heading above a bulleted list.
    SectionList,
    /// Left out entirely.
    Omit,
    /// One line each, for a profile that names no disposition.
    Lines,
}

impl RenderDisposition {
    /// Parses the wire name. An unknown disposition falls back to one line each, which is what the
    /// other bindings do rather than failing a render over a profile they do not recognize.
    pub fn parse(value: &str) -> RenderDisposition {
        match value {
            "h1" => RenderDisposition::H1,
            "paragraphs" => RenderDisposition::Paragraphs,
            "labeled-list" => RenderDisposition::LabeledList,
            "section-list" => RenderDisposition::SectionList,
            "omit" => RenderDisposition::Omit,
            _ => RenderDisposition::Lines,
        }
    }
}

/// How a take becomes the Markdown the downstream agent receives.
#[derive(Debug, Clone)]
pub struct RenderConfig {
    /// The profile used when a host names none.
    pub profile: String,
    /// Every profile: a name, then a disposition per section.
    pub profiles: Vec<(String, Vec<(String, RenderDisposition)>)>,
    /// What each section is called in the rendered prompt.
    pub labels: Vec<(String, String)>,
    /// Whether a rendered prompt carries a provenance footer.
    pub include_provenance_footer: bool,
}

impl RenderConfig {
    /// The dispositions of a named profile.
    pub fn profile(&self, name: &str) -> Option<&[(String, RenderDisposition)]> {
        self.profiles
            .iter()
            .find(|(profile, _)| profile == name)
            .map(|(_, dispositions)| dispositions.as_slice())
    }

    /// What a section is called, falling back to its own name in title case.
    pub fn label(&self, key: &str) -> String {
        self.labels
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| title_case(key))
    }
}

fn title_case(value: &str) -> String {
    let spaced = value.replace('_', " ");
    let mut characters = spaced.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => spaced,
    }
}

/// What the agent is and is not allowed to do with a take.
#[derive(Debug, Clone)]
pub struct PolicyConfig {
    /// How many takes may be open at once.
    pub max_takes: usize,
    /// Sections a prompt must have before it can be sent.
    pub readiness_requires: Vec<Section>,
    /// Always false. The speaker decides when a prompt is sent.
    pub auto_submit: bool,
    /// How much of the draft to read back by default.
    pub readback_default: String,
    /// Always false. The transcript is kept because the prompt is built from it; the audio is not.
    pub persist_audio: bool,
    /// Whether credentials are stripped on the way into the ledger.
    pub redact_secrets_from_transcript: bool,
}

/// One tool the agent may call.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// What the model calls it.
    pub name: String,
    /// `local` runs inside Riff; `host` is delegated to the embedding application.
    pub kind: String,
    /// What it does, as the model reads it.
    pub description: String,
    /// JSON Schema for the arguments.
    pub parameters: Json,
    /// JSON Schema for the result, when the definition states one.
    pub returns: Option<Json>,
    /// Which file in `core/agent/tools` it was compiled from.
    pub source: Option<String>,
}

/// One part of the composed system prompt.
#[derive(Debug, Clone)]
pub struct InstructionSection {
    /// Identity within the manifest.
    pub id: String,
    /// What the section is about.
    pub title: String,
    /// Where it sits in the composed instructions.
    pub order: i64,
    /// Which file in `core/agent/instructions` it was compiled from.
    pub source: String,
    /// The text.
    pub text: String,
}

/// Provider-neutral session defaults. Providers map these onto their own wire format.
#[derive(Debug, Clone)]
pub struct SessionDefaults {
    /// Which speech model to ask for.
    pub model: ModelDefaults,
    /// What goes in and what comes out.
    pub modalities: Modalities,
    /// How the agent sounds.
    pub voice: VoiceDefaults,
    /// Audio formats in each direction.
    pub audio: AudioDefaults,
    /// How the end of a turn is decided.
    pub turn_detection: TurnDetection,
    /// How speech becomes text, which the prompt body depends on.
    pub transcription: TranscriptionDefaults,
    /// How hard the model should think, for models that take the setting.
    pub reasoning: Option<ReasoningDefaults>,
    /// Ceilings on output, session length, and how long a tool may take.
    pub limits: Limits,
    /// How the conversation is trimmed when it outgrows the context window.
    pub truncation: Option<Truncation>,
    /// Whether the model must call a tool, may call one, or must not.
    pub tool_choice: String,
    /// Whether a turn may carry several tool calls.
    pub parallel_tool_calls: Option<bool>,
}

/// Which speech model to ask for.
#[derive(Debug, Clone)]
pub struct ModelDefaults {
    /// First choice.
    pub preferred: String,
    /// What to fall back to.
    pub fallbacks: Vec<String>,
}

/// What goes in and what comes out.
#[derive(Debug, Clone)]
pub struct Modalities {
    /// Accepted input.
    pub input: Vec<String>,
    /// Produced output.
    pub output: Vec<String>,
}

/// How the agent sounds.
#[derive(Debug, Clone)]
pub struct VoiceDefaults {
    /// Which voice.
    pub name: String,
    /// How fast it talks.
    pub speed: Option<f64>,
}

/// Audio formats in each direction.
#[derive(Debug, Clone)]
pub struct AudioDefaults {
    /// Captured audio.
    pub input: AudioStream,
    /// Played audio.
    pub output: AudioStream,
}

/// One audio format.
#[derive(Debug, Clone)]
pub struct AudioStream {
    /// How samples are encoded.
    pub encoding: String,
    /// Samples per second.
    pub sample_rate: u32,
    /// How many channels.
    pub channels: u32,
    /// Noise reduction profile, when the provider offers one.
    pub noise_reduction: Option<String>,
}

/// How the end of a turn is decided.
#[derive(Debug, Clone)]
pub struct TurnDetection {
    /// `semantic`, `vad`, or `manual`.
    pub mode: String,
    /// How readily a semantic detector calls the turn over.
    pub eagerness: Option<String>,
    /// Whether the agent answers on its own once a turn ends.
    pub auto_respond: bool,
    /// Whether the speaker can talk over the agent.
    pub allow_barge_in: bool,
    /// Settings for providers that only do silence detection.
    pub server_vad_fallback: Option<ServerVadFallback>,
}

/// Silence-detection settings, for providers with no semantic endpointing.
#[derive(Debug, Clone)]
pub struct ServerVadFallback {
    /// How loud counts as speech.
    pub threshold: f64,
    /// How much audio before the speech to keep.
    pub prefix_padding_ms: u32,
    /// How long a silence ends the turn.
    pub silence_duration_ms: u32,
}

/// How speech becomes text.
#[derive(Debug, Clone)]
pub struct TranscriptionDefaults {
    /// First choice.
    pub preferred: String,
    /// What to fall back to.
    pub fallbacks: Vec<String>,
    /// Which language to expect, or none to detect it.
    pub language: Option<String>,
    /// How the lexicon is pushed to the transcriber.
    pub biasing: Option<Biasing>,
}

/// How the lexicon is pushed to the transcriber.
#[derive(Debug, Clone)]
pub struct Biasing {
    /// Whether to bias at all.
    pub enabled: bool,
    /// How many terms survive the cap.
    pub max_keywords: usize,
    /// What to say before the terms, for models that take a free-text hint.
    pub prompt_preamble: String,
}

/// How hard the model should think.
#[derive(Debug, Clone)]
pub struct ReasoningDefaults {
    /// `low`, `medium`, or `high`.
    pub effort: String,
}

/// Ceilings the session runs under.
#[derive(Debug, Clone)]
pub struct Limits {
    /// How long a single answer may be.
    pub max_output_tokens: u32,
    /// How long the session may run.
    pub max_session_seconds: u64,
    /// How long a tool may take before the model is told it did not answer.
    pub tool_timeout_ms: u64,
}

/// How the conversation is trimmed when it outgrows the context window.
#[derive(Debug, Clone)]
pub struct Truncation {
    /// Which strategy the provider should use.
    pub strategy: String,
    /// How much of the conversation to keep.
    pub retention_ratio: f64,
    /// The budget after the instructions.
    pub post_instruction_token_limit: u32,
}

/// Everything that governs the agent, compiled from `core/agent`.
#[derive(Debug, Clone)]
pub struct AgentBundle {
    /// Which agent this is.
    pub id: String,
    /// What it is called.
    pub name: String,
    /// Which version of the definition.
    pub version: String,
    /// A hash of the compiled bundle, so a prompt can name the exact agent that captured it.
    pub revision: String,
    /// What the agent does.
    pub description: Option<String>,
    /// The composed system prompt handed to the realtime model.
    pub instructions: String,
    /// The sections it was composed from.
    pub instruction_sections: Vec<InstructionSection>,
    /// Every tool the agent may call.
    pub tools: Vec<ToolDefinition>,
    /// Provider-neutral session defaults.
    pub session: SessionDefaults,
    /// Vocabulary the agent starts with.
    pub lexicon: SeedLexicon,
    /// The thresholds the grounding check runs on.
    pub grounding: GroundingConfig,
    /// How a take becomes Markdown.
    pub render: RenderConfig,
    /// What the agent is allowed to do with a take.
    pub policy: PolicyConfig,
}

/// Vocabulary the agent starts every session with.
#[derive(Debug, Clone, Default)]
pub struct SeedLexicon {
    /// Which revision of the seed list.
    pub version: i64,
    /// The terms.
    pub terms: Vec<LexiconTerm>,
}

impl AgentBundle {
    /// The tool with this name, when the bundle defines one.
    pub fn tool(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.iter().find(|tool| tool.name == name)
    }
}
