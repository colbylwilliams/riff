# Conformance

Riff runs natively on each platform rather than through a shared binary, so "the same agent
everywhere" needs to be checkable rather than asserted. The conformance suite is how.

Cases live in [`core/conformance/cases`](../core/conformance/cases) as data. Every binding executes
the same files.

## What is pinned

**Grounding.** Which proposed lines are made of the speaker's words and which are not. This is the
behavior that defines the product, so it is specified case by case: verbatim, filler removed, false
starts dropped, connective words added, paraphrase rejected, vocabulary upgrade rejected, word order
changed rejected, invented detail rejected, sentences spanning two and four utterances, lexicon
corrections, one sentence selected out of a long turn, an empty ledger, a content-free line, and the
looser bar a title is held to.

**Rendering.** The exact bytes a finished prompt renders to, under both profiles, including that
context is listed in the order the speaker raised it rather than sorted by an id the host chose.

**Biasing vocabulary.** The order of the transcription keyword list. This is capped before it
reaches the provider, so the comparator decides which terms survive, and different biasing produces
different transcripts — which is upstream of the ledger, the grounding check, and the prompt body.

The suite also pins its own thresholds to the shipped bundle, so a change to `agent.json` that alters
grounding fails the suite until the cases are reconsidered.

## Case format

```jsonc
{
  "id": "paraphrase-rejected",
  "description": "Same meaning, different words. This is the failure the check exists to prevent.",
  "utterances": ["the login page is busted on Safari"],
  "lexicon": [{ "canonical": "GitHub", "kind": "product", "heardAs": ["get hub"] }],
  "candidate": "the sign-in screen is not working correctly in Safari",
  "mode": "title",
  "expect": {
    "ok": false,
    "kind": "trimmed",
    "ratio": 1,
    "unmatchedIncludes": ["sign-in", "screen"],
    "sourceUtterances": ["u1", "u2"]
  }
}
```

`lexicon`, `mode`, and each field of `expect` beyond `ok` are optional. Descriptions are read aloud
by the test runners, so they are written to explain why the case exists rather than to restate it.

## Running

```bash
npm test                          # TypeScript, including the suite
swift test --package-path swift   # Swift, the same cases
```

The cases are mirrored into the Swift test target by `tools/build-bundle.mjs`, the same way the agent
bundle is, and `npm test` fails if either copy has drifted.

## Adding a case

Add behavior to the suite when it is behavior a speaker could notice. Grounding decisions, rendering
output, and section ordering belong here. Internal structure does not.

1. Add the case to the appropriate file in `core/conformance/cases`.
2. `npm run bundle` to mirror it into the Swift target.
3. Run both suites. A case that passes in one implementation and fails in the other has found a real
   divergence, and the fix belongs in the implementation, not in the case.

Writing the case first is worth it. A grounding change is easy to describe as data and easy to get
subtly wrong in code.

## Adding a platform binding

A new binding is a native implementation of the engine, plus a provider. The bundle and the
conformance suite tell it what to build.

**Ship the agent definition, do not restate it.** Vendor `core/dist/riff-agent.bundle.json` and read
instructions, tools, thresholds, render profiles, and policy from it. Every rule that governs the
agent is in that file. Extend `tools/build-bundle.mjs` to copy it where your build expects it, so
drift is impossible.

**Implement the engine.** Text normalization, lexicon, grounding, ledger, drafts and takes, tool
registry with schema validation, renderer, session state machine. The TypeScript and Swift versions
are deliberately parallel; use whichever is closer to your language.

**Validate on load.** Reject a bundle with an out-of-range threshold, an undefined render profile, or
`autoSubmit` set to true. This is the last line of defense against a binding running a modified
agent.

**Run the suite.** Not a subset.

**Then check the behavior the suite cannot express.** A few things matter and are not data:

- Nothing enters the ledger except completed transcripts and typed input — not environment notes,
  not tool results, not anything the model says. This single-entrance rule is what makes grounding
  hold.
- A turn's tool calls are dispatched as a batch and produce exactly one continuation request.
- Interruption cancels the response and drops buffered audio.
- Credentials are stripped from transcripts before storage.
- Submission is refused when the policy's required sections are missing, with a message telling the
  agent what to ask for.

The Swift `SessionTests` cover each of these and are a reasonable template.

## Divergences found this way

The suite has already earned its keep. Building the Swift engine surfaced two real bugs:

**Lexicon corrections were misclassified.** A line where the transcriber wrote "get hub" and the
agent wrote "GitHub" was reported as `trimmed` rather than `corrected`, because substitution was
detected on the candidate side only. The lexicon usually corrects the *source*. Fixed by running the
alignment a second time without canonicalization and comparing.

**Tool-call batching relied on event ordering.** The original design counted outstanding calls and
requested a continuation when the count reached zero. That works when events are emitted
synchronously in a loop and breaks when they arrive through an async sequence, where each call
completes before the next is read — producing one spoken reply per tool. Both implementations now
carry a turn's calls in a single `tool.calls` event, and the in-flight flag is set when the batch
arrives and cleared when the continuation starts, so nothing depends on delivery timing.

A later review found four more divergences of the same kind, each present in one implementation
only: a submitted take stayed active in TypeScript so the next sentence spoken landed inside a
prompt that had already been sent; Swift sorted the context block by reference id while TypeScript
kept the order the speaker raised things in; Swift dropped the confidence a host reports for a term,
which is the signal that decides whether a word is corrected silently or asked about; and the two
biasing comparators scored acronyms differently and broke ties with different string ordering, one
of them locale-dependent.

None of these would have been found by testing one implementation, and each is now pinned by a case
or a regression test.
