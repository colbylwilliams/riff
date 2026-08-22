/**
 * Runs the scripted demo headlessly and checks it still produces the README's prompt.
 *
 * The page and this file drive the same script through the same engine, so this is the fast way to
 * find out that a change to the agent bundle, the grounding config, or the render profile has
 * quietly broken the demo — without opening a browser or spending a token.
 *
 *   node examples/riff-web/verify.mjs
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { MemoryStore, RiffSession, loadBundle } from "@riff/core";

import { DEMO_MOTIFS, DemoHost } from "./public/demo-host.js";
import { DEMO_SCRIPT } from "./public/demo-script.js";
import { ScriptedProvider } from "./public/scripted-provider.js";
import { observeProvider } from "./public/observe-provider.js";

const bundlePath = new URL("../../core/dist/riff-agent.bundle.json", import.meta.url);
const bundle = loadBundle(JSON.parse(readFileSync(bundlePath, "utf8")));

const host = new DemoHost();
const store = new MemoryStore({ motifs: DEMO_MOTIFS });
const toolResults = [];
const events = [];

const scripted = new ScriptedProvider({ script: DEMO_SCRIPT, speed: 60 });
const provider = observeProvider(scripted, { onToolResult: (entry) => toolResults.push(entry) });

const session = new RiffSession({ bundle, provider, host, store });
session.on((event) => events.push(event));

await session.start();
await scripted.run();
await session.stop("verified");

const submitted = events.find((event) => event.type === "submitted");
assert.ok(submitted, "the script never reached a submitted prompt");

const artifact = submitted.artifact;
const rejections = toolResults.flatMap((entry) => entry.result?.rejected ?? []);

assert.equal(rejections.length, 1, "expected exactly one rejected line");
assert.match(rejections[0].reason, /they did not say/);
for (const invented of ["csv", "functionality", "silently"]) {
  assert.ok(
    rejections[0].unmatchedTokens.includes(invented),
    `expected the rejection to name "${invented}"`,
  );
}

assert.equal(artifact.title.text, "Fix the export button");
assert.equal(artifact.provenance.fidelity, 1, "every line should be provably the speaker's");
assert.equal(artifact.context.length, 1);
assert.equal(artifact.context[0].identifier, "acme/web#412");
assert.equal(artifact.context[0].resolvedFrom, "the PR I just opened");
assert.equal(artifact.lines.filter((line) => line.grounding.kind === "motif").length, 1);
assert.equal(host.submitted.length, 1, "the host should have received the artifact exactly once");

const expected = [
  "# Fix the export button",
  "",
  "the export button on the dashboard does nothing if you've got more than about a thousand rows. just spins. it's related to the PR I just opened.",
  "",
  "**Constraints**",
  "- don't touch the generated files",
  "",
  "**Done when**",
  "- I should be able to export like fifty thousand rows without it falling over",
  "",
  "**Context**",
  '- acme/web#412 "Chunked uploads" (open) by you — https://github.com/acme/web/pull/412 — referred to as "the PR I just opened"',
].join("\n");

assert.equal(artifact.rendered, expected, "the rendered prompt drifted from the README");

console.log(artifact.rendered);
console.log(
  `\nok — ${artifact.lines.length} lines, ${rejections.length} rejected, fidelity ${Math.round(
    artifact.provenance.fidelity * 100,
  )}%, ${artifact.provenance.utteranceCount} utterances`,
);
