import type { CredentialProvider, RealtimeTransport } from "./transport.ts";
import { endpointWithModel } from "./transport.ts";

/**
 * Minimal structural types for the WebRTC objects this transport touches, so the package compiles
 * without the DOM library and stays usable from React Native, where these come from a polyfill.
 */
export interface RTCDataChannelLike {
  readyState: string;
  send(data: string): void;
  addEventListener(type: string, listener: (event: any) => void): void;
  close(): void;
}

export interface RTCPeerConnectionLike {
  createDataChannel(label: string): RTCDataChannelLike;
  createOffer(): Promise<{ type: string; sdp?: string }>;
  setLocalDescription(description: { type: string; sdp?: string }): Promise<void>;
  setRemoteDescription(description: { type: string; sdp: string }): Promise<void>;
  addTrack(track: unknown, stream?: unknown): unknown;
  addEventListener(type: string, listener: (event: any) => void): void;
  close(): void;
}

export interface WebRTCTransportOptions {
  url?: string;
  model: string;
  credentials: CredentialProvider;
  /** Peer connection, created by the caller so it can supply ICE servers and platform specifics. */
  peerConnection: RTCPeerConnectionLike;
  /** Microphone tracks to publish. On WebRTC the mic is a media track, not an event payload. */
  tracks?: unknown[];
  /** Called with the agent's audio track so the caller can route and play it. */
  onRemoteTrack?: (track: unknown, streams: unknown[]) => void;
  signal?: AbortSignal;
}

const DEFAULT_URL = "https://api.openai.com/v1/realtime/calls";
const EVENT_CHANNEL = "oai-events";

/**
 * WebRTC transport: JSON events ride a data channel, audio rides a media track.
 *
 * This is the transport to prefer on a phone or in a browser, because the media stack handles jitter
 * and packet loss that a WebSocket carrying raw PCM does not.
 */
export async function connectWebRTC(options: WebRTCTransportOptions): Promise<RealtimeTransport> {
  const credentials = await options.credentials.get();
  const peer = options.peerConnection;

  const messageListeners = new Set<(raw: Record<string, unknown>) => void>();
  const closeListeners = new Set<(reason?: string) => void>();
  const errorListeners = new Set<(error: Error) => void>();

  const channel = peer.createDataChannel(EVENT_CHANNEL);

  channel.addEventListener("message", (event: { data: unknown }) => {
    const text = typeof event.data === "string" ? event.data : String(event.data);
    try {
      const parsed: unknown = JSON.parse(text);
      if (typeof parsed === "object" && parsed !== null) {
        for (const listener of messageListeners) listener(parsed as Record<string, unknown>);
      }
    } catch {
      for (const listener of errorListeners) listener(new Error("received a data channel message that was not JSON"));
    }
  });

  channel.addEventListener("close", () => {
    for (const listener of closeListeners) listener("data channel closed");
  });

  peer.addEventListener("track", (event: { track: unknown; streams: unknown[] }) => {
    options.onRemoteTrack?.(event.track, event.streams ?? []);
  });

  peer.addEventListener("connectionstatechange", (event: { target?: { connectionState?: string } }) => {
    const state = event?.target?.connectionState;
    if (state === "failed" || state === "disconnected") {
      for (const listener of errorListeners) listener(new Error(`peer connection ${state}`));
    }
  });

  for (const track of options.tracks ?? []) peer.addTrack(track);

  const offer = await peer.createOffer();
  await peer.setLocalDescription(offer);

  const response = await fetch(endpointWithModel(options.url ?? DEFAULT_URL, options.model), {
    method: "POST",
    body: offer.sdp ?? "",
    headers: {
      Authorization: `Bearer ${credentials.token}`,
      "Content-Type": "application/sdp",
    },
    ...(options.signal ? { signal: options.signal } : {}),
  });

  if (!response.ok) {
    throw new Error(`the realtime call was refused (${response.status}): ${await safeText(response)}`);
  }

  await peer.setRemoteDescription({ type: "answer", sdp: await response.text() });

  await new Promise<void>((resolve, reject) => {
    if (channel.readyState === "open") return resolve();
    const onAbort = () => reject(new Error("connection aborted"));
    options.signal?.addEventListener("abort", onAbort, { once: true });
    channel.addEventListener("open", () => {
      options.signal?.removeEventListener("abort", onAbort);
      resolve();
    });
    channel.addEventListener("error", () => reject(new Error("the realtime data channel failed to open")));
  });

  return {
    kind: "webrtc",
    carriesAudioOutOfBand: true,

    send(message) {
      if (channel.readyState !== "open") throw new Error("the realtime data channel is not open");
      channel.send(JSON.stringify(message));
    },

    sendAudio() {
      // Audio is published as a media track; there is nothing to do here.
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

    async close() {
      try {
        channel.close();
      } finally {
        peer.close();
      }
    },
  };
}

async function safeText(response: Response): Promise<string> {
  try {
    return (await response.text()).slice(0, 300);
  } catch {
    return "(no body)";
  }
}
