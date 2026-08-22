# Riff on a web page

The smallest thing that shows Riff actually working: a microphone, a live draft, and a prompt that gets sent.

There is no framework, no bundler, and no dependency beyond the repo itself. The Riff packages are dependency-free ESM, so the browser loads the compiled output directly and an import map resolves the one bare specifier between them. The server exists to serve files, mint a token, and hold credentials.

```bash
npm install && npm run build
npm run demo                     # http://localhost:4173
```

That runs **scripted mode** against a canned world, which needs no API key, no network, and no microphone.

## Keys

To add live mic or a real repository, click **keys** in the header and paste them in. The page posts them once to the server on your machine, which holds them in memory for as long as it is running, checks each against the API it is for so a bad paste fails next to the field you typed it into, and never writes them anywhere or sends them back. Environment variables still work if you prefer them, and the panel says which source each credential came from.

```bash
OPENAI_API_KEY=sk-... npm run demo             # adds live mode
GITHUB_TOKEN=$(gh auth token) npm run demo     # resolves against a real repository
```

> [!IMPORTANT]
> Because this process holds credentials it listens on loopback only, and it refuses requests carrying a foreign `Origin`. A page on another origin can send a POST it cannot read the reply to, which would be enough to spend a stored token.

## The host

With no GitHub token, references resolve against [`demo-host.js`](public/demo-host.js) — one PR, one motif, a destination that goes nowhere. With one, [`GitHubHost`](../../packages/riff-github/src/host.ts) takes over and "the PR I just opened" resolves against a real repository, detected from the `origin` remote or typed into the keys panel.

**The host runs on the server, not in the page.** A host is whatever the embedding application knows about the world, and it runs wherever the session runs — which here is the browser, so a GitHub-backed host in the page would mean a GitHub token in the page. That is the same mistake as shipping an API key to a browser. So [`proxy-host.js`](public/proxy-host.js) implements [`RiffHost`](../../docs/host-bridge.md) as five POSTs to [`server.mjs`](server.mjs), which holds the token and delegates to the real host. Riff cannot tell the difference, which is a fair demonstration that the seam is small enough to remote without ceremony.

Sending is a **dry run** unless you tick *file a real issue*. The script ends by submitting, so without that every replay would open an issue. With it ticked, `submit_prompt` files a real one.

## The two modes

**Scripted** replays the conversation from the project [README](../../README.md) through [`scripted-provider.js`](public/scripted-provider.js). It is not a mockup. Every line goes into the real utterance ledger, every tool call is dispatched by the real registry, and the prompt on screen is produced by the same engine a live model drives — the script only stands in for the model deciding what to call. Swapping it for [`OpenAIRealtimeProvider`](../../packages/riff-openai-realtime/src/provider.ts) changes who is talking and nothing else, which is the provider seam doing its job.

**Live mic** opens a real WebRTC session. The microphone is published as a media track rather than pushed through `sendAudio`, so the browser handles echo cancellation and jitter and this example needs no audio code beyond `getUserMedia`. Speak, and the same panes fill in.

The script refers to a PR by the words the speaker used, never by an id, so one recording runs against either host: whatever `resolve_reference` returns is filled in before the call goes out, and Riff names what it actually found. Against the demo host it says *"that's 412, chunked uploads"*; against your repository it says whatever it really resolved.

## What you are looking at

| | |
|---|---|
| **What was said** | The utterance ledger. The only thing the prompt body may draw from. |
| **The prompt** | The draft. Each line is tinted by how it relates to what was said. |
| **What Riff did** | Tool calls, and what the engine sent back — including rejections. |

Hover a draft line to highlight the utterances it came from, and hover an utterance to see which lines drew on it. That link is the whole argument: the prompt is not a summary of the conversation, it is a selection from it.

The colored chip on each line is its grounding kind — `verbatim`, `trimmed` (filler and false starts dropped), `corrected` (a word the transcriber got wrong), `motif` (a standing instruction), or `derived` (not the speaker's, and the only kind that costs fidelity). The ring in the header is the fidelity score carried on the artifact; [`grounding.md`](../../docs/grounding.md) explains how it is arrived at.

The script deliberately opens with a paraphrase — *"The CSV export functionality fails silently for large result sets"* — so you can watch the grounding check throw it out and name the words that were invented, then watch the same claim go in using words that were actually said. Riff hides that exchange from the speaker. The demo shows it, because it is the part worth seeing.

Tool results are visible for the same reason. Riff's own event stream reports that a tool ran, not what it returned, which is right for a real embedding application. [`observe-provider.js`](public/observe-provider.js) wraps the provider to watch results go by, which works for both modes and needs no change to the engine.

## Checking it without a browser

```bash
npm run demo:verify
```

Runs the same script through a real [`RiffSession`](../../packages/riff-core/src/session.ts) in Node and asserts the finished prompt still matches the one in the project README, byte for byte. This is the fast way to find out that a change to the agent bundle, the grounding config, or a render profile has quietly broken the demo.

## Files

| | |
|---|---|
| [`server.mjs`](server.mjs) | Static files, the agent bundle, credentials, client secrets, and the GitHub host |
| [`verify.mjs`](verify.mjs) | The same script, headless, as an assertion |
| [`index.html`](public/index.html) | Layout and the import map |
| [`app.js`](public/app.js) | Session wiring and rendering |
| [`demo-script.js`](public/demo-script.js) | The README conversation, as data |
| [`scripted-provider.js`](public/scripted-provider.js) | A `RealtimeProvider` that replays it |
| [`demo-host.js`](public/demo-host.js) | A small fixed world: one PR, one motif, a destination that goes nowhere |
| [`proxy-host.js`](public/proxy-host.js) | A `RiffHost` that answers from the server, so no token reaches the page |
| [`observe-provider.js`](public/observe-provider.js) | A decorator that surfaces tool results to the UI |

`mintClientSecret` and the GitHub token both live in [`server.mjs`](server.mjs) and nowhere else. The browser only ever receives a short-lived client secret and answers to host questions.
