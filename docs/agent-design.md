# Agent design

The agent definition lives in [`core/agent`](../core/agent) as plain files and compiles to a single
bundle every platform binding loads. This document explains what the agent is instructed to do and
why, so that changes to those files are made deliberately.

## Shape of the definition

| Path | What it is |
|---|---|
| `instructions/*.md` | The system prompt, in ordered sections with front matter |
| `tools/*.json` | One file per tool: name, `local` or `host`, description, JSON Schema |
| `session.json` | Provider-neutral session defaults — turn detection, transcription, limits |
| `lexicon.seed.json` | Vocabulary speech recognition reliably mangles, and the corrections |
| `agent.json` | The manifest: section order, tool list, grounding, rendering, policy |

`tools/build-bundle.mjs` composes these into `core/dist/riff-agent.bundle.json` and copies it into
the Swift package. The build is also a linter: it rejects a file that exists but is not listed in the
manifest, sections listed out of order, duplicate tool names, a tool never mentioned in the tools
instruction section, an undefined render profile, and any manifest with `autoSubmit` set to true.

The composed instructions are about 2,700 tokens. That is a deliberate ceiling. Realtime models
degrade with long system prompts, and the reasoning behind each rule belongs in this document rather
than in the prompt.

## The five rules

### 1. The prompt is in their words

The most important rule, and the one that most needs mechanical backing, because models paraphrase
by default and instructions alone do not stop it. The instruction states exactly which edits are
allowed — drop filler, drop false starts, drop whole sentences, fix a misheard word, fix punctuation
— and exactly which are not.

It is backed by the grounding check ([grounding.md](grounding.md)), which rejects lines that are not
made of the speaker's words and tells the model which words it invented. The instruction also tells
the agent not to mention rejections to the speaker, because a rejection is an internal correction,
not something the person needs to manage.

### 2. Stay out of the work

Riff never proposes an implementation, an approach, a library, a file to change, or a sequence of
steps, does not write code, and does not judge whether the idea is good.

This is the line that separates a prompting tool from a second, worse coding agent. A voice agent
that starts designing is doing the downstream agent's job with less context and no ability to run
anything, and it pulls the conversation away from the thing it is actually good at.

The instruction carves out one exception, and names it precisely: **vocabulary is context, code is
work**. Knowing that `flakeguard` is the retry wrapper in the CI package is required to record the
speaker correctly. Deciding they should change `flakeguard` is not.

### 3. Ask only when it changes the outcome

Clarifying questions are the main reason a voice agent beats a recording. They are also the main way
one becomes unbearable.

The instruction lists four cases worth asking about — ambiguous target, unresolved reference, an
implied but unstated finish line, and a load-bearing word that a lookup did not settle — and
explicitly rules out asking about anything lookup-able, anything that does not change the outcome,
and anything asked to seem thorough.

It also says to hold a question while someone is mid-thought. Interrupting a train of thought costs
more than the question gains, and questions often answer themselves a sentence later.

Each question is asked **once**. Whatever comes back is the answer; when it does not settle the matter, the gap stays open and the conversation moves on. A second ask — reworded, or returned to later — reads as not having listened, and the loop it creates is the failure mode that makes a voice agent unusable: a lookup that cannot succeed produces a question that cannot be answered, asked forever.

### 4. Feedback is opt-in

The agent does not critique the draft, score it, warn that it is thin, or offer to tighten it up
unless asked. The instruction enumerates what counts as asking ("how's that look", "anything
missing", "read it back") and states that silence and pauses are not asking.

Unsolicited feedback is how a capture tool turns into an editor, and an editor is what makes people
stop trusting that the prompt is still theirs.

A **blocking gap** is separated out and permitted at any time: if something is missing that makes the
prompt unusable, ask the one question that fixes it. Wanting the prompt to be better is not a
blocking gap.

### 5. Never invent a fact

Numbers, URLs, names, file paths, statuses. An unresolved reference stays unresolved and gets asked about once. A guessed reference sends the downstream agent somewhere real and wrong, which is strictly worse than an empty one.

When a reference will not resolve even after the speaker answers, their sentence stays in the prompt exactly as they said it and nothing is attached to it. A reference the downstream agent has to chase costs it a minute; a speaker interrogated about one has lost the thought they were holding.

## Conversational behavior

**Speaking style.** Most turns produce no speech at all. Capturing what was said is a tool call, not a sentence said back, so the whole of a response to a turn is normally a `draft_update` and nothing else. This is also how silence is actually reachable: turn detection runs with `autoRespond`, so the provider creates a response after every turn whether or not there is anything to say — a response made only of tool calls is what "saying nothing" looks like on the wire.

When Riff does speak: short, a sentence, usually less. It does not volunteer acknowledgment phrases, repeat back what was just said, or narrate its tool calls — recording a line is the acknowledgement of it, and the draft is already on screen. Results are mentioned only when they change something the speaker needs to know: "that's 412." A silent pause while someone thinks is correct behavior, not a failure to respond.

Two things are never *volunteered*. The line being recorded — the draft is already on screen, and speaking it is the readback Riff exists to replace. And anything about what comes next: its own capabilities, what it is about to do, what the speaker might want to add, or an assurance that it will keep listening if they keep going. Both are what a model reaches for to fill a turn it was forced to take, and both land hardest right when the speaker is mid-thought.

Neither is a gag rule, and the difference is the split the identity section draws: Riff does not volunteer, but it always answers. A direct question gets a direct answer, however often it is asked — "did you get that?" is answered in a word, and the how-to question in [Stay out of the work](#2-stay-out-of-the-work) requires naming the boundary and offering to record it as an open question. Only the unprompted licenses are spent after one use.

The rules that survive being asked are the ones that are not about volunteering at all: brevity, no filler phrases, and never reading a list aloud hold in an answer exactly as they hold in an aside. Being asked shortens what Riff says; it never widens the job.

**Never read the draft aloud unless asked.** Reading it back is the workflow Riff exists to replace.
When a readback is requested, the default is a one-sentence gist of what is covered, not the prompt
itself.

**Corrections.** "No, not that" supersedes rather than deletes: the earlier line leaves the draft but
stays in history, so a change of mind can be walked back. Tangents that go nowhere are dropped, and
abandoned fragments never enter the prompt. When a change of direction is large enough to be a
different request, it becomes a new take rather than an edit.

**Interruption.** Talking over the agent stops it immediately. The session emits an interruption
event so the audio layer can drop buffered speech; without that, already-queued audio keeps playing
over the person who just interrupted.

## Turn detection

`session.json` sets semantic turn detection with **low eagerness**. This is the single most
consequential runtime setting in the file.

Silence-based endpointing treats a pause as the end of a turn. People thinking out loud pause
constantly, mid-sentence, and an agent that jumps into those pauses is the specific failure that
makes voice agents unpleasant. Semantic endpointing waits for a turn that is actually finished, and
low eagerness widens that further. Riff would rather wait too long than interrupt.

Configuration is provider-neutral; providers map `mode: "semantic"` onto their own mechanism, and
`serverVadFallback` supplies the parameters for providers without semantic endpointing.

## Tools

Nine tools. The count is deliberate — realtime models get worse at tool selection as the list grows,
so related operations are folded into one tool with an `action` rather than split out.

| Tool | Kind | What it is for |
|---|---|---|
| `draft_update` | local | Add, replace, remove, reorder lines; attach resolved context. Grounding-checked. |
| `read_draft` | local | What is captured so far, plus a gist. Only when asked. |
| `resolve_reference` | host | "The PR I just opened" → an identifier and a URL |
| `lookup_term` | host | What a term means here, and how it is spelled |
| `record_term` | local | Teach the lexicon a correction so it is right next time |
| `recall_prompts` | host | Earlier prompts, as background. Recalled text is not quotable |
| `motifs` | local | List, attach, detach, save, retire standing instructions |
| `takes` | local | New, switch, park, list, discard drafts |
| `submit_prompt` | host | Hand the finished prompt to whatever does the work |

`local` tools run inside Riff and touch the ledger, drafts, and lexicon. `host` tools are delegated
to the embedding application ([host-bridge.md](host-bridge.md)).

Tool descriptions are written for the model and carry behavioral instruction, not just type
information: `draft_update` says to call it continuously as the person talks rather than at the end,
`resolve_reference` says to call it the moment a reference is heard, and `submit_prompt` says not to
confirm first.

Arguments are validated against each tool's JSON Schema before a handler runs. Failures return a
structured error the model can act on rather than throwing, because a malformed call should cost one
retry, not the conversation.

## Policy

`agent.json` carries a small policy block enforced by the engine rather than by instruction.

- `autoSubmit` must be `false`. Both `loadBundle` implementations reject a bundle where it is not.
  The speaker decides when a prompt is sent; this is not configurable.
- `readinessRequires` lists the sections a prompt needs before it can be submitted. Submitting
  without them returns a message telling the agent what to ask for, rather than sending something
  empty.
- `maxTakes` bounds how many drafts a session can hold.
- `redactSecretsFromTranscript` strips credentials before anything is stored.
- `persistAudio` is false. Riff keeps transcripts because the prompt is built from them; it does not
  keep the recording.

## Changing the agent

1. Edit the files in `core/agent`.
2. Run `npm run bundle`. It will refuse changes that break the manifest's invariants.
3. Run `npm test` and `swift test --package-path swift`. Behavioral changes usually need a
   conformance case ([conformance.md](conformance.md)).
4. Commit the regenerated bundle. CI checks that it matches its source.
