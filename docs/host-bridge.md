# The host bridge

A host is how Riff learns about your world. Riff knows how to keep a prompt in someone's voice; it
does not know that "the PR I just opened" means `acme/web#412`, that `flakeguard` is your retry
wrapper, or where a finished prompt should go.

## The interface

```ts
interface RiffHost {
  resolveReference(request): Promise<{ candidates: ContextItem[] }>;
  lookupTerm(request): Promise<{ matches: LexiconTerm[] }>;
  recallPrompts(request): Promise<{ prompts: PriorPrompt[] }>;
  submitPrompt(artifact, options): Promise<SubmitResult>;
  environment?(): Promise<HostEnvironment>;
}
```

Four required methods and one optional one. Everything else about Riff is fixed.

`NullHost` implements all of it by resolving nothing. Riff works with it — the agent simply asks
about references it cannot resolve instead of attaching them. That is the correct degraded behavior:
a fabricated PR number sends the downstream agent somewhere real and wrong, which is worse than an
unresolved reference someone gets asked about.

## `resolveReference`

The one that earns the most. It receives the referring phrase as spoken, along with what the agent
thinks it is, the implied time window, who it was attributed to, and recent transcript.

```ts
{ phrase: "the PR I just opened", kind: "pull_request", recency: "latest", actor: "me" }
```

Return candidates with a stable `referenceId`, a human-facing `identifier`, a `url`, and a
`confidence`. Return an empty list when nothing matches — the agent will ask rather than guess.

Three things the shipped GitHub host does that are worth copying:

**Prefer exact resolution to search.** A URL, `owner/repo#412`, or a bare number with a noun beside
it resolves with one request and no ambiguity.

**Handle spoken numbers.** Nobody says "hash" out loud. A transcript contains "PR 412", never
"#412". A host that only understands `#` will fail on the single most common reference people make.
Guard it: treat a bare number as an identifier only when the sentence or the `kind` hint says it is
one, or "it breaks past 1000 rows" becomes issue 1000.

**Resolve links before running any number heuristics.** A tracker or doc URL is full of digits and
often carries a `#fragment`, so shorthand and spoken-number matching will happily read
`https://linear.app/team/issue/ENG-4821` as issue 4821 of *your* repository and hand it to the model
with full confidence. Recognize a link first, resolve it if it is yours, record it untouched if it
is not.

**Do not use the pointing words as search terms.** "The PR I just opened" is a recency claim, not a
query. Searching for "opened" returns nothing useful. Strip the referring vocabulary and search only
what is left, if anything is.

## `lookupTerm`

Called before the agent asks someone to repeat a word. Riff checks its own lexicon first, so this is
for terms it has not learned yet.

Answer from a curated glossary where you have one — a workspace's own service names are more
reliable than any search. The GitHub host falls back to repository and user search, which is enough
to recognize a repo or a colleague.

Confidence matters here, and it has to survive back to the model. High confidence means the agent
applies the correction silently; low confidence on a load-bearing word means it asks. A host that
returns a fuzzy search guess without saying so will have that guess substituted into someone's
prompt as though it were confirmed.

## `recallPrompts`

Earlier prompts, for when someone refers back to a previous request, or when the agent needs to know
how they usually phrase this kind of thing. Their old wording is a legitimate source of their words.

Backing this with the same store used for `saveArtifact` is usually enough.

## `submitPrompt`

Where a finished prompt goes. The default should do nothing destructive: the shipped GitHub host
returns the prompt and reports `destination: "none"` unless a destination is configured, because
silently opening issues is not a good surprise.

Destinations are named, and the environment advertises them, so someone can say "send it to the
backlog" and have that mean something.

```ts
new GitHubHost({
  token,
  repository: "acme/web",
  destinations: [
    issueDestination({ repository: "acme/web", labels: ["riff"], default: true }),
    issueDestination({ id: "backlog", repository: "acme/planning" }),
  ],
});
```

## `environment`

Optional, and the highest-value method for how the conversation feels. It supplies ambient facts at
connect time — repository, branch, speaker, destinations, recently touched items, workspace
vocabulary — so that the agent does not open by asking which repo you mean.

Two things happen with it. `vocabulary` is folded into the lexicon and compiled into transcription
biasing, so your service names transcribe correctly from the first sentence. Everything else is
stated to the model as context.

That context goes to the model but **not** into the ledger. Environment facts are not things the
speaker said, so the grounding check will reject any attempt to quote them as though they were.

## Writing one

Anything world-shaped works: a Jira host, a Linear host, a filesystem host resolving "that file I
was just in", a design-tool host resolving "the frame Priya shared". The contract does not assume
software development.

Rules of thumb:

- **Never fabricate.** Empty beats wrong. The agent handles empty well.
- **Be fast.** These calls happen mid-conversation. Sub-second or the agent goes quiet at the wrong
  moment. Cache aggressively.
- **Fail soft.** A search that errors should return nothing, not throw. The shipped GitHub host
  swallows search failures for exactly this reason.
- **Return stable ids.** `referenceId` is how the agent attaches something later in the
  conversation.
- **Scope credentials narrowly.** A host runs with whatever token you give it.

## Storage

`RiffStore` is the other half, and it is what makes Riff improve with use.

```ts
interface RiffStore {
  loadLexicon(): Promise<LexiconTerm[]>;
  saveTerm(term: LexiconTerm): Promise<void>;
  listMotifs(): Promise<Motif[]>;
  saveMotif(motif: Motif): Promise<void>;
  retireMotif(id: string, at: string): Promise<void>;
  saveArtifact(artifact: PromptArtifact): Promise<void>;
  listArtifacts(query?: { limit?: number }): Promise<PromptArtifact[]>;
}
```

Learned corrections and saved motifs persist across sessions, which is the difference between a tool
that works and one that gets better every week. `MemoryStore` ships for tests and for applications
that genuinely want a session to leave nothing behind.
