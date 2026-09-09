# Riff on a web page

The smallest thing that shows Riff actually working: a microphone, a live draft, and a prompt that gets sent.

There is no framework, no bundler, and no dependency beyond the repo itself. The Riff packages are dependency-free ESM, so the browser loads the compiled output directly and an import map resolves the one bare specifier between them. The server exists to serve files, mint a token, and hold credentials.

```bash
npm install && npm run build
npm run demo                     # http://localhost:4173
```

That runs **scripted mode** against a canned world, which needs no API key, no network, and no microphone.

## Keys

To use live mic mode, click **keys** in the header and add an OpenAI key, plus a GitHub token if you want real repository references. The page posts them once to the server on your machine, which holds them in memory for as long as it is running, checks each against the API it is for so a bad paste fails next to the field you typed it into, and never writes them anywhere or sends them back. Environment variables still work if you prefer them, and the panel says which source each credential came from. Keys never change the scripted demo's sample world.

```bash
OPENAI_API_KEY=sk-... npm run demo             # adds live mode
GITHUB_TOKEN=$(gh auth token) npm run demo     # adds real repository references to live mode
```

> [!IMPORTANT]
> Because this process holds credentials it listens on loopback only, and it refuses requests carrying a foreign `Origin`. A page on another origin can send a POST it cannot read the reply to, which would be enough to spend a stored token.

## The host

**Scripted mode** always uses [`demo-host.js`](public/demo-host.js): a sample Slack thread, two earlier prompts with recorded outcomes, a PR, a saved motif, and a destination that goes nowhere. These are fixtures, not Slack or session-history integrations.

**Live mode** uses [`GitHubHost`](../../packages/riff-github/src/host.ts) when a GitHub token is configured, so "the PR I just opened" resolves against a real repository, detected from the `origin` remote or typed into the keys panel. Without one, [`NullHost`](../../packages/riff-core/src/host.ts) resolves nothing and the agent asks instead of attaching fictional context. Live mode starts with no sample motifs.

**The real GitHub host runs on the server, not in the page.** A host is whatever the embedding application knows about the world, and it runs wherever the session runs — which here is the browser, so a GitHub-backed host in the page would mean a GitHub token in the page. That is the same mistake as shipping an API key to a browser. So [`proxy-host.js`](public/proxy-host.js) implements [`RiffHost`](../../docs/host-bridge.md) as five POSTs to [`server.mjs`](server.mjs), which holds the token and delegates to the real host. Riff cannot tell the difference, which is a fair demonstration that the seam is small enough to remote without ceremony.

Sending is always a **dry run** in scripted mode. In live mode with GitHub configured, *file a real issue* opts the next send into creating an issue. The control is hidden and cleared in scripted mode.

## The two modes

**Scripted** replays the conversation in the project [README](../../README.md#what-riff-does-instead) through [`scripted-provider.js`](public/scripted-provider.js). A riff about offline drafts resolves a Slack thread, recalls earlier attempts, asks which one the speaker meant, and attaches that session and its related PR. A tangent opens a separate prompt instead of contaminating the first. It is not a mockup: every spoken user line goes into the real utterance ledger, every tool call is dispatched by the real registry, and the prompt on screen is produced by the same engine a live model drives.

**Live mic** opens a real WebRTC session. The microphone is published as a media track rather than pushed through `sendAudio`, so the browser handles echo cancellation and jitter and this example needs no audio code beyond `getUserMedia`. Speak, and the same panes fill in. With both an OpenAI key and a GitHub token on the server there is a real conversation to have against a real repository, so the page opens on this mode instead of scripted — until you pick for yourself, after which it stays where you put it.

The script refers to sources by the words the speaker used, never by an id: whatever `resolve_reference` returns is filled into `attach_context` before the call goes out. Lookups stay silent while the activity pane shows the resolved references and recalled excerpts and outcomes. Only the clarification is spoken. Recalled text stays out of the utterance ledger and prompt body; a link to the selected session travels as separately labeled context.

## What you are looking at

| | |
|---|---|
| **What was said** | The utterance ledger. The only thing the prompt body may draw from. |
| **The prompt** | The draft. Each line is tinted by how it relates to what was said. |
| **What Riff did** | Tool calls, and what the engine sent back — including rejections. |

A session holds several unsent prompts, so a strip of them appears above the draft as soon as there is more than one. `live` is the one the next thing said lands in, `parked` is one set aside, `sent` is one that has gone. Which of them Riff writes into is decided by voice and nothing else — say the subject has changed and it opens a new one, say you want to go back and it switches. Clicking a prompt here only chooses which one is on screen, so a parked one can be read without becoming the one being spoken into.

Hover a draft line to highlight the utterances it came from, and hover an utterance to see which lines drew on it. That link is the whole argument: the prompt is not a summary of the conversation, it is a selection from it.

The colored chip on each line is its grounding kind — `verbatim`, `trimmed` (filler and false starts dropped), `corrected` (a word the transcriber got wrong), `motif` (a standing instruction), or `derived` (not the speaker's, and the only kind that costs fidelity). The ring in the header is the fidelity score carried on the artifact; [`grounding.md`](../../docs/grounding.md) explains how it is arrived at.

The script deliberately opens with a paraphrase — *"Implement offline-first persistence and automatic synchronization for draft content"* — so you can watch the grounding check throw it out and name the words that were invented, then watch the same claim go in using words that were actually said. Riff hides that exchange from the speaker. The demo shows it, because it is the part worth seeing.

Tool results are visible for the same reason. Riff's own event stream reports that a tool ran, not what it returned, which is right for a real embedding application. [`observe-provider.js`](public/observe-provider.js) wraps the provider to watch results go by, which works for both modes and needs no change to the engine.

## Checking it without a browser

```bash
npm run demo:verify
```

Runs the same script through a real [`RiffSession`](../../packages/riff-core/src/session.ts) in Node and asserts the dialogue and finished prompt match the main README, with the rendered prompt compared byte for byte. It also checks the three resolved sources, prompt recall, the grounding rejection, the separation of retrieved context from spoken input, the parked tangent, and the isolation of sample hosts and motifs from live mode. This is the fast way to find out that a change to the agent bundle, the grounding config, or a render profile has quietly broken the demo.

## Files

| | |
|---|---|
| [`server.mjs`](server.mjs) | Static files, the agent bundle, credentials, client secrets, and the GitHub host |
| [`verify.mjs`](verify.mjs) | The same script, headless, as an assertion |
| [`index.html`](public/index.html) | Layout and the import map |
| [`app.js`](public/app.js) | Session wiring and rendering |
| [`demo-script.js`](public/demo-script.js) | The main README conversation, as data |
| [`scripted-provider.js`](public/scripted-provider.js) | A `RealtimeProvider` that replays it |
| [`demo-host.js`](public/demo-host.js) | Sample Slack, session-history, GitHub, and motif data; a destination that goes nowhere |
| [`session-context.js`](public/session-context.js) | Host and store selection shared by the page and headless checks; sample data stays in scripted mode |
| [`proxy-host.js`](public/proxy-host.js) | A `RiffHost` that answers from the server, so no token reaches the page |
| [`observe-provider.js`](public/observe-provider.js) | A decorator that surfaces tool results to the UI |

`mintClientSecret` and the GitHub token both live in [`server.mjs`](server.mjs) and nowhere else. The browser only ever receives a short-lived client secret and answers to host questions.
