import type { SessionDefaults, ToolDefinition } from "@riff/core";

/**
 * Translates Riff's provider-neutral session defaults into the OpenAI Realtime GA session object.
 *
 * The GA interface differs from the beta one in ways that fail loudly and confusingly if missed:
 * `type: "realtime"` is required, audio formats are objects rather than strings, and the field is
 * `output_modalities` rather than `modalities`. Keeping the translation in one place means the rest
 * of Riff never encodes an assumption about any provider's wire format.
 */

export const REALTIME_MODELS = {
  /** Reasoning models, the only family that accepts `reasoning` and `parallel_tool_calls`. */
  reasoning: ["gpt-realtime-2", "gpt-realtime-2.1", "gpt-realtime-2.1-mini"],
} as const;

/** How each transcription model accepts vocabulary hints. */
const TRANSCRIPTION_BIASING: Record<string, "prompt" | "keywords" | "none"> = {
  "whisper-1": "prompt",
  "gpt-4o-transcribe": "prompt",
  "gpt-4o-mini-transcribe": "prompt",
  "gpt-4o-mini-transcribe-2025-12-15": "prompt",
  "gpt-transcribe": "keywords",
  "gpt-live-transcribe": "keywords",
  "gpt-4o-transcribe-diarize": "none",
  "gpt-realtime-whisper": "none",
};

export function biasingStyleFor(model: string): "prompt" | "keywords" | "none" {
  return TRANSCRIPTION_BIASING[model] ?? "prompt";
}

export function isReasoningModel(model: string): boolean {
  return REALTIME_MODELS.reasoning.some((known) => model === known || model.startsWith(`${known}-`));
}

export interface BuildSessionOptions {
  session: SessionDefaults;
  instructions: string;
  tools: ToolDefinition[];
  vocabulary?: string[];
  model?: string;
}

export function buildOpenAISession(options: BuildSessionOptions): Record<string, unknown> {
  const { session, instructions, tools } = options;
  const model = options.model ?? session.model.preferred;
  const transcriptionModel = session.transcription.preferred;

  const payload: Record<string, unknown> = {
    type: "realtime",
    model,
    instructions,
    output_modalities: session.modalities.output.includes("audio") ? ["audio"] : ["text"],
    audio: {
      input: {
        format: audioFormat(session.audio.input.encoding, session.audio.input.sampleRate),
        noise_reduction: session.audio.input.noiseReduction
          ? { type: session.audio.input.noiseReduction }
          : null,
        transcription: transcription(options, transcriptionModel),
        turn_detection: turnDetection(session),
      },
      output: {
        format: audioFormat(session.audio.output.encoding, session.audio.output.sampleRate),
        voice: session.voice.name,
        ...(session.voice.speed === undefined ? {} : { speed: session.voice.speed }),
      },
    },
    tools: tools.map((tool) => ({
      type: "function",
      name: tool.name,
      description: tool.description,
      parameters: tool.parameters,
    })),
    tool_choice: session.toolChoice,
    max_output_tokens: session.limits.maxOutputTokens,
  };

  if (session.truncation) {
    payload["truncation"] = {
      type: session.truncation.strategy,
      retention_ratio: session.truncation.retentionRatio,
      token_limits: { post_instructions: session.truncation.postInstructionTokenLimit },
    };
  }

  // Rejected outright by non-reasoning models, so these are gated rather than always sent.
  if (isReasoningModel(model)) {
    if (session.reasoning) payload["reasoning"] = { effort: session.reasoning.effort };
    if (session.parallelToolCalls !== undefined) payload["parallel_tool_calls"] = session.parallelToolCalls;
  }

  return payload;
}

function audioFormat(encoding: string, rate: number): Record<string, unknown> {
  switch (encoding) {
    case "g711_ulaw":
      return { type: "audio/pcmu" };
    case "g711_alaw":
      return { type: "audio/pcma" };
    case "pcm_s16le":
    default:
      return { type: "audio/pcm", rate };
  }
}

function transcription(options: BuildSessionOptions, model: string): Record<string, unknown> {
  const { session, vocabulary = [] } = options;
  const config: Record<string, unknown> = { model };
  if (session.transcription.language) config["language"] = session.transcription.language;

  const biasing = session.transcription.biasing;
  if (!biasing?.enabled || vocabulary.length === 0) return config;

  const words = vocabulary.slice(0, biasing.maxKeywords);
  switch (biasingStyleFor(model)) {
    case "keywords":
      config["keywords"] = words;
      break;
    case "prompt":
      config["prompt"] = `${biasing.promptPreamble} ${words.join(", ")}.`;
      break;
    case "none":
      break;
  }
  return config;
}

function turnDetection(session: SessionDefaults): Record<string, unknown> | null {
  const detection = session.turnDetection;
  if (detection.mode === "manual") return null;

  if (detection.mode === "semantic") {
    return {
      type: "semantic_vad",
      eagerness: detection.eagerness ?? "auto",
      create_response: detection.autoRespond,
      interrupt_response: detection.allowBargeIn,
    };
  }

  const fallback = detection.serverVadFallback;
  return {
    type: "server_vad",
    threshold: fallback?.threshold ?? 0.5,
    prefix_padding_ms: fallback?.prefixPaddingMs ?? 300,
    silence_duration_ms: fallback?.silenceDurationMs ?? 500,
    create_response: detection.autoRespond,
    interrupt_response: detection.allowBargeIn,
  };
}

/** Session patch for a mid-conversation vocabulary change, without resending the whole config. */
export function buildVocabularyPatch(
  session: SessionDefaults,
  vocabulary: string[],
): Record<string, unknown> | null {
  const biasing = session.transcription.biasing;
  if (!biasing?.enabled || vocabulary.length === 0) return null;
  const model = session.transcription.preferred;
  const style = biasingStyleFor(model);
  if (style === "none") return null;

  const words = vocabulary.slice(0, biasing.maxKeywords);
  return {
    type: "realtime",
    audio: {
      input: {
        transcription: {
          model,
          ...(session.transcription.language ? { language: session.transcription.language } : {}),
          ...(style === "keywords"
            ? { keywords: words }
            : { prompt: `${biasing.promptPreamble} ${words.join(", ")}.` }),
        },
      },
    },
  };
}
