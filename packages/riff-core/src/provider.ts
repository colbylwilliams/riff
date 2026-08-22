import type { SessionDefaults, ToolDefinition } from "./types.ts";

/**
 * The seam between Riff and whatever produces speech-to-speech.
 *
 * It is deliberately small. Everything that makes Riff what it is — the ledger, the grounding
 * check, drafts, takes, the artifact — lives above this line and is provider independent. A
 * provider only has to move audio, report what was heard, and relay tool calls. That is the common
 * denominator of OpenAI Realtime, Gemini Live, and a chained on-device speech pipeline, so swapping
 * one for another changes no behavior that a speaker can observe.
 */
export interface RealtimeProvider {
  readonly id: string;
  readonly capabilities: ProviderCapabilities;
  connect(request: ConnectRequest): Promise<RealtimeConnection>;
}

export interface ProviderCapabilities {
  /** Speech in and speech out through one model, rather than a transcribe-think-speak chain. */
  speechToSpeech: boolean;
  /** The speaker can talk over the agent and cut it off mid-sentence. */
  bargeIn: boolean;
  /** The provider decides when a turn has ended from meaning, not just from silence. */
  semanticTurnDetection: boolean;
  /** Transcription can be biased toward a supplied vocabulary. */
  vocabularyBiasing: "keywords" | "prompt" | "none";
  /** Verbatim transcripts of the speaker are available, which the prompt body depends on. */
  inputTranscription: boolean;
  functionCalling: boolean;
  audio: { input: AudioFormat; output: AudioFormat };
  maxSessionSeconds?: number;
}

export interface AudioFormat {
  encoding: "pcm_s16le" | "opus" | "g711_ulaw" | "g711_alaw";
  sampleRate: number;
  channels: number;
}

export interface ConnectRequest {
  instructions: string;
  tools: ToolDefinition[];
  session: SessionDefaults;
  /** Canonical spellings to bias transcription toward, compiled from the active lexicon. */
  vocabulary?: string[];
  signal?: AbortSignal;
}

export interface RealtimeConnection {
  readonly sessionId: string;
  readonly model: string;
  /** Appends captured audio in the format the provider advertised. */
  sendAudio(chunk: Uint8Array): void;
  /** Ends the current turn explicitly. Only needed when turn detection is manual. */
  commitAudio(): void;
  /** Injects text as if the speaker had said it, for typed input and host notifications. */
  sendText(text: string, options?: { respond?: boolean }): void;
  /** Returns a tool result. `resultJson` is a JSON string, matching every provider's expectation. */
  respondToTool(callId: string, resultJson: string): void;
  requestResponse(): void;
  /** Stops the agent mid-sentence. Used when the speaker talks over it. */
  cancelResponse(): void;
  updateSession(patch: Partial<SessionDefaults> & { vocabulary?: string[] }): void;
  on(listener: ProviderEventListener): () => void;
  close(reason?: string): Promise<void>;
}

export type ProviderEventListener = (event: ProviderEvent) => void;

export type ProviderEvent =
  | { type: "connected"; sessionId: string; model: string }
  | { type: "speech.started" }
  | { type: "speech.stopped" }
  | { type: "transcript.delta"; itemId: string; delta: string }
  | { type: "transcript.completed"; itemId: string; text: string; confidence?: number }
  | { type: "transcript.failed"; itemId: string; reason: string }
  | { type: "response.started"; responseId: string }
  | { type: "response.audio"; responseId: string; audio: Uint8Array }
  | { type: "response.audio.done"; responseId: string }
  | { type: "response.text.delta"; responseId: string; delta: string }
  | { type: "response.text"; responseId: string; text: string }
  | { type: "response.done"; responseId: string; usage?: TokenUsage }
  | { type: "response.cancelled"; responseId: string }
  | { type: "tool.calls"; calls: ToolCallRequest[] }
  | { type: "rate_limit"; remaining: number; resetSeconds: number }
  | { type: "error"; error: ProviderError }
  | { type: "closed"; reason?: string };

export interface ToolCallRequest {
  callId: string;
  name: string;
  argumentsJson: string;
}

export interface TokenUsage {
  inputTokens?: number;
  outputTokens?: number;
  inputAudioTokens?: number;
  outputAudioTokens?: number;
  cachedTokens?: number;
}

export interface ProviderError {
  code: string;
  message: string;
  /** Whether reconnecting is worth trying. Transport faults are; a rejected session config is not. */
  retryable: boolean;
  cause?: unknown;
}

export class RiffProviderError extends Error implements ProviderError {
  readonly code: string;
  readonly retryable: boolean;
  override readonly cause?: unknown;

  constructor(code: string, message: string, options: { retryable?: boolean; cause?: unknown } = {}) {
    super(message);
    this.name = "RiffProviderError";
    this.code = code;
    this.retryable = options.retryable ?? false;
    if (options.cause !== undefined) this.cause = options.cause;
  }
}

/** Minimal event fan-out shared by provider implementations. */
export function createEventHub(): {
  emit: (event: ProviderEvent) => void;
  on: (listener: ProviderEventListener) => () => void;
} {
  const listeners = new Set<ProviderEventListener>();
  return {
    emit(event) {
      for (const listener of [...listeners]) listener(event);
    },
    on(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
