# Riff on a web page

The smallest thing that shows Riff actually working: a microphone, a live draft, and a prompt that
gets sent.

There is no framework, no bundler, and no dependency beyond the repo itself. The Riff packages are
dependency-free ESM, so the browser loads the compiled output directly and an import map resolves
the one bare specifier between them. The server exists to serve files and mint a token.

```bash
npm install && npm run build
npm run demo                     # http://localhost:4173
```

That runs **scripted mode** against a canned world, which needs no API key, no network, and no
microphone.

To add live mic or a real repository, click **keys** in the header and paste them in. The page posts
them once to the server on your machine, which holds them in memory for as long as it is running,
checks each one against the API it is for so a bad paste fails next to the field you typed it into,
and never writes them anywhere or sends them back. Environment variables still work if you prefer
them, and the page says which source a credential came from:

```bash
OPENAI_API_KEY=sk-... npm run demo             # adds live mode
GITHUB_TOKEN=$(gh auth token) npm run demo     # resolves against a real repository
```

The server listens on loopback only and refuses cross-origin requests, because it holds credentials
and a page on another origin could otherwise spend them without ever reading the reply.

## The host

With no GitHub token, references resolve against `DemoHost` — one PR, one motif, a destination that
goes nowhere. With one, `GitHubHost` takes over and "the PR I just opened" resolves against a real
repository, detected from the `origin` remote or typed into the keys panel.

**The host runs on the server, not in the page.** A host is whatever the embedding application knows
about the world, and it runs wherever the session runs — which here is the browser, so a
GitHub-backed host in the page would mean a GitHub token in the page. That is the same mistake as
shipping an API key to a browser. So `proxy-host.js` implements `RiffHost` as five POSTs to
`server.mjs`, which holds the token and delegates to the real `GitHubHost`. Riff cannot tell the
difference, which is a fair demonstration that the seam is small enough to remote without ceremony.

Sending is a **dry run** unless you tick *file a real issue*. The script ends by submitting, so
without that every replay would open an issue. With it ticked, `submit_prompt` files a real one and
the sheet shows its URL.

## The two modes

**Scripted** replays the conversation from the project README through a `ScriptedProvider`. It is
not a mockup. Every line goes into the real utterance ledger, every tool call is dispatched by the
real registry, and the prompt on screen is produced by the same engine a live model drives — the
script only stands in for the model deciding what to call. Swapping it for `OpenAIRealtimeProvider`
changes who is talking and nothing else, which is the provider seam doing its job.

**Live mic** opens a real WebRTC session. The microphone is published as a media track rather than
pushed through `sendAudio`, so the browser handles echo cancellation and jitter and this example
needs no audio code beyond `getUserMedia`. Speak, and the same panes fill in.

Nothing is ever sent anywhere by default. `DemoHost.submitPrompt` accepts the artifact and returns
success without delivering it, and the GitHub host's default destination is a dry run, so the "sent"
moment is real all the way through the render — it just has nowhere to go until you say otherwise.

The script refers to a PR by the words the speaker used, never by an id, so the same recording runs
against either host: whatever `resolve_reference` returns is filled in before the call goes out, and
Riff names what it actually found. Against `DemoHost` it says *"that's 412, chunked uploads"*;
against your repository it says whatever it really resolved.

## What you are looking at

| | |
|---|---|
| **What was said** | The utterance ledger. The only thing the prompt body may draw from. |
| **The prompt** | The draft. Each line is tinted by how it relates to what was said. |
| **What Riff did** | Tool calls, and what the engine sent back — including rejections. |

Hover a draft line to highlight the utterances it came from, and hover an utterance to see which
lines drew on it. That link is the whole argument: the prompt is not a summary of the conversation,
it is a selection from it.

The colored chip on each line is its grounding kind — `verbatim`, `trimmed` (filler and false starts
dropped), `corrected` (a word the transcriber got wrong), `motif` (a standing instruction), or
`derived` (not the speaker's, and the only kind that costs fidelity). The ring in the header is the
fidelity score carried on the artifact.

The script deliberately opens with a paraphrase — *"The CSV export functionality fails silently for
large result sets"* — so you can watch the grounding check throw it out and name the words that were
invented, then watch the same claim go in using words that were actually said. Riff hides that
exchange from the speaker. The demo shows it, because it is the part worth seeing.

`submit_prompt` results are visible for the same reason. Riff's own event stream reports that a tool
ran, not what it returned, which is right for a real embedding application. `observe-provider.js`
wraps the provider to watch tool results go by, which works for both modes and needs no change to
the engine.

## Checking it without a browser

```bash
npm run demo:verify
```

Runs the same script through a real `RiffSession` in Node and asserts the finished prompt still
matches the one in the project README, byte for byte. This is the fast way to find out that a change
to the agent bundle, the grounding config, or a render profile has quietly broken the demo.

## Files

```
server.mjs               static files, the agent bundle, credentials, tokens, and the GitHub host
verify.mjs               the same script, headless, as an assertion
public/
  index.html             layout and the import map
  app.js                 session wiring and rendering
  app.css
  demo-script.js         the README conversation, as data
  scripted-provider.js   a RealtimeProvider that replays it
  demo-host.js           a small fixed world: one PR, one motif, a destination that goes nowhere
  proxy-host.js          a RiffHost that answers from the server, so no token reaches the page
  observe-provider.js    a decorator that surfaces tool results to the UI
```

`mintClientSecret` and the GitHub token both live in `server.mjs` and nowhere else. The browser only
ever receives a short-lived client secret and answers to host questions.
