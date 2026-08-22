import type { ProviderEvent, ToolCallRequest } from "@riff/core";

/**
 * Server event names on the GA interface. The beta names for the same events differ
 * (`response.audio.delta` rather than `response.output_audio.delta`, and so on), and a client that
 * listens for the wrong ones connects successfully and then sits in silence, so both are handled
 * and the GA names are what the mapping is written against.
 */
export const SERVER_EVENTS = {
  sessionCreated: "session.created",
  sessionUpdated: "session.updated",
  speechStarted: "input_audio_buffer.speech_started",
  speechStopped: "input_audio_buffer.speech_stopped",
  transcriptDelta: "conversation.item.input_audio_transcription.delta",
  transcriptCompleted: "conversation.item.input_audio_transcription.completed",
  transcriptFailed: "conversation.item.input_audio_transcription.failed",
  responseCreated: "response.created",
  responseDone: "response.done",
  audioDelta: "response.output_audio.delta",
  audioDone: "response.output_audio.done",
  audioTranscriptDelta: "response.output_audio_transcript.delta",
  audioTranscriptDone: "response.output_audio_transcript.done",
  textDelta: "response.output_text.delta",
  textDone: "response.output_text.done",
  rateLimits: "rate_limits.updated",
  error: "error",
} as const;

/** Beta names still emitted by older snapshots, mapped onto their GA equivalents. */
const LEGACY_ALIASES: Record<string, string> = {
  "response.audio.delta": SERVER_EVENTS.audioDelta,
  "response.audio.done": SERVER_EVENTS.audioDone,
  "response.audio_transcript.delta": SERVER_EVENTS.audioTranscriptDelta,
  "response.audio_transcript.done": SERVER_EVENTS.audioTranscriptDone,
  "response.text.delta": SERVER_EVENTS.textDelta,
  "response.text.done": SERVER_EVENTS.textDone,
};

export const CLIENT_EVENTS = {
  sessionUpdate: "session.update",
  appendAudio: "input_audio_buffer.append",
  commitAudio: "input_audio_buffer.commit",
  clearAudio: "input_audio_buffer.clear",
  createItem: "conversation.item.create",
  createResponse: "response.create",
  cancelResponse: "response.cancel",
} as const;

interface RawEvent {
  type?: string;
  [key: string]: unknown;
}

export interface MappedEvents {
  events: ProviderEvent[];
  /** Session id from `session.created`, which the connection reports as its own identity. */
  sessionId?: string;
  model?: string;
}

/**
 * Turns one OpenAI server event into zero or more provider events.
 *
 * Function calls are read from `response.done` rather than from the streaming argument deltas,
 * because that is the only event guaranteed to carry every call of a turn at once. The session
 * relies on that to know when it has dispatched them all and may ask the model to continue.
 */
export function mapServerEvent(raw: RawEvent, decodeBase64: (value: string) => Uint8Array): MappedEvents {
  const type = raw.type ? (LEGACY_ALIASES[raw.type] ?? raw.type) : "";
  const events: ProviderEvent[] = [];

  switch (type) {
    case SERVER_EVENTS.sessionCreated: {
      const session = asRecord(raw["session"]);
      const sessionId = asString(session?.["id"]) ?? asString(raw["session_id"]) ?? "unknown";
      const model = asString(session?.["model"]) ?? "unknown";
      events.push({ type: "connected", sessionId, model });
      return { events, sessionId, model };
    }

    case SERVER_EVENTS.speechStarted:
      events.push({ type: "speech.started" });
      break;

    case SERVER_EVENTS.speechStopped:
      events.push({ type: "speech.stopped" });
      break;

    case SERVER_EVENTS.transcriptDelta:
      events.push({
        type: "transcript.delta",
        itemId: asString(raw["item_id"]) ?? "",
        delta: asString(raw["delta"]) ?? "",
      });
      break;

    case SERVER_EVENTS.transcriptCompleted: {
      const confidence = averageConfidence(raw["logprobs"]);
      events.push({
        type: "transcript.completed",
        itemId: asString(raw["item_id"]) ?? "",
        text: asString(raw["transcript"]) ?? "",
        ...(confidence === undefined ? {} : { confidence }),
      });
      break;
    }

    case SERVER_EVENTS.transcriptFailed:
      events.push({
        type: "transcript.failed",
        itemId: asString(raw["item_id"]) ?? "",
        reason: asString(asRecord(raw["error"])?.["message"]) ?? "transcription failed",
      });
      break;

    case SERVER_EVENTS.responseCreated:
      events.push({ type: "response.started", responseId: responseId(raw) });
      break;

    case SERVER_EVENTS.audioDelta: {
      const delta = asString(raw["delta"]);
      if (delta) {
        events.push({ type: "response.audio", responseId: asString(raw["response_id"]) ?? "", audio: decodeBase64(delta) });
      }
      break;
    }

    case SERVER_EVENTS.audioDone:
      events.push({ type: "response.audio.done", responseId: asString(raw["response_id"]) ?? "" });
      break;

    case SERVER_EVENTS.audioTranscriptDelta:
    case SERVER_EVENTS.textDelta:
      events.push({
        type: "response.text.delta",
        responseId: asString(raw["response_id"]) ?? "",
        delta: asString(raw["delta"]) ?? "",
      });
      break;

    case SERVER_EVENTS.audioTranscriptDone:
    case SERVER_EVENTS.textDone:
      events.push({
        type: "response.text",
        responseId: asString(raw["response_id"]) ?? "",
        text: asString(raw["transcript"]) ?? asString(raw["text"]) ?? "",
      });
      break;

    case SERVER_EVENTS.responseDone: {
      const response = asRecord(raw["response"]);
      const id = asString(response?.["id"]) ?? "";
      const status = asString(response?.["status"]);

      const calls: ToolCallRequest[] = [];
      for (const item of asArray(response?.["output"])) {
        const record = asRecord(item);
        if (record?.["type"] !== "function_call") continue;
        calls.push({
          callId: asString(record["call_id"]) ?? "",
          name: asString(record["name"]) ?? "",
          argumentsJson: asString(record["arguments"]) ?? "{}",
        });
      }
      if (calls.length > 0) events.push({ type: "tool.calls", calls });

      if (status === "cancelled") {
        events.push({ type: "response.cancelled", responseId: id });
      } else if (status === "failed" || status === "incomplete") {
        const details = asRecord(response?.["status_details"]);
        events.push({
          type: "error",
          error: {
            code: asString(asRecord(details?.["error"])?.["code"]) ?? `response_${status}`,
            message:
              asString(asRecord(details?.["error"])?.["message"]) ??
              `response ${status}${asString(details?.["reason"]) ? `: ${asString(details?.["reason"])}` : ""}`,
            retryable: true,
          },
        });
        events.push({ type: "response.done", responseId: id });
      } else {
        const usage = mapUsage(asRecord(response?.["usage"]));
        events.push({ type: "response.done", responseId: id, ...(usage ? { usage } : {}) });
      }
      break;
    }

    case SERVER_EVENTS.rateLimits: {
      const limits = asArray(raw["rate_limits"]).map(asRecord);
      const tightest = limits
        .filter((limit): limit is Record<string, unknown> => limit !== undefined)
        .sort((a, b) => (asNumber(a["remaining"]) ?? 0) - (asNumber(b["remaining"]) ?? 0))[0];
      if (tightest) {
        events.push({
          type: "rate_limit",
          remaining: asNumber(tightest["remaining"]) ?? 0,
          resetSeconds: asNumber(tightest["reset_seconds"]) ?? 0,
        });
      }
      break;
    }

    case SERVER_EVENTS.error: {
      const error = asRecord(raw["error"]) ?? raw;
      const code = asString(error["code"]) ?? asString(error["type"]) ?? "provider_error";
      events.push({
        type: "error",
        error: {
          code,
          message: asString(error["message"]) ?? "the provider reported an error",
          // A rejected session config will be rejected again; a transport hiccup will not.
          retryable: !code.includes("invalid_request"),
        },
      });
      break;
    }

    default:
      break;
  }

  return { events };
}

function responseId(raw: RawEvent): string {
  return asString(asRecord(raw["response"])?.["id"]) ?? asString(raw["response_id"]) ?? "";
}

function mapUsage(usage?: Record<string, unknown>) {
  if (!usage) return undefined;
  const inputDetails = asRecord(usage["input_token_details"]);
  return {
    ...(asNumber(usage["input_tokens"]) === undefined ? {} : { inputTokens: asNumber(usage["input_tokens"]) }),
    ...(asNumber(usage["output_tokens"]) === undefined ? {} : { outputTokens: asNumber(usage["output_tokens"]) }),
    ...(asNumber(inputDetails?.["audio_tokens"]) === undefined
      ? {}
      : { inputAudioTokens: asNumber(inputDetails?.["audio_tokens"]) }),
    ...(asNumber(inputDetails?.["cached_tokens"]) === undefined
      ? {}
      : { cachedTokens: asNumber(inputDetails?.["cached_tokens"]) }),
  };
}

/** Turns transcription logprobs into a rough confidence, used to decide when to double check a word. */
function averageConfidence(value: unknown): number | undefined {
  const entries = asArray(value)
    .map(asRecord)
    .map((entry) => asNumber(entry?.["logprob"]))
    .filter((logprob): logprob is number => logprob !== undefined);
  if (entries.length === 0) return undefined;
  const mean = entries.reduce((total, logprob) => total + logprob, 0) / entries.length;
  return Math.round(Math.exp(mean) * 1000) / 1000;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function asString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}
