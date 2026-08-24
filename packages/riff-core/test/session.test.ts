import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { beforeEach, describe, it } from "node:test";

import { loadBundle } from "../src/bundle.ts";
import { MemoryStore, NullHost } from "../src/host.ts";
import { RiffSession } from "../src/session.ts";
import type { RiffEvent } from "../src/session.ts";
import type { AgentBundle, ContextItem, PromptArtifact } from "../src/types.ts";
import { FakeProvider, settle } from "./fake-provider.ts";

const root = new URL("../../../", import.meta.url).pathname;
const bundle: AgentBundle = loadBundle(
  JSON.parse(readFileSync(join(root, "core/dist/riff-agent.bundle.json"), "utf8")),
);

class RecordingHost extends NullHost {
  candidates: ContextItem[] = [];
  submitted: PromptArtifact[] = [];
  /** Vocabulary this world knows about, which is what corroborates a spelling correction. */
  knownTerms: string[] = [];

  override async resolveReference() {
    return { candidates: this.candidates };
  }

  override async lookupTerm(request: { heard: string }) {
    const match = this.knownTerms.find((term) => term.toLowerCase() === request.heard.toLowerCase());
    return { matches: match ? [{ canonical: match, kind: "product", confidence: 1 }] : [] };
  }

  override async submitPrompt(artifact: PromptArtifact) {
    this.submitted.push(artifact);
    return { submitted: true, promptId: "p1", destination: "test", url: "https://example.test/p1" };
  }
}

describe("RiffSession", () => {
  let provider: FakeProvider;
  let host: RecordingHost;
  let store: MemoryStore;
  let session: RiffSession;
  let events: RiffEvent[];

  beforeEach(async () => {
    provider = new FakeProvider();
    host = new RecordingHost();
    store = new MemoryStore();
    session = new RiffSession({ bundle, provider, host, store });
    events = [];
    session.on((event) => events.push(event));
    await session.start();
    await settle();
  });

  async function draftOneLine(text: string): Promise<string> {
    provider.say(text);
    await settle();
    provider.callTool("draft_update", {
      operations: [{ op: "upsert_line", section: "intent", text }],
    });
    await settle();
    return session.book.activeId!;
  }

  it("hands the model the composed instructions, the tools, and the vocabulary", () => {
    assert.ok(provider.request);
    assert.match(provider.request.instructions, /You are Riff/);
    assert.deepEqual(
      provider.request.tools.map((tool) => tool.name).sort(),
      bundle.tools.map((tool) => tool.name).sort(),
    );
    assert.ok(provider.request.vocabulary && provider.request.vocabulary.includes("GitHub"));
  });

  it("records what was said and lets it be drafted verbatim", async () => {
    provider.say("the export button does nothing when you have more than a thousand rows");
    await settle();

    const callId = provider.callTool("draft_update", {
      operations: [
        { op: "set_title", text: "Fix the export button" },
        {
          op: "upsert_line",
          section: "intent",
          text: "the export button does nothing when you have more than a thousand rows",
        },
      ],
    });
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.rejected.length, 0);
    assert.equal(result.fidelity, 1);
    assert.equal(result.draft.ready, true);

    const artifact = session.artifact();
    assert.ok(artifact);
    assert.equal(artifact.provenance.fidelity, 1);
    assert.equal(artifact.lines[0]?.grounding.kind, "verbatim");
    assert.match(artifact.rendered, /# Fix the export button/);
  });

  it("rejects a paraphrase and tells the model which words it invented", async () => {
    provider.say("the export button does nothing when you have a ton of rows");
    await settle();

    const callId = provider.callTool("draft_update", {
      operations: [
        {
          op: "upsert_line",
          section: "intent",
          text: "The CSV export functionality fails silently for large result sets",
        },
      ],
    });
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.accepted.length, 0);
    assert.equal(result.rejected.length, 1);
    assert.match(result.rejected[0].reason, /they did not say/);
    assert.ok(result.rejected[0].unmatchedTokens.includes("csv"));
    assert.equal(session.artifact()?.lines.length, 0);
  });

  it("keeps a line that only drops filler and false starts", async () => {
    provider.say("um so like we should maybe the retry logic needs a backoff you know");
    await settle();

    const callId = provider.callTool("draft_update", {
      operations: [{ op: "upsert_line", section: "intent", text: "the retry logic needs a backoff" }],
    });
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.rejected.length, 0);
    assert.equal(result.accepted[0].kind, "trimmed");
  });

  it("replaces a superseded line when they change their mind", async () => {
    provider.say("roll it back to the previous release");
    await settle();
    const first = provider.callTool("draft_update", {
      operations: [{ op: "upsert_line", section: "intent", text: "roll it back to the previous release" }],
    });
    await settle();
    const lineId = provider.resultFor(first).accepted[0].lineId;

    provider.say("no wait roll it back two releases");
    await settle();
    const second = provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "roll it back two releases", supersedes: [lineId] },
      ],
    });
    await settle();

    const draft = provider.resultFor(second).draft;
    assert.equal(draft.sections.intent.length, 1);
    assert.equal(draft.sections.intent[0].text, "roll it back two releases");
  });

  it("attaches a resolved reference without touching what they said", async () => {
    host.candidates = [
      {
        referenceId: "pr-412",
        kind: "pull_request",
        title: "Chunked uploads",
        identifier: "acme/web#412",
        url: "https://github.com/acme/web/pull/412",
        state: "open",
      },
    ];

    provider.say("take another look at the PR I just opened");
    await settle();

    const resolve = provider.callTool("resolve_reference", {
      phrase: "the PR I just opened",
      kind: "pull_request",
      recency: "latest",
    });
    await settle();
    assert.equal(provider.resultFor(resolve).candidates[0].reference_id, "pr-412");

    const update = provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "take another look at the PR I just opened" },
        { op: "attach_context", reference_id: "pr-412" },
      ],
    });
    await settle();

    assert.equal(provider.resultFor(update).rejected.length, 0);
    const artifact = session.artifact();
    assert.equal(artifact?.context[0]?.identifier, "acme/web#412");
    assert.equal(artifact?.context[0]?.resolvedFrom, "the PR I just opened");
    assert.match(artifact!.rendered, /acme\/web#412/);
    assert.match(artifact!.rendered, /take another look at the PR I just opened/);
  });

  it("refuses to attach a reference that was never resolved", async () => {
    provider.say("look at the migration");
    await settle();

    const callId = provider.callTool("draft_update", {
      operations: [{ op: "attach_context", reference_id: "pr-999" }],
    });
    await settle();

    assert.match(provider.resultFor(callId).rejected[0].reason, /unknown reference_id/);
  });

  it("saves a standing instruction in their words and reuses it", async () => {
    provider.say("and never touch the generated files");
    await settle();

    const save = provider.callTool("motifs", {
      action: "save",
      text: "never touch the generated files",
      applies_when: "any code change",
    });
    await settle();
    const motifId = provider.resultFor(save).motif_id;
    assert.ok(motifId);

    const attach = provider.callTool("motifs", { action: "attach", motif_id: motifId });
    await settle();
    assert.equal(provider.resultFor(attach).attached, true);

    const constraint = session.artifact()?.lines.find((line) => line.section === "constraint");
    assert.equal(constraint?.text, "never touch the generated files");
    assert.equal(constraint?.grounding.kind, "motif");
    assert.equal((await store.listMotifs()).length, 1);
  });

  it("will not save a motif the agent made up", async () => {
    provider.say("keep the diffs small");
    await settle();

    const callId = provider.callTool("motifs", {
      action: "save",
      text: "adhere to conventional commit standards",
    });
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.saved, false);
    assert.match(result.reason, /their wording/);
  });

  it("parks a take and starts a new one when the subject changes", async () => {
    provider.say("the export button is broken");
    await settle();
    provider.callTool("draft_update", {
      operations: [{ op: "upsert_line", section: "intent", text: "the export button is broken" }],
    });
    await settle();
    const firstTake = session.book.activeId!;

    const created = provider.callTool("takes", { action: "new", label: "avatars" });
    await settle();
    const secondTake = provider.resultFor(created).take_id;

    assert.notEqual(firstTake, secondTake);
    assert.equal(session.artifact()?.lines.length, 0);
    // `60-corrections` tells the speaker they can come back to the parked one, so starting a take
    // has to leave the outgoing one parked rather than still marked as being drafted.
    assert.equal(session.book.get(firstTake)?.status, "parked");
    assert.equal(session.book.get(secondTake)?.status, "drafting");

    // Both prompts are open at once, and the parked one is readable without becoming active.
    const parked = session.artifact(firstTake);
    assert.equal(parked?.takeId, firstTake);
    assert.equal(parked?.lines.length, 1);
    assert.equal(session.artifact()?.takeId, secondTake);

    const switched = provider.callTool("takes", { action: "switch", take_id: firstTake });
    await settle();
    assert.equal(provider.resultFor(switched).draft.sections.intent.length, 1);
    assert.equal(session.book.get(firstTake)?.status, "drafting");
    assert.equal(session.book.get(secondTake)?.status, "parked");
  });

  it("submits only what was captured, with provenance attached", async () => {
    provider.say("the export button does nothing past a thousand rows");
    await settle();
    provider.callTool("draft_update", {
      operations: [
        { op: "set_title", text: "Fix the export button" },
        { op: "upsert_line", section: "intent", text: "the export button does nothing past a thousand rows" },
      ],
    });
    await settle();

    const callId = provider.callTool("submit_prompt", {});
    await settle();

    assert.equal(provider.resultFor(callId).submitted, true);
    assert.equal(host.submitted.length, 1);

    const artifact = host.submitted[0]!;
    assert.equal(artifact.provenance.fidelity, 1);
    assert.equal(artifact.provenance.agentAuthoredTokens, 0);
    assert.equal(artifact.provenance.providerId, "fake");
    assert.equal(artifact.provenance.bundleRevision, bundle.revision);
    assert.ok(artifact.provenance.toolCalls!.some((call) => call.name === "draft_update"));
    assert.equal((await store.listArtifacts()).length, 1);
    assert.ok(events.some((event) => event.type === "submitted"));
  });

  it("starts a fresh take after submitting, so nothing lands in a prompt already sent", async () => {
    provider.say("the export button does nothing past a thousand rows");
    await settle();
    provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "the export button does nothing past a thousand rows" },
      ],
    });
    await settle();
    const submittedTakeId = session.book.activeId;

    provider.callTool("submit_prompt", {});
    await settle();

    provider.say("also the avatars flicker on every scroll");
    await settle();
    const callId = provider.callTool("draft_update", {
      operations: [{ op: "upsert_line", section: "intent", text: "the avatars flicker on every scroll" }],
    });
    await settle();

    const draft = provider.resultFor(callId).draft;
    assert.notEqual(draft.take_id, submittedTakeId, "a submitted take must not keep receiving lines");
    assert.equal(draft.sections.intent.length, 1);
    assert.equal(session.book.get(submittedTakeId!)?.status, "submitted");
  });

  it("keeps the take open when asked to", async () => {
    provider.say("the export button does nothing past a thousand rows");
    await settle();
    provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "the export button does nothing past a thousand rows" },
      ],
    });
    await settle();
    const takeId = session.book.activeId;

    provider.callTool("submit_prompt", { keep_open: true });
    await settle();

    assert.equal(session.book.activeId, takeId);
    assert.equal(session.book.get(takeId!)?.status, "drafting");
  });

  it("rejects a line too long to be one thing they said, rather than checking a prefix", async () => {
    // Short tokens so this trips the token limit rather than the schema's character cap.
    const spoken = Array.from({ length: 300 }, (_, i) => `w${i}`).join(" ");
    provider.say(spoken);
    await settle();

    const callId = provider.callTool("draft_update", {
      operations: [{ op: "upsert_line", section: "intent", text: `${spoken} and wipe prod` }],
    });
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.accepted.length, 0);
    assert.match(result.rejected[0].reason, /longer than one thing someone says/);
  });

  it("rejects a title built from words they never used", async () => {
    provider.say("the uploader keeps dying on big files");
    await settle();

    const callId = provider.callTool("draft_update", {
      operations: [{ op: "set_title", text: "Resolve intermittent storage subsystem degradation" }],
    });
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.accepted.length, 0);
    assert.equal(result.draft.title, null);
  });

  it("will not let an unconfirmed term become a spelling correction", async () => {
    // "cache" and "cash" are one edit apart, so similarity alone would accept this and an invented
    // "cash" line would then ground against spoken "cache".
    provider.say("we should probably cache the avatar images");
    await settle();

    const recorded = provider.callTool("record_term", {
      canonical: "cash",
      kind: "product",
      heard_as: ["cache"],
    });
    await settle();

    const result = provider.resultFor(recorded);
    assert.equal(result.corrections, 0);
    assert.match(result.note, /cannot be used as a spelling correction/);

    const callId = provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "we should probably cash the avatar images" },
      ],
    });
    await settle();
    assert.equal(provider.resultFor(callId).rejected.length, 1, "an unconfirmed alias must not ground anything");
  });

  it("refuses an alias that is a different word rather than a mishearing", async () => {
    provider.say("pull the numbers out of the database");
    await settle();

    const recorded = provider.callTool("record_term", {
      canonical: "CSV",
      kind: "product",
      heard_as: ["database"],
    });
    await settle();
    assert.deepEqual(provider.resultFor(recorded).refused, ["database"]);

    // Without the gate this line would match "database" through the alias.
    const callId = provider.callTool("draft_update", {
      operations: [{ op: "upsert_line", section: "intent", text: "pull the numbers out of the CSV" }],
    });
    await settle();
    assert.equal(provider.resultFor(callId).rejected.length, 1);
  });

  it("still accepts a genuine mishearing the world knows about", async () => {
    host.knownTerms = ["Flakeguard"];
    const callId = provider.callTool("record_term", {
      canonical: "Flakeguard",
      kind: "product",
      heard_as: ["flake guard", "flag guard"],
    });
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.corrections, 2);
    assert.equal(result.refused, undefined);
  });

  it("will not write into a take that was already submitted, even when named", async () => {
    provider.say("the export button does nothing past a thousand rows");
    await settle();
    provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "the export button does nothing past a thousand rows" },
      ],
    });
    await settle();
    const takeId = session.book.activeId!;

    provider.callTool("submit_prompt", {});
    await settle();

    provider.say("also the avatars flicker");
    await settle();
    const callId = provider.callTool("draft_update", {
      take_id: takeId,
      operations: [{ op: "upsert_line", section: "intent", text: "the avatars flicker" }],
    });
    await settle();

    assert.match(provider.resultFor(callId).error, /already submitted/);
    assert.equal(session.book.get(takeId)?.lines().length, 1);
  });

  it("leaves the take drafting when the host throws on submit", async () => {
    host.submitPrompt = async () => {
      throw new Error("the destination is unreachable");
    };

    provider.say("the export button does nothing past a thousand rows");
    await settle();
    provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "the export button does nothing past a thousand rows" },
      ],
    });
    await settle();
    const takeId = session.book.activeId!;

    const callId = provider.callTool("submit_prompt", {});
    await settle();

    assert.match(provider.resultFor(callId).error, /unreachable/);
    assert.equal(session.book.get(takeId)?.status, "drafting");
  });

  it("does not resurrect a parked take when a submission fails after the active take changed", async () => {
    // The host is held open so `takes new` in the same batch runs while the submission is in
    // flight, which is the ordering `Promise.all` produces for one turn's tool calls.
    let refuse: (error: Error) => void;
    host.submitPrompt = () =>
      new Promise((_resolve, reject) => {
        refuse = reject;
      });

    provider.say("the export button does nothing past a thousand rows");
    await settle();
    provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "the export button does nothing past a thousand rows" },
      ],
    });
    await settle();
    const firstTake = session.book.activeId!;

    const [submitCall, newCall] = provider.callTools([
      { name: "submit_prompt", args: {} },
      { name: "takes", args: { action: "new", label: "avatars" } },
    ]);
    await settle();

    const secondTake = provider.resultFor(newCall!).take_id;
    assert.equal(session.book.get(firstTake)?.status, "parked");

    refuse!(new Error("the destination is unreachable"));
    await settle();

    assert.match(provider.resultFor(submitCall!).error, /unreachable/);
    // A failed send has no business promoting a take the speaker has already moved on from.
    assert.equal(session.book.get(firstTake)?.status, "parked");
    assert.equal(session.book.get(secondTake)?.status, "drafting");
    assert.equal(
      session.takes().filter((take) => take.status === "drafting").length,
      1,
      "exactly one take is ever the one being spoken into",
    );
  });

  it("gives the model a timeout rather than hanging when a host never answers", async () => {
    host.resolveReference = () => new Promise(() => {});

    const callId = provider.callTool("resolve_reference", { phrase: "the PR I just opened" });
    await new Promise((resolve) => setTimeout(resolve, bundle.session.limits.toolTimeoutMs + 200));

    assert.match(provider.resultFor(callId).error, /did not answer within/);
    assert.equal(provider.calls.filter((call) => call.kind === "response").length, 1);
  });

  it("carries the negotiated model and session id into a submitted artifact", async () => {
    provider.say("the export button does nothing past a thousand rows");
    await settle();
    provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "the export button does nothing past a thousand rows" },
      ],
    });
    await settle();
    provider.callTool("submit_prompt", {});
    await settle();

    const artifact = host.submitted[0]!;
    assert.equal(artifact.provenance.model, "fake-realtime");
    assert.equal(artifact.provenance.sessionId, "sess_fake");
    assert.ok((artifact.provenance.durationMs ?? -1) >= 0);
  });

  it("will not resurrect a submitted take by parking or switching to it", async () => {
    const takeId = await draftOneLine("the export button does nothing past a thousand rows");
    provider.callTool("submit_prompt", {});
    await settle();

    const parked = provider.callTool("takes", { action: "park", take_id: takeId });
    await settle();
    assert.match(provider.resultFor(parked).error, /cannot be parked/);

    const switched = provider.callTool("takes", { action: "switch", take_id: takeId });
    await settle();
    assert.match(provider.resultFor(switched).error, /cannot be reopened/);
    assert.equal(session.book.get(takeId)?.status, "submitted");
  });

  it("will not attach a motif to a take that was already submitted", async () => {
    provider.say("and never touch the generated files");
    await settle();
    const saved = provider.callTool("motifs", { action: "save", text: "never touch the generated files" });
    await settle();
    const motifId = provider.resultFor(saved).motif_id;

    const takeId = await draftOneLine("the export button does nothing past a thousand rows");
    provider.callTool("submit_prompt", {});
    await settle();
    assert.equal(session.book.activeId, null);

    // Nothing is active, so this would land on the submitted take if the guard were missing.
    const attached = provider.callTool("motifs", { action: "attach", motif_id: motifId });
    await settle();

    // A fresh take is started instead, and the submitted one is untouched.
    assert.notEqual(session.book.activeId, takeId);
    assert.equal(session.book.get(takeId)?.lines().length, 1);
    assert.equal(provider.resultFor(attached).attached, true);
  });

  it("rejects a line id that does not name an existing line", async () => {
    provider.say("the export button does nothing past a thousand rows");
    await settle();

    const callId = provider.callTool("draft_update", {
      operations: [
        {
          op: "upsert_line",
          line_id: "t1-l1",
          section: "intent",
          text: "the export button does nothing past a thousand rows",
        },
      ],
    });
    await settle();

    assert.match(provider.resultFor(callId).rejected[0].reason, /omit line_id to add a new one/);
  });

  it("cancels the response when they start talking, not just when asked to", async () => {
    provider.emit({ type: "response.started", responseId: "r1" });
    provider.emit({ type: "response.audio", responseId: "r1", audio: new Uint8Array([1]) });
    assert.equal(session.state, "speaking");

    provider.emit({ type: "speech.started" });

    assert.ok(provider.calls.some((call) => call.kind === "cancel"), "buffered audio must be dropped");
    assert.ok(events.some((event) => event.type === "interrupted"));
    assert.equal(session.state, "listening");
  });

  it("does not report a send as failed because saving a copy failed", async () => {
    store.saveArtifact = async () => {
      throw new Error("disk is full");
    };

    await draftOneLine("the export button does nothing past a thousand rows");
    const callId = provider.callTool("submit_prompt", {});
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.submitted, true, "the destination already has it; a retry would send twice");
    assert.match(result.warning, /saving a copy failed/);
    assert.equal(host.submitted.length, 1);
  });

  it("submits with the render profile the session was configured with", async () => {
    const styled = new FakeProvider();
    const session = new RiffSession({ bundle, provider: styled, host, store, renderProfile: "structured" });
    await session.start();
    await settle();

    styled.say("the uploader keeps dying on big files");
    styled.say("it only happens over about fifty megs");
    await settle();
    styled.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "the uploader keeps dying on big files" },
        { op: "upsert_line", section: "detail", text: "it only happens over about fifty megs" },
      ],
    });
    await settle();
    styled.callTool("submit_prompt", {});
    await settle();

    const artifact = host.submitted.at(-1)!;
    assert.match(artifact.rendered, /## Details/, "submitted text must match the configured profile");
  });

  it("never reissues a motif id that a retired motif still holds", async () => {
    const seeded = new MemoryStore({
      motifs: [
        { id: "m1", text: "never touch the generated files", scope: "user", createdAt: "t0" },
        { id: "m2", text: "keep the diffs small", scope: "user", createdAt: "t0", retiredAt: "t1" },
      ],
    });
    const fresh = new FakeProvider();
    const reopened = new RiffSession({ bundle, provider: fresh, host, store: seeded });
    await reopened.start();
    await settle();

    fresh.say("and always run the linter before you're done");
    await settle();
    const callId = fresh.callTool("motifs", {
      action: "save",
      text: "always run the linter before you're done",
    });
    await settle();

    const saved = fresh.resultFor(callId);
    assert.equal(saved.saved, true);
    assert.notEqual(saved.motif_id, "m2", "m2 is retired but still exists; reusing it would overwrite it");

    const listed = fresh.callTool("motifs", { action: "list" });
    await settle();
    const ids = fresh.resultFor(listed).motifs.map((m: { motif_id: string }) => m.motif_id);
    assert.ok(!ids.includes("m2"), "a retired motif is not offered");
    assert.ok(ids.includes(saved.motif_id));
  });

  it("refuses to submit a take with nothing in it", async () => {
    const callId = provider.callTool("submit_prompt", {});
    await settle();

    const result = provider.resultFor(callId);
    assert.equal(result.submitted, false);
    assert.match(result.reason, /Ask them what they want done/);
    assert.equal(host.submitted.length, 0);
  });

  it("returns a usable error rather than throwing when the model sends bad arguments", async () => {
    const callId = provider.callTool("draft_update", { operations: [{ op: "upsert_line", section: "nonsense" }] });
    await settle();

    const result = provider.resultFor(callId);
    assert.ok(Array.isArray(result.details));
    assert.match(result.details.join(" "), /must be one of/);
  });

  it("teaches the transcriber a corrected term and pushes it to the provider", async () => {
    host.knownTerms = ["Flakeguard"];
    const callId = provider.callTool("record_term", {
      canonical: "Flakeguard",
      kind: "product",
      heard_as: ["flake guard", "flag guard"],
    });
    await settle();

    assert.equal(provider.resultFor(callId).recorded, "Flakeguard");
    const update = provider.calls.find((call) => call.kind === "session");
    assert.ok(update, "the provider should be told about the new vocabulary");
    assert.ok((update.payload as { vocabulary: string[] }).vocabulary.includes("Flakeguard"));

    provider.say("the flake guard wrapper is retrying too many times");
    await settle();
    const draft = provider.callTool("draft_update", {
      operations: [
        { op: "upsert_line", section: "intent", text: "the Flakeguard wrapper is retrying too many times" },
      ],
    });
    await settle();

    const result = provider.resultFor(draft);
    assert.equal(result.rejected.length, 0);
    assert.equal(result.accepted[0].kind, "corrected");
  });

  it("stops talking the moment they start", async () => {
    provider.emit({ type: "response.started", responseId: "r1" });
    provider.emit({ type: "response.audio", responseId: "r1", audio: new Uint8Array([1, 2, 3]) });
    assert.equal(session.state, "speaking");

    session.interrupt();

    assert.ok(provider.calls.some((call) => call.kind === "cancel"));
    assert.ok(events.some((event) => event.type === "interrupted"));
    assert.equal(session.state, "listening");
  });

  it("treats typed input as something they said", async () => {
    const utterance = session.sendText("the webhook retries forever when the endpoint is down");
    assert.equal(utterance.source, "typed");

    const callId = provider.callTool("draft_update", {
      operations: [
        {
          op: "upsert_line",
          section: "intent",
          text: "the webhook retries forever when the endpoint is down",
        },
      ],
    });
    await settle();

    assert.equal(provider.resultFor(callId).rejected.length, 0);
  });

  it("keeps credentials out of the transcript", () => {
    session.sendText("the token is ghp_abcdefghijklmnopqrstuvwxyz0123456789 and it leaked");
    const utterance = session.ledger.all().at(-1);
    assert.match(utterance!.text, /\[redacted\]/);
    assert.doesNotMatch(utterance!.text, /ghp_/);
  });

  it("asks the model to continue exactly once after a batch of tool calls", async () => {
    provider.say("cache the avatar images");
    await settle();

    provider.callTools([
      {
        name: "draft_update",
        args: { operations: [{ op: "upsert_line", section: "intent", text: "cache the avatar images" }] },
      },
      { name: "read_draft", args: {} },
    ]);
    await settle();

    assert.equal(provider.calls.filter((call) => call.kind === "tool-result").length, 2);
    assert.equal(provider.calls.filter((call) => call.kind === "response").length, 1);
  });
});
