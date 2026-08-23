//! Minting the ephemeral client secret a device connects with.
//!
//! Run this behind your own authenticated endpoint. It is the one part of the provider that must
//! stay on a server, and the whole reason Riff's client side never sees a long-lived credential.
//!
//! The HTTP call is yours to make. This module builds the request and reads the response, so the
//! crate stays free of an HTTP client the embedder did not choose — and so the API key stays in code
//! you control rather than being handed to a library.

use std::fmt;

use riff_core::{Json, ProviderFault, SessionDefaults, ToolDefinition};

use crate::session_config::{BuildSessionOptions, build_openai_session};
use crate::transport::REDACTED;

/// The default OpenAI API root.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// What the client secret should be minted for.
pub struct MintClientSecretOptions<'a> {
    /// Server side only. This must never reach a phone or a browser.
    pub api_key: &'a str,
    /// The session the secret authorizes.
    pub session: &'a SessionDefaults,
    /// The composed system prompt.
    pub instructions: &'a str,
    /// The tools the model may call.
    pub tools: &'a [ToolDefinition],
    /// Canonical spellings to bias transcription toward.
    pub vocabulary: &'a [String],
    /// Overrides the model in the session defaults.
    pub model: Option<&'a str>,
    /// 10 to 7200 seconds. The API default is 600.
    pub expires_in_seconds: Option<u32>,
    /// Point at Azure OpenAI or a gateway.
    pub base_url: Option<&'a str>,
    /// Hashed user id for abuse monitoring. Never send a raw identifier.
    pub safety_identifier: Option<&'a str>,
}

/// An HTTP request for the embedder to make.
#[derive(Clone)]
pub struct ClientSecretRequest {
    /// Where to POST.
    pub url: String,
    /// Headers to send, including the `Authorization` the key goes in.
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: String,
}

/// Written with header names but not their values, and the body by length.
///
/// One of those headers carries the durable API key. Tracing a failed mint is exactly when someone
/// reaches for `{:?}`, which is exactly when the key would reach a log.
impl fmt::Debug for ClientSecretRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientSecretRequest")
            .field("url", &self.url)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// A short-lived secret a device may connect with.
#[derive(Clone)]
pub struct ClientSecret {
    /// The token.
    pub value: String,
    /// When it stops working, as a Unix timestamp, when the API said.
    pub expires_at: Option<i64>,
}

/// Written without the value. Short-lived is not the same as harmless: it is still a bearer token.
impl fmt::Debug for ClientSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientSecret")
            .field("value", &REDACTED)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Builds the request that mints a client secret.
pub fn client_secret_request(options: &MintClientSecretOptions<'_>) -> ClientSecretRequest {
    let session = build_openai_session(BuildSessionOptions {
        session: options.session,
        instructions: options.instructions,
        tools: options.tools,
        vocabulary: options.vocabulary,
        model: options.model,
    });

    let mut body = riff_core::JsonObject::new();
    if let Some(seconds) = options.expires_in_seconds {
        body.insert(
            "expires_after",
            Json::Object(riff_core::json_object! {
                "anchor" => "created_at",
                "seconds" => f64::from(seconds),
            }),
        );
    }
    body.insert("session", session);

    let mut headers = vec![
        (
            "Authorization".to_owned(),
            format!("Bearer {}", options.api_key),
        ),
        ("Content-Type".to_owned(), "application/json".to_owned()),
    ];
    if let Some(identifier) = options.safety_identifier {
        headers.push(("OpenAI-Safety-Identifier".to_owned(), identifier.to_owned()));
    }

    ClientSecretRequest {
        url: format!(
            "{}/realtime/client_secrets",
            options.base_url.unwrap_or(DEFAULT_BASE_URL)
        ),
        headers,
        body: Json::Object(body).serialize(),
    }
}

/// Reads what the API answered with.
pub fn parse_client_secret(status: u16, body: &str) -> Result<ClientSecret, ProviderFault> {
    if !(200..300).contains(&status) {
        let detail: String = body.chars().take(300).collect();
        return Err(ProviderFault::retryable(
            "client_secret_failed",
            format!("could not mint a realtime client secret ({status}): {detail}"),
        ));
    }

    let parsed = Json::parse(body).map_err(|error| {
        ProviderFault::retryable(
            "client_secret_failed",
            format!("the client secret response was not JSON: {error}"),
        )
    })?;

    let value = parsed.get_str("value").ok_or_else(|| {
        ProviderFault::retryable(
            "client_secret_failed",
            "the client secret response had no value",
        )
    })?;

    Ok(ClientSecret {
        value: value.to_owned(),
        expires_at: parsed.get("expires_at").and_then(Json::as_i64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use riff_core::AgentBundle;

    #[test]
    fn puts_the_key_in_a_header_and_the_session_in_the_body() {
        let bundle = AgentBundle::bundled().unwrap();
        let request = client_secret_request(&MintClientSecretOptions {
            api_key: "sk-test",
            session: &bundle.session,
            instructions: &bundle.instructions,
            tools: &bundle.tools,
            vocabulary: &[],
            model: None,
            expires_in_seconds: Some(120),
            base_url: None,
            safety_identifier: Some("hashed-user"),
        });

        assert_eq!(
            request.url,
            "https://api.openai.com/v1/realtime/client_secrets"
        );
        assert!(
            request
                .headers
                .contains(&("Authorization".to_owned(), "Bearer sk-test".to_owned()))
        );
        assert!(
            request
                .headers
                .iter()
                .any(|(name, _)| name == "OpenAI-Safety-Identifier")
        );

        let body = Json::parse(&request.body).unwrap();
        assert_eq!(
            body.get("expires_after")
                .and_then(|after| after.get("seconds"))
                .and_then(Json::as_i64),
            Some(120)
        );
        assert_eq!(
            body.get("session")
                .and_then(|session| session.get_str("type")),
            Some("realtime")
        );
        // The key travels in the header, never in the body.
        assert!(!request.body.contains("sk-test"));
    }

    #[test]
    fn keeps_credentials_out_of_diagnostic_output() {
        let bundle = AgentBundle::bundled().unwrap();
        let request = client_secret_request(&MintClientSecretOptions {
            api_key: "sk-super-secret",
            session: &bundle.session,
            instructions: "",
            tools: &[],
            vocabulary: &[],
            model: None,
            expires_in_seconds: None,
            base_url: None,
            safety_identifier: None,
        });
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("sk-super-secret"), "{rendered}");
        assert!(
            rendered.contains("Authorization"),
            "the header is still named"
        );

        let secret = ClientSecret {
            value: "ek_super_secret".to_owned(),
            expires_at: Some(123),
        };
        assert!(!format!("{secret:?}").contains("ek_super_secret"));
    }

    #[test]
    fn reports_a_refusal_rather_than_returning_an_empty_secret() {
        let error = parse_client_secret(401, "{\"error\":\"nope\"}").unwrap_err();
        assert!(error.message.contains("401"));
        assert!(parse_client_secret(200, "{}").is_err());
        assert_eq!(
            parse_client_secret(200, "{\"value\":\"ek_1\",\"expires_at\":123}")
                .unwrap()
                .expires_at,
            Some(123)
        );
    }
}
