//! Translates Riff's provider-neutral session defaults into the OpenAI Realtime GA session object.
//!
//! The GA interface differs from the beta one in ways that fail loudly and confusingly if missed:
//! `type: "realtime"` is required, audio formats are objects rather than strings, and the field is
//! `output_modalities` rather than `modalities`. Keeping the translation in one place means the rest
//! of Riff never encodes an assumption about any provider's wire format.

use riff_core::{Json, SessionDefaults, ToolDefinition, json_object};

/// Reasoning models, the only family that accepts `reasoning` and `parallel_tool_calls`.
pub const REASONING_MODELS: &[&str] = &[
    "gpt-realtime-2",
    "gpt-realtime-2.1",
    "gpt-realtime-2.1-mini",
];

/// How a transcription model accepts vocabulary hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiasingStyle {
    /// A list of words.
    Keywords,
    /// A sentence of free text.
    Prompt,
    /// Not at all.
    None,
}

/// How each transcription model accepts vocabulary hints.
const TRANSCRIPTION_BIASING: &[(&str, BiasingStyle)] = &[
    ("whisper-1", BiasingStyle::Prompt),
    ("gpt-4o-transcribe", BiasingStyle::Prompt),
    ("gpt-4o-mini-transcribe", BiasingStyle::Prompt),
    ("gpt-4o-mini-transcribe-2025-12-15", BiasingStyle::Prompt),
    ("gpt-transcribe", BiasingStyle::Keywords),
    ("gpt-live-transcribe", BiasingStyle::Keywords),
    ("gpt-4o-transcribe-diarize", BiasingStyle::None),
    ("gpt-realtime-whisper", BiasingStyle::None),
];

/// How the named transcription model wants vocabulary hints.
pub fn biasing_style_for(model: &str) -> BiasingStyle {
    TRANSCRIPTION_BIASING
        .iter()
        .find(|(name, _)| *name == model)
        .map_or(BiasingStyle::Prompt, |(_, style)| *style)
}

/// Whether the model accepts reasoning settings.
pub fn is_reasoning_model(model: &str) -> bool {
    REASONING_MODELS
        .iter()
        .any(|known| model == *known || model.starts_with(&format!("{known}-")))
}

/// Everything the session object is built from.
pub struct BuildSessionOptions<'a> {
    /// Provider-neutral session defaults.
    pub session: &'a SessionDefaults,
    /// The composed system prompt.
    pub instructions: &'a str,
    /// The tools the model may call.
    pub tools: &'a [ToolDefinition],
    /// Canonical spellings to bias transcription toward.
    pub vocabulary: &'a [String],
    /// Overrides the model in the session defaults.
    pub model: Option<&'a str>,
}

/// Builds the GA `session` object.
pub fn build_openai_session(options: BuildSessionOptions<'_>) -> Json {
    let session = options.session;
    let model = options.model.unwrap_or(&session.model.preferred);
    let transcription_model = session.transcription.preferred.as_str();

    let mut audio_input = json_object! {
        "format" => audio_format(&session.audio.input.encoding, session.audio.input.sample_rate),
    };
    audio_input.insert(
        "noise_reduction",
        match &session.audio.input.noise_reduction {
            Some(profile) => Json::Object(json_object! { "type" => profile.clone() }),
            None => Json::Null,
        },
    );
    audio_input.insert(
        "transcription",
        transcription(&options, transcription_model),
    );
    audio_input.insert("turn_detection", turn_detection(session));

    let mut audio_output = json_object! {
        "format" => audio_format(&session.audio.output.encoding, session.audio.output.sample_rate),
        "voice" => session.voice.name.clone(),
    };
    audio_output.insert_some("speed", session.voice.speed.map(Json::from));

    let mut payload = json_object! {
        "type" => "realtime",
        "model" => model,
        "instructions" => options.instructions,
        "output_modalities" => vec![if session.modalities.output.iter().any(|kind| kind == "audio") {
            "audio"
        } else {
            "text"
        }],
        "audio" => Json::Object(json_object! {
            "input" => Json::Object(audio_input),
            "output" => Json::Object(audio_output),
        }),
        "tools" => Json::Array(options.tools.iter().map(|tool| Json::Object(json_object! {
            "type" => "function",
            "name" => tool.name.clone(),
            "description" => tool.description.clone(),
            "parameters" => tool.parameters.clone(),
        })).collect()),
        "tool_choice" => session.tool_choice.clone(),
        "max_output_tokens" => f64::from(session.limits.max_output_tokens),
    };

    if let Some(truncation) = &session.truncation {
        payload.insert(
            "truncation",
            Json::Object(json_object! {
                "type" => truncation.strategy.clone(),
                "retention_ratio" => truncation.retention_ratio,
                "token_limits" => Json::Object(json_object! {
                    "post_instructions" => f64::from(truncation.post_instruction_token_limit),
                }),
            }),
        );
    }

    // Rejected outright by non-reasoning models, so these are gated rather than always sent.
    if is_reasoning_model(model) {
        if let Some(reasoning) = &session.reasoning {
            payload.insert(
                "reasoning",
                Json::Object(json_object! { "effort" => reasoning.effort.clone() }),
            );
        }
        if let Some(parallel) = session.parallel_tool_calls {
            payload.insert("parallel_tool_calls", Json::from(parallel));
        }
    }

    Json::Object(payload)
}

fn audio_format(encoding: &str, rate: u32) -> Json {
    match encoding {
        "g711_ulaw" => Json::Object(json_object! { "type" => "audio/pcmu" }),
        "g711_alaw" => Json::Object(json_object! { "type" => "audio/pcma" }),
        _ => Json::Object(json_object! { "type" => "audio/pcm", "rate" => f64::from(rate) }),
    }
}

fn transcription(options: &BuildSessionOptions<'_>, model: &str) -> Json {
    let session = options.session;
    let mut config = json_object! { "model" => model };
    if let Some(language) = &session.transcription.language {
        config.insert("language", Json::from(language.clone()));
    }

    let Some(biasing) = &session.transcription.biasing else {
        return Json::Object(config);
    };
    if !biasing.enabled || options.vocabulary.is_empty() {
        return Json::Object(config);
    }

    let words: Vec<String> = options
        .vocabulary
        .iter()
        .take(biasing.max_keywords)
        .cloned()
        .collect();
    match biasing_style_for(model) {
        BiasingStyle::Keywords => config.insert("keywords", Json::from(words)),
        BiasingStyle::Prompt => config.insert(
            "prompt",
            Json::from(format!("{} {}.", biasing.prompt_preamble, words.join(", "))),
        ),
        BiasingStyle::None => {}
    }
    Json::Object(config)
}

fn turn_detection(session: &SessionDefaults) -> Json {
    let detection = &session.turn_detection;
    if detection.mode == "manual" {
        return Json::Null;
    }

    if detection.mode == "semantic" {
        return Json::Object(json_object! {
            "type" => "semantic_vad",
            "eagerness" => detection.eagerness.clone().unwrap_or_else(|| "auto".to_owned()),
            "create_response" => detection.auto_respond,
            "interrupt_response" => detection.allow_barge_in,
        });
    }

    let fallback = detection.server_vad_fallback.as_ref();
    Json::Object(json_object! {
        "type" => "server_vad",
        "threshold" => fallback.map_or(0.5, |fallback| fallback.threshold),
        "prefix_padding_ms" => f64::from(fallback.map_or(300, |fallback| fallback.prefix_padding_ms)),
        "silence_duration_ms" => f64::from(fallback.map_or(500, |fallback| fallback.silence_duration_ms)),
        "create_response" => detection.auto_respond,
        "interrupt_response" => detection.allow_barge_in,
    })
}

/// Session patch for a mid-conversation vocabulary change, without resending the whole config.
pub fn build_vocabulary_patch(session: &SessionDefaults, vocabulary: &[String]) -> Option<Json> {
    let biasing = session.transcription.biasing.as_ref()?;
    if !biasing.enabled || vocabulary.is_empty() {
        return None;
    }

    let model = session.transcription.preferred.as_str();
    let style = biasing_style_for(model);
    if style == BiasingStyle::None {
        return None;
    }

    let words: Vec<String> = vocabulary
        .iter()
        .take(biasing.max_keywords)
        .cloned()
        .collect();
    let mut transcription = json_object! { "model" => model };
    if let Some(language) = &session.transcription.language {
        transcription.insert("language", Json::from(language.clone()));
    }
    match style {
        BiasingStyle::Keywords => transcription.insert("keywords", Json::from(words)),
        _ => transcription.insert(
            "prompt",
            Json::from(format!("{} {}.", biasing.prompt_preamble, words.join(", "))),
        ),
    }

    Some(Json::Object(json_object! {
        "type" => "realtime",
        "audio" => Json::Object(json_object! {
            "input" => Json::Object(json_object! {
                "transcription" => Json::Object(transcription),
            }),
        }),
    }))
}
