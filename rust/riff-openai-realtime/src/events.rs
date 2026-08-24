//! Turning OpenAI server events into provider events, and naming the client events Riff sends.

use riff_core::{Json, ProviderEvent, ProviderFault, TokenUsage, ToolCallRequest};

use crate::transport::decode_base64;

/// Server event names on the GA interface.
///
/// The beta names for the same events differ (`response.audio.delta` rather than
/// `response.output_audio.delta`, and so on), and a client that listens for the wrong ones connects
/// successfully and then sits in silence, so both are handled and the GA names are what the mapping
/// is written against.
pub mod server_events {
    /// The session is open, and carries its negotiated identity.
    pub const SESSION_CREATED: &str = "session.created";
    /// The session configuration was applied.
    pub const SESSION_UPDATED: &str = "session.updated";
    /// The speaker started talking.
    pub const SPEECH_STARTED: &str = "input_audio_buffer.speech_started";
    /// The speaker stopped talking.
    pub const SPEECH_STOPPED: &str = "input_audio_buffer.speech_stopped";
    /// Part of a transcript.
    pub const TRANSCRIPT_DELTA: &str = "conversation.item.input_audio_transcription.delta";
    /// A finished transcript.
    pub const TRANSCRIPT_COMPLETED: &str = "conversation.item.input_audio_transcription.completed";
    /// A transcript that could not be produced.
    pub const TRANSCRIPT_FAILED: &str = "conversation.item.input_audio_transcription.failed";
    /// The model started a turn.
    pub const RESPONSE_CREATED: &str = "response.created";
    /// The model finished a turn, and this is where its tool calls arrive.
    pub const RESPONSE_DONE: &str = "response.done";
    /// Audio from the agent.
    pub const AUDIO_DELTA: &str = "response.output_audio.delta";
    /// The agent finished speaking.
    pub const AUDIO_DONE: &str = "response.output_audio.done";
    /// Part of what the agent is saying.
    pub const AUDIO_TRANSCRIPT_DELTA: &str = "response.output_audio_transcript.delta";
    /// The whole of what the agent said.
    pub const AUDIO_TRANSCRIPT_DONE: &str = "response.output_audio_transcript.done";
    /// Part of a text-only answer.
    pub const TEXT_DELTA: &str = "response.output_text.delta";
    /// The whole of a text-only answer.
    pub const TEXT_DONE: &str = "response.output_text.done";
    /// How much headroom is left.
    pub const RATE_LIMITS: &str = "rate_limits.updated";
    /// Something went wrong.
    pub const ERROR: &str = "error";
}

/// Client event names.
pub mod client_events {
    /// Applies a session configuration.
    pub const SESSION_UPDATE: &str = "session.update";
    /// Adds captured audio.
    pub const APPEND_AUDIO: &str = "input_audio_buffer.append";
    /// Ends the turn explicitly.
    pub const COMMIT_AUDIO: &str = "input_audio_buffer.commit";
    /// Adds a conversation item, which is how text and tool results are sent.
    pub const CREATE_ITEM: &str = "conversation.item.create";
    /// Asks the model to answer.
    pub const CREATE_RESPONSE: &str = "response.create";
    /// Stops the model mid-sentence.
    pub const CANCEL_RESPONSE: &str = "response.cancel";
    /// Drops audio already buffered for playback, which WebRTC needs on an interruption.
    pub const CLEAR_OUTPUT_AUDIO: &str = "output_audio_buffer.clear";
}

/// Beta names still emitted by older snapshots, mapped onto their GA equivalents.
fn canonical_type(raw: &str) -> &str {
    match raw {
        "response.audio.delta" => server_events::AUDIO_DELTA,
        "response.audio.done" => server_events::AUDIO_DONE,
        "response.audio_transcript.delta" => server_events::AUDIO_TRANSCRIPT_DELTA,
        "response.audio_transcript.done" => server_events::AUDIO_TRANSCRIPT_DONE,
        "response.text.delta" => server_events::TEXT_DELTA,
        "response.text.done" => server_events::TEXT_DONE,
        other => other,
    }
}

/// What one server event became.
#[derive(Debug, Clone, Default)]
pub struct MappedEvents {
    /// Zero or more provider events.
    pub events: Vec<ProviderEvent>,
    /// Session id from `session.created`, which the connection reports as its own identity.
    pub session_id: Option<String>,
    /// The model the server negotiated.
    pub model: Option<String>,
}

/// Turns one OpenAI server event into zero or more provider events.
///
/// Function calls are read from `response.done` rather than from the streaming argument deltas,
/// because that is the only event guaranteed to carry every call of a turn at once. The session
/// relies on that to know when it has dispatched them all and may ask the model to continue.
pub fn map_server_event(raw: &Json) -> MappedEvents {
    let mut mapped = MappedEvents::default();
    let kind = canonical_type(raw.get_str("type").unwrap_or_default());

    match kind {
        server_events::SESSION_CREATED => {
            let session = raw.get("session");
            let session_id = session
                .and_then(|session| session.get_str("id"))
                .or_else(|| raw.get_str("session_id"))
                .unwrap_or("unknown")
                .to_owned();
            let model = session
                .and_then(|session| session.get_str("model"))
                .unwrap_or("unknown")
                .to_owned();
            mapped.events.push(ProviderEvent::Connected {
                session_id: session_id.clone(),
                model: model.clone(),
            });
            mapped.session_id = Some(session_id);
            mapped.model = Some(model);
        }

        server_events::SPEECH_STARTED => mapped.events.push(ProviderEvent::SpeechStarted),
        server_events::SPEECH_STOPPED => mapped.events.push(ProviderEvent::SpeechStopped),

        server_events::TRANSCRIPT_DELTA => mapped.events.push(ProviderEvent::TranscriptDelta {
            item_id: raw.get_str("item_id").unwrap_or_default().to_owned(),
            delta: raw.get_str("delta").unwrap_or_default().to_owned(),
        }),

        server_events::TRANSCRIPT_COMPLETED => {
            mapped.events.push(ProviderEvent::TranscriptCompleted {
                item_id: raw.get_str("item_id").unwrap_or_default().to_owned(),
                text: raw.get_str("transcript").unwrap_or_default().to_owned(),
                confidence: average_confidence(raw.get("logprobs")),
            });
        }

        server_events::TRANSCRIPT_FAILED => mapped.events.push(ProviderEvent::TranscriptFailed {
            item_id: raw.get_str("item_id").unwrap_or_default().to_owned(),
            reason: raw
                .get("error")
                .and_then(|error| error.get_str("message"))
                .unwrap_or("transcription failed")
                .to_owned(),
        }),

        server_events::RESPONSE_CREATED => mapped.events.push(ProviderEvent::ResponseStarted {
            response_id: response_id(raw),
        }),

        server_events::AUDIO_DELTA => {
            if let Some(delta) = raw.get_str("delta").filter(|delta| !delta.is_empty()) {
                mapped.events.push(ProviderEvent::ResponseAudio {
                    response_id: raw.get_str("response_id").unwrap_or_default().to_owned(),
                    audio: decode_base64(delta),
                });
            }
        }

        server_events::AUDIO_DONE => mapped.events.push(ProviderEvent::ResponseAudioDone {
            response_id: raw.get_str("response_id").unwrap_or_default().to_owned(),
        }),

        server_events::AUDIO_TRANSCRIPT_DELTA | server_events::TEXT_DELTA => {
            mapped.events.push(ProviderEvent::ResponseTextDelta {
                response_id: raw.get_str("response_id").unwrap_or_default().to_owned(),
                delta: raw.get_str("delta").unwrap_or_default().to_owned(),
            });
        }

        server_events::AUDIO_TRANSCRIPT_DONE | server_events::TEXT_DONE => {
            mapped.events.push(ProviderEvent::ResponseText {
                response_id: raw.get_str("response_id").unwrap_or_default().to_owned(),
                text: raw
                    .get_str("transcript")
                    .or_else(|| raw.get_str("text"))
                    .unwrap_or_default()
                    .to_owned(),
            });
        }

        server_events::RESPONSE_DONE => map_response_done(raw, &mut mapped),

        server_events::RATE_LIMITS => {
            let tightest = raw
                .get("rate_limits")
                .map(Json::array_or_empty)
                .unwrap_or(&[])
                .iter()
                .min_by(|a, b| {
                    number(a, "remaining")
                        .unwrap_or(0.0)
                        .total_cmp(&number(b, "remaining").unwrap_or(0.0))
                });
            if let Some(limit) = tightest {
                mapped.events.push(ProviderEvent::RateLimit {
                    remaining: number(limit, "remaining").unwrap_or(0.0) as u64,
                    reset_seconds: number(limit, "reset_seconds").unwrap_or(0.0) as u64,
                });
            }
        }

        server_events::ERROR => {
            let error = raw.get("error").unwrap_or(raw);
            let code = error
                .get_str("code")
                .or_else(|| error.get_str("type"))
                .unwrap_or("provider_error")
                .to_owned();
            // A rejected session config will be rejected again; a transport hiccup will not.
            let retryable = !code.contains("invalid_request");
            mapped.events.push(ProviderEvent::Failed {
                fault: ProviderFault {
                    code,
                    message: error
                        .get_str("message")
                        .unwrap_or("the provider reported an error")
                        .to_owned(),
                    retryable,
                },
            });
        }

        _ => {}
    }

    mapped
}

fn map_response_done(raw: &Json, mapped: &mut MappedEvents) {
    let response = raw.get("response");
    let id = response
        .and_then(|response| response.get_str("id"))
        .unwrap_or_default()
        .to_owned();
    let status = response.and_then(|response| response.get_str("status"));

    let calls: Vec<ToolCallRequest> = response
        .and_then(|response| response.get("output"))
        .map(Json::array_or_empty)
        .unwrap_or(&[])
        .iter()
        .filter(|item| item.get_str("type") == Some("function_call"))
        .map(|item| ToolCallRequest {
            call_id: item.get_str("call_id").unwrap_or_default().to_owned(),
            name: item.get_str("name").unwrap_or_default().to_owned(),
            arguments_json: item.get_str("arguments").unwrap_or("{}").to_owned(),
        })
        .collect();
    if !calls.is_empty() {
        mapped.events.push(ProviderEvent::ToolCalls { calls });
    }

    match status {
        Some("cancelled") => mapped
            .events
            .push(ProviderEvent::ResponseCancelled { response_id: id }),
        Some(status @ ("failed" | "incomplete")) => {
            let details = response.and_then(|response| response.get("status_details"));
            let error = details.and_then(|details| details.get("error"));
            let reason = details.and_then(|details| details.get_str("reason"));
            mapped.events.push(ProviderEvent::Failed {
                fault: ProviderFault {
                    code: error
                        .and_then(|error| error.get_str("code"))
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("response_{status}")),
                    message: error
                        .and_then(|error| error.get_str("message"))
                        .map(str::to_owned)
                        .unwrap_or_else(|| match reason {
                            Some(reason) => format!("response {status}: {reason}"),
                            None => format!("response {status}"),
                        }),
                    retryable: true,
                },
            });
            mapped.events.push(ProviderEvent::ResponseDone {
                response_id: id,
                usage: None,
            });
        }
        _ => {
            let usage = response
                .and_then(|response| response.get("usage"))
                .map(map_usage);
            mapped.events.push(ProviderEvent::ResponseDone {
                response_id: id,
                usage,
            });
        }
    }
}

fn response_id(raw: &Json) -> String {
    raw.get("response")
        .and_then(|response| response.get_str("id"))
        .or_else(|| raw.get_str("response_id"))
        .unwrap_or_default()
        .to_owned()
}

fn map_usage(usage: &Json) -> TokenUsage {
    let details = usage.get("input_token_details");
    TokenUsage {
        input_tokens: number(usage, "input_tokens").map(|value| value as u64),
        output_tokens: number(usage, "output_tokens").map(|value| value as u64),
        input_audio_tokens: details
            .and_then(|details| number(details, "audio_tokens"))
            .map(|value| value as u64),
        cached_tokens: details
            .and_then(|details| number(details, "cached_tokens"))
            .map(|value| value as u64),
    }
}

/// Turns transcription logprobs into a rough confidence, used to decide when to double check a word.
fn average_confidence(value: Option<&Json>) -> Option<f64> {
    let entries: Vec<f64> = value
        .map(Json::array_or_empty)
        .unwrap_or(&[])
        .iter()
        .filter_map(|entry| number(entry, "logprob"))
        .collect();
    if entries.is_empty() {
        return None;
    }
    let mean = entries.iter().sum::<f64>() / entries.len() as f64;
    Some((mean.exp() * 1000.0).round() / 1000.0)
}

fn number(value: &Json, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(Json::as_f64)
        .filter(|number| number.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use riff_core::json_object;

    #[test]
    fn understands_the_ga_and_the_beta_audio_event_names() {
        for name in ["response.output_audio.delta", "response.audio.delta"] {
            let raw = Json::Object(json_object! {
                "type" => name,
                "response_id" => "resp_1",
                "delta" => "TWFu",
            });
            let mapped = map_server_event(&raw);
            assert!(matches!(
                mapped.events.as_slice(),
                [ProviderEvent::ResponseAudio { audio, .. }] if audio == b"Man"
            ));
        }
    }

    #[test]
    fn reads_every_function_call_of_a_turn_out_of_response_done() {
        let raw = Json::Object(json_object! {
            "type" => "response.done",
            "response" => Json::Object(json_object! {
                "id" => "resp_1",
                "status" => "completed",
                "output" => vec![
                    Json::Object(json_object! {
                        "type" => "function_call",
                        "call_id" => "call_1",
                        "name" => "read_draft",
                        "arguments" => "{}",
                    }),
                    Json::Object(json_object! { "type" => "message" }),
                    Json::Object(json_object! {
                        "type" => "function_call",
                        "call_id" => "call_2",
                        "name" => "draft_update",
                        "arguments" => "{\"operations\":[]}",
                    }),
                ],
            }),
        });

        let mapped = map_server_event(&raw);
        let ProviderEvent::ToolCalls { calls } = &mapped.events[0] else {
            panic!(
                "expected the whole batch in one event, got {:?}",
                mapped.events
            );
        };
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].name, "draft_update");
        assert!(matches!(
            mapped.events[1],
            ProviderEvent::ResponseDone { .. }
        ));
    }

    #[test]
    fn reports_a_rejected_session_as_something_not_worth_retrying() {
        let raw = Json::Object(json_object! {
            "type" => "error",
            "error" => Json::Object(json_object! {
                "code" => "invalid_request_error",
                "message" => "unknown parameter",
            }),
        });
        let mapped = map_server_event(&raw);
        assert!(matches!(
            mapped.events.as_slice(),
            [ProviderEvent::Failed { fault }] if !fault.retryable
        ));
    }

    #[test]
    fn takes_its_identity_from_session_created() {
        let raw = Json::Object(json_object! {
            "type" => "session.created",
            "session" => Json::Object(json_object! { "id" => "sess_1", "model" => "gpt-realtime-2.1" }),
        });
        let mapped = map_server_event(&raw);
        assert_eq!(mapped.session_id.as_deref(), Some("sess_1"));
        assert_eq!(mapped.model.as_deref(), Some("gpt-realtime-2.1"));
    }
}
