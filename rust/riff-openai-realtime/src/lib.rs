//! Riff on the OpenAI Realtime API.
//!
//! This crate is the only place in the Rust binding that knows OpenAI's wire format. Everything
//! above [`riff_core::RealtimeProvider`] — the ledger, the grounding check, drafts, takes, the
//! artifact — is provider independent, so swapping this for another provider changes no behavior a
//! speaker can observe.
//!
//! # What you supply
//!
//! Rust ships no socket or HTTP client, and this crate depends only on the engine, so two seams are
//! yours:
//!
//! - [`TransportFactory`] opens the WebSocket (or WebRTC data channel) and hands back parsed server
//!   events. [`encode_base64`] and [`endpoint_with_model`] are here so an implementation does not
//!   have to work them out.
//! - [`client_secret_request`] builds the HTTP request that mints an ephemeral client secret, and
//!   [`parse_client_secret`] reads the answer. **Make that call from a server**, behind your own
//!   auth: an API key on a device is a key you have published. [`ApiKeyCredentials`] is named to
//!   make that obvious at the call site.

pub mod client_secret;
pub mod events;
pub mod provider;
pub mod session_config;
pub mod transport;

pub use client_secret::{
    ClientSecret, ClientSecretRequest, DEFAULT_BASE_URL, MintClientSecretOptions,
    client_secret_request, parse_client_secret,
};
pub use events::{MappedEvents, client_events, map_server_event, server_events};
pub use provider::{DEFAULT_WEBSOCKET_URL, OpenAIRealtimeOptions, OpenAIRealtimeProvider};
pub use session_config::{
    BiasingStyle, BuildSessionOptions, REASONING_MODELS, biasing_style_for, build_openai_session,
    build_vocabulary_patch, is_reasoning_model,
};
pub use transport::{
    ApiKeyCredentials, Credential, CredentialKind, CredentialProvider, RealtimeTransport,
    TransportFactory, TransportKind, TransportMessage, TransportRequest, decode_base64,
    encode_base64, endpoint_with_model,
};
