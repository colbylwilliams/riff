//! Loads the compiled agent bundle and refuses anything that would let the agent drift from its
//! contract.
//!
//! The bundle is produced by `tools/build-bundle.mjs` and shipped to every platform binding, so
//! these checks are the last line of defense against a binding running a modified definition.

use crate::error::{Result, RiffError};
use crate::json::Json;
use crate::schema::SUPPORTED_KEYWORDS;
use crate::types::{
    AgentBundle, AudioDefaults, AudioStream, Biasing, GroundingConfig, GroundingMode,
    InstructionSection, LexiconTerm, Limits, Modalities, ModelDefaults, PolicyConfig,
    ReasoningDefaults, RenderConfig, RenderDisposition, Section, SeedLexicon, ServerVadFallback,
    SessionDefaults, ToolDefinition, TranscriptionDefaults, Truncation, TurnDetection,
    VoiceDefaults,
};

/// The compiled bundle, vendored from `core/dist` by `npm run bundle`.
///
/// It is compiled in rather than read from disk so the crate works the same in a binary, a test, and
/// a cross-compiled build, and so nothing at runtime can substitute a different agent.
pub const BUNDLED_AGENT: &str = include_str!("../resources/riff-agent.bundle.json");

impl AgentBundle {
    /// The agent this build of the crate ships with.
    pub fn bundled() -> Result<AgentBundle> {
        AgentBundle::parse(BUNDLED_AGENT)
    }

    /// Reads and validates a bundle from JSON text.
    pub fn parse(text: &str) -> Result<AgentBundle> {
        let value =
            Json::parse(text).map_err(|error| RiffError::bundle(format!("not JSON: {error}")))?;
        AgentBundle::load(&value)
    }

    /// Validates an already-parsed bundle.
    pub fn load(value: &Json) -> Result<AgentBundle> {
        if value.as_object().is_none() {
            return Err(RiffError::bundle("agent bundle must be an object"));
        }

        let required = |key: &str| -> Result<String> {
            value
                .get_str(key)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| RiffError::bundle(format!("agent bundle is missing \"{key}\"")))
        };

        let id = required("id")?;
        let version = required("version")?;
        let revision = required("revision")?;
        let instructions = required("instructions")?;

        let tools = load_tools(value)?;
        let grounding = load_grounding(value)?;
        let render = load_render(value)?;
        let policy = load_policy(value)?;
        let session = load_session(
            value
                .get("session")
                .ok_or_else(|| RiffError::bundle("agent bundle is missing session defaults"))?,
        )?;

        Ok(AgentBundle {
            id,
            name: value.get_str("name").unwrap_or_default().to_owned(),
            version,
            revision,
            description: value.get_str("description").map(str::to_owned),
            instructions,
            instruction_sections: value
                .get("instructionSections")
                .map(Json::array_or_empty)
                .unwrap_or(&[])
                .iter()
                .map(|section| InstructionSection {
                    id: section.get_str("id").unwrap_or_default().to_owned(),
                    title: section.get_str("title").unwrap_or_default().to_owned(),
                    order: section
                        .get("order")
                        .and_then(Json::as_i64)
                        .unwrap_or_default(),
                    source: section.get_str("source").unwrap_or_default().to_owned(),
                    text: section.get_str("text").unwrap_or_default().to_owned(),
                })
                .collect(),
            tools,
            session,
            lexicon: value
                .get("lexicon")
                .map(|lexicon| SeedLexicon {
                    version: lexicon.get("version").and_then(Json::as_i64).unwrap_or(1),
                    terms: lexicon
                        .get("terms")
                        .map(Json::array_or_empty)
                        .unwrap_or(&[])
                        .iter()
                        .map(LexiconTerm::from_json)
                        .collect(),
                })
                .unwrap_or(SeedLexicon {
                    version: 1,
                    terms: Vec::new(),
                }),
            grounding,
            render,
            policy,
        })
    }
}

fn load_tools(value: &Json) -> Result<Vec<ToolDefinition>> {
    let tools = value.get("tools").map(Json::array_or_empty).unwrap_or(&[]);
    if tools.is_empty() {
        return Err(RiffError::bundle("agent bundle has no tools"));
    }

    tools
        .iter()
        .map(|tool| {
            let name = tool.get_str("name").unwrap_or_default().to_owned();
            let description = tool.get_str("description").unwrap_or_default().to_owned();
            let parameters = tool.get("parameters").cloned();
            if name.is_empty() || description.is_empty() || parameters.is_none() {
                return Err(RiffError::bundle(format!(
                    "tool \"{}\" is incomplete",
                    if name.is_empty() { "(unnamed)" } else { &name }
                )));
            }
            let kind = tool.get_str("kind").unwrap_or_default();
            if kind != "local" && kind != "host" {
                return Err(RiffError::bundle(format!("tool \"{name}\" has unknown kind \"{kind}\"")));
            }
            let parameters = parameters.expect("checked above");
            // A schema keyword the validator does not implement would be declared in `core/agent`
            // and silently unenforced here, which is a worse failure than refusing to load: every
            // other binding would apply it and this one would not.
            if let Some(keyword) = unsupported_keyword(&parameters) {
                return Err(RiffError::bundle(format!(
                    "tool \"{name}\" uses JSON Schema \"{keyword}\", which this binding does not implement",
                )));
            }

            Ok(ToolDefinition {
                name,
                kind: kind.to_owned(),
                description,
                parameters,
                returns: tool.get("returns").cloned(),
                source: tool.get_str("source").map(str::to_owned),
            })
        })
        .collect()
}

/// Walks a parameter schema for a keyword [`crate::schema`] does not enforce.
fn unsupported_keyword(schema: &Json) -> Option<&str> {
    let object = schema.as_object()?;
    for (key, value) in object.iter() {
        if !SUPPORTED_KEYWORDS.contains(&key) {
            return Some(key);
        }
        match key {
            // `additionalProperties: false` is a rule the validator enforces; a *schema* there is a
            // rule it would drop on the floor, so it is refused for the same reason a keyword it
            // has never heard of is.
            "additionalProperties" if value.as_object().is_some() => {
                return Some("additionalProperties as a schema");
            }
            "properties" => {
                if let Some(properties) = value.as_object() {
                    for (_, child) in properties.iter() {
                        if let Some(found) = unsupported_keyword(child) {
                            return Some(found);
                        }
                    }
                }
            }
            "items" => {
                if let Some(found) = unsupported_keyword(value) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

fn load_grounding(value: &Json) -> Result<GroundingConfig> {
    let grounding = value
        .get("grounding")
        .ok_or_else(|| RiffError::bundle("agent bundle is missing grounding configuration"))?;

    let threshold = grounding
        .get("threshold")
        .and_then(Json::as_f64)
        .unwrap_or_default();
    if !(threshold > 0.0 && threshold <= 1.0) {
        return Err(RiffError::bundle(
            "grounding.threshold must be greater than 0 and at most 1",
        ));
    }

    let window_size = grounding
        .get("windowSize")
        .and_then(Json::as_i64)
        .unwrap_or_default();
    if window_size < 1 {
        return Err(RiffError::bundle("grounding.windowSize must be at least 1"));
    }

    let mut sections = Vec::new();
    if let Some(configured) = grounding.get("sections").and_then(Json::as_object) {
        for (name, mode) in configured.iter() {
            let section = Section::parse(name).ok_or_else(|| {
                RiffError::bundle(format!("grounding.sections has unknown section \"{name}\""))
            })?;
            sections.push((
                section,
                GroundingMode::parse(mode.as_str().unwrap_or_default()),
            ));
        }
    }

    let title_threshold = grounding.get("titleThreshold").and_then(Json::as_f64);
    if let Some(title_threshold) = title_threshold
        && !(title_threshold > 0.0 && title_threshold <= 1.0)
    {
        return Err(RiffError::bundle(
            "grounding.titleThreshold must be greater than 0 and at most 1",
        ));
    }

    Ok(GroundingConfig {
        threshold,
        title_threshold,
        window_size: window_size as usize,
        sections,
        free_tokens: string_list(grounding.get("freeTokens")),
        filler: string_list(grounding.get("filler")),
    })
}

fn load_render(value: &Json) -> Result<RenderConfig> {
    let render = value
        .get("render")
        .ok_or_else(|| RiffError::bundle("agent bundle is missing render configuration"))?;
    let profile = render.get_str("profile").unwrap_or_default().to_owned();

    let profiles: Vec<(String, Vec<(String, RenderDisposition)>)> = render
        .get("profiles")
        .and_then(Json::as_object)
        .map(|profiles| {
            profiles
                .iter()
                .map(|(name, dispositions)| {
                    let dispositions = dispositions
                        .as_object()
                        .map(|object| {
                            object
                                .iter()
                                .map(|(section, disposition)| {
                                    (
                                        section.to_owned(),
                                        RenderDisposition::parse(
                                            disposition.as_str().unwrap_or_default(),
                                        ),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (name.to_owned(), dispositions)
                })
                .collect()
        })
        .unwrap_or_default();

    if !profiles.iter().any(|(name, _)| *name == profile) {
        return Err(RiffError::bundle(format!(
            "render profile \"{profile}\" is not defined"
        )));
    }

    Ok(RenderConfig {
        profile,
        profiles,
        labels: render
            .get("labels")
            .and_then(Json::as_object)
            .map(|labels| {
                labels
                    .iter()
                    .map(|(key, label)| {
                        (
                            key.to_owned(),
                            label.as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        include_provenance_footer: render
            .get("includeProvenanceFooter")
            .and_then(Json::as_bool)
            .unwrap_or(false),
    })
}

fn load_policy(value: &Json) -> Result<PolicyConfig> {
    let policy = value
        .get("policy")
        .ok_or_else(|| RiffError::bundle("agent bundle is missing policy configuration"))?;

    if policy.get("autoSubmit").and_then(Json::as_bool) != Some(false) {
        return Err(RiffError::bundle(
            "policy.autoSubmit must be false: the speaker decides when a prompt is sent",
        ));
    }

    let Some(required) = policy.get("readinessRequires").and_then(Json::as_array) else {
        return Err(RiffError::bundle(
            "policy.readinessRequires must be an array",
        ));
    };
    let readiness_requires = required
        .iter()
        .map(|section| {
            let name = section.as_str().unwrap_or_default();
            Section::parse(name).ok_or_else(|| {
                RiffError::bundle(format!(
                    "policy.readinessRequires has unknown section \"{name}\""
                ))
            })
        })
        .collect::<Result<Vec<Section>>>()?;

    Ok(PolicyConfig {
        max_takes: policy
            .get("maxTakes")
            .and_then(Json::as_usize)
            .ok_or_else(|| RiffError::bundle("policy.maxTakes must be a number"))?,
        readiness_requires,
        auto_submit: false,
        readback_default: policy
            .get_str("readbackDefault")
            .unwrap_or("gist")
            .to_owned(),
        persist_audio: policy
            .get("persistAudio")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        redact_secrets_from_transcript: policy
            .get("redactSecretsFromTranscript")
            .and_then(Json::as_bool)
            .unwrap_or(true),
    })
}

fn load_session(value: &Json) -> Result<SessionDefaults> {
    let required = |key: &'static str| -> Result<&Json> {
        value
            .get(key)
            .ok_or_else(|| RiffError::bundle(format!("session defaults are missing \"{key}\"")))
    };

    let model = required("model")?;
    let audio = required("audio")?;
    let limits = required("limits")?;
    let turn_detection = required("turnDetection")?;
    let transcription = required("transcription")?;

    Ok(SessionDefaults {
        model: ModelDefaults {
            preferred: model.get_str("preferred").unwrap_or_default().to_owned(),
            fallbacks: string_list(model.get("fallbacks")),
        },
        modalities: Modalities {
            input: string_list(value.get("modalities").and_then(|m| m.get("input"))),
            output: string_list(value.get("modalities").and_then(|m| m.get("output"))),
        },
        voice: VoiceDefaults {
            name: value
                .get("voice")
                .and_then(|voice| voice.get_str("name"))
                .unwrap_or_default()
                .to_owned(),
            speed: value
                .get("voice")
                .and_then(|voice| voice.get("speed"))
                .and_then(Json::as_f64),
        },
        audio: AudioDefaults {
            input: audio_stream(audio.get("input")),
            output: audio_stream(audio.get("output")),
        },
        turn_detection: TurnDetection {
            mode: turn_detection
                .get_str("mode")
                .unwrap_or("semantic")
                .to_owned(),
            eagerness: turn_detection.get_str("eagerness").map(str::to_owned),
            auto_respond: turn_detection
                .get("autoRespond")
                .and_then(Json::as_bool)
                .unwrap_or(true),
            allow_barge_in: turn_detection
                .get("allowBargeIn")
                .and_then(Json::as_bool)
                .unwrap_or(true),
            server_vad_fallback: turn_detection.get("serverVadFallback").map(|fallback| {
                ServerVadFallback {
                    threshold: fallback
                        .get("threshold")
                        .and_then(Json::as_f64)
                        .unwrap_or(0.5),
                    prefix_padding_ms: fallback
                        .get("prefixPaddingMs")
                        .and_then(Json::as_i64)
                        .unwrap_or(300) as u32,
                    silence_duration_ms: fallback
                        .get("silenceDurationMs")
                        .and_then(Json::as_i64)
                        .unwrap_or(500) as u32,
                }
            }),
        },
        transcription: TranscriptionDefaults {
            preferred: transcription
                .get_str("preferred")
                .unwrap_or_default()
                .to_owned(),
            fallbacks: string_list(transcription.get("fallbacks")),
            language: transcription.get_str("language").map(str::to_owned),
            biasing: transcription.get("biasing").map(|biasing| Biasing {
                enabled: biasing
                    .get("enabled")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
                max_keywords: biasing
                    .get("maxKeywords")
                    .and_then(Json::as_usize)
                    .unwrap_or(100),
                prompt_preamble: biasing
                    .get_str("promptPreamble")
                    .unwrap_or_default()
                    .to_owned(),
            }),
        },
        reasoning: value.get("reasoning").map(|reasoning| ReasoningDefaults {
            effort: reasoning.get_str("effort").unwrap_or("low").to_owned(),
        }),
        limits: Limits {
            max_output_tokens: limits
                .get("maxOutputTokens")
                .and_then(Json::as_i64)
                .unwrap_or(4096) as u32,
            max_session_seconds: limits
                .get("maxSessionSeconds")
                .and_then(Json::as_i64)
                .unwrap_or(3600) as u64,
            tool_timeout_ms: limits
                .get("toolTimeoutMs")
                .and_then(Json::as_i64)
                .unwrap_or(6000)
                .max(0) as u64,
        },
        truncation: value.get("truncation").map(|truncation| Truncation {
            strategy: truncation
                .get_str("strategy")
                .unwrap_or_default()
                .to_owned(),
            retention_ratio: truncation
                .get("retentionRatio")
                .and_then(Json::as_f64)
                .unwrap_or(1.0),
            post_instruction_token_limit: truncation
                .get("postInstructionTokenLimit")
                .and_then(Json::as_i64)
                .unwrap_or(0) as u32,
        }),
        tool_choice: value.get_str("toolChoice").unwrap_or("auto").to_owned(),
        parallel_tool_calls: value.get("parallelToolCalls").and_then(Json::as_bool),
    })
}

fn audio_stream(value: Option<&Json>) -> AudioStream {
    let value = value.cloned().unwrap_or_else(Json::object);
    AudioStream {
        encoding: value.get_str("encoding").unwrap_or("pcm_s16le").to_owned(),
        sample_rate: value
            .get("sampleRate")
            .and_then(Json::as_i64)
            .unwrap_or(24_000) as u32,
        channels: value.get("channels").and_then(Json::as_i64).unwrap_or(1) as u32,
        noise_reduction: value.get_str("noiseReduction").map(str::to_owned),
    }
}

fn string_list(value: Option<&Json>) -> Vec<String> {
    value
        .map(Json::array_or_empty)
        .unwrap_or(&[])
        .iter()
        .filter_map(|entry| entry.as_str().map(str::to_owned))
        .collect()
}

/// Host overrides applied to the bundle's session defaults.
#[derive(Debug, Clone, Default)]
pub struct SessionOverrides {
    /// Which speech model to ask for instead.
    pub model: Option<String>,
    /// Which voice.
    pub voice: Option<String>,
    /// How fast it talks.
    pub speed: Option<f64>,
    /// Which language to expect.
    pub language: Option<String>,
    /// Which transcription model.
    pub transcription_model: Option<String>,
    /// A tighter ceiling on one answer.
    pub max_output_tokens: Option<u32>,
    /// How hard the model should think.
    pub reasoning_effort: Option<String>,
}

impl SessionOverrides {
    /// Applies these overrides without mutating the bundle.
    pub fn apply(&self, defaults: &SessionDefaults) -> SessionDefaults {
        let mut session = defaults.clone();
        if let Some(model) = &self.model {
            session.model.preferred = model.clone();
        }
        if let Some(voice) = &self.voice {
            session.voice.name = voice.clone();
        }
        if let Some(speed) = self.speed {
            session.voice.speed = Some(speed);
        }
        if let Some(language) = &self.language {
            session.transcription.language = Some(language.clone());
        }
        if let Some(model) = &self.transcription_model {
            session.transcription.preferred = model.clone();
        }
        if let Some(tokens) = self.max_output_tokens {
            session.limits.max_output_tokens = tokens;
        }
        if let Some(effort) = &self.reasoning_effort {
            session.reasoning = Some(ReasoningDefaults {
                effort: effort.clone(),
            });
        }
        session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_agent_this_build_ships_with() {
        let bundle = AgentBundle::bundled().expect("the vendored bundle must load");
        assert_eq!(bundle.id, "riff");
        assert!(!bundle.instructions.is_empty());
        assert!(bundle.tool("draft_update").is_some());
        assert!(!bundle.policy.auto_submit);
        assert!(!bundle.policy.persist_audio);
    }

    #[test]
    fn refuses_a_bundle_that_would_send_on_its_own() {
        let modified = BUNDLED_AGENT.replace("\"autoSubmit\": false", "\"autoSubmit\": true");
        let error = AgentBundle::parse(&modified).expect_err("autoSubmit must be refused");
        assert!(error.to_string().contains("autoSubmit"));
    }

    #[test]
    fn refuses_a_threshold_outside_its_range() {
        let modified = BUNDLED_AGENT.replace("\"threshold\": 0.82", "\"threshold\": 1.5");
        assert!(AgentBundle::parse(&modified).is_err());
    }

    #[test]
    fn refuses_an_undefined_render_profile() {
        let modified = BUNDLED_AGENT.replace("\"profile\": \"prose\"", "\"profile\": \"nope\"");
        assert!(AgentBundle::parse(&modified).is_err());
    }

    #[test]
    fn refuses_additional_properties_it_would_have_to_ignore() {
        // The boolean form is a rule the validator applies. A schema there is a rule it would drop,
        // which is the thing the whole scanner exists to prevent.
        let modified = BUNDLED_AGENT.replace(
            "\"additionalProperties\": false",
            "\"additionalProperties\": { \"type\": \"string\" }",
        );
        let error =
            AgentBundle::parse(&modified).expect_err("a schema-valued form must be refused");
        assert!(
            error.to_string().contains("additionalProperties"),
            "got {error}"
        );

        // `true` says nothing the validator has to enforce, so it loads.
        let permissive = BUNDLED_AGENT.replace(
            "\"additionalProperties\": false",
            "\"additionalProperties\": true",
        );
        assert!(AgentBundle::parse(&permissive).is_ok());
    }

    #[test]
    fn refuses_a_schema_constraint_this_binding_cannot_enforce() {
        // Declared in `core/agent` and silently unenforced here would be worse than absent: every
        // other binding would apply it and this one would quietly accept what they reject.
        let modified = BUNDLED_AGENT.replace(
            "\"maxLength\": 2000",
            "\"maxLength\": 2000,\n              \"pattern\": \"^.+$\"",
        );
        let error = AgentBundle::parse(&modified).expect_err("an unenforceable keyword is refused");
        assert!(error.to_string().contains("pattern"), "got {error}");
    }
}
