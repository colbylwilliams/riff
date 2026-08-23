# Agent guide — `riff`

Cross-cutting rules for AI coding agents working in this repository. Pair this guide with the [README](README.md) (what Riff is, and why each boundary exists) and the [`docs/`](docs/) tree (mechanics and rationale). There is no `CONTRIBUTING.md` — this guide is the process doc.

## Repository orientation

| Where | What |
|---|---|
| [`core/`](core/) | **The portable contract.** The shared agent definition, the JSON Schemas, and the conformance cases every binding reproduces. Not implementation. See [Working in `core/`](#working-in-core). |
| [`packages/`](packages/) | The TypeScript binding: [`riff-core`](packages/riff-core/) (engine), [`riff-openai-realtime`](packages/riff-openai-realtime/) (provider), [`riff-github`](packages/riff-github/) (host). |
| [`swift/`](swift/) | The Swift binding: `RiffCore` (the same engine, natively), `RiffOpenAIRealtime`, and `RiffAudio` (capture, playback, echo cancellation). |
| [`rust/`](rust/) | The Rust binding: [`riff-core`](rust/riff-core/) (the same engine again) and [`riff-openai-realtime`](rust/riff-openai-realtime/). No dependencies and no runtime — see [its README](rust/README.md). |
| [`tools/build-bundle.mjs`](tools/build-bundle.mjs) | Compiles `core/agent` into the committed bundle, validates the manifest, and mirrors the bundle and conformance cases into each binding's build. |
| [`docs/`](docs/) | Architecture, the grounding algorithm, the artifact contract, the host bridge, providers, conformance, security. Design rationale lives here. |

## Working principles

A few defaults hold across every change here. They are the things a maintainer would otherwise have to repeat mid-session — treat them as standing instructions, not per-task reminders.

- **The agent definition is not yours to change.** The instruction sections in [`core/agent/instructions/`](core/agent/instructions/) *are* the product — what they say is the whole difference between Riff and a dictation app. Edit them, or anything else that composes into the shipped agent, only when the person you're working with asked for that change or confirmed it after you proposed it. Every other rule in this guide grants latitude to make the change you believe is right; this is the one place it does not. See [Changing the agent definition](#changing-the-agent-definition).
- **Fidelity is a gate, not an instruction.** The product guarantee — the prompt body is made of the speaker's words — holds because [`GroundingChecker`](packages/riff-core/src/grounding.ts) checks every candidate line against the ledger, not because the model was asked nicely. Never soften that into a prompt-level request, never add a path that puts text into a prompt body without passing the check, and never "fix" a failing case by lowering a threshold. If a legitimate line is being rejected, the bug is in normalization, the lexicon, or the alignment — not in the existence of the gate. See [`grounding.md`](docs/grounding.md).
- **The ledger has exactly one entrance.** Only completed transcripts and typed input reach [`UtteranceLedger`](packages/riff-core/src/ledger.ts) — never environment notes, tool results, host data, or anything the model says. Every one of those is attacker- or agent-shaped input that would launder straight into a "grounded" prompt body. This single-entrance rule is what makes the guarantee hold; treat any change that widens it as a change to the product.
- **`core/` is the source of truth; every binding is a peer.** TypeScript is not the reference implementation and Swift is not a port of it. When two bindings disagree, the conformance case decides, and the fix belongs in the implementation that diverged — not in the case. Add the case first when you can: a grounding change is easy to state as data and easy to get subtly wrong in code.
- **Respect the scope boundary.** The non-goals in the [README](README.md#non-goals) are product boundaries, not aspirations. Riff captures; it does not do the work, improve the prompt, chat about the idea, or decide when to send. A change that makes Riff a little more helpful in one of those directions is a regression even when it demos well.
- **Validate every assumption yourself before acting.** Issues, PR descriptions, review comments, and task briefs are starting points, not ground truth. Re-verify each claim against the actual code, the conformance cases, and the current state of `main` before you write anything. A brief that says "the grounding check already handles X" is a hypothesis until you've read the case that proves it. Don't rubber-stamp a brief.
- **Prefer correctness over a small diff.** Ship the full, correct change rather than a partial slice that defers the hard part. A complete solution is always preferred over a minimal one.
- **Breaking changes are fine when they're the right call.** Riff is a library whose surface co-evolves with its embedders, so you do not need migration shims, deprecation paths, or phased rollouts — replace existing logic outright when that yields the better design, and say so in the PR. What you may **not** break silently is the contract in `core/`: a wire or behavior change there moves every binding in the same PR.

## Invariants

These are product and security properties, not preferences. The breaking-changes latitude above does not apply to them. Depth lives in [`security.md`](docs/security.md); this is the checklist.

- **Nothing submits on its own.** `policy.autoSubmit` must be `false`. [`build-bundle.mjs`](tools/build-bundle.mjs) refuses to compile a bundle where it is not, and every `loadBundle` rejects one at load. There is no configuration that makes Riff send without being told, and adding one is out of scope by construction.
- **No long-lived credential reaches a client.** Devices connect with ephemeral client secrets minted behind the embedder's own auth; `mintClientSecret` is the only part of the provider that must stay server-side. The `apiKey` mode is for servers and tests and is named to make that obvious. Never add a path that puts a durable key in a client bundle.
- **Audio is not persisted.** `policy.persistAudio` is `false`. Transcripts are kept because the prompt is built from them; the recording is not. The ledger lives in memory for the session and the engine does not write it.
- **Redaction runs on entry to the ledger.** Credentials arriving through typed input are stripped before storage, so a leaked token never reaches the ledger, the draft, the artifact, or the model. It is a pattern-based backstop — extend the patterns, never relocate the call site downstream of the ledger.
- **Riff does not crawl, and does not invent facts.** References resolve through the host; an unrecognized link is recorded as a URL and not fetched. No fabricated PR numbers, URLs, names, or statuses — an unresolved reference stays unresolved and gets asked about. A plausible wrong identifier sends the downstream agent somewhere real, which makes this a security property as much as a quality one.
- **Hosts read narrowly and write only where told.** Treat every [`RiffHost`](docs/host-bridge.md) method as reachable by a model interpreting speech in a noisy room. Reads stay scoped to what the speaker can already see; writes are limited to `submitPrompt` against an explicitly configured destination, and idempotent where they can be.
- **Environment facts go to the model, never to the ledger.** Repository, branch, speaker, and destinations are injected so the agent does not have to ask "which repo?" — and because they are not in the ledger, the grounding check rejects any attempt to quote them as though the speaker had said them. Keep it that way.

## Build, test, lint

```sh
npm install
npm run bundle:check                          # committed bundle and mirrored cases match core/agent
npm run build                                 # bundle + tsc --build
npm test                                      # bundle:check + build + every TypeScript suite
swift test --package-path swift               # the same conformance cases, natively
cargo test --manifest-path rust/Cargo.toml    # and again, natively
```

`npm test` is the gate that matters most: it fails when the committed bundle has drifted from `core/agent`, so no binding can quietly ship a different agent than the one in source.

> [!IMPORTANT]
> **The Swift suite needs macOS.** `RiffAudio` imports `AVFoundation` unguarded, so the package does not build on Linux — [CI](.github/workflows/ci.yml) runs it on `macos-15`, and the Linux devcontainer cannot. The TypeScript and Rust suites run anywhere. When you change engine behavior from a container, say plainly in the PR that the Swift half was validated by CI rather than locally; never assume parity you did not observe.

Run every suite locally before the initial push and before declaring a PR ready. There is no linter or formatter configured for the TypeScript and Swift halves — match the surrounding style rather than reformatting a file you touched. Rust is the exception: it has one canonical formatter and linter, so `cargo fmt --all --check` and `cargo clippy --all-targets -- -D warnings` gate [CI](.github/workflows/ci.yml) and must pass before a push.

## Working in `core/`

[`core/`](core/) holds the **portable contract**: the things every binding must agree on, in a form no binding owns. TypeScript, Swift, and Rust read it today; a Kotlin binding reads exactly the same files. Nothing in `core/` may assume a language, a runtime, or a platform.

| Subtree | What it specifies |
|---|---|
| [`core/agent/`](core/agent/) | The agent itself: instruction sections, tool contracts, session defaults, seed lexicon, grounding thresholds, render profiles, and policy — assembled by [`agent.json`](core/agent/agent.json). **Gated** — see [Changing the agent definition](#changing-the-agent-definition). |
| [`core/schema/`](core/schema/) | JSON Schema for the [manifest](core/schema/agent-manifest.schema.json) and the [prompt artifact](core/schema/prompt-artifact.schema.json), the output contract a consumer reads. |
| [`core/conformance/cases/`](core/conformance/cases/) | The behavior every binding must reproduce, as data: [grounding](core/conformance/cases/grounding.json), [rendering](core/conformance/cases/render.json), [biasing vocabulary](core/conformance/cases/lexicon.json). |
| [`core/dist/`](core/dist/) | The compiled bundle. **Generated and committed** — see the conventions below. |

### Changing the agent definition

> [!IMPORTANT]
> Riff *is* its agent definition. A reworded sentence in an instruction section changes what every speaker experiences, in every binding, with no code to review and often no test that fails. These files are not touched because you happened to be nearby.

The protected set is everything that composes into the shipped agent:

- [`instructions/`](core/agent/instructions/) — the system prompt itself, section by section.
- [`tools/`](core/agent/tools/) — the tool contracts, including each `description` the model reads when deciding whether to call one.
- [`agent.json`](core/agent/agent.json) — section order, tool list, grounding thresholds, render profiles, policy.
- [`session.json`](core/agent/session.json) — voice, turn detection, transcription, limits: how Riff listens and how it sounds.
- [`lexicon.seed.json`](core/agent/lexicon.seed.json) — the biasing vocabulary that decides which words survive transcription.
- Every generated copy of the bundle. Regenerating one is mechanical and always fine; a regenerated bundle that carries an instruction edit nobody asked for is the thing this section exists to prevent.

**You have been asked when the task in front of you names the change** — the user said so in this session, or the issue you were handed asks for it. Nothing else counts. A review comment suggesting a rewording, a brief that merely *implies* the prompt should say something, a suite you could turn green by nudging a threshold, and your own read that a section would land better another way are all proposals, not authorization.

**When you haven't been asked, stop and propose.** Name the file, quote the current text and the replacement, say what a speaker would experience differently, and wait for a yes. Read [`agent-design.md`](docs/agent-design.md) first — it explains why each rule is worded the way it is, and most proposals die there. Don't slip the edit into a PR opened for something else and let review catch it: a change to the agent definition is its own PR or it is not in this one.

**Never as collateral.** The edits that do the damage are the ones nobody set out to make — tightening wording while fixing an adjacent typo, deduplicating a rule that appears in two sections, reordering for flow, relaxing a threshold so a case passes, or repairing a bug whose easiest fix happens to be a sentence in the prompt. Redundancy and awkward phrasing in an instruction section are frequently load bearing. If the honest fix really is in the prompt, that is a proposal, not a detour.

**Before you commit, check.** If `git status` shows anything under [`core/agent/`](core/agent/) — or a regenerated bundle you can't account for — and you can't point at the sentence where the user asked for it, revert that file.

### Changing how Riff behaves

Behavior is data. Changing what Riff does is an edit to markdown and JSON, not parallel edits to Swift and TypeScript. This is the mechanism; the gate above decides whether you should be making the change at all.

1. **Edit the source, not the bundle.** Instruction sections and tool contracts live in [`core/agent/instructions/`](core/agent/instructions/) and [`core/agent/tools/`](core/agent/tools/). Every file in either directory must be listed in [`agent.json`](core/agent/agent.json) — the builder fails on an unlisted file, a duplicate tool name, a duplicate or out-of-order instruction `order`, and a tool that the `tools` instruction section never mentions.
2. **Run `npm run bundle`** and commit the regenerated bundle together with the source edit, in the same commit. Never hand-edit a generated copy.
3. **Reconsider the conformance cases.** The suite pins its thresholds to the shipped bundle, so a change to `grounding` in `agent.json` fails the suite until the cases are revisited. That failure is the design working: decide whether the case or the threshold was wrong, and say which in the PR.
4. **Run both suites.** A change that passes in one binding and fails in the other has found a real divergence.

### Adding a conformance case

Add behavior to the suite when it is behavior a **speaker could notice** — a grounding decision, rendered output, section ordering, biasing order. Internal structure does not belong here.

1. Add the case to the right file in [`core/conformance/cases/`](core/conformance/cases/). Write the `description` to explain why the case exists; the runners read it aloud.
2. `npm run bundle` to mirror it into each binding's test resources.
3. Run every suite. Fix the implementation, not the case.

### What does *not* go in `core/`

Host-internal refactors, provider wire details, transport concerns, and anything only one binding can observe. Provider mapping belongs in [`providers.md`](docs/providers.md); design rationale belongs in [`docs/`](docs/); a bug fix that brings an implementation back into compliance with an existing case gets a regression test and leaves `core/` alone.

## Adding a platform binding

A binding is a native implementation of the engine plus a provider. Kotlin is the next one; the rules below are what keeps each new implementation from becoming a new dialect.

- **Ship the agent definition, do not restate it.** Vendor [`core/dist/riff-agent.bundle.json`](core/dist/riff-agent.bundle.json) and read instructions, tools, thresholds, render profiles, and policy from it. Every rule that governs the agent is in that file. A threshold, a filler word, or an instruction retyped into your source is a divergence waiting to happen.
- **Extend [`build-bundle.mjs`](tools/build-bundle.mjs), don't copy by hand.** A new binding adds its bundle destination to `outputs` and its test-resource directory to `conformanceTargets`, so `npm run bundle:check` covers it and drift becomes impossible rather than merely discouraged. This is what lets a binding build without a Node toolchain.
- **Validate on load.** Reject an out-of-range threshold, an undefined render profile, and `autoSubmit: true`. This is the last line of defense against a binding running a modified agent, and it is not optional.
- **Run the whole suite.** Not a subset. Mirror the case files rather than transcribing them.
- **Then cover the behavior the cases cannot express.** These are the divergences that data cannot catch, and each has already been a real bug: the [single-entrance ledger rule](#invariants); a turn's tool calls dispatched as one batch producing exactly one continuation, with the in-flight flag set when the batch arrives and cleared when the continuation starts (never scoped to the dispatch — that is precisely where an async binding diverges from an event-driven one); interruption cancelling the response and dropping buffered audio; credentials stripped before storage; and submission refused when the policy's required sections are missing. [`SessionTests.swift`](swift/Tests/RiffCoreTests/SessionTests.swift), [`session.test.ts`](packages/riff-core/test/session.test.ts), and [`session.rs`](rust/riff-core/tests/session.rs) are the templates.
- **Keep the module seams parallel.** The TypeScript, Swift, and Rust engines are deliberately the same decomposition — ledger, lexicon, grounding, draft/takes, tools, render, session. Follow it. Reviewers diff bindings against each other, and a binding that reorganizes the seams makes every future divergence harder to see.
- **Add a CI job.** A binding that is not in [`ci.yml`](.github/workflows/ci.yml) is not held to the suite. Note the toolchain it needs — Swift is macOS-only for the reason described above; Rust and Kotlin run on Linux and belong in the devcontainer.

## Pull requests

The review loop is the same every time; run it without being asked.

1. **Open the PR unless told otherwise.** Keep the title and description current as the change evolves — when scope shifts mid-review, update the description so it always describes the PR as it stands.
2. **Don't assign Copilot as a reviewer yourself** — it is auto-assigned shortly after the PR opens. Wait for that review rather than racing to request it manually.
3. **Triage review feedback by materiality — you are the bot's editor, not its patch-applier.** For each comment decide whether it affects correctness, security, the PR's stated goal, or the `core/` contract. Fix those. For style nits, speculative suggestions, things already handled, or things a stacked PR owns, reply briefly with why you're declining and resolve the thread. Do not grow the diff to satisfy a non-material comment. A comment proposing a reworded instruction section, a renamed tool, or a moved threshold is a proposal about the product no matter how well argued — say the agent definition is out of scope for this PR and resolve the thread, then take it to the user if you think it has merit.
4. **Reply to and resolve every thread you address**, and every one you decline. Keep replies terse and factual — state what changed or why you're declining, and skip the pleasantries.
5. **Guard scope and converge.** A PR stays about the thing it opened for; genuinely separate concerns become a follow-up issue or a stacked PR. Treat comments as evidence about an invariant rather than a patch checklist — if successive findings expose the same hole, repair the design once instead of adding one guard per example. "Ready" is a green PR whose *material* threads are resolved, not one with zero comments.
6. **For review-fix rounds, push before the full gates so review and validation run in parallel.** The initial PR still gets both suites before it opens. After review feedback, make one coherent fix round, run only the smallest targeted check needed to catch an obvious failure, then commit and push promptly. Run `npm test` and the Swift suite on the pushed commit while CI and the bot review the same SHA; never call the PR ready until they pass. Do not gate a push on an extra local code-review pass. Before each push, merge `main` if it moved (or rebase per the stacking rules below), resolve conflicts carefully, and ensure the pushed tree contains every intended fix.
7. **Give a direct readiness verdict, but don't merge unless explicitly told to.** Once CI is green, both suites pass, and every thread is replied to and resolved, say the PR is ready and stop.

### Stacked PRs

When a change touches the same files as an in-flight PR, stack it instead of racing it.

- Branch the child off the **parent PR's branch**, not `main`, and open the child PR with the parent branch as its base. Don't rebase a stacked branch onto `main` while the parent is still open.
- When the parent merges, GitHub auto-retargets the child to `main`. Rebase the child onto `main` and re-run both suites before doing anything else — until you do, the child's diff includes anything the parent picked up after you branched, shown as a revert. If the parent was squash-merged, rebase with `--onto main <old-parent-head>` so its pre-squash commits drop out instead of replaying as conflicts; after a merge commit a plain `git rebase origin/main` is enough.
- Independent changes — no shared files, no code dependency — get their own branch off `main`; don't stack unnecessarily.
- Because a bundle edit regenerates [`core/dist/riff-agent.bundle.json`](core/dist/riff-agent.bundle.json) and every mirrored copy, two PRs that both touch `core/agent` will always conflict there. Stack them, and regenerate rather than hand-resolving the conflict in a generated file.
- **Reviewing a PR you don't own?** Leave *actionable* review comments the author can act on — not a prose description of the problem. If you can't phrase a comment concretely enough to act on, or the fix spans several threads and keeps recurring, open a stacked fix PR on top of theirs (per the rules above) instead of narrating the issue.

## Repository conventions

- **This repository is public; treat everything in it as published.** Riff ships under [MIT](LICENSE) and is read by people with no access to any internal system. Don't describe private code, internal services, or unreleased plans anywhere in the tree — including commit messages, PR descriptions, and issues — and don't make the build depend on anything an outside reader cannot resolve. This applies to tooling config as much as to prose: a devcontainer feature, base image, or registry that only answers from behind the fence breaks the repo for everyone who clones it. The test is whether someone outside can pull it, not who built it — a published artifact is fine even when the source repository behind it is private. When a tool really is internal-only, keep it in untracked local config rather than in the tree.
- **Generated files are generated.** [`core/dist/riff-agent.bundle.json`](core/dist/riff-agent.bundle.json), the vendored bundles under [`swift/Sources/RiffCore/Resources/`](swift/Sources/RiffCore/Resources/) and [`rust/riff-core/resources/`](rust/riff-core/resources/), and the mirrored cases under [`swift/Tests/RiffCoreTests/Resources/`](swift/Tests/RiffCoreTests/Resources/) and [`rust/riff-core/tests/resources/`](rust/riff-core/tests/resources/) are outputs of `npm run bundle`. Edit `core/agent` and regenerate; never patch a copy. They are committed on purpose so a binding can build without Node.
- **The engine has no third-party runtime dependencies, in any binding.** `@riff/core` has none, `RiffCore` has none, `riff-core` has none, and the provider and host packages depend only on the engine. Keep it that way: the engine is embedded into other people's applications, and a dependency there is a dependency they did not choose. A new third-party dependency needs a stated reason in the PR.
- **Thresholds, filler words, labels, and policy live in [`agent.json`](core/agent/agent.json)** — never as constants in a binding. If you find yourself typing a number that governs grounding or rendering into TypeScript, Swift, Rust, or Kotlin, it belongs in the manifest.
- **Provider wire formats stay inside the provider.** Nothing above [`RealtimeProvider`](docs/providers.md) knows an event name, a field name, or a transport detail from any vendor. That seam is what lets the speech engine change without changing anything a speaker can observe.
- **Riff is a library, not an app.** It does not own the microphone, the screen, credentials, or storage. When a change needs one of those, the answer is an interface the embedder implements, not a dependency the engine acquires.
- **Keep the three suites honest about what they cover.** The conformance cases pin cross-binding behavior; the per-binding session tests pin the behavior data cannot express. Put a new test in the one that actually constrains it.

## Code style notes

- Comment only where comments add value. Don't narrate trivial code; do explain non-obvious invariants and the *reason* for an unusual choice. The existing headers in [`RiffAudioEngine.swift`](swift/Sources/RiffAudio/RiffAudioEngine.swift) and [`build-bundle.mjs`](tools/build-bundle.mjs) are the register to aim for — they explain why the code is shaped the way it is, not what the next line does.
- Write comments for the current code, not its history. A reader should grasp a comment without git history, the issue tracker, the review thread, or any session context. Keep out: process meta-attribution ("review flagged", "as the reviewer noted"), "used to / after the fix" setups (state the current invariant instead), prompt-era references ("per the design plan"), and bare PR numbers in body comments. Legitimately keep `TODO` / `FIXME`, cross-binding parity notes, and external references.
- When a constant or a check exists because of a rule in `core/`, say so and name the rule. A reader walking the code should be one step from the contract.
- Match the surrounding language's conventions — the bindings are deliberately parallel in structure, not in syntax. Don't write Swift that reads like transliterated TypeScript, or the reverse.

## Documentation & Markdown

> [!IMPORTANT]
> These rules cover prose — the [README](README.md), [`docs/`](docs/), and this guide. **They do not cover [`core/agent/instructions/`](core/agent/instructions/).** Those files are a system prompt that happens to be written in Markdown: their wrapping, ordering, and wording are agent behavior, so reflowing or copy-editing one is a product change governed by [Changing the agent definition](#changing-the-agent-definition).

- **Don't hard-wrap prose.** Write one line per paragraph or list item and let the renderer wrap it; only break where it's semantically meaningful (between paragraphs, list items, or other block elements). Existing files under [`docs/`](docs/) predate this rule and are still hard-wrapped — leave them alone unless you're already editing the paragraph, rather than reflowing a file as a drive-by.
- **Reference files and symbols as basename links, not bare inline-code paths.** Write [`grounding.ts`](packages/riff-core/src/grounding.ts), not a bare `` `packages/riff-core/src/grounding.ts` `` — the reader gets a click-through and the prose stays short.
- **Use GitHub alert blocks for callouts** (`> [!NOTE]`, `> [!IMPORTANT]`, `> [!TIP]`, `> [!WARNING]`) rather than plain `>` blockquotes; reserve plain blockquotes for actual quotations and for transcript examples like the one in the README.
- **Write docs as current fact, not as a proposal or a history.** When you implement a design, update its doc to read as documentation of what *is* — present tense, no "will" — and strip session- or PR-era narration the same way you would from a code comment.
- **The README is the argument; `docs/` is the mechanism.** Keep the README about what Riff does and why it is shaped that way, and put algorithms, contracts, and edge cases in `docs/`. When a change makes one of them wrong, fix it in the same PR.
