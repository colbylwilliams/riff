import { MemoryStore, RiffSession, SECTIONS, loadBundle } from "@riff/core";

import { DEMO_MOTIFS, DemoHost } from "./demo-host.js";
import {
  DEMO_SCRIPT,
  spokenReference,
  substituteReferences,
  substituteSpokenReference,
} from "./demo-script.js";
import { ProxyHost } from "./proxy-host.js";
import { ScriptedProvider } from "./scripted-provider.js";
import { observeProvider } from "./observe-provider.js";

const ui = {
  statePill: document.getElementById("state-pill"),
  takeLabel: document.getElementById("take-label"),
  ring: document.getElementById("ring-value"),
  fidelityNumber: document.getElementById("fidelity-number"),
  ledger: document.getElementById("ledger"),
  utteranceCount: document.getElementById("utterance-count"),
  pending: document.getElementById("pending"),
  draft: document.getElementById("draft"),
  markdown: document.getElementById("markdown"),
  toggleMarkdown: document.getElementById("toggle-markdown"),
  activity: document.getElementById("activity"),
  mic: document.getElementById("mic"),
  micLabel: document.getElementById("mic-label"),
  meter: document.getElementById("meter"),
  meterFill: document.getElementById("meter-fill"),
  interrupt: document.getElementById("interrupt"),
  typeForm: document.getElementById("type-form"),
  typeInput: document.getElementById("type-input"),
  liveMode: document.getElementById("live-mode"),
  issueMode: document.getElementById("issue-mode"),
  allowIssues: document.getElementById("allow-issues"),
  hostPill: document.getElementById("host-pill"),
  keys: document.getElementById("keys"),
  keysForm: document.getElementById("keys-form"),
  keysOpen: document.getElementById("open-keys"),
  keysClose: document.getElementById("keys-close"),
  keysForget: document.getElementById("keys-forget"),
  keysProblem: document.getElementById("keys-problem"),
  keysSave: document.getElementById("keys-save"),
  keysSaveLabel: document.getElementById("keys-save-label"),
  openaiKey: document.getElementById("openai-key"),
  openaiStatus: document.getElementById("openai-status"),
  githubKey: document.getElementById("github-key"),
  githubStatus: document.getElementById("github-status"),
  githubRepo: document.getElementById("github-repo"),
  agentAudio: document.getElementById("agent-audio"),
  sent: document.getElementById("sent"),
  sentTitle: document.getElementById("sent-title"),
  sentBody: document.getElementById("sent-body"),
  sentDestination: document.getElementById("sent-destination"),
  sentFidelity: document.getElementById("sent-fidelity"),
  sentUtterances: document.getElementById("sent-utterances"),
  sentNote: document.getElementById("sent-note"),
  sentClose: document.getElementById("sent-close"),
};

const RING_CIRCUMFERENCE = 2 * Math.PI * 18;
const KIND_COLORS = {
  verbatim: "var(--verbatim)",
  trimmed: "var(--trimmed)",
  corrected: "var(--corrected)",
  motif: "var(--motif)",
  derived: "var(--derived)",
};

const bundle = loadBundle(await (await fetch("/riff-agent.bundle.json")).json());

/** Everything torn down and rebuilt each time the mic is started. */
let run = null;
/**
 * Which run is current.
 *
 * Starting and stopping are both asynchronous, so a teardown can still be in flight when the next
 * session starts. Without a way to tell whose continuation is running, the old one's tail resets the
 * chrome for the new session and the old session's events keep painting the panes after it closed.
 */
let runSequence = 0;
let lastArtifact = null;
let lastSubmission = null;
let agentEntry = null;
let liveAvailable = false;
let githubAvailable = false;
/** The id the host gave the last thing it resolved, so the script can attach what was found. */
let lastReferenceId = null;
let lastReferenceSpoken = null;

/**
 * GitHub when the server has a token, the canned world otherwise.
 *
 * The proxy is a real `RiffHost` as far as the session is concerned; it just answers from the other
 * end of a fetch, which is what keeps the GitHub token off this page.
 */
function buildHost() {
  return githubAvailable
    ? new ProxyHost({ settings: () => ({ allowIssues: ui.allowIssues.checked }) })
    : new DemoHost();
}

/* ── running a session ───────────────────────────────────────────────────── */

async function start() {
  const mode = document.querySelector('input[name="mode"]:checked').value;
  const id = ++runSequence;
  reset();
  setMicBusy(true);

  let started;
  try {
    started = mode === "live" ? await buildLive() : buildScripted();
  } catch (error) {
    // A refused microphone or a missing key should read as a normal outcome, not a dead page.
    addEntry({ head: `could not start ${mode} mode`, note: error.message, variant: "rejected" });
    setMicBusy(false);
    return;
  }

  // Building live mode awaits the microphone, which is long enough for someone to have given up and
  // started something else. Whatever was built is then already obsolete.
  if (id !== runSequence) return teardown(started);

  started.id = id;
  run = started;
  started.session.on((event) => {
    if (started.id === runSequence) handleEvent(event);
  });

  try {
    await started.session.start();
  } catch (error) {
    addEntry({ head: "connection failed", note: error.message, variant: "rejected" });
    await stop();
    return;
  }

  setMicRunning(true);
  ui.typeInput.disabled = false;

  if (started.scripted) {
    await started.scripted.run();
    // Reaching the end of the script ends that session — but only if it is still the current one.
    if (started.id === runSequence) await stop();
  }
}

async function stop() {
  const current = run;
  run = null;
  if (!current) return;
  await teardown(current);

  // Skipped when something newer started while this was tearing down, so a slow stop cannot reset
  // the chrome out from under the session that replaced it.
  if (run) return;
  setMicRunning(false);
  setMicBusy(false);
  ui.typeInput.disabled = true;
  ui.interrupt.hidden = true;
  ui.meter.hidden = true;
  ui.pending.hidden = true;
}

/** Releases everything one run holds: the script, the meter, the microphone, the session. */
async function teardown(current) {
  current.scripted?.stop();
  current.stopMeter?.();
  for (const track of current.stream?.getTracks() ?? []) track.stop();
  await current.session.stop("stopped");
  await current.audioContext?.close().catch(() => {});
}

function buildScripted() {
  const scripted = new ScriptedProvider({
    script: DEMO_SCRIPT,
    speed: 1,
    onStep: (step, phase) => {
      if (step.user) ui.pending.hidden = phase !== "begin";
      if (step.note && phase === "begin") addEntry({ head: "…", note: step.note, variant: "said" });
    },
    // The script names a PR by the words used, not by an id, so whatever the host called the thing
    // it found is filled in here. That is what lets one script run against either world.
    prepareArgs: (_name, args) => substituteReferences(args, lastReferenceId),
    prepareText: (text) => substituteSpokenReference(text, lastReferenceSpoken),
  });

  return {
    scripted,
    session: new RiffSession({
      bundle,
      host: buildHost(),
      provider: observeProvider(scripted, { onToolResult: handleToolResult }),
      store: new MemoryStore({ motifs: DEMO_MOTIFS }),
    }),
  };
}

/**
 * The live path.
 *
 * On WebRTC the microphone is a media track handed to the peer connection rather than PCM pushed
 * through `sendAudio`, so the browser's own echo cancellation and jitter buffering do the work that
 * would otherwise be an audio worklet in this file.
 */
async function buildLive() {
  const { OpenAIRealtimeProvider, clientSecretCredentials } = await import("@riff/openai-realtime");

  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
  });
  const peer = new RTCPeerConnection();

  const provider = new OpenAIRealtimeProvider({
    transport: "webrtc",
    credentials: clientSecretCredentials(async () => {
      const response = await fetch("/api/riff/token", { method: "POST" });
      if (!response.ok) throw new Error(`token endpoint said ${response.status}: ${await response.text()}`);
      return response.json();
    }),
    webrtc: {
      peerConnection: () => peer,
      tracks: stream.getAudioTracks(),
      onRemoteTrack: (track, streams) => {
        ui.agentAudio.srcObject = streams[0] ?? new MediaStream([track]);
      },
    },
  });

  const { stopMeter, audioContext } = startMeter(stream);

  return {
    stream,
    peer,
    stopMeter,
    audioContext,
    session: new RiffSession({
      bundle,
      host: buildHost(),
      provider: observeProvider(provider, { onToolResult: handleToolResult }),
      store: new MemoryStore({ motifs: DEMO_MOTIFS }),
    }),
  };
}

/* ── events ──────────────────────────────────────────────────────────────── */

function handleEvent(event) {
  switch (event.type) {
    case "state":
      ui.statePill.textContent = event.state;
      ui.statePill.dataset.state = event.state;
      ui.interrupt.hidden = event.state !== "speaking" && event.state !== "thinking";
      break;

    case "utterance":
      addUtterance(event.utterance);
      break;

    case "draft":
      setFidelity(event.fidelity);
      renderDraft(run?.session.artifact());
      break;

    case "take":
      ui.takeLabel.textContent = event.takeId ? `take ${event.takeId}` : "";
      break;

    case "agent.transcript":
      showAgentTranscript(event.text, event.final);
      break;

    case "interrupted":
      addEntry({ head: "interrupted", note: "they started talking over it", variant: "said" });
      break;

    case "closed":
      // A session can end without anyone pressing Stop — a dropped data channel, or the model
      // hanging up. Without this the run stays installed and the microphone keeps capturing after
      // the conversation is over. Re-entrant when `stop()` closes the session itself, which is
      // harmless: `run` is already null by then and `stop()` returns immediately.
      void stop();
      break;

    case "submitted":
      lastArtifact = event.artifact;
      renderDraft(event.artifact);
      setFidelity(event.artifact.provenance.fidelity);
      showSent();
      break;

    case "error":
      addEntry({ head: event.error.code, note: event.error.message, variant: "rejected" });
      break;

    default:
      break;
  }
}

/**
 * Tool results, seen through the provider decorator.
 *
 * This is where a rejected line shows up. Riff hides it from the speaker on purpose; the demo shows
 * it, because a paraphrase being thrown out is the clearest evidence the gate is real.
 */
function handleToolResult({ name, args, result }) {
  for (const { text, rejection } of matchRejections(args, result?.rejected ?? [])) {
    addEntry({
      head: `${name} · rejected`,
      quote: text,
      note: rejection.reason,
      variant: "rejected",
    });
  }

  if (name === "resolve_reference") {
    const found = result?.candidates?.[0];
    lastReferenceId = found?.reference_id ?? null;
    lastReferenceSpoken = found ? spokenReference(found) : null;
    addEntry({
      head: "resolve_reference",
      note: found
        ? `"${args.phrase}" → ${found.identifier ?? found.title}`
        : `"${args.phrase}" → nothing found, so it will ask`,
    });
    return;
  }

  if (name === "submit_prompt") {
    lastSubmission = result;
    addEntry({ head: "submit_prompt", note: result?.message ?? "sent" });
    showSent();
    return;
  }

  const accepted = result?.accepted?.length ?? 0;
  if (accepted > 0) {
    addEntry({ head: name, note: `${accepted} operation${accepted === 1 ? "" : "s"} accepted` });
  } else if (result?.attached) {
    addEntry({ head: "motifs · attach", note: "a standing instruction, in their words" });
  }
}

/**
 * Pairs each rejection with the text that was turned away.
 *
 * A rejected `upsert_line` carries no line id when it was trying to create one, so the operation has
 * to be found by shape. Matched operations are consumed, or two rejections in the same batch would
 * both point at the first one.
 */
function matchRejections(args, rejected) {
  const operations = [...(args?.operations ?? [])];

  return rejected.map((rejection) => {
    const index = operations.findIndex((operation) =>
      rejection.lineId ? operation.line_id === rejection.lineId : operation.op === rejection.op,
    );
    const operation = index === -1 ? undefined : operations.splice(index, 1)[0];
    return { rejection, text: operation?.text ?? "" };
  });
}

/* ── rendering ───────────────────────────────────────────────────────────── */

function addUtterance(utterance) {
  const item = document.createElement("li");
  item.className = "utterance";
  item.dataset.utteranceId = utterance.id;
  // The cross-link is the point of this pane, so it has to be reachable without a mouse.
  item.tabIndex = 0;

  const head = document.createElement("div");
  head.className = "utterance-head";
  head.append(span(utterance.id), span(utterance.source));
  item.append(head, span(utterance.text));

  const show = () => highlightFromUtterance(utterance.id);
  item.addEventListener("mouseenter", show);
  item.addEventListener("focus", show);
  item.addEventListener("mouseleave", clearHighlights);
  item.addEventListener("blur", clearHighlights);

  ui.ledger.append(item);
  ui.utteranceCount.textContent = ui.ledger.children.length;
  item.scrollIntoView({ block: "nearest", behavior: "smooth" });
}

function renderDraft(artifact) {
  if (artifact) lastArtifact = artifact;
  const current = artifact ?? lastArtifact;

  ui.draft.replaceChildren();
  ui.markdown.textContent = current?.rendered ?? "";

  if (!current || current.lines.length === 0) {
    ui.draft.append(paragraph("Nothing captured yet.", "hint"));
    return;
  }

  const title = document.createElement("h3");
  title.className = "draft-title";
  title.textContent = current.title.text;
  ui.draft.append(title);

  for (const section of SECTIONS) {
    const lines = current.lines.filter((line) => line.section === section);
    if (lines.length === 0) continue;

    const block = document.createElement("div");
    block.className = "draft-section";

    const heading = document.createElement("h3");
    heading.textContent = bundle.render.labels[section] ?? section.replace(/_/g, " ");
    block.append(heading);

    const list = document.createElement("ul");
    for (const line of lines) list.append(renderLine(line));
    block.append(list);
    ui.draft.append(block);
  }

  if (current.context.length > 0) {
    const context = document.createElement("div");
    context.className = "context";
    for (const item of current.context) context.append(renderContext(item));
    ui.draft.append(context);
  }
}

function renderLine(line) {
  const item = document.createElement("li");
  item.className = "line";
  item.dataset.kind = line.grounding.kind;
  item.dataset.sources = line.sourceUtteranceIds.join(" ");
  item.tabIndex = 0;

  const chip = document.createElement("span");
  chip.className = `chip chip--${line.grounding.kind}`;
  chip.textContent =
    line.grounding.kind === "motif"
      ? "motif"
      : `${line.grounding.kind} ${Math.round(line.grounding.ratio * 100)}%`;

  item.append(span(line.text, "line-text"), chip);

  const show = () => highlightFromLine(item);
  item.addEventListener("mouseenter", show);
  item.addEventListener("focus", show);
  item.addEventListener("mouseleave", clearHighlights);
  item.addEventListener("blur", clearHighlights);
  return item;
}

function renderContext(item) {
  const chip = document.createElement(item.url ? "a" : "span");
  chip.className = "context-chip";
  if (item.url) {
    chip.href = item.url;
    chip.target = "_blank";
    chip.rel = "noreferrer";
  }

  chip.append(span(item.identifier ?? item.title));
  if (item.resolvedFrom) {
    const note = document.createElement("small");
    note.textContent = `“${item.resolvedFrom}”`;
    chip.append(note);
  }
  return chip;
}

function showAgentTranscript(text, final) {
  // The note element is held onto rather than queried back, because an empty first delta would
  // leave nothing to query and the rest of the sentence would never appear.
  if (!agentEntry) {
    const note = paragraph(text, "entry-note");
    agentEntry = { entry: addEntry({ head: "Riff", variant: "agent" }), note };
    agentEntry.entry.append(note);
  } else {
    agentEntry.note.textContent = text;
  }
  if (final) agentEntry = null;
}

function addEntry({ head, quote, note, variant }) {
  const item = document.createElement("li");
  item.className = variant ? `entry entry--${variant}` : "entry";

  const heading = document.createElement("div");
  heading.className = "entry-head";
  heading.textContent = head;
  item.append(heading);

  if (quote) item.append(paragraph(`“${quote}”`, "entry-quote"));
  if (note) item.append(paragraph(note, "entry-note"));

  ui.activity.append(item);
  item.scrollIntoView({ block: "nearest", behavior: "smooth" });
  return item;
}

function setFidelity(fidelity) {
  ui.ring.style.strokeDashoffset = String(RING_CIRCUMFERENCE * (1 - fidelity));
  ui.ring.style.stroke = fidelity === 1 ? KIND_COLORS.verbatim : KIND_COLORS.corrected;
  ui.fidelityNumber.textContent = `${Math.round(fidelity * 100)}%`;
}

function showSent() {
  if (!lastArtifact) return;
  ui.sentTitle.textContent = lastArtifact.title.text;
  ui.sentBody.textContent = lastArtifact.rendered;
  ui.sentFidelity.textContent = `${Math.round(lastArtifact.provenance.fidelity * 100)}%`;
  ui.sentUtterances.textContent = `${lastArtifact.provenance.utteranceCount} utterances`;
  ui.sentDestination.textContent = lastSubmission?.destination ?? "…";

  // Telling someone nothing was delivered when an issue was in fact filed invites them to send it
  // again, so this says what the host reported rather than what the demo usually does.
  const url = lastSubmission?.url;
  ui.sentNote.textContent = url
    ? `Delivered to ${url}`
    : (lastSubmission?.message ??
      "This is the prompt that would have been delivered. The host accepted it and sent it nowhere.");

  if (ui.sent.hidden) openSheet(ui.sent);
}

/* ── cross-highlighting ──────────────────────────────────────────────────── */

function highlightFromLine(item) {
  const sources = new Set(item.dataset.sources.split(" ").filter(Boolean));
  for (const row of ui.ledger.children) {
    row.classList.toggle("is-source", sources.has(row.dataset.utteranceId));
  }
}

function highlightFromUtterance(utteranceId) {
  for (const line of ui.draft.querySelectorAll(".line")) {
    const matched = line.dataset.sources.split(" ").includes(utteranceId);
    line.style.background = matched ? "var(--panel-soft)" : "";
  }
  for (const row of ui.ledger.children) {
    row.classList.toggle("is-source", row.dataset.utteranceId === utteranceId);
  }
}

function clearHighlights() {
  for (const row of ui.ledger.children) row.classList.remove("is-source");
  for (const line of ui.draft.querySelectorAll(".line")) line.style.background = "";
}

/* ── microphone level ────────────────────────────────────────────────────── */

function startMeter(stream) {
  const audioContext = new AudioContext();
  const analyser = audioContext.createAnalyser();
  analyser.fftSize = 512;
  audioContext.createMediaStreamSource(stream).connect(analyser);

  const samples = new Uint8Array(analyser.frequencyBinCount);
  let frame = 0;

  ui.meter.hidden = false;
  const tick = () => {
    analyser.getByteTimeDomainData(samples);
    let sum = 0;
    for (const sample of samples) sum += (sample - 128) ** 2;
    const level = Math.min(1, Math.sqrt(sum / samples.length) / 40);
    ui.meterFill.style.width = `${level * 100}%`;
    frame = requestAnimationFrame(tick);
  };
  frame = requestAnimationFrame(tick);

  return { audioContext, stopMeter: () => cancelAnimationFrame(frame) };
}

/* ── credentials ─────────────────────────────────────────────────────────── */

/**
 * Reflects what the server can currently do.
 *
 * Called at load and again after keys change, so pasting one takes effect without a reload. It only
 * ever reads whether a credential is present — the values stay on the server.
 */
function applyConfig(config) {
  liveAvailable = Boolean(config.live);
  githubAvailable = Boolean(config.github?.available);

  const liveInput = ui.liveMode.querySelector("input");
  liveInput.disabled = !liveAvailable;
  ui.liveMode.toggleAttribute("aria-disabled", !liveAvailable);
  ui.liveMode.title = liveAvailable
    ? "Talk to it for real"
    : "Add an OpenAI API key under keys to enable live mode";

  // Falling back without saying so would look like the key had been forgotten.
  if (!liveAvailable && liveInput.checked) {
    document.querySelector('input[value="scripted"]').checked = true;
    ui.micLabel.textContent = defaultMicLabel();
  }

  const repository = config.github?.repository;
  ui.hostPill.textContent = githubAvailable ? `host: ${repository}` : "host: demo";
  ui.hostPill.title = githubAvailable
    ? `References resolve against ${repository}, through the server so the token stays there`
    : "A canned world: one PR, one motif. Add a GitHub token under keys to use a real repository";

  // Sending is a side effect someone can see, so filing for real is opt-in every time.
  ui.issueMode.hidden = !githubAvailable;
  if (githubAvailable) {
    ui.issueMode.title = `Off, sending is a dry run. On, it files an issue in ${repository}`;
  } else {
    ui.allowIssues.checked = false;
  }

  describeCredential(ui.openaiStatus, config.openai?.source);
  describeCredential(ui.githubStatus, config.github?.source, config.github?.login);
  if (repository) ui.githubRepo.placeholder = repository;
}

function describeCredential(element, source, login) {
  const set = Boolean(source);
  element.dataset.set = String(set);
  element.textContent = set
    ? `${login ? `${login}, ` : ""}from the ${source === "pasted" ? "page" : "environment"}`
    : "not set";
}

async function saveKeys(body) {
  ui.keysProblem.hidden = true;
  ui.keysSave.disabled = true;
  ui.keysSaveLabel.textContent = "Checking…";

  try {
    const response = await fetch("/api/riff/credentials", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    const result = await response.json();
    applyConfig(result);

    if (result.problems?.length) {
      ui.keysProblem.textContent = result.problems.join("\n");
      ui.keysProblem.hidden = false;
      return false;
    }

    // Only clear the inputs once the server has taken them, so a rejected key can be corrected.
    ui.openaiKey.value = "";
    ui.githubKey.value = "";
    return true;
  } catch (error) {
    ui.keysProblem.textContent = error.message;
    ui.keysProblem.hidden = false;
    return false;
  } finally {
    ui.keysSave.disabled = false;
    ui.keysSaveLabel.textContent = "Save";
  }
}


/* ── sheets ──────────────────────────────────────────────────────────────── */

/**
 * Opening a sheet leaves focus behind it otherwise, so a keyboard is still driving the controls
 * underneath something that has covered them, and assistive technology is never told it appeared.
 *
 * The rest of the page is made `inert` for the duration. Without that, `aria-modal` is a claim the
 * page does not honor — Tab walks straight out of the dialog and operates what it is covering.
 */
let focusBeforeSheet = null;
let openSheetElement = null;

function openSheet(sheet) {
  // Only one sheet is ever the modal one; a second opening over the first would otherwise leave it
  // visible but inert, which looks like the page has frozen.
  if (openSheetElement && openSheetElement !== sheet) closeSheet(openSheetElement);

  focusBeforeSheet = document.activeElement;
  openSheetElement = sheet;
  sheet.hidden = false;

  for (const sibling of document.body.children) {
    if (sibling !== sheet) sibling.inert = true;
  }

  // `querySelector` walks the document, not the selector list, so without a marker focus lands on
  // whichever control comes first in the markup rather than the one worth starting on.
  const target =
    sheet.querySelector("[data-autofocus]") ?? sheet.querySelector("input:not([disabled]), button");
  target?.focus();
}

function closeSheet(sheet) {
  sheet.hidden = true;
  if (openSheetElement !== sheet) return;

  // Leaving these set would make the whole page unusable, so it happens on every close path.
  for (const sibling of document.body.children) sibling.inert = false;
  openSheetElement = null;
  focusBeforeSheet?.focus?.();
  focusBeforeSheet = null;
}

/* ── chrome ──────────────────────────────────────────────────────────────── */

function reset() {
  lastArtifact = null;
  lastSubmission = null;
  lastReferenceId = null;
  lastReferenceSpoken = null;
  agentEntry = null;
  ui.ledger.replaceChildren();
  ui.activity.replaceChildren();
  ui.utteranceCount.textContent = "0";
  closeSheet(ui.sent);
  setFidelity(0);
  renderDraft(null);
}

function setMicRunning(running) {
  ui.mic.classList.toggle("is-running", running);
  ui.mic.disabled = false;
  ui.micLabel.textContent = running ? "Stop" : defaultMicLabel();
  // A session holds the host it was built with, so changing credentials underneath it would move
  // the pill to a repository the conversation is not actually talking to.
  setKeysAvailable(!running);
  // Live mode stays off without a key on the server, so it is not simply the inverse of `running`.
  for (const input of document.querySelectorAll('input[name="mode"]')) {
    input.disabled = running || (input.value === "live" && !liveAvailable);
  }
}

function setMicBusy(busy) {
  ui.mic.disabled = busy;
  // `busy` is the window between choosing a host and having a session; `run` is after. Keys are
  // unavailable for both, and this path also has to re-enable them when a start fails outright.
  setKeysAvailable(!busy && !run);
  if (busy) ui.micLabel.textContent = "Starting…";
  else if (!run) ui.micLabel.textContent = defaultMicLabel();
}

function setKeysAvailable(available) {
  ui.keysOpen.disabled = !available;
  ui.keysOpen.title = available ? "" : "Stop the session to change keys";
}

function defaultMicLabel() {
  const mode = document.querySelector('input[name="mode"]:checked').value;
  return mode === "live" ? "Start talking" : "Play the script";
}

function span(text, className) {
  const element = document.createElement("span");
  if (className) element.className = className;
  element.textContent = text;
  return element;
}

function paragraph(text, className) {
  const element = document.createElement("p");
  element.className = className;
  element.textContent = text;
  return element;
}

ui.mic.addEventListener("click", () => (run ? stop() : start()));
ui.interrupt.addEventListener("click", () => run?.session.interrupt());
ui.sentClose.addEventListener("click", () => closeSheet(ui.sent));

ui.keysOpen.addEventListener("click", () => openSheet(ui.keys));
ui.keysClose.addEventListener("click", () => closeSheet(ui.keys));
ui.keys.addEventListener("click", (event) => {
  if (event.target === ui.keys) closeSheet(ui.keys);
});

ui.keysForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  // Only fields that were filled in are sent. An empty one means "leave it alone" — sending it
  // would clear the value, and an empty repository would silently retarget the host at the
  // checkout's own remote. Forgetting a credential is what the Forget button is for.
  const body = {};
  if (ui.openaiKey.value.trim()) body.openaiApiKey = ui.openaiKey.value;
  if (ui.githubKey.value.trim()) body.githubToken = ui.githubKey.value;
  if (ui.githubRepo.value.trim()) body.repository = ui.githubRepo.value;

  if (await saveKeys(body)) closeSheet(ui.keys);
});

ui.keysForget.addEventListener("click", () =>
  saveKeys({ openaiApiKey: "", githubToken: "", repository: "" }),
);
ui.toggleMarkdown.addEventListener("click", () => {
  const showingMarkdown = ui.markdown.hidden;
  ui.markdown.hidden = !showingMarkdown;
  ui.draft.hidden = showingMarkdown;
  ui.toggleMarkdown.textContent = showingMarkdown ? "lines" : "markdown";
});

ui.typeForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const text = ui.typeInput.value.trim();
  if (!text || !run) return;
  run.session.sendText(text);
  ui.typeInput.value = "";
});

for (const input of document.querySelectorAll('input[name="mode"]')) {
  input.addEventListener("change", () => (ui.micLabel.textContent = defaultMicLabel()));
}

// Live mode needs a key on the server, so say so up front rather than failing on the first click.
const config = await fetch("/api/riff/config")
  .then((response) => response.json())
  .catch(() => ({ live: false }));

applyConfig(config);
renderDraft(null);
setFidelity(0);
