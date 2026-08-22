import type {
  ConnectRequest,
  ProviderCapabilities,
  ProviderEvent,
  RealtimeConnection,
  RealtimeProvider,
  SessionDefaults,
} from "@riff/core";
import { RiffProviderError, createEventHub } from "@riff/core";
import type { CredentialProvider, RealtimeTransport } from "./transport.ts";
import { decodeBase64 } from "./transport.ts";
import { CLIENT_EVENTS, SERVER_EVENTS, mapServerEvent } from "./events.ts";
import { biasingStyleFor, buildOpenAISession, buildVocabularyPatch } from "./session-config.ts";
import { connectWebSocket, type WebSocketLike } from "./websocket-transport.ts";
import { connectWebRTC, type RTCPeerConnectionLike } from "./webrtc-transport.ts";

export interface OpenAIRealtimeProviderOptions {
  credentials: CredentialProvider;
  /** `websocket` works everywhere; `webrtc` is better on a phone or in a browser. */
  transport?: "websocket" | "webrtc";
  /** Overrides the model in the bundle's session defaults. */
  model?: string;
  organization?: string;
  project?: string;
  /** Point at Azure OpenAI or a gateway. The event protocol is the same. */
  webSocketUrl?: string;
  callsUrl?: string;
  createWebSocket?: (url: string, protocols: string[]) => WebSocketLike;
  /** Required for the WebRTC transport, where the caller owns the peer connection and the mic. */
  webrtc?: {
    peerConnection: () => RTCPeerConnectionLike | Promise<RTCPeerConnectionLike>;
    tracks?: unknown[];
    onRemoteTrack?: (track: unknown, streams: unknown[]) => void;
  };
  /** Time to wait for `session.created` before giving up. */
  handshakeTimeoutMs?: number;
}

const HANDSHAKE_TIMEOUT_MS = 15_000;

/**
 * Riff on the OpenAI Realtime API.
 *
 * This class is the only place in the project that knows OpenAI's wire format. It implements the
 * provider interface and nothing more, which is what keeps the agent's behavior — the ledger, the
 * grounding check, the drafts — identical no matter what is generating the speech.
 */
export class OpenAIRealtimeProvider implements RealtimeProvider {
  readonly id = "openai-realtime";
  readonly #options: OpenAIRealtimeProviderOptions;

  constructor(options: OpenAIRealtimeProviderOptions) {
    this.#options = options;
    if (options.transport === "webrtc" && !options.webrtc) {
      throw new Error("the webrtc transport needs a webrtc.peerConnection factory");
    }
  }

  get capabilities(): ProviderCapabilities {
    const overWebRTC = this.#options.transport === "webrtc";
    return {
      speechToSpeech: true,
      bargeIn: true,
      semanticTurnDetection: true,
      vocabularyBiasing: "prompt",
      inputTranscription: true,
      functionCalling: true,
      audio: overWebRTC
        ? {
            input: { encoding: "opus", sampleRate: 48000, channels: 1 },
            output: { encoding: "opus", sampleRate: 48000, channels: 1 },
          }
        : {
            input: { encoding: "pcm_s16le", sampleRate: 24000, channels: 1 },
            output: { encoding: "pcm_s16le", sampleRate: 24000, channels: 1 },
          },
      maxSessionSeconds: 3600,
    };
  }

  /** How the configured transcription model wants vocabulary hints. */
  biasingStyle(session: SessionDefaults): "prompt" | "keywords" | "none" {
    return biasingStyleFor(session.transcription.preferred);
  }

  async connect(request: ConnectRequest): Promise<RealtimeConnection> {
    const model = this.#options.model ?? request.session.model.preferred;
    const hub = createEventHub();

    const transport = await this.#openTransport(model, request.signal);

    let sessionId = "";
    let negotiatedModel = model;
    let resolveHandshake: (() => void) | null = null;
    let rejectHandshake: ((error: Error) => void) | null = null;

    const handshake = new Promise<void>((resolve, reject) => {
      resolveHandshake = resolve;
      rejectHandshake = reject;
    });

    transport.onMessage((raw) => {
      const mapped = mapServerEvent(raw, decodeBase64);
      if (mapped.sessionId) {
        sessionId = mapped.sessionId;
        negotiatedModel = mapped.model ?? model;
        resolveHandshake?.();
        resolveHandshake = null;
      }
      // A failure during the handshake has to surface as a connect error, not a silent timeout.
      if (raw["type"] === SERVER_EVENTS.error && rejectHandshake) {
        const error = (raw["error"] ?? {}) as { message?: string; code?: string };
        rejectHandshake(new Error(error.message ?? "the realtime session was rejected"));
        rejectHandshake = null;
      }
      for (const event of mapped.events) hub.emit(event);
    });

    transport.onError((error) => {
      rejectHandshake?.(error);
      rejectHandshake = null;
      hub.emit({
        type: "error",
        error: { code: "transport_error", message: error.message, retryable: true, cause: error },
      });
    });

    transport.onClose((reason) => {
      rejectHandshake?.(new Error(reason ? `connection closed: ${reason}` : "connection closed"));
      rejectHandshake = null;
      hub.emit({ type: "closed", ...(reason ? { reason } : {}) });
    });

    try {
      await withTimeout(
        handshake,
        this.#options.handshakeTimeoutMs ?? HANDSHAKE_TIMEOUT_MS,
        "session.created",
        // Opening the transport is not the whole of connecting. Without this, an abort arriving
        // after the socket is open is ignored until the handshake timeout expires.
        request.signal,
      );
    } catch (error) {
      await transport.close("handshake failed");
      throw new RiffProviderError("handshake_failed", (error as Error).message, { retryable: true, cause: error });
    }

    transport.send({
      type: CLIENT_EVENTS.sessionUpdate,
      session: buildOpenAISession({
        session: request.session,
        instructions: request.instructions,
        tools: request.tools,
        ...(request.vocabulary ? { vocabulary: request.vocabulary } : {}),
        model,
      }),
    });

    return this.#createConnection(transport, hub, request.session, sessionId, negotiatedModel);
  }

  async #openTransport(model: string, signal?: AbortSignal): Promise<RealtimeTransport> {
    if (this.#options.transport === "webrtc") {
      const webrtc = this.#options.webrtc!;
      return connectWebRTC({
        model,
        credentials: this.#options.credentials,
        peerConnection: await webrtc.peerConnection(),
        ...(this.#options.callsUrl ? { url: this.#options.callsUrl } : {}),
        ...(webrtc.tracks ? { tracks: webrtc.tracks } : {}),
        ...(webrtc.onRemoteTrack ? { onRemoteTrack: webrtc.onRemoteTrack } : {}),
        ...(signal ? { signal } : {}),
      });
    }

    return connectWebSocket({
      model,
      credentials: this.#options.credentials,
      ...(this.#options.webSocketUrl ? { url: this.#options.webSocketUrl } : {}),
      ...(this.#options.organization ? { organization: this.#options.organization } : {}),
      ...(this.#options.project ? { project: this.#options.project } : {}),
      ...(this.#options.createWebSocket ? { createWebSocket: this.#options.createWebSocket } : {}),
      ...(signal ? { signal } : {}),
    });
  }

  #createConnection(
    transport: RealtimeTransport,
    hub: { emit: (event: ProviderEvent) => void; on: (listener: (event: ProviderEvent) => void) => () => void },
    session: SessionDefaults,
    sessionId: string,
    model: string,
  ): RealtimeConnection {
    return {
      sessionId,
      model,

      sendAudio(chunk) {
        transport.sendAudio(chunk);
      },

      commitAudio() {
        transport.send({ type: CLIENT_EVENTS.commitAudio });
      },

      sendText(text, options) {
        transport.send({
          type: CLIENT_EVENTS.createItem,
          item: { type: "message", role: "user", content: [{ type: "input_text", text }] },
        });
        if (options?.respond !== false) transport.send({ type: CLIENT_EVENTS.createResponse });
      },

      respondToTool(callId, resultJson) {
        transport.send({
          type: CLIENT_EVENTS.createItem,
          item: { type: "function_call_output", call_id: callId, output: resultJson },
        });
      },

      requestResponse() {
        transport.send({ type: CLIENT_EVENTS.createResponse });
      },

      cancelResponse() {
        transport.send({ type: CLIENT_EVENTS.cancelResponse });
        // On WebRTC the already-buffered audio keeps playing unless it is explicitly dropped, so
        // the agent would talk over someone who just interrupted it.
        if (transport.kind === "webrtc") transport.send({ type: "output_audio_buffer.clear" });
      },

      updateSession(patch) {
        if (patch.vocabulary) {
          const vocabularyPatch = buildVocabularyPatch(session, patch.vocabulary);
          if (vocabularyPatch) transport.send({ type: CLIENT_EVENTS.sessionUpdate, session: vocabularyPatch });
        }

        const { vocabulary: _vocabulary, ...rest } = patch;
        if (Object.keys(rest).length === 0) return;

        transport.send({
          type: CLIENT_EVENTS.sessionUpdate,
          session: buildOpenAISession({
            session: { ...session, ...rest } as SessionDefaults,
            instructions: "",
            tools: [],
          }),
        });
      },

      on(listener) {
        return hub.on(listener);
      },

      async close(reason) {
        await transport.close(reason);
      },
    };
  }
}

function withTimeout<T>(promise: Promise<T>, ms: number, what: string, signal?: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    if (signal?.aborted) return reject(new Error("connection aborted"));

    const timer = setTimeout(() => reject(new Error(`timed out after ${ms}ms waiting for ${what}`)), ms);
    timer.unref?.();

    const onAbort = () => {
      clearTimeout(timer);
      reject(new Error("connection aborted"));
    };
    signal?.addEventListener("abort", onAbort, { once: true });

    const settle = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
    };

    promise.then(
      (value) => {
        settle();
        resolve(value);
      },
      (error) => {
        settle();
        reject(error);
      },
    );
  });
}
