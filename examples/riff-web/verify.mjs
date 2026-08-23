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
import {
  DEMO_SCRIPT,
  spokenReference,
  substituteReferences,
  substituteSpokenReference,
} from "./public/demo-script.js";
import { ScriptedProvider } from "./public/scripted-provider.js";
import { observeProvider } from "./public/observe-provider.js";

const bundlePath = new URL("../../core/dist/riff-agent.bundle.json", import.meta.url);
const bundle = loadBundle(JSON.parse(readFileSync(bundlePath, "utf8")));

const host = new DemoHost();
const store = new MemoryStore({ motifs: DEMO_MOTIFS });
const toolResults = [];
const events = [];
let lastReferenceId = null;
let lastReferenceSpoken = null;

const scripted = new ScriptedProvider({
  script: DEMO_SCRIPT,
  speed: 60,
  prepareArgs: (_name, args) => substituteReferences(args, lastReferenceId),
  prepareText: (text) => substituteSpokenReference(text, lastReferenceSpoken),
});

const provider = observeProvider(scripted, {
  onToolResult: (entry) => {
    toolResults.push(entry);
    if (entry.name !== "resolve_reference") return;
    const found = entry.result?.candidates?.[0];
    lastReferenceId = found?.reference_id ?? null;
    lastReferenceSpoken = found ? spokenReference(found) : null;
  },
});

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

// The agent names what the host actually found rather than an id written into the script.
const spoken = events.findLast((event) => event.type === "agent.transcript" && event.final);
assert.equal(spoken?.text, "that's 412, chunked uploads.");

assert.equal(artifact.rendered, readmePrompt(), "the rendered prompt drifted from the README");

/**
 * The prompt the README promises, read from the README.
 *
 * Comparing against a copy pasted into this file would pass while the two drifted apart, which is
 * the one failure this check exists to catch. The block is hard-wrapped for reading, so continuation
 * lines are folded back into the single lines the renderer actually produces.
 */
function readmePrompt() {
  const readme = readFileSync(new URL("../../README.md", import.meta.url), "utf8");
  const block = /^What gets submitted:\s*```markdown\n([\s\S]*?)```/m.exec(readme);
  assert.ok(block, "could not find the submitted-prompt block in the README");

  return block[1]
    .trim()
    .split("\n\n")
    .map((paragraph) =>
      paragraph
        .split("\n")
        .reduce((lines, line) => {
          // A heading, a list item, or a bold label starts a line; anything else continues one.
          const starts = /^(#|-\s|\*\*)/.test(line) || lines.length === 0;
          if (starts) lines.push(line.trim());
          else lines[lines.length - 1] += ` ${line.trim()}`;
          return lines;
        }, [])
        .join("\n"),
    )
    .join("\n\n");
}

console.log(artifact.rendered);
console.log(
  `\nok — ${artifact.lines.length} lines, ${rejections.length} rejected, fidelity ${Math.round(
    artifact.provenance.fidelity * 100,
  )}%, ${artifact.provenance.utteranceCount} utterances`,
);
