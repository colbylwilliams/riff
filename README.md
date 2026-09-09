# Riff

**Think out loud. Put agents to work.**

Riff turns thinking out loud into ready-to-send prompts for agents, in your own words. Talk naturally, change your mind, and move between tasks. Riff keeps each prompt distinct, resolves the references that matter, and asks only the questions that change the work. When you say "send it," the prompt is ready — without first cleaning up a transcript.

## Keep your train of thought

Delegation should not require a writing session first. Yet getting an agent started means stopping to repair names, untangle contradictions, gather links and context, and turn several ideas into separate prompts. Dictation leaves that work to you. Asking a model to "clean it up" can quietly replace your meaning with something that reads better.

- **Follow the tangent.** Park one request, start another, and return to either in the same conversation without mixing their instructions.
- **Bring the context along.** Ask about a reference or a term, recall earlier work, and attach the relevant links without breaking your train of thought.
- **Keep the instructions that matter.** Reuse standing instructions in your own words instead of repeating them for every prompt.

**Your words are checked, not just promised.** Draft body lines are checked against what you said or typed; saved instructions carry their original provenance. Lines below the grounding threshold are rejected before they enter the draft. This is a wording check, not a guarantee against meaning lost through selection or omission. See [how grounding works](docs/grounding.md).

## Try it

With Node.js 22.18 or newer, run from the repository root:

```bash
npm install && npm run build
npm run demo                     # http://localhost:4173
```

Open http://localhost:4173 to watch the conversation below assemble two distinct prompts through the real engine, including an invented paraphrase being rejected. Scripted mode needs no API key or microphone and sends nothing outside the demo. For live mic mode and optional real GitHub references, see the [demo README](examples/riff-web/README.md).

## What Riff does instead

**One prompt sent. One parked. Context attached.** The offline-drafts request is submitted when you say to send it, with the relevant thread, earlier session, and PR attached. The onboarding-doc request stays separate, ready to resume.

The demo's Slack, past-session, and GitHub results are sample fixtures, not built-in Slack or session-history connectors. Real context comes from your app's [host bridge](docs/host-bridge.md); the conversation, grounding check, and rendered prompt below run through the real engine.

> **You** — okay, I think we should make drafts work offline. people keep losing edits on the train.
>
> **You** — the Slack thread from Monday has the examples
>
> **Riff** — *(finds the thread and attaches its link without interrupting)*
>
> **You** — didn't we start something on this last week?
>
> **Riff** — The local-drafts session or the sync-retry one?
>
> **You** — use the local-drafts session as background, and use the PR I just opened as the starting point. no sync engine yet.
>
> **You** — oh, separate thing, the onboarding doc still says node sixteen. different thing entirely.
>
> **Riff** — *(parks the offline-drafts prompt; opens a separate take for the doc)*
>
> **You** — let's finish the offline drafts one first
>
> **You** — and don't touch the generated files, you know how it is
>
> **You** — I should be able to close the app offline and come back to my edits. send it.

Riff kept "I think," asked only which earlier session you meant, and reattached your saved constraint verbatim. Nothing from Slack or the recalled prompts is passed off as something you said.

<details>
<summary><strong>Read the submitted prompt</strong></summary>

What gets submitted:

```markdown
# Make drafts work offline

I think we should make drafts work offline.

people keep losing edits on the train. the Slack thread from Monday has the examples. use the local-drafts session as background. use the PR I just opened as the starting point.

**Constraints**
- no sync engine yet
- don't touch the generated files

**Done when**
- I should be able to close the app offline and come back to my edits

**Context**
- Slack #feedback "Drafts lost on the train" — https://example.com/slack/offline-drafts — referred to as "the Slack thread from Monday"
- session-local-drafts "Local drafts" — https://example.com/sessions/local-drafts — referred to as "the local-drafts session"
- acme/web#412 "Save drafts locally" (open) — https://github.com/acme/web/pull/412 — referred to as "the PR I just opened"
```

</details>

## Your request, your decisions

- **Organizes your request without substituting its own ideas.** Riff removes filler and false starts, corrects recognized transcription errors, and arranges what you said. It does not silently rewrite your voice or invent decisions.
- **Helps clarify the work; leaves solving it to the downstream agent.** Questions about references and terminology belong in the conversation. Proposing implementations, choosing libraries, writing code, and judging the idea do not. Feedback on the draft is opt-in.
- **You choose when to send.** Each handoff needs your explicit direction. A pause or a finished-looking draft is not permission to submit, and there is no auto-submit mode.

## Build with Riff

Riff ships as an embeddable library with TypeScript, Swift, and Rust bindings, an OpenAI Realtime provider for each, and a GitHub-backed host in TypeScript. Your app supplies the microphone, interface, context, and handoff destination. The engines have no third-party runtime dependencies and share one agent definition and conformance suite.

| | |
|---|---|
| [Getting started](docs/getting-started.md) | Session wiring in TypeScript, Swift, and Rust |
| [Agent design](docs/agent-design.md) | The behavioral contract, and why each rule is there |
| [Architecture](docs/architecture.md) | Layers, core concepts, session lifecycle, repository layout |
| [Grounding](docs/grounding.md) | How "their words" is enforced, and where the edges are |
| [Prompt artifact](docs/prompt-artifact.md) | The output contract and its provenance |
| [Host bridge](docs/host-bridge.md) | Implementing a host for your world |
| [Providers](docs/providers.md) | The provider interface, OpenAI mapping, swapping engines |
| [Conformance](docs/conformance.md) | Running the shared suite and adding a platform binding |
| [Security and privacy](docs/security.md) | Credentials, redaction, audio and transcript handling |

## License

MIT. See [LICENSE](LICENSE).
