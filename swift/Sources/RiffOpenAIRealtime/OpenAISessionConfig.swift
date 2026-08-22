import Foundation
import RiffCore

/// Translates Riff's provider-neutral session defaults into the OpenAI Realtime GA session object.
///
/// The GA interface differs from the beta one in ways that fail loudly and confusingly if missed:
/// `type: "realtime"` is required, audio formats are objects rather than strings, and the field is
/// `output_modalities` rather than `modalities`.
public enum OpenAISessionConfig {
    /// Reasoning models, the only family that accepts `reasoning` and `parallel_tool_calls`.
    public static let reasoningModels = ["gpt-realtime-2", "gpt-realtime-2.1", "gpt-realtime-2.1-mini"]

    public enum BiasingStyle: String, Sendable { case prompt, keywords, none }

    /// How each transcription model accepts vocabulary hints.
    private static let biasingStyles: [String: BiasingStyle] = [
        "whisper-1": .prompt,
        "gpt-4o-transcribe": .prompt,
        "gpt-4o-mini-transcribe": .prompt,
        "gpt-4o-mini-transcribe-2025-12-15": .prompt,
        "gpt-transcribe": .keywords,
        "gpt-live-transcribe": .keywords,
        "gpt-4o-transcribe-diarize": .none,
        "gpt-realtime-whisper": .none,
    ]

    public static func biasingStyle(for model: String) -> BiasingStyle {
        biasingStyles[model] ?? .prompt
    }

    public static func isReasoningModel(_ model: String) -> Bool {
        reasoningModels.contains { model == $0 || model.hasPrefix("\($0)-") }
    }

    public static func build(
        session: SessionDefaults,
        instructions: String,
        tools: [ToolDefinition],
        vocabulary: [String] = [],
        model: String? = nil
    ) -> JSONValue {
        let model = model ?? session.model.preferred

        var payload: [String: JSONValue] = [
            "type": .string("realtime"),
            "model": .string(model),
            "instructions": .string(instructions),
            "output_modalities": .array([.string(session.modalities.output.contains("audio") ? "audio" : "text")]),
            "audio": .object([
                "input": .object([
                    "format": audioFormat(session.audio.input.encoding, session.audio.input.sampleRate),
                    "noise_reduction": session.audio.input.noiseReduction.map { .object(["type": .string($0)]) } ?? .null,
                    "transcription": transcription(session, vocabulary),
                    "turn_detection": turnDetection(session),
                ]),
                "output": {
                    var output: [String: JSONValue] = [
                        "format": audioFormat(session.audio.output.encoding, session.audio.output.sampleRate),
                        "voice": .string(session.voice.name),
                    ]
                    if let speed = session.voice.speed { output["speed"] = .number(speed) }
                    return .object(output)
                }(),
            ]),
            "tools": .array(tools.map { tool in
                .object([
                    "type": .string("function"),
                    "name": .string(tool.name),
                    "description": .string(tool.description),
                    "parameters": tool.parameters,
                ])
            }),
            "tool_choice": .string(session.toolChoice),
            "max_output_tokens": .number(Double(session.limits.maxOutputTokens)),
        ]

        if let truncation = session.truncation {
            payload["truncation"] = .object([
                "type": .string(truncation.strategy),
                "retention_ratio": .number(truncation.retentionRatio),
                "token_limits": .object(["post_instructions": .number(Double(truncation.postInstructionTokenLimit))]),
            ])
        }

        // Rejected outright by non-reasoning models, so these are gated rather than always sent.
        if isReasoningModel(model) {
            if let reasoning = session.reasoning {
                payload["reasoning"] = .object(["effort": .string(reasoning.effort)])
            }
            if let parallel = session.parallelToolCalls {
                payload["parallel_tool_calls"] = .bool(parallel)
            }
        }

        return .object(payload)
    }

    /// Session patch for a mid-conversation vocabulary change, without resending the whole config.
    public static func vocabularyPatch(session: SessionDefaults, vocabulary: [String]) -> JSONValue? {
        guard let biasing = session.transcription.biasing, biasing.enabled, !vocabulary.isEmpty else { return nil }
        let model = session.transcription.preferred
        let style = biasingStyle(for: model)
        guard style != .none else { return nil }

        let words = Array(vocabulary.prefix(biasing.maxKeywords))
        var config: [String: JSONValue] = ["model": .string(model)]
        if let language = session.transcription.language { config["language"] = .string(language) }
        if style == .keywords {
            config["keywords"] = .array(words.map { .string($0) })
        } else {
            config["prompt"] = .string("\(biasing.promptPreamble) \(words.joined(separator: ", ")).")
        }

        return .object([
            "type": .string("realtime"),
            "audio": .object(["input": .object(["transcription": .object(config)])]),
        ])
    }

    private static func audioFormat(_ encoding: String, _ rate: Int) -> JSONValue {
        switch encoding {
        case "g711_ulaw": .object(["type": .string("audio/pcmu")])
        case "g711_alaw": .object(["type": .string("audio/pcma")])
        default: .object(["type": .string("audio/pcm"), "rate": .number(Double(rate))])
        }
    }

    private static func transcription(_ session: SessionDefaults, _ vocabulary: [String]) -> JSONValue {
        let model = session.transcription.preferred
        var config: [String: JSONValue] = ["model": .string(model)]
        if let language = session.transcription.language { config["language"] = .string(language) }

        guard let biasing = session.transcription.biasing, biasing.enabled, !vocabulary.isEmpty else {
            return .object(config)
        }

        let words = Array(vocabulary.prefix(biasing.maxKeywords))
        switch biasingStyle(for: model) {
        case .keywords: config["keywords"] = .array(words.map { .string($0) })
        case .prompt: config["prompt"] = .string("\(biasing.promptPreamble) \(words.joined(separator: ", ")).")
        case .none: break
        }
        return .object(config)
    }

    private static func turnDetection(_ session: SessionDefaults) -> JSONValue {
        let detection = session.turnDetection
        switch detection.mode {
        case .manual:
            return .null
        case .semantic:
            return .object([
                "type": .string("semantic_vad"),
                "eagerness": .string(detection.eagerness ?? "auto"),
                "create_response": .bool(detection.autoRespond),
                "interrupt_response": .bool(detection.allowBargeIn),
            ])
        case .vad:
            let fallback = detection.serverVadFallback
            return .object([
                "type": .string("server_vad"),
                "threshold": .number(fallback?.threshold ?? 0.5),
                "prefix_padding_ms": .number(Double(fallback?.prefixPaddingMs ?? 300)),
                "silence_duration_ms": .number(Double(fallback?.silenceDurationMs ?? 500)),
                "create_response": .bool(detection.autoRespond),
                "interrupt_response": .bool(detection.allowBargeIn),
            ])
        }
    }
}
