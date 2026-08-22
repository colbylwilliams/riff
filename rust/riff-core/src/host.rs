//! What the embedding application supplies.
//!
//! Riff knows how to keep a prompt in someone's voice; it does not know what their world contains.
//! Everything world shaped — which PR they just opened, what an acronym means here, what they asked
//! for last week, where a finished prompt goes — comes through this interface, which is why the same
//! agent works in an editor, a terminal, a phone, and a design tool without knowing the difference.

use std::sync::Mutex;

use crate::error::HostError;
use crate::runtime::BoxFuture;
use crate::types::{ContextItem, LexiconTerm, Motif, PromptArtifact};

/// What a host call returns. The error is the embedder's own, boxed.
pub type HostResult<T> = std::result::Result<T, HostError>;

/// "The PR I just opened", as the agent heard it.
#[derive(Debug, Clone, Default)]
pub struct ResolveReferenceRequest {
    /// The phrase the speaker used.
    pub phrase: String,
    /// What sort of thing the agent thinks it is.
    pub kind: Option<String>,
    /// How recent: `latest`, `today`, `this_week`, `this_month`, or `any`.
    pub recency: Option<String>,
    /// Whose thing it is.
    pub actor: Option<String>,
    /// How many candidates to return.
    pub limit: Option<usize>,
    /// Recent transcript, so a host can disambiguate from what was being discussed.
    pub transcript: Option<String>,
}

/// A word the transcriber may have mangled.
#[derive(Debug, Clone, Default)]
pub struct LookupTermRequest {
    /// What the transcriber produced.
    pub heard: String,
    /// What was being talked about.
    pub context: Option<String>,
    /// What sort of thing the agent thinks it is.
    pub kind: Option<String>,
}

/// A term a host recognized, with how sure it is.
///
/// Confidence is load bearing: a confirmed glossary hit is applied silently, while a fuzzy search
/// guess on a word the request depends on is worth asking about.
#[derive(Debug, Clone)]
pub struct TermMatch {
    /// The term.
    pub term: LexiconTerm,
    /// How sure the host is.
    pub confidence: Option<f64>,
}

/// A search for prompts the speaker wrote before.
#[derive(Debug, Clone, Default)]
pub struct RecallPromptsRequest {
    /// What to look for.
    pub query: String,
    /// How recent.
    pub recency: Option<String>,
    /// `any`, `submitted`, or `parked`.
    pub status: Option<String>,
    /// How many to return.
    pub limit: Option<usize>,
}

/// A prompt the speaker wrote before.
#[derive(Debug, Clone, Default)]
pub struct PriorPrompt {
    /// The host's identity for it.
    pub prompt_id: String,
    /// What it was called.
    pub title: String,
    /// Enough of it to recognize.
    pub excerpt: String,
    /// When it was sent.
    pub submitted_at: Option<String>,
    /// Where it ended up.
    pub status: Option<String>,
    /// What came of it.
    pub outcome: Option<String>,
    /// Where to find it.
    pub url: Option<String>,
}

/// How a prompt should be sent.
#[derive(Debug, Clone, Default)]
pub struct SubmitOptions {
    /// Which destination, when the speaker named one.
    pub target: Option<String>,
    /// Whether the take stays open afterwards.
    pub keep_open: bool,
}

/// What the host did with a prompt.
#[derive(Debug, Clone, Default)]
pub struct SubmitResult {
    /// Whether it went.
    pub submitted: bool,
    /// The host's identity for it.
    pub prompt_id: Option<String>,
    /// Where it went.
    pub destination: Option<String>,
    /// Where to find it.
    pub url: Option<String>,
    /// Anything the speaker should be told.
    pub message: Option<String>,
}

/// A destination `submit_prompt` may target.
#[derive(Debug, Clone, Default)]
pub struct Destination {
    /// What to name in `submit_prompt`.
    pub id: String,
    /// What to call it out loud.
    pub label: String,
    /// Whether it is where a prompt goes when the speaker names none.
    pub is_default: bool,
}

/// Ambient facts that make references resolvable without asking.
#[derive(Debug, Clone, Default)]
pub struct HostEnvironment {
    /// Where the speaker is working, phrased as they would say it.
    pub workspace: Option<String>,
    /// Which repository.
    pub repository: Option<String>,
    /// Which branch.
    pub branch: Option<String>,
    /// Who they are.
    pub user_login: Option<String>,
    /// What to call them.
    pub user_name: Option<String>,
    /// Destinations `submit_prompt` may target.
    pub destinations: Vec<Destination>,
    /// Names, repos, and jargon specific to this workspace, folded into the lexicon at connect time.
    pub vocabulary: Vec<LexiconTerm>,
    /// Things recently touched, which make "the one I just opened" resolvable.
    pub recent: Vec<ContextItem>,
}

/// The embedding application, as Riff sees it.
///
/// Treat every method as reachable by a model interpreting speech in a noisy room. Reads stay scoped
/// to what the speaker can already see; writes are limited to [`RiffHost::submit_prompt`] against an
/// explicitly configured destination, and idempotent where they can be — a call abandoned at its
/// deadline may already have landed.
pub trait RiffHost: Send + Sync {
    /// Turns "the PR I just opened" into a thing with an identifier and a URL.
    fn resolve_reference(
        &self,
        request: ResolveReferenceRequest,
    ) -> BoxFuture<'_, HostResult<Vec<ContextItem>>>;

    /// Says what a term means here and how it is spelled.
    fn lookup_term(&self, request: LookupTermRequest) -> BoxFuture<'_, HostResult<Vec<TermMatch>>>;

    /// Finds prompts the speaker wrote before.
    fn recall_prompts(
        &self,
        request: RecallPromptsRequest,
    ) -> BoxFuture<'_, HostResult<Vec<PriorPrompt>>>;

    /// Hands the finished prompt to whatever does the work.
    fn submit_prompt(
        &self,
        artifact: PromptArtifact,
        options: SubmitOptions,
    ) -> BoxFuture<'_, HostResult<SubmitResult>>;

    /// Ambient facts that make references resolvable without asking.
    fn environment(&self) -> BoxFuture<'_, HostResult<HostEnvironment>> {
        Box::pin(async { Ok(HostEnvironment::default()) })
    }
}

/// A host for when there is nothing to look things up in.
///
/// It resolves nothing rather than guessing, because a fabricated PR number sends the downstream
/// agent somewhere real and wrong, which is worse than an unresolved reference the agent asks about.
#[derive(Debug, Default)]
pub struct NullHost;

impl RiffHost for NullHost {
    fn resolve_reference(
        &self,
        _request: ResolveReferenceRequest,
    ) -> BoxFuture<'_, HostResult<Vec<ContextItem>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn lookup_term(
        &self,
        _request: LookupTermRequest,
    ) -> BoxFuture<'_, HostResult<Vec<TermMatch>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn recall_prompts(
        &self,
        _request: RecallPromptsRequest,
    ) -> BoxFuture<'_, HostResult<Vec<PriorPrompt>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn submit_prompt(
        &self,
        artifact: PromptArtifact,
        _options: SubmitOptions,
    ) -> BoxFuture<'_, HostResult<SubmitResult>> {
        Box::pin(async move {
            Ok(SubmitResult {
                submitted: true,
                prompt_id: Some(artifact.id),
                destination: Some("none".to_owned()),
                ..SubmitResult::default()
            })
        })
    }
}

/// Persistence for the things that outlive a session.
pub trait RiffStore: Send + Sync {
    /// Vocabulary learned in earlier sessions.
    fn load_lexicon(&self) -> BoxFuture<'_, HostResult<Vec<LexiconTerm>>>;

    /// Remembers a term past the end of the session.
    fn save_term(&self, term: LexiconTerm) -> BoxFuture<'_, HostResult<()>>;

    /// Every motif, retired ones included. Retired motifs are filtered out where they are offered,
    /// and keeping them here is what stops a new motif being given an id a retired one still holds.
    fn list_motifs(&self) -> BoxFuture<'_, HostResult<Vec<Motif>>>;

    /// Remembers a standing instruction.
    fn save_motif(&self, motif: Motif) -> BoxFuture<'_, HostResult<()>>;

    /// Marks a standing instruction as no longer wanted.
    fn retire_motif(&self, id: String, at: String) -> BoxFuture<'_, HostResult<()>>;

    /// Keeps a copy of a prompt that was sent.
    fn save_artifact(&self, artifact: PromptArtifact) -> BoxFuture<'_, HostResult<()>>;

    /// The most recent prompts, newest first.
    fn list_artifacts(&self, limit: usize) -> BoxFuture<'_, HostResult<Vec<PromptArtifact>>>;
}

/// A store that keeps everything for the life of the process and nothing longer.
#[derive(Debug, Default)]
pub struct MemoryStore {
    state: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    terms: Vec<LexiconTerm>,
    motifs: Vec<Motif>,
    artifacts: Vec<PromptArtifact>,
}

impl MemoryStore {
    /// A store seeded with terms and motifs, as a host would have loaded them.
    pub fn seeded(terms: Vec<LexiconTerm>, motifs: Vec<Motif>) -> Self {
        Self {
            state: Mutex::new(MemoryState {
                terms,
                motifs,
                artifacts: Vec::new(),
            }),
        }
    }

    /// Every prompt saved so far, oldest first.
    pub fn artifacts(&self) -> Vec<PromptArtifact> {
        self.lock().artifacts.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl RiffStore for MemoryStore {
    fn load_lexicon(&self) -> BoxFuture<'_, HostResult<Vec<LexiconTerm>>> {
        let terms = self.lock().terms.clone();
        Box::pin(async move { Ok(terms) })
    }

    fn save_term(&self, term: LexiconTerm) -> BoxFuture<'_, HostResult<()>> {
        let mut state = self.lock();
        match state
            .terms
            .iter()
            .position(|existing| existing.canonical.to_lowercase() == term.canonical.to_lowercase())
        {
            Some(index) => state.terms[index] = term,
            None => state.terms.push(term),
        }
        Box::pin(async { Ok(()) })
    }

    fn list_motifs(&self) -> BoxFuture<'_, HostResult<Vec<Motif>>> {
        let mut motifs = self.lock().motifs.clone();
        motifs.sort_by(|a, b| a.id.cmp(&b.id));
        Box::pin(async move { Ok(motifs) })
    }

    fn save_motif(&self, motif: Motif) -> BoxFuture<'_, HostResult<()>> {
        let mut state = self.lock();
        match state
            .motifs
            .iter()
            .position(|existing| existing.id == motif.id)
        {
            Some(index) => state.motifs[index] = motif,
            None => state.motifs.push(motif),
        }
        Box::pin(async { Ok(()) })
    }

    fn retire_motif(&self, id: String, at: String) -> BoxFuture<'_, HostResult<()>> {
        let mut state = self.lock();
        if let Some(motif) = state.motifs.iter_mut().find(|motif| motif.id == id) {
            motif.retired_at = Some(at);
        }
        Box::pin(async { Ok(()) })
    }

    fn save_artifact(&self, artifact: PromptArtifact) -> BoxFuture<'_, HostResult<()>> {
        self.lock().artifacts.push(artifact);
        Box::pin(async { Ok(()) })
    }

    fn list_artifacts(&self, limit: usize) -> BoxFuture<'_, HostResult<Vec<PromptArtifact>>> {
        let artifacts = self.lock().artifacts.clone();
        let start = artifacts.len().saturating_sub(limit);
        let recent: Vec<PromptArtifact> = artifacts[start..].iter().rev().cloned().collect();
        Box::pin(async move { Ok(recent) })
    }
}
