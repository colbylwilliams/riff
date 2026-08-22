import type { CredentialProvider, RealtimeTransport } from "./transport.ts";
import { encodeBase64, endpointWithModel } from "./transport.ts";
import { CLIENT_EVENTS } from "./events.ts";

export interface WebSocketLike {
  readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  addEventListener(type: string, listener: (event: any) => void): void;
}

export interface WebSocketTransportOptions {
  url?: string;
  model: string;
  credentials: CredentialProvider;
  organization?: string;
  project?: string;
  /** Supply your own socket to add headers, a proxy, or a custom TLS configuration. */
  createWebSocket?: (url: string, protocols: string[]) => WebSocketLike;
  signal?: AbortSignal;
}

const DEFAULT_URL = "wss://api.openai.com/v1/realtime";
const OPEN = 1;

/**
 * WebSocket transport, where audio is base64 PCM inside JSON events.
 *
 * WebRTC is the better choice on a phone or in a browser because it handles packet loss and jitter,
 * but WebSocket needs no media stack at all, which makes it the transport that works everywhere:
 * servers, tests, terminals, and any platform without a WebRTC dependency.
 */
export async function connectWebSocket(options: WebSocketTransportOptions): Promise<RealtimeTransport> {
  const credentials = await options.credentials.get();
  const url = endpointWithModel(options.url ?? DEFAULT_URL, options.model);

  const protocols = [
    "realtime",
    `openai-insecure-api-key.${credentials.token}`,
    ...(options.organization ? [`openai-organization.${options.organization}`] : []),
    ...(options.project ? [`openai-project.${options.project}`] : []),
  ];

  const factory =
    options.createWebSocket ??
    ((target: string, subprotocols: string[]) => {
      const Ctor = (globalThis as { WebSocket?: new (url: string, protocols?: string[]) => WebSocketLike }).WebSocket;
      if (!Ctor) throw new Error("no WebSocket implementation available; pass createWebSocket");
      return new Ctor(target, subprotocols);
    });

  const socket = factory(url, protocols);

  const messageListeners = new Set<(raw: Record<string, unknown>) => void>();
  const closeListeners = new Set<(reason?: string) => void>();
  const errorListeners = new Set<(error: Error) => void>();

  socket.addEventListener("message", (event: { data: unknown }) => {
    const text = typeof event.data === "string" ? event.data : String(event.data);
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      for (const listener of errorListeners) listener(new Error("received a message that was not JSON"));
      return;
    }
    if (typeof parsed === "object" && parsed !== null) {
      for (const listener of messageListeners) listener(parsed as Record<string, unknown>);
    }
  });

  socket.addEventListener("close", (event: { reason?: string; code?: number }) => {
    for (const listener of closeListeners) listener(event?.reason || (event?.code ? `code ${event.code}` : undefined));
  });

  socket.addEventListener("error", () => {
    for (const listener of errorListeners) listener(new Error("the realtime socket failed"));
  });

  await new Promise<void>((resolve, reject) => {
    if (socket.readyState === OPEN) return resolve();
    const onAbort = () => reject(new Error("connection aborted"));
    options.signal?.addEventListener("abort", onAbort, { once: true });
    socket.addEventListener("open", () => {
      options.signal?.removeEventListener("abort", onAbort);
      resolve();
    });
    socket.addEventListener("error", () => reject(new Error("could not open the realtime socket")));
    socket.addEventListener("close", (event: { reason?: string }) =>
      reject(new Error(`the realtime socket closed before it opened${event?.reason ? `: ${event.reason}` : ""}`)),
    );
  });

  return {
    kind: "websocket",
    carriesAudioOutOfBand: false,

    send(message) {
      if (socket.readyState !== OPEN) throw new Error("the realtime socket is not open");
      socket.send(JSON.stringify(message));
    },

    sendAudio(pcm) {
      if (socket.readyState !== OPEN) return;
      socket.send(JSON.stringify({ type: CLIENT_EVENTS.appendAudio, audio: encodeBase64(pcm) }));
    },

    onMessage(listener) {
      messageListeners.add(listener);
    },
    onClose(listener) {
      closeListeners.add(listener);
    },
    onError(listener) {
      errorListeners.add(listener);
    },

    async close(reason) {
      if (socket.readyState === OPEN) socket.close(1000, reason?.slice(0, 120));
    },
  };
}
