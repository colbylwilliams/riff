import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

import type { AgentBundle, ProviderEvent } from "@riff/core";
import { loadBundle } from "@riff/core";
import { OpenAIRealtimeProvider } from "../src/provider.ts";
import { apiKeyCredentials, clientSecretCredentials, endpointWithModel } from "../src/transport.ts";
import { mintClientSecret } from "../src/client-secret.ts";
import type { WebSocketLike } from "../src/websocket-transport.ts";

const root = new URL("../../../", import.meta.url).pathname;
const bundle: AgentBundle = loadBundle(
  JSON.parse(readFileSync(join(root, "core/dist/riff-agent.bundle.json"), "utf8")),
);

class FakeWebSocket implements WebSocketLike {
  readyState = 0;
  readonly sent: string[] = [];
  readonly protocols: string[];
  readonly url: string;
  #listeners = new Map<string, Array<(event: any) => void>>();

  constructor(url: string, protocols: string[]) {
    this.url = url;
    this.protocols = protocols;
    setTimeout(() => {
      this.readyState = 1;
      this.#fire("open", {});
    }, 0);
  }

  addEventListener(type: string, listener: (event: any) => void): void {
    const existing = this.#listeners.get(type) ?? [];
    existing.push(listener);
    this.#listeners.set(type, existing);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(code?: number, reason?: string): void {
    this.readyState = 3;
    this.#fire("close", { code, reason });
  }

  receive(message: unknown): void {
    this.#fire("message", { data: JSON.stringify(message) });
  }

  sentOfType(type: string): any[] {
    return this.sent.map((raw) => JSON.parse(raw)).filter((message) => message.type === type);
  }

  #fire(type: string, event: unknown): void {
    for (const listener of this.#listeners.get(type) ?? []) listener(event);
  }
}

async function connect(): Promise<{ socket: FakeWebSocket; connection: Awaited<ReturnType<OpenAIRealtimeProvider["connect"]>> }> {
  let socket: FakeWebSocket | null = null;
  const provider = new OpenAIRealtimeProvider({
    credentials: apiKeyCredentials("sk-test-key"),
    createWebSocket: (url, protocols) => (socket = new FakeWebSocket(url, protocols)),
  });

  const connecting = provider.connect({
    instructions: bundle.instructions,
    tools: bundle.tools,
    session: bundle.session,
    vocabulary: ["Flakeguard"],
  });

  await new Promise((resolve) => setTimeout(resolve, 5));
  socket!.receive({ type: "session.created", session: { id: "sess_1", model: "gpt-realtime-2.1" } });

  return { socket: socket!, connection: await connecting };
}

describe("OpenAIRealtimeProvider", () => {
  it("connects with the model in the query string and the credential in the subprotocol", async () => {
    const { socket } = await connect();
    assert.match(socket.url, /wss:\/\/api\.openai\.com\/v1\/realtime\?model=gpt-realtime-2\.1/);
    assert.equal(socket.protocols[0], "realtime");
    assert.equal(socket.protocols[1], "openai-insecure-api-key.sk-test-key");
  });

  it("configures the session once the handshake lands", async () => {
    const { socket, connection } = await connect();
    assert.equal(connection.sessionId, "sess_1");
    assert.equal(connection.model, "gpt-realtime-2.1");

    const [update] = socket.sentOfType("session.update");
    assert.equal(update.session.type, "realtime");
    assert.match(update.session.instructions, /You are Riff/);
    assert.equal(update.session.tools.length, bundle.tools.length);
    assert.match(update.session.audio.input.transcription.prompt, /Flakeguard/);
  });

  it("returns a tool result in the shape the API requires", async () => {
    const { socket, connection } = await connect();
    connection.respondToTool("call_a", '{"ok":true}');
    connection.requestResponse();

    const [item] = socket.sentOfType("conversation.item.create");
    assert.deepEqual(item.item, {
      type: "function_call_output",
      call_id: "call_a",
      output: '{"ok":true}',
    });
    assert.equal(socket.sentOfType("response.create").length, 1);
  });

  it("adds typed input as a user message", async () => {
    const { socket, connection } = await connect();
    connection.sendText("hello", { respond: false });

    const [item] = socket.sentOfType("conversation.item.create");
    assert.deepEqual(item.item, {
      type: "message",
      role: "user",
      content: [{ type: "input_text", text: "hello" }],
    });
    assert.equal(socket.sentOfType("response.create").length, 0);
  });

  it("streams audio as base64 append events", async () => {
    const { socket, connection } = await connect();
    connection.sendAudio(new Uint8Array([0, 1, 2, 3]));

    const [append] = socket.sentOfType("input_audio_buffer.append");
    assert.equal(append.audio, "AAECAw==");
  });

  it("forwards provider events to listeners", async () => {
    const { socket, connection } = await connect();
    const received: ProviderEvent[] = [];
    connection.on((event) => received.push(event));

    socket.receive({
      type: "conversation.item.input_audio_transcription.completed",
      item_id: "item_1",
      transcript: "the export button is broken",
    });

    assert.deepEqual(received, [
      { type: "transcript.completed", itemId: "item_1", text: "the export button is broken" },
    ]);
  });

  it("only patches transcription when the vocabulary changes", async () => {
    const { socket, connection } = await connect();
    const before = socket.sentOfType("session.update").length;

    connection.updateSession({ vocabulary: ["Flakeguard", "Riff"] });

    const updates = socket.sentOfType("session.update");
    assert.equal(updates.length, before + 1);
    assert.match(updates.at(-1).session.audio.input.transcription.prompt, /Riff/);
    assert.equal(updates.at(-1).session.tools, undefined);
  });

  it("fails the connect rather than hanging when the session is rejected", async () => {
    let socket: FakeWebSocket | null = null;
    const provider = new OpenAIRealtimeProvider({
      credentials: apiKeyCredentials("sk-test-key"),
      createWebSocket: (url, protocols) => (socket = new FakeWebSocket(url, protocols)),
      handshakeTimeoutMs: 500,
    });

    const connecting = provider.connect({
      instructions: "",
      tools: [],
      session: bundle.session,
    });

    await new Promise((resolve) => setTimeout(resolve, 5));
    socket!.receive({ type: "error", error: { code: "invalid_request_error", message: "bad model" } });

    await assert.rejects(connecting, /bad model/);
  });

  it("refuses the webrtc transport without a peer connection factory", () => {
    assert.throws(
      () => new OpenAIRealtimeProvider({ credentials: apiKeyCredentials("sk"), transport: "webrtc" }),
      /peerConnection/,
    );
  });
});

describe("client secrets", () => {
  it("mints an ephemeral secret with the session baked in", async () => {
    const requests: Array<{ url: string; init: any }> = [];
    const original = globalThis.fetch;
    globalThis.fetch = (async (url: string, init: any) => {
      requests.push({ url, init });
      return new Response(JSON.stringify({ value: "ek_abc", expires_at: 1234 }), { status: 200 });
    }) as typeof fetch;

    try {
      const secret = await mintClientSecret({
        apiKey: "sk-server-only",
        session: bundle.session,
        instructions: bundle.instructions,
        tools: bundle.tools,
        expiresInSeconds: 600,
        safetyIdentifier: "hashed-user",
      });

      assert.deepEqual(secret, { value: "ek_abc", expiresAt: 1234 });
      assert.equal(requests[0]!.url, "https://api.openai.com/v1/realtime/client_secrets");
      assert.equal(requests[0]!.init.headers["OpenAI-Safety-Identifier"], "hashed-user");

      const body = JSON.parse(requests[0]!.init.body);
      assert.deepEqual(body.expires_after, { anchor: "created_at", seconds: 600 });
      assert.equal(body.session.type, "realtime");
      assert.match(body.session.instructions, /You are Riff/);
    } finally {
      globalThis.fetch = original;
    }
  });

  it("reuses a client secret until it is close to expiring", async () => {
    let mints = 0;
    const credentials = clientSecretCredentials(async () => {
      mints += 1;
      return { value: `ek_${mints}`, expiresAt: Math.floor(Date.now() / 1000) + 600 };
    });

    assert.equal((await credentials.get()).token, "ek_1");
    assert.equal((await credentials.get()).token, "ek_1");
    assert.equal(mints, 1);
  });

  it("mints a fresh secret once the cached one has expired", async () => {
    let mints = 0;
    const credentials = clientSecretCredentials(async () => {
      mints += 1;
      return { value: `ek_${mints}`, expiresAt: Math.floor(Date.now() / 1000) - 1 };
    });

    await credentials.get();
    await credentials.get();
    assert.equal(mints, 2);
  });
});

describe("endpoint construction", () => {
  it("preserves query parameters a custom endpoint already carries", () => {
    const azure = "wss://acme.openai.azure.com/openai/realtime?api-version=2026-01-01&deployment=riff";
    const built = new URL(endpointWithModel(azure, "gpt-realtime-2.1"));

    assert.equal(built.searchParams.get("api-version"), "2026-01-01");
    assert.equal(built.searchParams.get("deployment"), "riff");
    assert.equal(built.searchParams.get("model"), "gpt-realtime-2.1");
  });

  it("replaces a model already present rather than adding a second one", () => {
    const built = new URL(endpointWithModel("wss://api.openai.com/v1/realtime?model=old", "new"));
    assert.deepEqual(built.searchParams.getAll("model"), ["new"]);
  });
});

describe("connection teardown", () => {
  it("fails immediately when the signal is already aborted", async () => {
    const provider = new OpenAIRealtimeProvider({
      credentials: apiKeyCredentials("sk-test-key"),
      createWebSocket: (url, protocols) => new FakeWebSocket(url, protocols),
    });

    await assert.rejects(
      provider.connect({
        instructions: "",
        tools: [],
        session: bundle.session,
        signal: AbortSignal.abort(),
      }),
      /aborted/,
    );
  });
});
