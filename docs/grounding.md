# Grounding

Grounding is the mechanism that keeps a prompt in the speaker's words. It is the part of Riff most
worth understanding, because it is what turns "in your own voice" from a claim into a property.

Implemented in [`packages/riff-core/src/grounding.ts`](../packages/riff-core/src/grounding.ts) and
[`swift/Sources/RiffCore/Grounding.swift`](../swift/Sources/RiffCore/Grounding.swift), and specified
as data in [`core/conformance/cases/grounding.json`](../core/conformance/cases/grounding.json).

## Why a mechanism and not an instruction

Ask a language model to preserve someone's phrasing and it will, mostly, for a while. Paraphrase is
close to the center of what these models do, and it reasserts itself under pressure: long sessions,
messy transcripts, a request that would read better reorganized.

The failure is quiet. The speaker is not reading the prompt — that is the entire point — so a prompt
that has been subtly professionalized looks like a success. The specificity that carried the intent
is gone and nobody notices until the downstream agent builds the wrong thing.

So the model proposes and the engine decides. The model has no way to put a line into the prompt
except through `draft_update`, and `draft_update` checks.

## What counts as an allowed edit

The instructions permit exactly one kind of change to what someone said: **deletion**.

- Drop filler — "um", "like", "you know", "basically"
- Drop false starts and abandoned fragments
- Drop whole sentences that are not part of the request
- Fix a word the transcriber got wrong
- Fix punctuation, casing, and sentence boundaries

Everything else — rewording, summarizing, expanding, reordering within a sentence, swapping in a
synonym — is a rewrite.

That definition has a useful consequence. If the only permitted operation is deletion, then a
legitimate line is an **ordered subsequence** of something the speaker said. That is a precise,
checkable property.

## The algorithm

For a candidate line and the ledger:

**1. Tokenize and canonicalize.** Both the candidate and each source span are lowercased,
punctuation-stripped, and passed through the lexicon so alias phrases become their canonical form.
Digits, dotted identifiers, and hyphenated words stay whole, because `owner/repo#412`, `v1.2.3`, and
`cherry-pick` are single things when someone says them.

**2. Build source spans.** Transcription splits on pauses, not on sentences, so one spoken sentence
routinely arrives as two or three utterances. Spans are every window of up to `windowSize`
consecutive utterances (default 4), so a line assembled across those boundaries still matches.

**3. Prune.** Multiset containment — how many of the candidate's meaningful tokens appear in the
span, counting repeats — upper-bounds the ordered match, so spans below the threshold are skipped
without any risk of a false rejection.

**4. Align.** Longest common subsequence between candidate and span. Order-sensitive by
construction, which is why shuffling words inside a sentence fails even though every word is
present.

**5. Score.** The ratio is *matched meaningful tokens over total meaningful tokens*. Connective words
listed in `freeTokens` ("the", "a", "and", "to") and filler are excluded from the denominator: they
carry no meaning of their own, so adding one to join two fragments is not a rewrite, and neither is
dropping one.

**6. Classify.**

| Kind | Meaning |
|---|---|
| `verbatim` | Exactly what they said |
| `trimmed` | Their words with filler, fragments, or sentences removed |
| `corrected` | As above, plus a lexicon spelling fix |
| `motif` | A saved standing instruction, itself captured verbatim |
| `derived` | Agent-authored — permitted only where the manifest allows it, currently titles only |

`corrected` is detected by running the alignment a second time on the best span without
canonicalization. The lexicon usually corrects the *source* rather than the candidate — the
transcriber wrote "get hub" and the agent wrote "GitHub" — so comparing the match with and without
it is what distinguishes a corrected line from a merely trimmed one.

A line at or above `threshold` (default 0.82) is accepted. Below it, the line is rejected and the
unmatched tokens are returned so the model can see exactly what it invented.

## Why the lexicon cannot be used to smuggle a paraphrase

The lexicon is applied to **both sides** of every comparison, so an alias lets two spellings of the
same word recognize each other.

That alone is not enough, because the agent can add aliases mid-session through `record_term`. An
alias that is not a mishearing but a different word would defeat the gate outright: install
`{canonical: "CSV", heardAs: ["database"]}` and an invented "CSV" line matches spoken "database".
So aliases arriving through `record_term` must be plausible mishearings of the canonical form —
close after collapsing to letters and digits. Curated vocabulary is exempt, because the seed and
workspace lexicons legitimately contain aliases that are not orthographically close, such as
"sequel" for SQL or "k8s" for Kubernetes.

With both rules in place, a bad alias costs a rejection rather than a false acceptance.

## Sections and modes

Grounding strictness is configured per section in `agent.json`:

- `intent`, `detail`, `acceptance`, `open_question` — `strict`
- `constraint` — `motif-or-strict`, so an attached motif passes on its own provenance
- Title — checked at a lower threshold (`titleThreshold`, default 0.5) and allowed to fall back to
  `derived`, because a title is a label rather than part of the request. Even so, it may not
  introduce vocabulary the speaker never used.

`open_question` is strict on purpose. It holds questions *the speaker* raised for the downstream
agent, not questions the agent has. If Riff has a question it should ask it out loud.

## What it deliberately does not catch

**Selection bias.** Choosing which of someone's sentences go in is editorial, and grounding does not
constrain it. This is intentional — selection is the job — but it means a determined model could
distort meaning by omission. Supersession history and per-line source ids on the artifact make that
auditable after the fact.

**Ambiguous single-token corrections.** If the lexicon maps a common English word to a technical
term, a legitimate use of that word can be silently "corrected". The seed lexicon avoids aliases
that are ordinary words for this reason.

**Truthfulness.** Grounding proves a line was said. It says nothing about whether it is correct.

**Wording from earlier sessions.** Only the current ledger is an authorized source, so text returned
by `recall_prompts` cannot be quoted into a prompt. The tool exists to orient the agent, not to
supply material. Carrying provenance across sessions is possible but is a larger change than the
gate itself.

## Fidelity

Each artifact carries a token-weighted fidelity score: the share of the prompt body provably drawn
from the ledger, with `derived` lines counted as zero. A normal captured prompt scores 1.0.

It is on the artifact so a consumer can judge a prompt without reading it, and so a regression in
capture quality shows up as a number rather than as a vague sense that prompts have gotten blander.

## Tuning

`agent.json` → `grounding`:

| Setting | Default | Effect |
|---|---|---|
| `threshold` | 0.82 | Lower accepts looser paraphrase; higher rejects legitimate trims |
| `titleThreshold` | 0.5 | Titles compress harder than body lines |
| `windowSize` | 4 | How far a line may reach across utterance boundaries |
| `freeTokens` | articles, conjunctions | Excluded from scoring in both directions |
| `filler` | "um", "like", … | Removable without counting as a rewrite |

Changing any of these is a behavioral change to the product. Add a conformance case with it.
