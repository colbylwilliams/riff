import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

import type { AgentBundle } from "@riff/core";
import { loadBundle } from "@riff/core";
import { buildOpenAISession, buildVocabularyPatch, biasingStyleFor } from "../src/session-config.ts";

const root = new URL("../../../", import.meta.url).pathname;
const bundle: AgentBundle = loadBundle(
  JSON.parse(readFileSync(join(root, "core/dist/riff-agent.bundle.json"), "utf8")),
);

const build = (overrides: Partial<AgentBundle["session"]> = {}, vocabulary: string[] = []) =>
  buildOpenAISession({
    session: { ...bundle.session, ...overrides },
    instructions: bundle.instructions,
    tools: bundle.tools,
    vocabulary,
  }) as any;

describe("OpenAI session configuration", () => {
  it("uses the GA session shape rather than the beta one", () => {
    const session = build();
    assert.equal(session.type, "realtime");
    assert.deepEqual(session.output_modalities, ["audio"]);
    assert.equal(session.modalities, undefined);
  });

  it("sends audio formats as objects, which is what GA expects", () => {
    const session = build();
    assert.deepEqual(session.audio.input.format, { type: "audio/pcm", rate: 24000 });
    assert.deepEqual(session.audio.output.format, { type: "audio/pcm", rate: 24000 });
  });

  it("waits for a real end of turn instead of a pause", () => {
    const session = build();
    assert.equal(session.audio.input.turn_detection.type, "semantic_vad");
    assert.equal(session.audio.input.turn_detection.eagerness, "low");
    assert.equal(session.audio.input.turn_detection.interrupt_response, true);
  });

  it("falls back to server VAD when semantic endpointing is turned off", () => {
    const session = build({
      turnDetection: { ...bundle.session.turnDetection, mode: "vad" },
    });
    assert.equal(session.audio.input.turn_detection.type, "server_vad");
    assert.equal(session.audio.input.turn_detection.silence_duration_ms, 900);
  });

  it("disables turn detection entirely for push to talk", () => {
    const session = build({ turnDetection: { ...bundle.session.turnDetection, mode: "manual" } });
    assert.equal(session.audio.input.turn_detection, null);
  });

  it("biases transcription the way the chosen model accepts hints", () => {
    const promptStyle = build({}, ["Flakeguard", "GitHub"]);
    assert.match(promptStyle.audio.input.transcription.prompt, /Flakeguard, GitHub\.$/);
    assert.equal(promptStyle.audio.input.transcription.keywords, undefined);

    const keywordStyle = build(
      { transcription: { ...bundle.session.transcription, preferred: "gpt-live-transcribe" } },
      ["Flakeguard", "GitHub"],
    );
    assert.deepEqual(keywordStyle.audio.input.transcription.keywords, ["Flakeguard", "GitHub"]);
    assert.equal(keywordStyle.audio.input.transcription.prompt, undefined);

    const noStyle = build(
      { transcription: { ...bundle.session.transcription, preferred: "gpt-realtime-whisper" } },
      ["Flakeguard"],
    );
    assert.equal(noStyle.audio.input.transcription.prompt, undefined);
    assert.equal(noStyle.audio.input.transcription.keywords, undefined);
  });

  it("caps the vocabulary at the configured limit", () => {
    const many = Array.from({ length: 500 }, (_, i) => `term${i}`);
    const session = build({}, many);
    const listed = session.audio.input.transcription.prompt.split(": ")[1].split(", ");
    assert.equal(listed.length, bundle.session.transcription.biasing!.maxKeywords);
  });

  it("only sends reasoning settings to models that accept them", () => {
    const reasoning = buildOpenAISession({
      session: bundle.session,
      instructions: "",
      tools: [],
      model: "gpt-realtime-2.1",
    }) as any;
    assert.deepEqual(reasoning.reasoning, { effort: "low" });
    assert.equal(reasoning.parallel_tool_calls, true);

    const nonReasoning = buildOpenAISession({
      session: bundle.session,
      instructions: "",
      tools: [],
      model: "gpt-realtime-mini",
    }) as any;
    assert.equal(nonReasoning.reasoning, undefined);
    assert.equal(nonReasoning.parallel_tool_calls, undefined);
  });

  it("declares tools in the flat realtime shape", () => {
    const session = build();
    const tool = session.tools.find((candidate: any) => candidate.name === "draft_update");
    assert.equal(tool.type, "function");
    assert.equal(typeof tool.description, "string");
    assert.equal(tool.parameters.type, "object");
    assert.equal(tool.function, undefined);
  });

  it("keeps the instructions from being truncated away in a long session", () => {
    const session = build();
    assert.equal(session.truncation.type, "retention_ratio");
    assert.equal(session.truncation.token_limits.post_instructions, 24000);
  });

  it("patches only transcription when the vocabulary changes mid conversation", () => {
    const patch = buildVocabularyPatch(bundle.session, ["Flakeguard"]) as any;
    assert.equal(patch.type, "realtime");
    assert.match(patch.audio.input.transcription.prompt, /Flakeguard/);
    assert.equal(patch.instructions, undefined);
    assert.equal(patch.tools, undefined);
  });

  it("knows how each transcription model takes hints", () => {
    assert.equal(biasingStyleFor("gpt-4o-transcribe"), "prompt");
    assert.equal(biasingStyleFor("gpt-live-transcribe"), "keywords");
    assert.equal(biasingStyleFor("gpt-4o-transcribe-diarize"), "none");
  });
});
