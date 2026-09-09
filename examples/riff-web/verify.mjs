/**
 * Runs the scripted demo headlessly and checks it still produces the main README's prompt.
 *
 * The page and this file drive the same script through the same engine, so this is the fast way to
 * find out that a change to the agent bundle, the grounding config, or the render profile has
 * quietly broken the demo — without opening a browser or spending a token.
 *
 *   node examples/riff-web/verify.mjs
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { NullHost, RiffSession, loadBundle } from "@riff/core";

import { DemoHost } from "./public/demo-host.js";
import { DEMO_SCRIPT, substituteReferences } from "./public/demo-script.js";
import { ProxyHost } from "./public/proxy-host.js";
import { createLiveContext, createScriptedContext } from "./public/session-context.js";
import { ScriptedProvider } from "./public/scripted-provider.js";
import { observeProvider } from "./public/observe-provider.js";

const bundlePath = new URL("../../core/dist/riff-agent.bundle.json", import.meta.url);
const bundle = loadBundle(JSON.parse(readFileSync(bundlePath, "utf8")));

const { host, store } = createScriptedContext();
assert.ok(host instanceof DemoHost);
assert.equal((await store.listMotifs()).length, 1);

const unconfigured = createLiveContext();
assert.ok(unconfigured.host instanceof NullHost);
assert.deepEqual(await unconfigured.store.listMotifs(), []);
assert.deepEqual(
  await unconfigured.host.resolveReference({ phrase: "the PR I just opened" }),
  { candidates: [] },
);
assert.deepEqual(await unconfigured.host.recallPrompts({ query: "offline drafts" }), { prompts: [] });

const configured = createLiveContext({ githubAvailable: true });
assert.ok(configured.host instanceof ProxyHost);
assert.deepEqual(await configured.store.listMotifs(), []);

const toolResults = [];
const events = [];
let lastReferenceId = null;

const scripted = new ScriptedProvider({
  script: DEMO_SCRIPT,
  speed: 60,
  prepareArgs: (_name, args) => substituteReferences(args, lastReferenceId),
});

const provider = observeProvider(scripted, {
  onToolResult: (entry) => {
    toolResults.push(entry);
    if (entry.name !== "resolve_reference") return;
    const found = entry.result?.candidates?.[0];
    lastReferenceId = found?.reference_id ?? null;
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
for (const invented of ["implement", "persistence", "synchronization"]) {
  assert.ok(
    rejections[0].unmatchedTokens.includes(invented),
    `expected the rejection to name "${invented}"`,
  );
}

assert.equal(artifact.title.text, "Make drafts work offline");
assert.equal(artifact.provenance.fidelity, 1, "every line should be provably the speaker's");
assert.deepEqual(
  artifact.context.map(({ kind, identifier, resolvedFrom }) => ({ kind, identifier, resolvedFrom })),
  [
    { kind: "message", identifier: "Slack #feedback", resolvedFrom: "the Slack thread from Monday" },
    { kind: "document", identifier: "session-local-drafts", resolvedFrom: "the local-drafts session" },
    { kind: "pull_request", identifier: "acme/web#412", resolvedFrom: "the PR I just opened" },
  ],
  "the thread, selected session, and PR should be attached to their spoken references",
);
assert.equal(artifact.lines.filter((line) => line.grounding.kind === "motif").length, 1);
assert.equal(host.submitted.length, 1, "the host should have received the artifact exactly once");
assert.equal(toolResults.find((entry) => entry.name === "submit_prompt").result.destination, "demo");

const recalls = toolResults.filter((entry) => entry.name === "recall_prompts");
assert.equal(recalls.length, 1, "the script should recall earlier attempts before asking which one");
assert.deepEqual(recalls[0].result.prompts.map((prompt) => prompt.title), ["Local drafts", "Sync retries"]);
assert.ok(recalls[0].result.prompts.every((prompt) => prompt.excerpt && prompt.outcome));
assert.doesNotMatch(
  artifact.lines.map((line) => line.text).join("\n"),
  /\b(IndexedDB|backoff)\b/i,
  "recalled implementation details must not become the speaker's words",
);
assert.deepEqual(
  events.filter((event) => event.type === "utterance").map((event) => event.utterance.text),
  DEMO_SCRIPT.filter((step) => step.user).map((step) => step.user),
  "only the scripted speaker's words may enter the ledger",
);

assert.deepEqual(
  await host.resolveReference({ phrase: "an unrelated Slack thread", kind: "message" }),
  { candidates: [] },
);
assert.deepEqual(
  await host.resolveReference({ phrase: "the PR I just opened", kind: "message" }),
  { candidates: [] },
);
assert.deepEqual(await host.recallPrompts({ query: "billing" }), { prompts: [] });

// Two prompts were open at once. A subject change starts a second take rather than overwriting the
// first, switching back leaves both intact, and sending one does not disturb the other.
const takes = session.takes();
assert.equal(takes.length, 2, "the subject change should have opened a second take");

const [offline, onboarding] = takes;
assert.equal(artifact.takeId, offline.id, "the offline-drafts take is the one that was sent");
assert.equal(offline.status, "submitted");
assert.equal(onboarding.status, "parked", "the take they set aside is still waiting for them");
assert.equal(onboarding.label, "onboarding doc");
assert.deepEqual(
  onboarding.lines().map((line) => line.text),
  ["the onboarding doc still says node sixteen"],
  "the parked take should hold its own line and nothing from the one that was sent",
);
assert.ok(
  !artifact.lines.some((line) => line.text.includes("onboarding")),
  "the tangent should never have reached the prompt that was sent",
);
// Naming a take is how an embedder shows a prompt that is not the one being spoken into.
assert.equal(session.artifact(onboarding.id)?.takeId, onboarding.id);

assert.deepEqual(
  events.filter((event) => event.type === "agent.transcript" && event.final).map((event) => event.text),
  ["The local-drafts session or the sync-retry one?"],
  "only the ambiguity needs speech; clean lookups and take changes stay silent",
);

const readme = readFileSync(new URL("../../README.md", import.meta.url), "utf8");
assert.equal(artifact.rendered, readmePrompt(readme), "the rendered prompt drifted from the main README");

const dialogue = [...readme.matchAll(/^> \*\*(You|Riff)\*\* — (.+)$/gm)]
  .filter(([, , text]) => !text.startsWith("*("))
  .map(([, speaker, text]) => ({ speaker, text }));
assert.deepEqual(
  dialogue,
  DEMO_SCRIPT.flatMap((step) => {
    if (step.user) return [{ speaker: "You", text: step.user }];
    if (step.agent) return [{ speaker: "Riff", text: step.agent }];
    return [];
  }),
  "the main README should show the conversation the demo actually runs",
);

/**
 * The prompt the main README promises, read from that README.
 *
 * Comparing against a copy pasted into this file would pass while the two drifted apart, which is
 * the one failure this check exists to catch. Continuation lines are folded back into the single
 * lines the renderer actually produces.
 */
function readmePrompt(readme) {
  const block = /^What gets submitted:\s*```markdown\n([\s\S]*?)```/m.exec(readme);
  assert.ok(block, "could not find the submitted-prompt block in the main README");

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
