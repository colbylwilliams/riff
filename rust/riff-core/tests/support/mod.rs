//! A provider, a host, and an executor the session tests drive by hand.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use riff_core::{
    AudioFormat, BoxFuture, Clock, ConnectRequest, ContextItem, HostEnvironment, HostResult, Json,
    LexiconTerm, LookupTermRequest, Motif, PriorPrompt, PromptArtifact, ProviderCapabilities,
    ProviderEvent, ProviderFault, RealtimeConnection, RealtimeProvider, RecallPromptsRequest,
    ResolveReferenceRequest, RiffHost, RiffStore, SubmitOptions, SubmitResult, SystemClock,
    TermMatch, ToolCallRequest,
};

/// Runs a future to completion on the calling thread.
///
/// The engine has no runtime of its own, so the tests bring the smallest one that works. It parks
/// between polls, which is what lets [`SystemClock`]'s thread-backed delay wake it.
pub fn block_on<F: Future>(future: F) -> F::Output {
    struct ParkWaker(std::thread::Thread);
    impl Wake for ParkWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut future = Box::pin(future);
    let waker = Waker::from(Arc::new(ParkWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park_timeout(std::time::Duration::from_millis(5)),
        }
    }
}

/// A clock that brings some delays forward to now, so a deadline can be observed without waiting.
///
/// The cutoff matters: a session runs two timers of very different magnitudes — a tool call has
/// seconds and the session expiry has most of an hour — and collapsing both makes the expiry
/// warning win every race. `up_to` keeps them apart.
pub struct InstantClock {
    inner: SystemClock,
    instant_up_to_ms: u64,
}

impl InstantClock {
    /// Every delay resolves immediately.
    pub fn new() -> Self {
        Self::up_to(u64::MAX)
    }

    /// Delays of at most `milliseconds` resolve immediately; longer ones never arrive.
    pub fn up_to(milliseconds: u64) -> Self {
        Self {
            inner: SystemClock::new(),
            instant_up_to_ms: milliseconds,
        }
    }
}

impl Clock for InstantClock {
    fn now(&self) -> String {
        self.inner.now()
    }

    fn monotonic_ms(&self) -> u64 {
        self.inner.monotonic_ms()
    }

    fn sleep(&self, milliseconds: u64) -> BoxFuture<'static, ()> {
        if milliseconds <= self.instant_up_to_ms {
            Box::pin(std::future::ready(()))
        } else {
            Box::pin(std::future::pending())
        }
    }
}

/// Everything the fake connection was told to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Audio(usize),
    Text { text: String, respond: bool },
    ToolResult { call_id: String, json: String },
    RequestResponse,
    Cancel,
    Vocabulary(Vec<String>),
    Close,
}

#[derive(Default)]
struct ConnectionState {
    queue: VecDeque<ProviderEvent>,
    calls: Vec<Call>,
    closed: bool,
    waker: Option<Waker>,
}

/// A provider connection the test drives by hand, standing in for a speech-to-speech model.
#[derive(Default)]
pub struct FakeConnection {
    state: Mutex<ConnectionState>,
}

impl FakeConnection {
    fn lock(&self) -> std::sync::MutexGuard<'_, ConnectionState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Queues an event for the session to read on its next step.
    pub fn emit(&self, event: ProviderEvent) {
        let mut state = self.lock();
        state.queue.push_back(event);
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Simulates a completed turn of speech arriving from transcription.
    pub fn say(&self, text: &str) {
        self.emit(ProviderEvent::TranscriptCompleted {
            item_id: "item_1".to_owned(),
            text: text.to_owned(),
            confidence: None,
        });
    }

    /// Simulates a turn in which the model calls one tool.
    pub fn call_tool(&self, name: &str, arguments: Json) -> String {
        self.call_tools(&[(name, arguments)]).remove(0)
    }

    /// Simulates a turn in which the model calls several tools at once.
    pub fn call_tools(&self, calls: &[(&str, Json)]) -> Vec<String> {
        let taken = self.lock().calls.len();
        let requests: Vec<ToolCallRequest> = calls
            .iter()
            .enumerate()
            .map(|(index, (name, arguments))| ToolCallRequest {
                call_id: format!("call_{taken}_{index}"),
                name: (*name).to_owned(),
                arguments_json: arguments.serialize(),
            })
            .collect();
        let ids = requests.iter().map(|call| call.call_id.clone()).collect();
        self.emit(ProviderEvent::ToolCalls { calls: requests });
        ids
    }

    /// Everything the session asked the connection to do, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// The JSON result handed back for a given tool call.
    pub fn result_for(&self, call_id: &str) -> Json {
        self.lock()
            .calls
            .iter()
            .rev()
            .find_map(|call| match call {
                Call::ToolResult { call_id: id, json } if id == call_id => Some(json.clone()),
                _ => None,
            })
            .map(|json| Json::parse(&json).expect("tool results are JSON"))
            .unwrap_or_else(|| panic!("no tool result was sent for {call_id}"))
    }

    /// Ends the event stream the way a transport that simply stops does: no closing event, no
    /// `close()` call, just nothing more to read.
    pub fn finish(&self) {
        let mut state = self.lock();
        state.closed = true;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    fn record(&self, call: Call) {
        self.lock().calls.push(call);
    }
}

struct NextEvent<'a>(&'a FakeConnection);

impl Future for NextEvent<'_> {
    type Output = Option<ProviderEvent>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.0.lock();
        if let Some(event) = state.queue.pop_front() {
            return Poll::Ready(Some(event));
        }
        if state.closed {
            return Poll::Ready(None);
        }
        state.waker = Some(context.waker().clone());
        Poll::Pending
    }
}

impl RealtimeConnection for FakeConnection {
    fn session_id(&self) -> String {
        "sess_fake".to_owned()
    }

    fn model(&self) -> String {
        "fake-realtime".to_owned()
    }

    fn next_event(&self) -> BoxFuture<'_, Option<ProviderEvent>> {
        Box::pin(NextEvent(self))
    }

    fn send_audio(&self, chunk: &[u8]) {
        self.record(Call::Audio(chunk.len()));
    }

    fn commit_audio(&self) {}

    fn send_text(&self, text: &str, respond: bool) {
        self.record(Call::Text {
            text: text.to_owned(),
            respond,
        });
    }

    fn respond_to_tool(&self, call_id: &str, result_json: &str) {
        self.record(Call::ToolResult {
            call_id: call_id.to_owned(),
            json: result_json.to_owned(),
        });
    }

    fn request_response(&self) {
        self.record(Call::RequestResponse);
    }

    fn cancel_response(&self) {
        self.record(Call::Cancel);
    }

    fn update_vocabulary(&self, vocabulary: &[String]) {
        self.record(Call::Vocabulary(vocabulary.to_vec()));
    }

    fn close(&self, _reason: Option<String>) -> BoxFuture<'_, ()> {
        let mut state = self.lock();
        state.calls.push(Call::Close);
        state.closed = true;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
        Box::pin(std::future::ready(()))
    }
}

/// A provider that hands out one connection per connect, as a real one does.
#[derive(Default)]
pub struct FakeProvider {
    state: Mutex<ProviderState>,
}

#[derive(Default)]
struct ProviderState {
    request: Option<ConnectRequest>,
    connection: Option<Arc<FakeConnection>>,
}

impl FakeProvider {
    /// The live connection, which only exists after a connect.
    pub fn connection(&self) -> Arc<FakeConnection> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .connection
            .clone()
            .expect("the session must be started first")
    }

    /// What the session asked the provider for.
    pub fn request(&self) -> ConnectRequest {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .request
            .clone()
            .expect("the session must be started first")
    }
}

impl RealtimeProvider for FakeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let format = AudioFormat {
            encoding: "pcm_s16le".to_owned(),
            sample_rate: 24_000,
            channels: 1,
        };
        ProviderCapabilities {
            speech_to_speech: true,
            barge_in: true,
            semantic_turn_detection: true,
            vocabulary_biasing: "keywords".to_owned(),
            input_transcription: true,
            function_calling: true,
            input_audio: format.clone(),
            output_audio: format,
            max_session_seconds: None,
        }
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'_, Result<Arc<dyn RealtimeConnection>, ProviderFault>> {
        // A fresh connection per connect, as a real provider gives: a closed one is done for good.
        let connection = Arc::new(FakeConnection::default());
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.request = Some(request);
            state.connection = Some(connection.clone());
        }
        Box::pin(async move { Ok(connection as Arc<dyn RealtimeConnection>) })
    }
}

/// A host the test configures and then inspects.
#[derive(Default)]
pub struct RecordingHost {
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    candidates: Vec<ContextItem>,
    known_terms: Vec<String>,
    submitted: Vec<PromptArtifact>,
    environment: HostEnvironment,
    refuse_submission: bool,
    fail_environment: bool,
}

impl RecordingHost {
    fn lock(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// What `resolve_reference` will hand back.
    pub fn set_candidates(&self, candidates: Vec<ContextItem>) {
        self.lock().candidates = candidates;
    }

    /// Vocabulary this world knows about, which is what corroborates a spelling correction.
    pub fn set_known_terms(&self, terms: &[&str]) {
        self.lock().known_terms = terms.iter().map(|term| (*term).to_owned()).collect();
    }

    /// Ambient facts the session states to the model at connect time.
    pub fn set_environment(&self, environment: HostEnvironment) {
        self.lock().environment = environment;
    }

    /// Makes the host refuse to send, as one whose destination is unreachable would.
    pub fn refuse_submission(&self) {
        self.lock().refuse_submission = true;
    }

    /// Makes the host unable to say what the speaker's world contains.
    pub fn fail_environment(&self) {
        self.lock().fail_environment = true;
    }

    /// Every prompt the host was handed.
    pub fn submitted(&self) -> Vec<PromptArtifact> {
        self.lock().submitted.clone()
    }
}

impl RiffHost for RecordingHost {
    fn resolve_reference(
        &self,
        _request: ResolveReferenceRequest,
    ) -> BoxFuture<'_, HostResult<Vec<ContextItem>>> {
        let candidates = self.lock().candidates.clone();
        Box::pin(async move { Ok(candidates) })
    }

    fn lookup_term(&self, request: LookupTermRequest) -> BoxFuture<'_, HostResult<Vec<TermMatch>>> {
        let matches: Vec<TermMatch> = self
            .lock()
            .known_terms
            .iter()
            .filter(|term| term.to_lowercase() == request.heard.to_lowercase())
            .map(|term| TermMatch {
                term: LexiconTerm::new(term.clone(), "product"),
                confidence: Some(1.0),
            })
            .collect();
        Box::pin(async move { Ok(matches) })
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
        let mut state = self.lock();
        if state.refuse_submission {
            return Box::pin(async {
                Ok(SubmitResult {
                    submitted: false,
                    message: Some("the destination is not reachable".to_owned()),
                    ..SubmitResult::default()
                })
            });
        }
        state.submitted.push(artifact);
        Box::pin(async {
            Ok(SubmitResult {
                submitted: true,
                prompt_id: Some("p1".to_owned()),
                destination: Some("test".to_owned()),
                url: Some("https://example.test/p1".to_owned()),
                message: None,
            })
        })
    }

    fn environment(&self) -> BoxFuture<'_, HostResult<HostEnvironment>> {
        let state = self.lock();
        if state.fail_environment {
            return Box::pin(async { Err("the workspace service is unreachable".into()) });
        }
        let environment = state.environment.clone();
        Box::pin(async move { Ok(environment) })
    }
}

/// A host that never answers, for observing what a tool deadline does.
pub struct StalledHost;

impl RiffHost for StalledHost {
    fn resolve_reference(
        &self,
        _request: ResolveReferenceRequest,
    ) -> BoxFuture<'_, HostResult<Vec<ContextItem>>> {
        Box::pin(std::future::pending())
    }

    fn lookup_term(
        &self,
        _request: LookupTermRequest,
    ) -> BoxFuture<'_, HostResult<Vec<TermMatch>>> {
        Box::pin(std::future::pending())
    }

    fn recall_prompts(
        &self,
        _request: RecallPromptsRequest,
    ) -> BoxFuture<'_, HostResult<Vec<PriorPrompt>>> {
        Box::pin(std::future::pending())
    }

    fn submit_prompt(
        &self,
        _artifact: PromptArtifact,
        _options: SubmitOptions,
    ) -> BoxFuture<'_, HostResult<SubmitResult>> {
        Box::pin(std::future::pending())
    }
}

/// A store whose save never returns, for landing a deadline after the host has already taken the
/// prompt.
#[derive(Default)]
pub struct StallingStore;

impl RiffStore for StallingStore {
    fn load_lexicon(&self) -> BoxFuture<'_, HostResult<Vec<LexiconTerm>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn save_term(&self, _term: LexiconTerm) -> BoxFuture<'_, HostResult<()>> {
        Box::pin(std::future::pending())
    }

    fn list_motifs(&self) -> BoxFuture<'_, HostResult<Vec<Motif>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn save_motif(&self, _motif: Motif) -> BoxFuture<'_, HostResult<()>> {
        Box::pin(std::future::pending())
    }

    fn retire_motif(&self, _id: String, _at: String) -> BoxFuture<'_, HostResult<()>> {
        Box::pin(std::future::pending())
    }

    fn save_artifact(&self, _artifact: PromptArtifact) -> BoxFuture<'_, HostResult<()>> {
        Box::pin(std::future::pending())
    }

    fn list_artifacts(&self, _limit: usize) -> BoxFuture<'_, HostResult<Vec<PromptArtifact>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// A store that keeps what it is given but can be told to refuse specific writes, for checking
/// that session state never runs ahead of what was actually persisted.
#[derive(Default)]
pub struct FailingStore {
    motifs: Mutex<Vec<Motif>>,
    fail_save: bool,
    fail_retire: bool,
}

impl FailingStore {
    /// Refuses every write.
    pub fn everything() -> Self {
        Self {
            fail_save: true,
            fail_retire: true,
            ..Self::default()
        }
    }

    /// Accepts a motif but refuses to retire one, so a retirement can be attempted against a motif
    /// that genuinely exists.
    pub fn only_retire() -> Self {
        Self {
            fail_retire: true,
            ..Self::default()
        }
    }
}

impl RiffStore for FailingStore {
    fn load_lexicon(&self) -> BoxFuture<'_, HostResult<Vec<LexiconTerm>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn save_term(&self, _term: LexiconTerm) -> BoxFuture<'_, HostResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn list_motifs(&self) -> BoxFuture<'_, HostResult<Vec<Motif>>> {
        let motifs = self
            .motifs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        Box::pin(async move { Ok(motifs) })
    }

    fn save_motif(&self, motif: Motif) -> BoxFuture<'_, HostResult<()>> {
        if self.fail_save {
            return Box::pin(async { Err("the store is unreachable".into()) });
        }
        self.motifs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(motif);
        Box::pin(async { Ok(()) })
    }

    fn retire_motif(&self, _id: String, _at: String) -> BoxFuture<'_, HostResult<()>> {
        if self.fail_retire {
            return Box::pin(async { Err("the store is unreachable".into()) });
        }
        Box::pin(async { Ok(()) })
    }

    fn save_artifact(&self, _artifact: PromptArtifact) -> BoxFuture<'_, HostResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn list_artifacts(&self, _limit: usize) -> BoxFuture<'_, HostResult<Vec<PromptArtifact>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
