import type {
  ConnectRequest,
  ProviderCapabilities,
  ProviderEvent,
  RealtimeConnection,
  RealtimeProvider,
} from "../src/provider.ts";
import { createEventHub } from "../src/provider.ts";

export interface FakeCall {
  kind: "audio" | "commit" | "text" | "tool-result" | "response" | "cancel" | "session" | "close";
  payload?: unknown;
}

/** A provider the test drives by hand, standing in for a speech-to-speech model. */
export class FakeProvider implements RealtimeProvider {
  readonly id = "fake";
  readonly calls: FakeCall[] = [];
  request: ConnectRequest | null = null;
  connection: RealtimeConnection | null = null;

  #hub = createEventHub();

  get capabilities(): ProviderCapabilities {
    return {
      speechToSpeech: true,
      bargeIn: true,
      semanticTurnDetection: true,
      vocabularyBiasing: "keywords",
      inputTranscription: true,
      functionCalling: true,
      audio: {
        input: { encoding: "pcm_s16le", sampleRate: 24000, channels: 1 },
        output: { encoding: "pcm_s16le", sampleRate: 24000, channels: 1 },
      },
      maxSessionSeconds: 3600,
    };
  }

  async connect(request: ConnectRequest): Promise<RealtimeConnection> {
    this.request = request;
    const calls = this.calls;
    const hub = this.#hub;

    this.connection = {
      sessionId: "sess_fake",
      model: "fake-realtime",
      sendAudio: (chunk) => calls.push({ kind: "audio", payload: chunk.length }),
      commitAudio: () => calls.push({ kind: "commit" }),
      sendText: (text, options) => calls.push({ kind: "text", payload: { text, options } }),
      respondToTool: (callId, resultJson) => calls.push({ kind: "tool-result", payload: { callId, resultJson } }),
      requestResponse: () => calls.push({ kind: "response" }),
      cancelResponse: () => calls.push({ kind: "cancel" }),
      updateSession: (patch) => calls.push({ kind: "session", payload: patch }),
      on: (listener) => hub.on(listener),
      close: async (reason) => {
        calls.push({ kind: "close", payload: reason });
      },
    };

    queueMicrotask(() => hub.emit({ type: "connected", sessionId: "sess_fake", model: "fake-realtime" }));
    return this.connection;
  }

  emit(event: ProviderEvent): void {
    this.#hub.emit(event);
  }

  /** Simulates a completed turn of speech arriving from transcription. */
  say(text: string, itemId = `item_${Math.random().toString(36).slice(2, 8)}`): void {
    this.emit({ type: "transcript.completed", itemId, text });
  }

  /** Simulates the model calling one tool in a turn. */
  callTool(name: string, args: unknown, callId = `call_${Math.random().toString(36).slice(2, 8)}`): string {
    this.callTools([{ name, args, callId }]);
    return callId;
  }

  /** Simulates a turn in which the model calls several tools at once. */
  callTools(calls: Array<{ name: string; args: unknown; callId?: string }>): string[] {
    const requests = calls.map((call) => ({
      callId: call.callId ?? `call_${Math.random().toString(36).slice(2, 8)}`,
      name: call.name,
      argumentsJson: JSON.stringify(call.args),
    }));
    this.emit({ type: "tool.calls", calls: requests });
    return requests.map((request) => request.callId);
  }

  /** The JSON result handed back for a given tool call. */
  resultFor(callId: string): any {
    const entry = [...this.calls]
      .reverse()
      .find((call) => call.kind === "tool-result" && (call.payload as { callId: string }).callId === callId);
    if (!entry) throw new Error(`no tool result was sent for ${callId}`);
    return JSON.parse((entry.payload as { resultJson: string }).resultJson);
  }
}

/** Waits for the session's asynchronous tool dispatch to settle. */
export async function settle(times = 4): Promise<void> {
  for (let i = 0; i < times; i++) await new Promise((resolve) => setImmediate(resolve));
}
