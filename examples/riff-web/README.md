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

That runs **scripted mode**, which needs no API key, no network, and no microphone.

```bash
OPENAI_API_KEY=sk-... npm run demo    # adds live mode
```

## The two modes

**Scripted** replays the conversation from the project README through a `ScriptedProvider`. It is
not a mockup. Every line goes into the real utterance ledger, every tool call is dispatched by the
real registry, and the prompt on screen is produced by the same engine a live model drives — the
script only stands in for the model deciding what to call. Swapping it for `OpenAIRealtimeProvider`
changes who is talking and nothing else, which is the provider seam doing its job.

**Live mic** opens a real WebRTC session. The microphone is published as a media track rather than
pushed through `sendAudio`, so the browser handles echo cancellation and jitter and this example
needs no audio code beyond `getUserMedia`. Speak, and the same panes fill in.

Nothing is ever sent anywhere. `DemoHost.submitPrompt` accepts the artifact and returns success
without delivering it, so the "sent" moment is real all the way through the render — it just has
nowhere to go. Point it at `GitHubHost` and it would.

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
server.mjs               static files, the agent bundle, and POST /api/riff/token
verify.mjs               the same script, headless, as an assertion
public/
  index.html             layout and the import map
  app.js                 session wiring and rendering
  app.css
  demo-script.js         the README conversation, as data
  scripted-provider.js   a RealtimeProvider that replays it
  demo-host.js           a small fixed world: one PR, one motif, a destination that goes nowhere
  observe-provider.js    a decorator that surfaces tool results to the UI
```

`mintClientSecret` runs in `server.mjs` and nowhere else. It is the one part of the provider that has
to stay server-side: an API key shipped to a browser is a key you have published. The browser only
ever receives a short-lived client secret.
