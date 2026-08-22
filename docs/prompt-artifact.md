# The prompt artifact

The artifact is what leaves a Riff session. It carries the prompt, the context that was resolved
while capturing it, and the provenance that proves the body came from the speaker.

Schema: [`core/schema/prompt-artifact.schema.json`](../core/schema/prompt-artifact.schema.json).

## Why it is more than a string

The downstream agent only needs `rendered`. Everything else exists because a prompt produced without
anyone reading it needs to be accountable afterward: which sentences were the speaker's, which
resolved thing a vague phrase pointed at, what the agent did while listening.

## Shape

```jsonc
{
  "id": "t1-2026-08-21T22:04:11.902Z",
  "takeId": "t1",
  "title": { "text": "Fix the export button", "origin": "derived" },

  "lines": [
    {
      "id": "t1-l1",
      "section": "intent",
      "text": "the export button does nothing past a thousand rows",
      "order": 1000,
      "sourceUtteranceIds": ["u1"],
      "grounding": { "ratio": 1, "kind": "trimmed" }
    }
  ],

  "context": [
    {
      "referenceId": "pr-acme/web-412",
      "kind": "pull_request",
      "identifier": "acme/web#412",
      "title": "Chunked uploads",
      "state": "open",
      "url": "https://github.com/acme/web/pull/412",
      "resolvedFrom": "the PR I just opened"
    }
  ],

  "terms": [{ "canonical": "Flakeguard", "heardAs": ["flake guard"], "kind": "service" }],

  "provenance": {
    "fidelity": 1,
    "utteranceCount": 7,
    "bodyTokens": 34,
    "agentAuthoredTokens": 0,
    "agentVersion": "1.0.0",
    "bundleRevision": "4c85414e21d0c9e8",
    "providerId": "openai-realtime",
    "model": "gpt-realtime-2.1",
    "durationMs": 41250,
    "toolCalls": [{ "name": "draft_update", "at": "…", "durationMs": 3, "ok": true }]
  },

  "rendered": "# Fix the export button\n\nthe export button does nothing past a thousand rows.\n…",
  "status": "submitted"
}
```

## Sections

Lines carry a section, and sections render in a fixed order.

| Section | Holds | Grounding |
|---|---|---|
| `intent` | The request itself. Usually one to three sentences. | strict |
| `detail` | Specifics that shape it — where, what about it, what they noticed. | strict |
| `constraint` | Limits and standing rules. Attached motifs land here. | motif or strict |
| `acceptance` | How they will know it worked, when they said it. | strict |
| `open_question` | Questions *the speaker* raised for the downstream agent. | strict |

`open_question` is strict deliberately. It is not a place for the agent's questions — if Riff has a
question it asks it out loud. It is for things the speaker wondered about and wants carried forward.

## Provenance

`fidelity` is the token-weighted share of the body provably drawn from the ledger, with `derived`
lines counted as zero. A normal captured prompt scores 1.0. It is worth surfacing in a UI and worth
alerting on: a drop is the earliest signal that capture quality has regressed.

`agentAuthoredTokens` is the same information as a count, which is easier to reason about for a
short prompt where a ratio is coarse.

`bundleRevision` is a content hash of the agent definition. If prompts change character after a
release, this is what identifies the definition that produced them.

`sourceUtteranceIds` on each line ties it back to specific utterances, which is what makes an
after-the-fact audit possible.

## Context

Context is agent-authored, and that is fine: it is retrieved data, not voice. Keeping it in its own
block is what lets the body stay untouched.

`resolvedFrom` records the phrase the speaker used. That is the link between "the PR I just opened"
in the body and `acme/web#412` in the context, and it is what lets the downstream agent connect them
without guessing.

## Rendering

Two profiles ship, configured in `agent.json` → `render`.

**`prose`** (default) — the title as a heading, `intent` and `detail` as paragraphs with terminal
punctuation supplied, and `constraint`, `acceptance`, `open_question`, and `context` as short
labeled lists. Empty sections are omitted entirely.

**`structured`** — the same content under `##` headings, for destinations that expect them.

The default is prose because heavy structure is something the speaker did not ask for. Someone
describing a bug is not writing a spec, and formatting their two sentences into six headed sections
makes the prompt look more considered than it is. Structure appears only where it carries real
information: a list of constraints is a list.

Both implementations produce byte-identical output, pinned by
[`core/conformance/cases/render.json`](../core/conformance/cases/render.json).

## Status

`drafting` → `ready` → `submitted`, with `parked` for a take set aside and `discarded` for one
abandoned. A take that fails to submit returns to `drafting` rather than being lost.

## Consuming an artifact

Send `rendered`. Then, depending on what you are building:

- Show `provenance.fidelity` where the person can see it. It is the answer to "is this still mine?"
- Use `context[].url` for links in whatever you create from the prompt.
- Use `terms` to disambiguate vocabulary — the downstream agent gets the same reading of
  "Flakeguard" the speaker meant.
- Keep `lines[].sourceUtteranceIds` if you need to explain later why a prompt said what it said.
