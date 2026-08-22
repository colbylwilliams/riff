import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { mapServerEvent } from "../src/events.ts";
import { decodeBase64, encodeBase64 } from "../src/transport.ts";

const map = (raw: Record<string, unknown>) => mapServerEvent(raw, decodeBase64).events;

describe("OpenAI event mapping", () => {
  it("reports the session id and model from session.created", () => {
    const mapped = mapServerEvent(
      { type: "session.created", session: { id: "sess_1", model: "gpt-realtime-2.1" } },
      decodeBase64,
    );
    assert.equal(mapped.sessionId, "sess_1");
    assert.equal(mapped.model, "gpt-realtime-2.1");
    assert.deepEqual(mapped.events, [{ type: "connected", sessionId: "sess_1", model: "gpt-realtime-2.1" }]);
  });

  it("turns a finished transcript into the one thing the prompt body may quote", () => {
    assert.deepEqual(
      map({
        type: "conversation.item.input_audio_transcription.completed",
        item_id: "item_1",
        transcript: "the export button is broken",
      }),
      [{ type: "transcript.completed", itemId: "item_1", text: "the export button is broken" }],
    );
  });

  it("understands the GA audio event names", () => {
    const events = map({ type: "response.output_audio.delta", response_id: "r1", delta: encodeBase64(new Uint8Array([7, 8])) });
    assert.equal(events[0]?.type, "response.audio");
    assert.deepEqual([...(events[0] as { audio: Uint8Array }).audio], [7, 8]);
  });

  it("still understands the beta names, which older snapshots emit", () => {
    const events = map({ type: "response.audio.delta", response_id: "r1", delta: encodeBase64(new Uint8Array([1])) });
    assert.equal(events[0]?.type, "response.audio");

    const transcript = map({ type: "response.audio_transcript.delta", response_id: "r1", delta: "hi" });
    assert.deepEqual(transcript, [{ type: "response.text.delta", responseId: "r1", delta: "hi" }]);
  });

  it("reads every function call of a turn out of response.done", () => {
    const events = map({
      type: "response.done",
      response: {
        id: "r1",
        status: "completed",
        output: [
          { type: "function_call", call_id: "call_a", name: "draft_update", arguments: '{"operations":[]}' },
          { type: "function_call", call_id: "call_b", name: "read_draft", arguments: "{}" },
          { type: "message", role: "assistant" },
        ],
        usage: { input_tokens: 10, output_tokens: 4, input_token_details: { audio_tokens: 6, cached_tokens: 2 } },
      },
    });

    assert.deepEqual(events[0], {
      type: "tool.calls",
      calls: [
        { callId: "call_a", name: "draft_update", argumentsJson: '{"operations":[]}' },
        { callId: "call_b", name: "read_draft", argumentsJson: "{}" },
      ],
    });
    assert.deepEqual(events[1], {
      type: "response.done",
      responseId: "r1",
      usage: { inputTokens: 10, outputTokens: 4, inputAudioTokens: 6, cachedTokens: 2 },
    });
  });

  it("surfaces a failed response as an error the session can act on", () => {
    const events = map({
      type: "response.done",
      response: {
        id: "r1",
        status: "failed",
        status_details: { error: { code: "server_error", message: "upstream failure" } },
      },
    });
    assert.deepEqual(events[0], {
      type: "error",
      error: { code: "server_error", message: "upstream failure", retryable: true },
    });
  });

  it("treats a rejected configuration as not worth retrying", () => {
    const events = map({
      type: "error",
      error: { type: "invalid_request_error", code: "invalid_request_error", message: "unknown parameter" },
    });
    assert.equal((events[0] as { error: { retryable: boolean } }).error.retryable, false);
  });

  it("derives a rough confidence from transcription logprobs", () => {
    const events = map({
      type: "conversation.item.input_audio_transcription.completed",
      item_id: "item_1",
      transcript: "flakeguard",
      logprobs: [{ logprob: -0.1 }, { logprob: -0.3 }],
    });
    const confidence = (events[0] as { confidence?: number }).confidence;
    assert.ok(confidence !== undefined && confidence > 0.7 && confidence < 1);
  });

  it("ignores events it has no use for", () => {
    assert.deepEqual(map({ type: "response.content_part.added" }), []);
  });
});

describe("base64", () => {
  it("round trips binary audio", () => {
    const bytes = new Uint8Array(Array.from({ length: 257 }, (_, i) => i % 256));
    assert.deepEqual([...decodeBase64(encodeBase64(bytes))], [...bytes]);
  });
});
