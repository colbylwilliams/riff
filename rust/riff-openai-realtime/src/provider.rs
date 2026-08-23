//! Riff on the OpenAI Realtime API.
//!
//! This is the only place in the Rust binding that knows OpenAI's wire format. It implements the
//! provider interface and nothing more, which is what keeps the agent's behavior — the ledger, the
//! grounding check, the drafts — identical no matter what is generating the speech.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use riff_core::{
    AudioFormat, BoxFuture, Clock, ConnectRequest, Either, Json, ProviderCapabilities,
    ProviderEvent, ProviderFault, RealtimeConnection, RealtimeProvider, SessionDefaults,
    SystemClock, json_object, race,
};

use crate::events::{client_events, map_server_event};
use crate::session_config::{
    BiasingStyle, BuildSessionOptions, biasing_style_for, build_openai_session,
    build_vocabulary_patch,
};
use crate::transport::{
    CredentialProvider, RealtimeTransport, TransportFactory, TransportKind, TransportMessage,
    TransportRequest, encode_base64, endpoint_with_model,
};

/// The default WebSocket endpoint.
pub const DEFAULT_WEBSOCKET_URL: &str = "wss://api.openai.com/v1/realtime";

/// How long to wait for `session.created` before giving up.
const HANDSHAKE_TIMEOUT_MS: u64 = 15_000;

/// How the provider is put together.
pub struct OpenAIRealtimeOptions {
    /// What supplies the token. Use [`crate::ApiKeyCredentials`] only on a server.
    pub credentials: Arc<dyn CredentialProvider>,
    /// What opens the socket.
    pub transport: Arc<dyn TransportFactory>,
    /// Time and delays, which bound the handshake.
    pub clock: Arc<dyn Clock>,
    /// Overrides the model in the bundle's session defaults.
    pub model: Option<String>,
    /// The OpenAI organization, when the account has more than one.
    pub organization: Option<String>,
    /// The OpenAI project.
    pub project: Option<String>,
    /// Point at Azure OpenAI or a gateway. The event protocol is the same.
    pub url: Option<String>,
    /// How long to wait for `session.created`.
    pub handshake_timeout_ms: u64,
}

impl OpenAIRealtimeOptions {
    /// The usual configuration: a credential source and a transport, everything else defaulted.
    pub fn new(
        credentials: Arc<dyn CredentialProvider>,
        transport: Arc<dyn TransportFactory>,
    ) -> Self {
        Self {
            credentials,
            transport,
            clock: Arc::new(SystemClock::new()),
            model: None,
            organization: None,
            project: None,
            url: None,
            handshake_timeout_ms: HANDSHAKE_TIMEOUT_MS,
        }
    }
}

/// The provider.
pub struct OpenAIRealtimeProvider {
    options: OpenAIRealtimeOptions,
}

impl OpenAIRealtimeProvider {
    /// Builds the provider.
    pub fn new(options: OpenAIRealtimeOptions) -> Self {
        Self { options }
    }

    /// How the configured transcription model wants vocabulary hints.
    pub fn biasing_style(&self, session: &SessionDefaults) -> BiasingStyle {
        biasing_style_for(&session.transcription.preferred)
    }
}

impl RealtimeProvider for OpenAIRealtimeProvider {
    fn id(&self) -> &str {
        "openai-realtime"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let pcm = AudioFormat {
            encoding: "pcm_s16le".to_owned(),
            sample_rate: 24_000,
            channels: 1,
        };
        ProviderCapabilities {
            speech_to_speech: true,
            barge_in: true,
            semantic_turn_detection: true,
            vocabulary_biasing: "prompt".to_owned(),
            input_transcription: true,
            function_calling: true,
            input_audio: pcm.clone(),
            output_audio: pcm,
            max_session_seconds: Some(3600),
        }
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'_, Result<Arc<dyn RealtimeConnection>, ProviderFault>> {
        Box::pin(async move {
            let model = self
                .options
                .model
                .clone()
                .unwrap_or_else(|| request.session.model.preferred.clone());
            let credential = self.options.credentials.credential().await?;

            let transport = self
                .options
                .transport
                .open(TransportRequest {
                    url: endpoint_with_model(
                        self.options.url.as_deref().unwrap_or(DEFAULT_WEBSOCKET_URL),
                        &model,
                    ),
                    credential,
                    organization: self.options.organization.clone(),
                    project: self.options.project.clone(),
                })
                .await?;

            let handshake = await_handshake(
                transport.as_ref(),
                self.options.clock.as_ref(),
                self.options.handshake_timeout_ms,
            )
            .await;

            let handshake = match handshake {
                Ok(handshake) => handshake,
                Err(fault) => {
                    transport.close(Some("handshake failed".to_owned())).await;
                    return Err(fault);
                }
            };

            transport.send(&Json::Object(json_object! {
                "type" => client_events::SESSION_UPDATE,
                "session" => build_openai_session(BuildSessionOptions {
                    session: &request.session,
                    instructions: &request.instructions,
                    tools: &request.tools,
                    vocabulary: &request.vocabulary,
                    model: Some(&model),
                }),
            }));

            Ok(Arc::new(OpenAIConnection {
                transport,
                session: request.session,
                session_id: handshake.session_id,
                model: handshake.model,
                pending: Mutex::new(handshake.pending),
            }) as Arc<dyn RealtimeConnection>)
        })
    }
}

struct Handshake {
    session_id: String,
    model: String,
    /// Events that arrived during the handshake, so `session.created` still reaches the session.
    pending: VecDeque<ProviderEvent>,
}

/// Waits for `session.created`, or for something that says it will never come.
///
/// A failure during the handshake has to surface as a connect error rather than as a silent timeout,
/// so a rejected session configuration is reported as what it is.
async fn await_handshake(
    transport: &dyn RealtimeTransport,
    clock: &dyn Clock,
    timeout_ms: u64,
) -> Result<Handshake, ProviderFault> {
    let mut pending = VecDeque::new();
    // Created once and re-polled across iterations, and raced ahead of the message stream. A fresh
    // delay per message would bound only the gap between messages; a delay raced second would never
    // be polled at all while an endpoint keeps a keepalive ready. Either way `connect` would hang.
    let mut deadline = (timeout_ms > 0).then(|| clock.sleep(timeout_ms));

    loop {
        let message = match deadline.as_mut() {
            None => transport.next_message().await,
            Some(deadline) => match race(deadline, transport.next_message()).await {
                Either::Left(()) => {
                    return Err(ProviderFault::retryable(
                        "handshake_failed",
                        format!("timed out after {timeout_ms}ms waiting for session.created"),
                    ));
                }
                Either::Right(message) => message,
            },
        };

        match message {
            Some(TransportMessage::Event(raw)) => {
                let mapped = map_server_event(&raw);
                let identity = mapped
                    .session_id
                    .clone()
                    .map(|session_id| (session_id, mapped.model.clone().unwrap_or_default()));
                let failure = mapped.events.iter().find_map(|event| match event {
                    ProviderEvent::Failed { fault } => Some(fault.clone()),
                    _ => None,
                });
                pending.extend(mapped.events);

                if let Some(fault) = failure {
                    return Err(ProviderFault::retryable("handshake_failed", fault.message));
                }
                if let Some((session_id, model)) = identity {
                    return Ok(Handshake {
                        session_id,
                        model,
                        pending,
                    });
                }
            }
            Some(TransportMessage::Failed(fault)) => {
                return Err(ProviderFault::retryable("handshake_failed", fault.message));
            }
            Some(TransportMessage::Closed(reason)) => {
                return Err(closed_during_handshake(reason));
            }
            None => return Err(closed_during_handshake(None)),
        }
    }
}

fn closed_during_handshake(reason: Option<String>) -> ProviderFault {
    ProviderFault::retryable(
        "handshake_failed",
        match reason {
            Some(reason) => format!("connection closed: {reason}"),
            None => "connection closed".to_owned(),
        },
    )
}

struct OpenAIConnection {
    transport: Arc<dyn RealtimeTransport>,
    session: SessionDefaults,
    session_id: String,
    model: String,
    pending: Mutex<VecDeque<ProviderEvent>>,
}

impl OpenAIConnection {
    fn take_pending(&self) -> Option<ProviderEvent> {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
    }

    fn push_pending<I: IntoIterator<Item = ProviderEvent>>(&self, events: I) {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .extend(events);
    }

    fn create_item(&self, item: Json) {
        self.transport.send(&Json::Object(json_object! {
            "type" => client_events::CREATE_ITEM,
            "item" => item,
        }));
    }
}

impl RealtimeConnection for OpenAIConnection {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn next_event(&self) -> BoxFuture<'_, Option<ProviderEvent>> {
        Box::pin(async move {
            loop {
                if let Some(event) = self.take_pending() {
                    return Some(event);
                }
                match self.transport.next_message().await {
                    Some(TransportMessage::Event(raw)) => {
                        self.push_pending(map_server_event(&raw).events);
                    }
                    Some(TransportMessage::Failed(fault)) => {
                        return Some(ProviderEvent::Failed { fault });
                    }
                    Some(TransportMessage::Closed(reason)) => {
                        return Some(ProviderEvent::Closed { reason });
                    }
                    None => return None,
                }
            }
        })
    }

    fn send_audio(&self, chunk: &[u8]) {
        if self.transport.kind() == TransportKind::WebRtc {
            // Audio travels on the negotiated media track, which the host attached itself.
            self.transport.send_audio(chunk);
            return;
        }
        self.transport.send(&Json::Object(json_object! {
            "type" => client_events::APPEND_AUDIO,
            "audio" => encode_base64(chunk),
        }));
    }

    fn commit_audio(&self) {
        self.transport.send(&Json::Object(
            json_object! { "type" => client_events::COMMIT_AUDIO },
        ));
    }

    fn send_text(&self, text: &str, respond: bool) {
        self.create_item(Json::Object(json_object! {
            "type" => "message",
            "role" => "user",
            "content" => vec![Json::Object(json_object! {
                "type" => "input_text",
                "text" => text,
            })],
        }));
        if respond {
            self.request_response();
        }
    }

    fn respond_to_tool(&self, call_id: &str, result_json: &str) {
        self.create_item(Json::Object(json_object! {
            "type" => "function_call_output",
            "call_id" => call_id,
            "output" => result_json,
        }));
    }

    fn request_response(&self) {
        self.transport.send(&Json::Object(
            json_object! { "type" => client_events::CREATE_RESPONSE },
        ));
    }

    fn cancel_response(&self) {
        self.transport.send(&Json::Object(
            json_object! { "type" => client_events::CANCEL_RESPONSE },
        ));
        // On WebRTC the already-buffered audio keeps playing unless it is explicitly dropped, so the
        // agent would talk over someone who just interrupted it.
        if self.transport.kind() == TransportKind::WebRtc {
            self.transport.send(&Json::Object(
                json_object! { "type" => client_events::CLEAR_OUTPUT_AUDIO },
            ));
        }
    }

    fn update_vocabulary(&self, vocabulary: &[String]) {
        if let Some(patch) = build_vocabulary_patch(&self.session, vocabulary) {
            self.transport.send(&Json::Object(json_object! {
                "type" => client_events::SESSION_UPDATE,
                "session" => patch,
            }));
        }
    }

    fn close(&self, reason: Option<String>) -> BoxFuture<'_, ()> {
        self.transport.close(reason)
    }
}
