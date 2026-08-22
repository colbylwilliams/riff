//! What the provider puts on the wire, and what it makes of what comes back.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use riff_core::{
    AgentBundle, BoxFuture, ConnectRequest, Json, ProviderEvent, ProviderFault, RealtimeProvider,
    SessionDefaults, json_object,
};
use riff_openai_realtime::{
    ApiKeyCredentials, BiasingStyle, BuildSessionOptions, OpenAIRealtimeOptions,
    OpenAIRealtimeProvider, RealtimeTransport, TransportFactory, TransportKind, TransportMessage,
    TransportRequest, biasing_style_for, build_openai_session, is_reasoning_model,
};

fn block_on<F: Future>(future: F) -> F::Output {
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

fn bundle() -> AgentBundle {
    AgentBundle::bundled().expect("the vendored bundle must load")
}

fn session_for(model: &str, transcription: &str) -> SessionDefaults {
    let mut session = bundle().session;
    session.model.preferred = model.to_owned();
    session.transcription.preferred = transcription.to_owned();
    session
}

fn built(session: &SessionDefaults, vocabulary: &[String]) -> Json {
    let bundle = bundle();
    build_openai_session(BuildSessionOptions {
        session,
        instructions: &bundle.instructions,
        tools: &bundle.tools,
        vocabulary,
        model: None,
    })
}

#[test]
fn uses_the_ga_session_shape_rather_than_the_beta_one() {
    let payload = built(&bundle().session, &[]);

    assert_eq!(payload.get_str("type"), Some("realtime"));
    assert!(
        payload.get("modalities").is_none(),
        "the beta field must not appear"
    );
    assert_eq!(
        payload
            .get("output_modalities")
            .map(Json::array_or_empty)
            .and_then(|modalities| modalities.first())
            .and_then(Json::as_str),
        Some("audio")
    );

    let input = payload
        .get("audio")
        .and_then(|audio| audio.get("input"))
        .unwrap();
    assert_eq!(
        input
            .get("format")
            .and_then(|format| format.get_str("type")),
        Some("audio/pcm"),
        "audio formats are objects on GA, not strings"
    );
    assert_eq!(
        input
            .get("turn_detection")
            .and_then(|detection| detection.get_str("type")),
        Some("semantic_vad")
    );

    let tools = payload
        .get("tools")
        .map(Json::array_or_empty)
        .unwrap_or(&[]);
    assert_eq!(tools.len(), bundle().tools.len());
    assert_eq!(tools[0].get_str("type"), Some("function"));
}

#[test]
fn only_sends_reasoning_settings_to_models_that_accept_them() {
    assert!(is_reasoning_model("gpt-realtime-2.1"));
    assert!(is_reasoning_model("gpt-realtime-2.1-2026-01-01"));
    assert!(!is_reasoning_model("gpt-realtime"));

    let reasoning = built(&session_for("gpt-realtime-2.1", "gpt-4o-transcribe"), &[]);
    assert!(reasoning.get("reasoning").is_some());
    assert!(reasoning.get("parallel_tool_calls").is_some());

    // Sent to a model that does not take them, these are rejected outright.
    let plain = built(&session_for("gpt-realtime", "gpt-4o-transcribe"), &[]);
    assert!(plain.get("reasoning").is_none());
    assert!(plain.get("parallel_tool_calls").is_none());
}

#[test]
fn knows_how_each_transcription_model_takes_hints() {
    assert_eq!(biasing_style_for("gpt-transcribe"), BiasingStyle::Keywords);
    assert_eq!(biasing_style_for("gpt-4o-transcribe"), BiasingStyle::Prompt);
    assert_eq!(
        biasing_style_for("gpt-4o-transcribe-diarize"),
        BiasingStyle::None
    );
    assert_eq!(biasing_style_for("something-new"), BiasingStyle::Prompt);

    let vocabulary = vec!["GitHub".to_owned(), "Kubernetes".to_owned()];

    let keywords = built(
        &session_for("gpt-realtime-2.1", "gpt-transcribe"),
        &vocabulary,
    );
    let transcription = keywords
        .get("audio")
        .and_then(|audio| audio.get("input"))
        .and_then(|input| input.get("transcription"))
        .unwrap();
    assert_eq!(
        transcription
            .get("keywords")
            .map(Json::array_or_empty)
            .map(<[Json]>::len),
        Some(2)
    );
    assert!(transcription.get("prompt").is_none());

    let prompted = built(
        &session_for("gpt-realtime-2.1", "gpt-4o-transcribe"),
        &vocabulary,
    );
    let transcription = prompted
        .get("audio")
        .and_then(|audio| audio.get("input"))
        .and_then(|input| input.get("transcription"))
        .unwrap();
    assert!(
        transcription
            .get_str("prompt")
            .is_some_and(|prompt| prompt.contains("GitHub"))
    );
    assert!(transcription.get("keywords").is_none());

    let silent = built(
        &session_for("gpt-realtime-2.1", "gpt-4o-transcribe-diarize"),
        &vocabulary,
    );
    let transcription = silent
        .get("audio")
        .and_then(|audio| audio.get("input"))
        .and_then(|input| input.get("transcription"))
        .unwrap();
    assert!(transcription.get("prompt").is_none());
    assert!(transcription.get("keywords").is_none());
}

/// A transport the test drives by hand.
struct FakeTransport {
    state: Mutex<FakeState>,
    kind: TransportKind,
}

#[derive(Default)]
struct FakeState {
    sent: Vec<Json>,
    audio: Vec<Vec<u8>>,
    incoming: VecDeque<TransportMessage>,
    closed: bool,
    waker: Option<Waker>,
}

impl FakeTransport {
    fn new(kind: TransportKind) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState::default()),
            kind,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn deliver(&self, message: TransportMessage) {
        let mut state = self.lock();
        state.incoming.push_back(message);
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    fn sent(&self) -> Vec<Json> {
        self.lock().sent.clone()
    }

    fn sent_types(&self) -> Vec<String> {
        self.sent()
            .iter()
            .filter_map(|message| message.get_str("type").map(str::to_owned))
            .collect()
    }
}

struct NextMessage<'a>(&'a FakeTransport);

impl Future for NextMessage<'_> {
    type Output = Option<TransportMessage>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.0.lock();
        if let Some(message) = state.incoming.pop_front() {
            return Poll::Ready(Some(message));
        }
        if state.closed {
            return Poll::Ready(None);
        }
        state.waker = Some(context.waker().clone());
        Poll::Pending
    }
}

impl RealtimeTransport for FakeTransport {
    fn kind(&self) -> TransportKind {
        self.kind
    }

    fn send(&self, message: &Json) {
        self.lock().sent.push(message.clone());
    }

    fn send_audio(&self, pcm: &[u8]) {
        self.lock().audio.push(pcm.to_vec());
    }

    fn next_message(&self) -> BoxFuture<'_, Option<TransportMessage>> {
        Box::pin(NextMessage(self))
    }

    fn close(&self, _reason: Option<String>) -> BoxFuture<'_, ()> {
        let mut state = self.lock();
        state.closed = true;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
        Box::pin(std::future::ready(()))
    }
}

struct FakeFactory {
    transport: Arc<FakeTransport>,
    request: Mutex<Option<TransportRequest>>,
}

impl TransportFactory for FakeFactory {
    fn open(
        &self,
        request: TransportRequest,
    ) -> BoxFuture<'_, Result<Arc<dyn RealtimeTransport>, ProviderFault>> {
        *self.request.lock().unwrap() = Some(request);
        let transport = self.transport.clone();
        Box::pin(async move { Ok(transport as Arc<dyn RealtimeTransport>) })
    }
}

fn session_created() -> TransportMessage {
    TransportMessage::Event(Json::Object(json_object! {
        "type" => "session.created",
        "session" => Json::Object(json_object! { "id" => "sess_1", "model" => "gpt-realtime-2.1" }),
    }))
}

fn connect(
    kind: TransportKind,
) -> (
    Arc<dyn riff_core::RealtimeConnection>,
    Arc<FakeTransport>,
    Arc<FakeFactory>,
) {
    let transport = FakeTransport::new(kind);
    let factory = Arc::new(FakeFactory {
        transport: transport.clone(),
        request: Mutex::new(None),
    });

    // Queued before connecting, so the handshake finds it waiting.
    transport.deliver(session_created());

    let bundle = bundle();
    let provider = OpenAIRealtimeProvider::new(OpenAIRealtimeOptions::new(
        Arc::new(ApiKeyCredentials::new("sk-test")),
        factory.clone(),
    ));
    let connection = block_on(provider.connect(ConnectRequest {
        instructions: bundle.instructions.clone(),
        tools: bundle.tools.clone(),
        session: bundle.session.clone(),
        vocabulary: vec!["GitHub".to_owned()],
    }))
    .expect("the fake transport completes the handshake");

    (connection, transport, factory)
}

#[test]
fn configures_the_session_once_the_handshake_lands() {
    let (connection, transport, factory) = connect(TransportKind::WebSocket);

    assert_eq!(connection.session_id(), "sess_1");
    assert_eq!(connection.model(), "gpt-realtime-2.1");

    let request = factory.request.lock().unwrap().clone().expect("opened");
    assert!(request.url.contains("model=gpt-realtime-2.1"));
    assert_eq!(request.credential.token, "sk-test");

    let update = transport
        .sent()
        .into_iter()
        .find(|message| message.get_str("type") == Some("session.update"))
        .expect("the session has to be configured after the handshake");
    assert_eq!(
        update
            .get("session")
            .and_then(|session| session.get_str("type")),
        Some("realtime")
    );

    // `session.created` still reaches the engine rather than being swallowed by the handshake.
    assert!(matches!(
        block_on(connection.next_event()),
        Some(ProviderEvent::Connected { .. })
    ));
}

#[test]
fn refuses_a_handshake_the_server_rejected_instead_of_waiting_for_a_timeout() {
    let transport = FakeTransport::new(TransportKind::WebSocket);
    let factory = Arc::new(FakeFactory {
        transport: transport.clone(),
        request: Mutex::new(None),
    });
    transport.deliver(TransportMessage::Event(Json::Object(json_object! {
        "type" => "error",
        "error" => Json::Object(json_object! {
            "code" => "invalid_request_error",
            "message" => "unknown parameter",
        }),
    })));

    let bundle = bundle();
    let provider = OpenAIRealtimeProvider::new(OpenAIRealtimeOptions::new(
        Arc::new(ApiKeyCredentials::new("sk-test")),
        factory,
    ));
    let Err(fault) = block_on(provider.connect(ConnectRequest {
        instructions: bundle.instructions.clone(),
        tools: bundle.tools.clone(),
        session: bundle.session.clone(),
        vocabulary: Vec::new(),
    })) else {
        panic!("a rejected session must surface as a connect failure");
    };

    assert_eq!(fault.code, "handshake_failed");
    assert!(fault.message.contains("unknown parameter"));
}

#[test]
fn drops_buffered_audio_when_interrupted_over_webrtc() {
    let (websocket, ws_transport, _) = connect(TransportKind::WebSocket);
    websocket.cancel_response();
    assert_eq!(
        ws_transport
            .sent_types()
            .iter()
            .filter(|kind| *kind == "output_audio_buffer.clear")
            .count(),
        0,
        "a WebSocket has no separate playback buffer to drop"
    );

    let (webrtc, rtc_transport, _) = connect(TransportKind::WebRtc);
    webrtc.cancel_response();
    assert!(
        rtc_transport
            .sent_types()
            .contains(&"output_audio_buffer.clear".to_owned()),
        "otherwise the agent talks over someone who just interrupted it"
    );
}

#[test]
fn sends_audio_as_base64_events_on_a_websocket_and_out_of_band_on_webrtc() {
    let (websocket, ws_transport, _) = connect(TransportKind::WebSocket);
    websocket.send_audio(b"Man");
    let appended = ws_transport
        .sent()
        .into_iter()
        .find(|message| message.get_str("type") == Some("input_audio_buffer.append"))
        .expect("audio has to reach the socket");
    assert_eq!(appended.get_str("audio"), Some("TWFu"));

    let (webrtc, rtc_transport, _) = connect(TransportKind::WebRtc);
    webrtc.send_audio(b"Man");
    assert!(
        !rtc_transport
            .sent_types()
            .contains(&"input_audio_buffer.append".to_owned()),
        "on WebRTC audio travels on the media track"
    );
    assert_eq!(rtc_transport.lock().audio, [b"Man".to_vec()]);
}

#[test]
fn answers_a_tool_call_and_asks_for_one_continuation() {
    let (connection, transport, _) = connect(TransportKind::WebSocket);
    connection.respond_to_tool("call_1", "{\"ok\":true}");
    connection.request_response();

    let sent = transport.sent();
    let output = sent
        .iter()
        .find(|message| message.get_str("type") == Some("conversation.item.create"))
        .and_then(|message| message.get("item"))
        .expect("a tool result is a conversation item");
    assert_eq!(output.get_str("type"), Some("function_call_output"));
    assert_eq!(output.get_str("call_id"), Some("call_1"));
    assert_eq!(output.get_str("output"), Some("{\"ok\":true}"));
    assert_eq!(
        transport
            .sent_types()
            .iter()
            .filter(|kind| *kind == "response.create")
            .count(),
        1
    );
}

#[test]
fn pushes_new_vocabulary_without_resending_the_whole_configuration() {
    let (connection, transport, _) = connect(TransportKind::WebSocket);
    connection.update_vocabulary(&["Kubernetes".to_owned()]);

    let patch = transport
        .sent()
        .into_iter()
        .rfind(|message| message.get_str("type") == Some("session.update"))
        .and_then(|message| message.get("session").cloned())
        .expect("a vocabulary patch");

    assert!(
        patch.get("instructions").is_none(),
        "the patch is not the whole session"
    );
    assert!(patch.get("tools").is_none());
    let transcription = patch
        .get("audio")
        .and_then(|audio| audio.get("input"))
        .and_then(|input| input.get("transcription"))
        .unwrap();
    assert!(
        transcription
            .get_str("prompt")
            .is_some_and(|prompt| prompt.contains("Kubernetes"))
    );
}

#[test]
fn bounds_the_whole_handshake_rather_than_the_gap_between_messages() {
    struct SleeplessClock(riff_core::SystemClock);
    impl riff_core::Clock for SleeplessClock {
        fn now(&self) -> String {
            self.0.now()
        }
        fn monotonic_ms(&self) -> u64 {
            self.0.monotonic_ms()
        }
        fn sleep(&self, _milliseconds: u64) -> BoxFuture<'static, ()> {
            Box::pin(std::future::ready(()))
        }
    }

    let transport = FakeTransport::new(TransportKind::WebSocket);
    let factory = Arc::new(FakeFactory {
        transport: transport.clone(),
        request: Mutex::new(None),
    });

    // A chatty endpoint that never says `session.created`. A deadline recreated per message would
    // never fire, and `connect` would stay pending forever.
    for _ in 0..4 {
        transport.deliver(TransportMessage::Event(Json::Object(
            json_object! { "type" => "rate_limits.updated" },
        )));
    }

    let bundle = bundle();
    let mut options =
        OpenAIRealtimeOptions::new(Arc::new(ApiKeyCredentials::new("sk-test")), factory);
    options.clock = Arc::new(SleeplessClock(riff_core::SystemClock::new()));

    let provider = OpenAIRealtimeProvider::new(options);
    let Err(fault) = block_on(provider.connect(ConnectRequest {
        instructions: bundle.instructions.clone(),
        tools: bundle.tools.clone(),
        session: bundle.session.clone(),
        vocabulary: Vec::new(),
    })) else {
        panic!("a handshake that never completes must time out");
    };

    assert_eq!(fault.code, "handshake_failed");
    assert!(fault.message.contains("timed out"), "got {}", fault.message);
}
