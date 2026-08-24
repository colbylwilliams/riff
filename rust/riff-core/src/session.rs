//! One conversation, from the first word to a submitted prompt.
//!
//! The session owns the pieces that have to agree with each other: what was heard, what has been
//! drafted from it, and what the agent is allowed to do next. Provider events flow in, tool calls
//! flow back out, and the ledger stays the single place the prompt body can come from — which is
//! what makes "in their own words" a property of the system rather than a request in a prompt.

use std::sync::Arc;

use crate::bundle::SessionOverrides;
use crate::draft::Take;
use crate::handle::{Command, CommandQueue, ConnectionSlot, RiffHandle};
use crate::host::{HostEnvironment, MemoryStore, NullHost, RiffHost, RiffStore};
use crate::provider::{
    ConnectRequest, ProviderEvent, ProviderFault, RealtimeConnection, RealtimeProvider,
    ToolCallRequest,
};
use crate::render::summarize_draft;
use crate::runtime::{BoxFuture, Clock, Either, SystemClock, race};
use crate::tools::{ToolEffect, ToolRuntime};
use crate::types::{AgentBundle, PromptArtifact, ToolCallRecord, Utterance, UtteranceSource};

/// Where the conversation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Not started.
    Idle,
    /// Opening the provider connection.
    Connecting,
    /// Waiting for the speaker.
    Listening,
    /// Working on an answer.
    Thinking,
    /// Talking.
    Speaking,
    /// Shutting down.
    Closing,
    /// Shut down.
    Closed,
    /// Stopped by something that will not resolve itself.
    Failed,
}

impl SessionState {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Idle => "idle",
            SessionState::Connecting => "connecting",
            SessionState::Listening => "listening",
            SessionState::Thinking => "thinking",
            SessionState::Speaking => "speaking",
            SessionState::Closing => "closing",
            SessionState::Closed => "closed",
            SessionState::Failed => "failed",
        }
    }
}

/// Everything the embedding application needs to render, in order.
#[derive(Debug, Clone)]
pub enum RiffEvent {
    /// The session moved.
    State {
        /// Where it is now.
        state: SessionState,
        /// Where it was.
        previous: SessionState,
    },
    /// Something the speaker said reached the ledger.
    Utterance(Utterance),
    /// A take was edited.
    Draft {
        /// Which take.
        take_id: String,
        /// Whether it has everything the policy requires.
        ready: bool,
        /// How much of it is provably theirs.
        fidelity: f64,
        /// A sentence or two about what it covers.
        gist: String,
    },
    /// A different take is active, or none is.
    Take(Option<String>),
    /// What the agent is saying, as text.
    AgentTranscript {
        /// The text so far.
        text: String,
        /// Whether the turn is over.
        final_text: bool,
    },
    /// Audio from the agent.
    AgentAudio(Vec<u8>),
    /// The agent was cut off.
    Interrupted,
    /// A tool ran.
    Tool {
        /// Which one.
        name: String,
        /// Whether it succeeded.
        ok: bool,
        /// How long it took.
        duration_ms: u64,
    },
    /// A prompt was sent.
    Submitted(Box<PromptArtifact>),
    /// The provider's session limit is approaching.
    Expiring {
        /// How long is left.
        seconds_remaining: u64,
    },
    /// Something went wrong.
    Failed(ProviderFault),
    /// The session is over.
    Closed {
        /// Why, when there is a reason to give.
        reason: Option<String>,
    },
}

/// How long before the provider's session limit the speaker is warned.
const EXPIRY_WARNINGS_SECONDS: [u64; 2] = [300, 60];

/// How a session is put together.
pub struct RiffSessionOptions {
    /// The agent definition.
    pub bundle: Arc<AgentBundle>,
    /// What produces speech.
    pub provider: Arc<dyn RealtimeProvider>,
    /// What knows the speaker's world.
    pub host: Arc<dyn RiffHost>,
    /// What outlives the session.
    pub store: Arc<dyn RiffStore>,
    /// Time and delays.
    pub clock: Arc<dyn Clock>,
    /// Host overrides on the bundle's session defaults.
    pub overrides: SessionOverrides,
    /// Which render profile the artifact uses. Defaults to the bundle's.
    pub render_profile: Option<String>,
}

impl RiffSessionOptions {
    /// A session on the vendored agent, with no host, no persistence, and the system clock.
    pub fn new(bundle: Arc<AgentBundle>, provider: Arc<dyn RealtimeProvider>) -> Self {
        Self {
            bundle,
            provider,
            host: Arc::new(NullHost),
            store: Arc::new(MemoryStore::default()),
            clock: Arc::new(SystemClock::new()),
            overrides: SessionOverrides::default(),
            render_profile: None,
        }
    }
}

/// A listener for session events.
pub type EventListener = Box<dyn FnMut(&RiffEvent) + Send>;

/// One conversation.
pub struct RiffSession {
    /// The agent definition every threshold and label comes from.
    pub bundle: Arc<AgentBundle>,
    /// Everything the tools read and write.
    pub runtime: ToolRuntime,

    provider: Arc<dyn RealtimeProvider>,
    host: Arc<dyn RiffHost>,
    store: Arc<dyn RiffStore>,
    clock: Arc<dyn Clock>,
    overrides: SessionOverrides,

    /// Shared with every [`RiffHandle`], so one keeps working across a reconnect.
    connection: Arc<ConnectionSlot>,
    commands: Arc<CommandQueue>,
    listeners: Vec<EventListener>,
    state: SessionState,
    tools_in_flight: bool,
    agent_transcript: String,
    started_at: Option<u64>,
    tool_log: Vec<ToolCallRecord>,
    /// Which expiry warnings are still to come, soonest last so one can be popped.
    pending_warnings: Vec<u64>,
    /// The delay until the next warning, kept across steps so it costs one timer, not one per event.
    warning_timer: Option<BoxFuture<'static, ()>>,
}

impl RiffSession {
    /// Builds a session. Nothing connects until [`RiffSession::start`] is called.
    pub fn new(options: RiffSessionOptions) -> Self {
        let mut runtime = ToolRuntime::new(
            Arc::clone(&options.bundle),
            Arc::clone(&options.host),
            Arc::clone(&options.store),
            Arc::clone(&options.clock),
        );
        runtime.render_profile = options.render_profile;

        Self {
            bundle: options.bundle,
            runtime,
            provider: options.provider,
            host: options.host,
            store: options.store,
            clock: options.clock,
            overrides: options.overrides,
            connection: Arc::new(ConnectionSlot::default()),
            commands: Arc::new(CommandQueue::default()),
            listeners: Vec::new(),
            state: SessionState::Idle,
            tools_in_flight: false,
            agent_transcript: String::new(),
            started_at: None,
            tool_log: Vec::new(),
            pending_warnings: Vec::new(),
            warning_timer: None,
        }
    }

    /// Where the conversation is.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The provider's identity for the session, once connected.
    pub fn session_id(&self) -> Option<String> {
        self.connection
            .get()
            .map(|connection| connection.session_id())
    }

    /// Adds a listener. Listeners are called in the order they were added.
    pub fn on(&mut self, listener: EventListener) {
        self.listeners.push(listener);
    }

    /// Loads what outlives the session, connects the provider, and starts listening.
    pub async fn start(&mut self) -> Result<(), ProviderFault> {
        if !matches!(
            self.state,
            SessionState::Idle | SessionState::Closed | SessionState::Failed
        ) {
            return Err(ProviderFault::fatal(
                "already_started",
                format!("session already {}", self.state.as_str()),
            ));
        }
        self.set_state(SessionState::Connecting);

        match self.connect().await {
            Ok(()) => {
                self.set_state(SessionState::Listening);
                Ok(())
            }
            Err(fault) => {
                self.set_state(SessionState::Failed);
                self.emit(RiffEvent::Failed(fault.clone()));
                Err(fault)
            }
        }
    }

    async fn connect(&mut self) -> Result<(), ProviderFault> {
        let failed = |error: crate::error::HostError| {
            ProviderFault::retryable("connect_failed", error.to_string())
        };

        // Connecting always starts from nothing attached. Every path that ends a session takes the
        // connection with it, so this is insurance rather than a case that arises today — but
        // replacing one here would leave a socket open that nobody can reach.
        if let Some(previous) = self.connection.replace(None) {
            previous.close(Some("reconnecting".to_owned())).await;
        }
        // Commands are scoped to a connection, not to the session object. Anything still queued was
        // meant for the conversation that ended.
        self.commands.drain();

        for term in self.store.load_lexicon().await.map_err(failed)? {
            self.runtime.lexicon.add(term);
        }
        // Includes retired motifs when the store keeps them, so their ids are never reissued.
        for motif in self.store.list_motifs().await.map_err(failed)? {
            self.runtime.motifs.insert(motif.id.clone(), motif);
        }

        // Propagated rather than defaulted, matching the TypeScript binding: a host that cannot
        // answer this is a host that will not resolve a reference either, and a session that starts
        // without knowing the repository or its destinations fails later, mid-conversation, in a way
        // the speaker cannot make sense of. `RiffHost::environment` defaults to empty, so only a
        // host that implements it and then fails ends up here.
        let environment = self.host.environment().await.map_err(failed)?;
        for term in &environment.vocabulary {
            self.runtime.lexicon.add(term.clone());
        }
        self.runtime.ledger.invalidate();

        let session = self.overrides.apply(&self.bundle.session);
        let max_session_seconds = session.limits.max_session_seconds;
        let connection = self
            .provider
            .connect(ConnectRequest {
                instructions: self.bundle.instructions.clone(),
                tools: self.bundle.tools.clone(),
                session,
                vocabulary: self.runtime.vocabulary(),
            })
            .await?;

        self.connection.replace(Some(connection.clone()));
        self.started_at = Some(self.clock.monotonic_ms());
        self.pending_warnings = EXPIRY_WARNINGS_SECONDS
            .into_iter()
            .filter(|remaining| max_session_seconds > *remaining)
            .rev()
            .collect();
        self.warning_timer = None;

        if let Some(note) = describe_environment(&environment) {
            connection.send_text(&note, false);
        }

        Ok(())
    }

    /// Pumps provider events until the connection closes.
    ///
    /// The embedder drives this on whatever runtime it brought; the engine never spawns a task of
    /// its own.
    pub async fn run(&mut self) {
        while self.step().await {}
    }

    /// Handles the next provider event, returning `false` once the connection is gone.
    ///
    /// Expiry warnings are raced against the event stream rather than scheduled, so a session that
    /// goes quiet still warns the speaker before the provider cuts it off — and so the engine needs
    /// nothing from the runtime beyond [`Clock::sleep`]. The delay is created once per warning and
    /// held across steps: a fresh one per event would cost a timer per event, which on the
    /// thread-backed [`SystemClock`] is a thread per event.
    pub async fn step(&mut self) -> bool {
        let Some(connection) = self.connection.get() else {
            return false;
        };

        if self.warning_timer.is_none()
            && let Some(delay_ms) = self.next_warning_delay()
        {
            self.warning_timer = Some(self.clock.sleep(delay_ms));
        }

        // Ordering is by what must not be starved, because `race` stops at the first ready future.
        // Commands are the speaker acting, so they go first; the timer next, because a session busy
        // enough for the warning to matter — continuous audio — would otherwise keep an event ready
        // on every poll and never reach it.
        let commands = Arc::clone(&self.commands);
        let mut timer = self.warning_timer.take();
        let outcome = {
            let events = async {
                match timer.as_mut() {
                    None => Either::Right(connection.next_event().await),
                    Some(timer) => race(timer, connection.next_event()).await,
                }
            };
            race(commands.next(), Box::pin(events)).await
        };

        let outcome = match outcome {
            Either::Left(command) => {
                self.warning_timer = timer;
                return self.apply_command(command).await;
            }
            Either::Right(outcome) => outcome,
        };

        match outcome {
            Either::Left(()) => {
                if let Some(seconds_remaining) = self.pending_warnings.pop() {
                    self.emit(RiffEvent::Expiring { seconds_remaining });
                }
                true
            }
            Either::Right(event) => {
                self.warning_timer = timer;
                match event {
                    Some(event) => {
                        self.handle(event).await;
                        true
                    }
                    None => {
                        // A provider whose stream ends without a closing event has still closed.
                        // Returning without saying so would leave `run` finished, the state reading
                        // `listening`, and a dead connection in hand.
                        self.close("the provider stopped sending events");
                        false
                    }
                }
            }
        }
    }

    /// A cloneable handle for driving the session while [`RiffSession::run`] holds it.
    ///
    /// The pump borrows the session for the length of the conversation, which is what keeps the
    /// engine's state single-owned. A handle is how the microphone and the stop button reach it
    /// anyway — see [`RiffHandle`] for what goes straight to the connection and what is queued.
    pub fn control_handle(&self) -> RiffHandle {
        RiffHandle {
            connection: Arc::clone(&self.connection),
            commands: Arc::clone(&self.commands),
        }
    }

    /// Carries out what a handle asked for. Returns whether the pump keeps running.
    async fn apply_command(&mut self, command: Command) -> bool {
        match command {
            Command::SendText(text) => {
                // The utterance reaches the embedder on the event stream, since the handle that
                // asked for this is not the thing that records it.
                let _ = self.send_text(&text);
                true
            }
            Command::Interrupt => {
                self.interrupt();
                true
            }
            Command::Stop(reason) => {
                self.stop(reason).await;
                false
            }
        }
    }

    /// Records that the connection is gone, whether the provider said so or simply stopped.
    fn close(&mut self, reason: impl Into<String>) {
        self.connection.replace(None);
        self.pending_warnings.clear();
        self.warning_timer = None;
        self.set_state(SessionState::Closed);
        self.emit(RiffEvent::Closed {
            reason: Some(reason.into()),
        });
    }

    /// How long until the next expiry warning is due.
    fn next_warning_delay(&self) -> Option<u64> {
        let remaining = *self.pending_warnings.last()?;
        let started_at = self.started_at?;
        let max_session_seconds = self.bundle.session.limits.max_session_seconds;
        let elapsed_ms = self.clock.monotonic_ms().saturating_sub(started_at);
        Some(((max_session_seconds - remaining) * 1000).saturating_sub(elapsed_ms))
    }

    /// Ends the session.
    pub async fn stop(&mut self, reason: impl Into<String>) {
        if matches!(self.state, SessionState::Closed | SessionState::Idle) {
            return;
        }
        let reason = reason.into();
        self.set_state(SessionState::Closing);
        self.pending_warnings.clear();
        self.warning_timer = None;
        if let Some(connection) = self.connection.replace(None) {
            connection.close(Some(reason.clone())).await;
        }
        self.set_state(SessionState::Closed);
        self.emit(RiffEvent::Closed {
            reason: Some(reason),
        });
    }

    /// Captured microphone audio, in the format the provider advertised.
    pub fn send_audio(&self, chunk: &[u8]) {
        if let Some(connection) = self.connection.get() {
            connection.send_audio(chunk);
        }
    }

    /// Typed input, treated exactly like speech: it lands in the ledger and can be quoted in the
    /// prompt. Someone switching to the keyboard mid-thought should not lose the ability to be quoted.
    pub fn send_text(&mut self, text: &str) -> Result<Utterance, String> {
        let connection = self
            .connection
            .get()
            .ok_or_else(|| "session is not connected".to_owned())?;
        let at = self.clock.now();
        let utterance = self
            .runtime
            .ledger
            .append(text, at, UtteranceSource::Typed, None);
        self.emit(RiffEvent::Utterance(utterance.clone()));
        connection.send_text(&utterance.text, true);
        Ok(utterance)
    }

    /// Cuts the agent off. Called when the speaker starts talking over it.
    pub fn interrupt(&mut self) {
        if !matches!(self.state, SessionState::Speaking | SessionState::Thinking) {
            return;
        }
        if let Some(connection) = self.connection.get() {
            connection.cancel_response();
        }
        self.emit(RiffEvent::Interrupted);
        self.set_state(SessionState::Listening);
    }

    /// The active take as it stands right now. Always safe to read mid-conversation.
    pub fn artifact(&mut self) -> Option<PromptArtifact> {
        self.refresh_provenance();
        self.runtime.active_artifact()
    }

    /// Every take in the session.
    pub fn takes(&self) -> &[Take] {
        self.runtime.book.takes()
    }

    // MARK: - Provider events

    async fn handle(&mut self, event: ProviderEvent) {
        match event {
            ProviderEvent::Connected { .. } => self.set_state(SessionState::Listening),

            ProviderEvent::SpeechStarted => {
                // Automatic barge-in has to do everything an explicit interrupt does. Emitting the
                // event without cancelling leaves buffered output playing over whoever just started.
                self.interrupt();
                self.set_state(SessionState::Listening);
            }

            ProviderEvent::TranscriptCompleted {
                text, confidence, ..
            } => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return;
                }
                let at = self.clock.now();
                let utterance =
                    self.runtime
                        .ledger
                        .append(trimmed, at, UtteranceSource::Speech, confidence);
                self.emit(RiffEvent::Utterance(utterance));
            }

            ProviderEvent::TranscriptFailed { reason, .. } => {
                self.emit(RiffEvent::Failed(ProviderFault::retryable(
                    "transcription_failed",
                    reason,
                )));
            }

            ProviderEvent::ResponseStarted { .. } => {
                // The continuation for a tool batch has begun, so the batch is no longer pending.
                self.tools_in_flight = false;
                self.agent_transcript.clear();
                self.set_state(SessionState::Thinking);
            }

            ProviderEvent::ResponseAudio { audio, .. } => {
                if self.state != SessionState::Speaking {
                    self.set_state(SessionState::Speaking);
                }
                self.emit(RiffEvent::AgentAudio(audio));
            }

            ProviderEvent::ResponseTextDelta { delta, .. } => {
                self.agent_transcript.push_str(&delta);
                let text = self.agent_transcript.clone();
                self.emit(RiffEvent::AgentTranscript {
                    text,
                    final_text: false,
                });
            }

            ProviderEvent::ResponseText { text, .. } => {
                self.agent_transcript = text.clone();
                self.emit(RiffEvent::AgentTranscript {
                    text,
                    final_text: true,
                });
            }

            ProviderEvent::ToolCalls { calls } => {
                // Set on receipt rather than inside the dispatch: the pump awaits `run_tools`, so a
                // flag scoped to that call is already cleared by the time `ResponseDone` is read.
                self.tools_in_flight = true;
                self.run_tools(calls).await;
            }

            ProviderEvent::ResponseDone { .. } | ProviderEvent::ResponseCancelled { .. } => {
                if !self.tools_in_flight {
                    self.set_state(SessionState::Listening);
                }
            }

            ProviderEvent::Failed { fault } => {
                self.tools_in_flight = false;
                let retryable = fault.retryable;
                self.emit(RiffEvent::Failed(fault));
                if !retryable {
                    // Terminal. Leaving the connection attached would keep `run` waiting on a
                    // stream with nothing left to say, and a restart from `failed` would replace a
                    // live connection without ever closing the one it displaced.
                    if let Some(connection) = self.connection.replace(None) {
                        connection
                            .close(Some("unrecoverable provider error".to_owned()))
                            .await;
                    }
                    self.pending_warnings.clear();
                    self.warning_timer = None;
                    self.set_state(SessionState::Failed);
                }
            }

            ProviderEvent::Closed { reason } => {
                self.connection.replace(None);
                self.pending_warnings.clear();
                self.warning_timer = None;
                self.set_state(SessionState::Closed);
                self.emit(RiffEvent::Closed { reason });
            }

            ProviderEvent::SpeechStopped
            | ProviderEvent::ResponseAudioDone { .. }
            | ProviderEvent::RateLimit { .. }
            | ProviderEvent::TranscriptDelta { .. } => {}
        }
    }

    /// Runs every tool call of one model turn, then asks for exactly one continuation.
    ///
    /// Providers deliver a turn's calls together, so they are dispatched together. Asking the model
    /// to continue once per call instead would produce one spoken reply per tool, which sounds like
    /// the agent stuttering.
    async fn run_tools(&mut self, calls: Vec<ToolCallRequest>) {
        let Some(connection) = self.connection.get() else {
            self.tools_in_flight = false;
            return;
        };
        if calls.is_empty() {
            self.tools_in_flight = false;
            return;
        }

        for call in calls {
            let at = self.clock.now();
            self.refresh_provenance();
            let outcome = self
                .runtime
                .dispatch(&call.name, &call.arguments_json)
                .await;

            self.tool_log.push(ToolCallRecord {
                name: call.name.clone(),
                at,
                duration_ms: Some(outcome.duration_ms),
                ok: Some(outcome.ok),
            });
            self.emit(RiffEvent::Tool {
                name: call.name,
                ok: outcome.ok,
                duration_ms: outcome.duration_ms,
            });

            connection.respond_to_tool(&call.call_id, &outcome.result.serialize());

            for effect in outcome.effects {
                self.apply(effect, connection.as_ref());
            }
        }

        connection.request_response();
    }

    fn apply(&mut self, effect: ToolEffect, connection: &dyn RealtimeConnection) {
        match effect {
            ToolEffect::LexiconChanged => {
                connection.update_vocabulary(&self.runtime.vocabulary());
            }
            ToolEffect::DraftChanged(take_id) => {
                let Some(take) = self.runtime.book.get(&take_id) else {
                    return;
                };
                let ready = take.is_ready(&self.bundle.policy);
                let gist = summarize_draft(take);
                let fidelity = self
                    .runtime
                    .artifact_for(take)
                    .map(|artifact| artifact.provenance.fidelity)
                    .unwrap_or_default();
                self.emit(RiffEvent::Draft {
                    take_id,
                    ready,
                    fidelity,
                    gist,
                });
            }
            ToolEffect::TakeChanged(take_id) => self.emit(RiffEvent::Take(take_id)),
            ToolEffect::Submitted(artifact) => self.emit(RiffEvent::Submitted(artifact)),
        }
    }

    // MARK: - Internals

    /// Reads provenance when an artifact is about to be built rather than storing it, so a prompt
    /// submitted mid-session carries the negotiated model, session id, and duration instead of
    /// whatever was known before connecting.
    fn refresh_provenance(&mut self) {
        self.runtime.provenance = crate::types::Provenance {
            agent_version: Some(self.bundle.version.clone()),
            bundle_revision: Some(self.bundle.revision.clone()),
            provider_id: Some(self.provider.id().to_owned()),
            model: self.connection.get().map(|connection| connection.model()),
            session_id: self
                .connection
                .get()
                .map(|connection| connection.session_id()),
            duration_ms: self
                .started_at
                .map(|started| self.clock.monotonic_ms().saturating_sub(started)),
            tool_calls: self.tool_log.clone(),
            ..crate::types::Provenance::default()
        };
    }

    fn set_state(&mut self, next: SessionState) {
        if next == self.state {
            return;
        }
        let previous = self.state;
        self.state = next;
        self.emit(RiffEvent::State {
            state: next,
            previous,
        });
    }

    fn emit(&mut self, event: RiffEvent) {
        for listener in &mut self.listeners {
            listener(&event);
        }
    }
}

/// Ambient facts stated once at connect time so references resolve without anyone being asked.
///
/// This is injected into the model's context, not into the ledger, so none of it can end up quoted
/// in the prompt as though the speaker had said it.
pub fn describe_environment(environment: &HostEnvironment) -> Option<String> {
    let mut facts: Vec<String> = Vec::new();
    if let Some(repository) = &environment.repository {
        facts.push(format!("Repository: {repository}"));
    }
    if let Some(branch) = &environment.branch {
        facts.push(format!("Branch: {branch}"));
    }
    if let Some(workspace) = &environment.workspace {
        facts.push(format!("Workspace: {workspace}"));
    }
    if let Some(login) = &environment.user_login {
        facts.push(format!(
            "Speaker: {}",
            environment.user_name.as_ref().unwrap_or(login)
        ));
    }
    if !environment.destinations.is_empty() {
        facts.push(format!(
            "Destinations: {}",
            environment
                .destinations
                .iter()
                .map(|destination| format!(
                    "{}{}",
                    destination.id,
                    if destination.is_default {
                        " (default)"
                    } else {
                        ""
                    }
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !environment.recent.is_empty() {
        facts.push(format!(
            "Recently touched: {}",
            environment
                .recent
                .iter()
                .take(5)
                .map(|item| item
                    .identifier
                    .clone()
                    .unwrap_or_else(|| item.title.clone()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if facts.is_empty() {
        return None;
    }
    Some(format!(
        "[context, not spoken by the user, never quote this in the prompt]\n{}",
        facts.join("\n")
    ))
}
