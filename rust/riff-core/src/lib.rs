//! Riff turns spoken thinking into a finished prompt for another agent, written in the speaker's own
//! words.
//!
//! This crate is the engine: the utterance ledger, the grounding check, drafts and takes, and the
//! prompt artifact. It behaves identically to the TypeScript and Swift bindings because all three
//! read the same compiled agent definition from `core/` and reproduce the same conformance cases.
//!
//! # What is guaranteed
//!
//! The prompt body is made of the speaker's words. That holds because [`GroundingChecker`] checks
//! every candidate line against [`UtteranceLedger`], and because only completed transcripts and
//! typed input ever reach the ledger. Nothing else — not environment notes, not tool results, not
//! anything the model says — can get into a prompt as though it had been spoken.
//!
//! # What the embedder supplies
//!
//! Riff is a library, not an application. It does not own the microphone, the screen, credentials,
//! or storage, and it has no async runtime of its own:
//!
//! - [`RealtimeProvider`] produces speech. `riff-openai-realtime` is one.
//! - [`RiffHost`] knows the speaker's world: what "the PR I just opened" is, and where a finished
//!   prompt goes.
//! - [`RiffStore`] persists what outlives a session.
//! - [`Clock`] supplies time and delays. [`SystemClock`] works anywhere; an embedder already on an
//!   async runtime should implement it over that runtime's timer.
//!
//! [`RiffSession::run`] borrows the session for the length of the conversation, so the microphone
//! and the stop button reach it through a [`RiffHandle`] — cloneable and `Send`, so the audio thread
//! can hold one.
//!
//! ```no_run
//! use std::sync::Arc;
//! use riff_core::{AgentBundle, RealtimeProvider, RiffSession, RiffSessionOptions};
//!
//! # async fn example(provider: Arc<dyn RealtimeProvider>, mic: std::sync::mpsc::Receiver<Vec<u8>>) -> Result<(), Box<dyn std::error::Error>> {
//! let bundle = Arc::new(AgentBundle::bundled()?);
//! let mut session = RiffSession::new(RiffSessionOptions::new(bundle, provider));
//! session.on(Box::new(|event| println!("{event:?}")));
//! session.start().await?;
//!
//! let handle = session.control_handle();
//! std::thread::spawn(move || {
//!     for chunk in mic {
//!         handle.send_audio(&chunk);
//!     }
//!     handle.stop("the speaker is done");
//! });
//!
//! session.run().await;
//! # Ok(())
//! # }
//! ```

pub mod bundle;
pub mod draft;
pub mod error;
pub mod grounding;
pub mod handle;
pub mod host;
pub mod json;
pub mod ledger;
pub mod lexicon;
pub mod provider;
pub mod render;
pub mod runtime;
pub mod schema;
pub mod session;
pub mod text;
pub mod tools;
pub mod types;

pub use bundle::{BUNDLED_AGENT, SessionOverrides};
pub use draft::{
    Anchor, ApplyContext, ApplyResult, DraftBook, DraftOperation, DraftOperationOutcome, Take,
    apply_draft_operations,
};
pub use error::{HostError, RiffError};
pub use grounding::{GroundingChecker, SourceSpan, longest_common_subsequence};
pub use handle::RiffHandle;
pub use host::{
    Destination, HostEnvironment, HostResult, LookupTermRequest, MemoryStore, NullHost,
    PriorPrompt, RecallPromptsRequest, ResolveReferenceRequest, RiffHost, RiffStore, SubmitOptions,
    SubmitResult, TermMatch,
};
pub use json::{Json, JsonObject};
pub use ledger::UtteranceLedger;
pub use lexicon::{
    Canonicalized, Lexicon, biasing_score, compare_by_code_point, is_plausible_mishearing, term_key,
};
pub use provider::{
    AudioFormat, ConnectRequest, ProviderCapabilities, ProviderEvent, ProviderFault,
    RealtimeConnection, RealtimeProvider, TokenUsage, ToolCallRequest,
};
pub use render::{
    BuildArtifactOptions, RenderOptions, build_artifact, render_prompt, summarize_draft,
};
pub use runtime::{BoxFuture, Clock, Either, SystemClock, race, with_deadline};
pub use schema::{ValidationResult, validate};
pub use session::{
    EventListener, RiffEvent, RiffSession, RiffSessionOptions, SessionState, describe_environment,
};
pub use text::{count_words, normalize_token, redact_secrets, tidy_whitespace, tokenize};
pub use tools::{ToolEffect, ToolOutcome, ToolRuntime};
pub use types::{
    AgentBundle, ArtifactTerm, ContextItem, GroundingConfig, GroundingKind, GroundingMode,
    GroundingResult, LexiconTerm, Line, LineGrounding, Motif, PolicyConfig, PromptArtifact,
    Provenance, RenderConfig, RenderDisposition, SECTIONS, Section, SessionDefaults, TakeStatus,
    Title, TitleOrigin, ToolCallRecord, ToolDefinition, Utterance, UtteranceSource,
};
