//! The seam between Riff and whatever produces speech-to-speech.
//!
//! It is deliberately small. Everything that makes Riff what it is — the ledger, the grounding
//! check, drafts, takes, the artifact — lives above this line and is provider independent. A
//! provider only has to move audio, report what was heard, and relay tool calls. That is the common
//! denominator of OpenAI Realtime, Gemini Live, and a chained on-device speech pipeline, so swapping
//! one for another changes no behavior that a speaker can observe.

use std::sync::Arc;

use crate::runtime::BoxFuture;
use crate::types::{SessionDefaults, ToolDefinition};

/// One audio format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFormat {
    /// `pcm_s16le`, `opus`, `g711_ulaw`, or `g711_alaw`.
    pub encoding: String,
    /// Samples per second.
    pub sample_rate: u32,
    /// How many channels.
    pub channels: u32,
}

/// What a provider can do, so a host knows what it is working with.
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    /// Speech in and speech out through one model, rather than a transcribe-think-speak chain.
    pub speech_to_speech: bool,
    /// The speaker can talk over the agent and cut it off mid-sentence.
    pub barge_in: bool,
    /// The provider decides when a turn has ended from meaning, not just from silence.
    pub semantic_turn_detection: bool,
    /// How transcription can be biased toward a supplied vocabulary: `keywords`, `prompt`, `none`.
    pub vocabulary_biasing: String,
    /// Verbatim transcripts of the speaker are available, which the prompt body depends on.
    pub input_transcription: bool,
    /// The model can call tools.
    pub function_calling: bool,
    /// Captured audio format.
    pub input_audio: AudioFormat,
    /// Played audio format.
    pub output_audio: AudioFormat,
    /// How long the provider lets one session run.
    pub max_session_seconds: Option<u64>,
}

/// Everything a provider needs to open a session.
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    /// The composed system prompt.
    pub instructions: String,
    /// The tools the model may call.
    pub tools: Vec<ToolDefinition>,
    /// Provider-neutral session defaults, with any host overrides already applied.
    pub session: SessionDefaults,
    /// Canonical spellings to bias transcription toward, compiled from the active lexicon.
    pub vocabulary: Vec<String>,
}

/// How much the model used.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Input tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Input tokens that were audio.
    pub input_audio_tokens: Option<u64>,
    /// Input tokens served from cache.
    pub cached_tokens: Option<u64>,
}

/// Something the provider could not do.
#[derive(Debug, Clone)]
pub struct ProviderFault {
    /// What sort of failure.
    pub code: String,
    /// What happened.
    pub message: String,
    /// Whether reconnecting is worth trying. Transport faults are; a rejected session config is not.
    pub retryable: bool,
}

impl ProviderFault {
    /// A fault worth reconnecting over.
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
        }
    }

    /// A fault that will happen again.
    pub fn fatal(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}

/// One tool call the model made.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    /// What to quote when answering it.
    pub call_id: String,
    /// Which tool.
    pub name: String,
    /// The arguments, as the JSON string the model produced.
    pub arguments_json: String,
}

/// Everything a provider reports.
#[derive(Debug, Clone)]
pub enum ProviderEvent {
    /// The session is open.
    Connected {
        /// The provider's identity for the session.
        session_id: String,
        /// The model it negotiated.
        model: String,
    },
    /// The speaker started talking.
    SpeechStarted,
    /// The speaker stopped talking.
    SpeechStopped,
    /// Part of a transcript, while it is still streaming.
    TranscriptDelta {
        /// Which item it belongs to.
        item_id: String,
        /// The new text.
        delta: String,
    },
    /// A finished transcript. This is the only speech that reaches the ledger.
    TranscriptCompleted {
        /// Which item it belongs to.
        item_id: String,
        /// What was heard.
        text: String,
        /// How sure the transcriber was.
        confidence: Option<f64>,
    },
    /// A transcript that could not be produced.
    TranscriptFailed {
        /// Which item it belongs to.
        item_id: String,
        /// Why.
        reason: String,
    },
    /// The model started answering.
    ResponseStarted {
        /// Which response.
        response_id: String,
    },
    /// Audio from the agent.
    ResponseAudio {
        /// Which response.
        response_id: String,
        /// The samples, in the format the provider advertised.
        audio: Vec<u8>,
    },
    /// The agent finished speaking.
    ResponseAudioDone {
        /// Which response.
        response_id: String,
    },
    /// Part of what the agent is saying, as text.
    ResponseTextDelta {
        /// Which response.
        response_id: String,
        /// The new text.
        delta: String,
    },
    /// The whole of what the agent said, as text.
    ResponseText {
        /// Which response.
        response_id: String,
        /// The text.
        text: String,
    },
    /// The model finished a turn.
    ResponseDone {
        /// Which response.
        response_id: String,
        /// What it cost.
        usage: Option<TokenUsage>,
    },
    /// The turn was cut off.
    ResponseCancelled {
        /// Which response.
        response_id: String,
    },
    /// Every tool call of one turn, delivered together.
    ToolCalls {
        /// The calls.
        calls: Vec<ToolCallRequest>,
    },
    /// How much headroom is left.
    RateLimit {
        /// Requests left.
        remaining: u64,
        /// When it resets.
        reset_seconds: u64,
    },
    /// Something went wrong.
    Failed {
        /// What.
        fault: ProviderFault,
    },
    /// The connection is gone.
    Closed {
        /// Why, when the provider said.
        reason: Option<String>,
    },
}

/// A speech-to-speech provider.
pub trait RealtimeProvider: Send + Sync {
    /// Which provider this is, recorded in an artifact's provenance.
    fn id(&self) -> &str;

    /// What it can do.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Opens a session.
    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'_, Result<Arc<dyn RealtimeConnection>, ProviderFault>>;
}

/// One open provider session.
///
/// Every method takes `&self` because the session reads events and writes commands from the same
/// handle; an implementation keeps whatever state it needs behind its own synchronization.
pub trait RealtimeConnection: Send + Sync {
    /// The provider's identity for the session.
    fn session_id(&self) -> String;

    /// The model the provider negotiated.
    fn model(&self) -> String;

    /// The next event, or `None` once the connection has closed for good.
    ///
    /// This is the whole of the provider's event stream: the session is the only caller, and it
    /// pumps this in a loop. Delivering events by pull rather than by callback is what lets the
    /// engine work on any runtime without owning one.
    fn next_event(&self) -> BoxFuture<'_, Option<ProviderEvent>>;

    /// Appends captured audio in the format the provider advertised.
    fn send_audio(&self, chunk: &[u8]);

    /// Ends the current turn explicitly. Only needed when turn detection is manual.
    fn commit_audio(&self);

    /// Injects text as if the speaker had said it, for typed input and host notifications.
    fn send_text(&self, text: &str, respond: bool);

    /// Returns a tool result. `result_json` is a JSON string, matching every provider's expectation.
    fn respond_to_tool(&self, call_id: &str, result_json: &str);

    /// Asks the model to continue.
    fn request_response(&self);

    /// Stops the agent mid-sentence. Used when the speaker talks over it.
    fn cancel_response(&self);

    /// Pushes newly learned vocabulary without resending the whole configuration.
    fn update_vocabulary(&self, vocabulary: &[String]);

    /// Closes the session.
    fn close(&self, reason: Option<String>) -> BoxFuture<'_, ()>;
}
