//! Transport-level plumbing, and the credentials that open a session.
//!
//! Rust has no socket or HTTP client in its standard library, and this crate depends only on the
//! engine, so the socket is an interface the embedder implements rather than a dependency Riff
//! acquires on their behalf. Everything above this line — the session shape, the event mapping — is
//! the same whichever socket they bring.

use std::sync::Arc;

use riff_core::{BoxFuture, Json, ProviderFault};

/// Which transport a connection is running over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// JSON events and base64 audio over one socket. Works everywhere.
    WebSocket,
    /// JSON events on a data channel, audio on a negotiated media track. Better on a phone.
    WebRtc,
}

/// One thing that arrived from the server.
#[derive(Debug, Clone)]
pub enum TransportMessage {
    /// A server event, already parsed.
    Event(Json),
    /// The transport failed.
    Failed(ProviderFault),
    /// The transport closed.
    Closed(Option<String>),
}

/// An open connection to the Realtime API.
pub trait RealtimeTransport: Send + Sync {
    /// Whether audio travels on its own channel rather than through JSON events.
    fn kind(&self) -> TransportKind;

    /// Sends one client event.
    fn send(&self, message: &Json);

    /// Sends captured audio.
    ///
    /// On WebRTC the host attaches the microphone to the negotiated track instead, and an
    /// implementation there does nothing here.
    fn send_audio(&self, pcm: &[u8]);

    /// The next thing from the server, or `None` once the transport is finished for good.
    fn next_message(&self) -> BoxFuture<'_, Option<TransportMessage>>;

    /// Closes the transport.
    fn close(&self, reason: Option<String>) -> BoxFuture<'_, ()>;
}

/// What a transport needs in order to open.
#[derive(Debug, Clone)]
pub struct TransportRequest {
    /// The endpoint, with the model already appended.
    pub url: String,
    /// What to authenticate with.
    pub credential: Credential,
    /// The OpenAI organization, when the account has more than one.
    pub organization: Option<String>,
    /// The OpenAI project.
    pub project: Option<String>,
}

/// Opens transports on demand, so a reconnect does not reuse a finished socket.
pub trait TransportFactory: Send + Sync {
    /// Opens one.
    fn open(
        &self,
        request: TransportRequest,
    ) -> BoxFuture<'_, Result<Arc<dyn RealtimeTransport>, ProviderFault>>;
}

/// What a token is, which is the difference between a server and a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// A durable API key. Server-side only.
    ApiKey,
    /// A short-lived secret minted behind the embedder's own auth.
    ClientSecret,
}

/// A token and what sort of token it is.
#[derive(Debug, Clone)]
pub struct Credential {
    /// The bearer token.
    pub token: String,
    /// What sort it is.
    pub kind: CredentialKind,
}

/// Supplies the token a connection opens with.
pub trait CredentialProvider: Send + Sync {
    /// The token to use now. Called once per connect, so an implementation can refresh.
    fn credential(&self) -> BoxFuture<'_, Result<Credential, ProviderFault>>;
}

/// For server-side use only.
///
/// An API key on a phone or in a browser is a key you have published, so clients mint a short-lived
/// client secret instead — see [`crate::client_secret_request`]. The type is named to make the
/// difference impossible to miss at a call site.
#[derive(Debug, Clone)]
pub struct ApiKeyCredentials {
    key: String,
}

impl ApiKeyCredentials {
    /// Wraps a durable API key. Never construct one on a device.
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

impl CredentialProvider for ApiKeyCredentials {
    fn credential(&self) -> BoxFuture<'_, Result<Credential, ProviderFault>> {
        let credential = Credential {
            token: self.key.clone(),
            kind: CredentialKind::ApiKey,
        };
        Box::pin(async move { Ok(credential) })
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes audio the way the Realtime API takes it on a WebSocket.
pub fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(BASE64_ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(BASE64_ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes audio arriving from the server, ignoring padding and line breaks.
pub fn decode_base64(value: &str) -> Vec<u8> {
    let symbols: Vec<u8> = value
        .bytes()
        .filter_map(|byte| BASE64_ALPHABET.iter().position(|entry| *entry == byte))
        .map(|index| index as u8)
        .collect();

    let mut out = Vec::with_capacity(symbols.len() * 3 / 4);
    for chunk in symbols.chunks(4) {
        // A lone trailing symbol carries no whole byte. Emitting one would shift every sample after
        // it, so a malformed payload costs nothing rather than corrupting the stream.
        if chunk.len() < 2 {
            break;
        }
        let mut packed = 0u32;
        for (index, symbol) in chunk.iter().enumerate() {
            packed |= u32::from(*symbol) << (18 - 6 * index);
        }
        out.push((packed >> 16) as u8);
        if chunk.len() > 2 {
            out.push((packed >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(packed as u8);
        }
    }
    out
}

/// Adds the model to an endpoint without disturbing what is already there.
///
/// Azure and gateway endpoints carry required parameters such as `api-version` and `deployment`.
/// Appending `?model=` to those produces a second `?` and folds the model into the previous value,
/// which fails in a way that looks like an auth problem.
pub fn endpoint_with_model(base: &str, model: &str) -> String {
    let separator = if base.contains('?') { '&' } else { '?' };
    format!("{base}{separator}model={}", percent_encode(model))
}

/// Percent-encodes everything outside the unreserved set, which is enough for a model name.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_audio() {
        for length in 0..8usize {
            let bytes: Vec<u8> = (0..length).map(|index| index as u8 * 37).collect();
            assert_eq!(
                decode_base64(&encode_base64(&bytes)),
                bytes,
                "length {length}"
            );
        }
    }

    #[test]
    fn ignores_a_lone_trailing_symbol_rather_than_shifting_the_stream() {
        // Four symbols carry three bytes; a fifth on its own carries none. Emitting a byte for it
        // would shift every sample the host plays after it.
        assert_eq!(decode_base64("TWFuX"), b"Man");
    }

    #[test]
    fn matches_the_canonical_encoding() {
        assert_eq!(encode_base64(b"Man"), "TWFu");
        assert_eq!(encode_base64(b"Ma"), "TWE=");
        assert_eq!(encode_base64(b"M"), "TQ==");
        assert_eq!(decode_base64("TWFu"), b"Man");
    }

    #[test]
    fn preserves_query_parameters_a_custom_endpoint_already_carries() {
        assert_eq!(
            endpoint_with_model(
                "https://example.test/realtime?api-version=2026-01-01",
                "gpt-realtime-2.1"
            ),
            "https://example.test/realtime?api-version=2026-01-01&model=gpt-realtime-2.1"
        );
        assert_eq!(
            endpoint_with_model("wss://api.openai.com/v1/realtime", "gpt-realtime-2.1"),
            "wss://api.openai.com/v1/realtime?model=gpt-realtime-2.1"
        );
    }
}
