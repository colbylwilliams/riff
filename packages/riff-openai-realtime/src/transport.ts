/** Transport-level plumbing shared by the WebSocket and WebRTC paths. */

export interface RealtimeTransport {
  readonly kind: "websocket" | "webrtc";
  /**
   * True when audio travels on its own channel rather than through JSON events. WebRTC negotiates a
   * media track, so the host attaches the microphone directly and `sendAudio` does nothing.
   */
  readonly carriesAudioOutOfBand: boolean;
  send(message: Record<string, unknown>): void;
  sendAudio(pcm: Uint8Array): void;
  onMessage(listener: (raw: Record<string, unknown>) => void): void;
  onClose(listener: (reason?: string) => void): void;
  onError(listener: (error: Error) => void): void;
  close(reason?: string): Promise<void>;
}

export interface Credentials {
  token: string;
  kind: "api-key" | "client-secret";
}

export interface CredentialProvider {
  get(): Promise<Credentials>;
}

/**
 * For server-side use only. An API key on a phone or in a browser is a key you have published, so
 * clients mint a short-lived client secret instead.
 */
export function apiKeyCredentials(apiKey: string): CredentialProvider {
  return { async get() { return { token: apiKey, kind: "api-key" }; } };
}

/**
 * The client-side path: a callback that asks your own backend for an ephemeral secret. Results are
 * cached until shortly before expiry so reconnects do not make a round trip they do not need.
 */
export function clientSecretCredentials(
  mint: () => Promise<{ value: string; expiresAt?: number }>,
  options: { refreshMarginSeconds?: number } = {},
): CredentialProvider {
  const margin = (options.refreshMarginSeconds ?? 30) * 1000;
  let cached: { value: string; expiresAt?: number } | null = null;

  return {
    async get() {
      const stillValid = cached && (!cached.expiresAt || cached.expiresAt * 1000 - margin > Date.now());
      if (!stillValid) cached = await mint();
      return { token: cached!.value, kind: "client-secret" };
    },
  };
}

const BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/** Base64 encode that works in Node, browsers, and React Native without a polyfill. */
export function encodeBase64(bytes: Uint8Array): string {
  const buffer = (globalThis as { Buffer?: { from(b: Uint8Array): { toString(enc: string): string } } }).Buffer;
  if (buffer) return buffer.from(bytes).toString("base64");

  let output = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i] ?? 0;
    const b = bytes[i + 1] ?? 0;
    const c = bytes[i + 2] ?? 0;
    const triple = (a << 16) | (b << 8) | c;
    output += BASE64_ALPHABET[(triple >> 18) & 63];
    output += BASE64_ALPHABET[(triple >> 12) & 63];
    output += i + 1 < bytes.length ? BASE64_ALPHABET[(triple >> 6) & 63] : "=";
    output += i + 2 < bytes.length ? BASE64_ALPHABET[triple & 63] : "=";
  }
  return output;
}

export function decodeBase64(value: string): Uint8Array {
  const buffer = (globalThis as { Buffer?: { from(v: string, enc: string): Uint8Array } }).Buffer;
  if (buffer) return new Uint8Array(buffer.from(value, "base64"));

  const clean = value.replace(/[^A-Za-z0-9+/]/g, "");
  const bytes = new Uint8Array((clean.length * 3) >> 2);
  let position = 0;
  for (let i = 0; i < clean.length; i += 4) {
    const chunk =
      (BASE64_ALPHABET.indexOf(clean[i] ?? "A") << 18) |
      (BASE64_ALPHABET.indexOf(clean[i + 1] ?? "A") << 12) |
      (BASE64_ALPHABET.indexOf(clean[i + 2] ?? "A") << 6) |
      BASE64_ALPHABET.indexOf(clean[i + 3] ?? "A");
    if (position < bytes.length) bytes[position++] = (chunk >> 16) & 255;
    if (position < bytes.length) bytes[position++] = (chunk >> 8) & 255;
    if (position < bytes.length) bytes[position++] = chunk & 255;
  }
  return bytes;
}

/**
 * Adds the model to an endpoint without disturbing what is already there.
 *
 * Azure and gateway endpoints carry required parameters such as `api-version` and `deployment`.
 * Appending `?model=` to those produces a second `?` and folds the model into the previous value,
 * which fails in a way that looks like an auth problem.
 */
export function endpointWithModel(base: string, model: string): string {
  try {
    const url = new URL(base);
    url.searchParams.set("model", model);
    return url.toString();
  } catch {
    const separator = base.includes("?") ? "&" : "?";
    return `${base}${separator}model=${encodeURIComponent(model)}`;
  }
}
