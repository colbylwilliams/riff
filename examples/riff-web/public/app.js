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
  ring: document.getElementById("ring-value"),
  fidelityNumber: document.getElementById("fidelity-number"),
  ledger: document.getElementById("ledger"),
  utteranceCount: document.getElementById("utterance-count"),
  pending: document.getElementById("pending"),
  takes: document.getElementById("takes"),
  takesHint: document.getElementById("takes-hint"),
  promptView: document.getElementById("prompt-view"),
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
/**
 * The session whose prompts are on screen.
 *
 * Held past the end of a run, unlike `run`. A closed session still knows everything it drafted, and
 * dropping it on stop would empty the pane the moment someone clicked one of the prompts to read it.
 */
let shown = null;
/**
 * Which take the prompt pane is showing, and whether the viewer chose it.
 *
 * A session holds several unsent prompts, so "the draft" is a choice. By default the pane follows
 * whichever one Riff is writing into. Opening one to read pins it, so a draft update landing in
 * another prompt does not yank the pane away mid-read — but an explicit change of take, which is the
 * speaker saying which prompt they are on, takes the pane with it and releases the pin.
 *
 * Deciding which take is being written into is Riff's job, from what it hears. This is only about
 * what is on screen.
 */
let viewingTakeId = null;
let pinnedTakeId = null;
/** Set once someone picks a mode, so a later key change does not move the selection under them. */
let modeChosen = false;
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
    started = mode === "live" ? await buildLive(id) : buildScripted(id);
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
  shown = started.session;
  started.session.on((event) => {
    if (id === runSequence) handleEvent(event);
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

function buildScripted(id) {
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
      provider: observeProvider(scripted, { onToolResult: guardedToolResult(id) }),
      store: new MemoryStore({ motifs: DEMO_MOTIFS }),
    }),
  };
}

/**
 * Tool results carry the same staleness risk as session events.
 *
 * Stopping a session does not cancel a dispatch already in flight, so a slow host call from the run
 * someone just abandoned can land mid-way through the next one and set its reference id, its
 * activity, or its submission. The run guard has to cover this path too.
 */
function guardedToolResult(id) {
  return (entry) => {
    if (id === runSequence) handleToolResult(entry);
  };
}

/**
 * The live path.
 *
 * On WebRTC the microphone is a media track handed to the peer connection rather than PCM pushed
 * through `sendAudio`, so the browser's own echo cancellation and jitter buffering do the work that
 * would otherwise be an audio worklet in this file.
 */
async function buildLive(id) {
  const { OpenAIRealtimeProvider, clientSecretCredentials } = await import("@riff/openai-realtime");

  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
  });

  // Everything past here can throw, and by now the microphone is live. Failing without releasing it
  // leaves the browser's recording indicator on for a session that never started.
  let peer;
  let meter;
  try {
    peer = new RTCPeerConnection();

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

    meter = startMeter(stream);

    return {
      stream,
      peer,
      stopMeter: meter.stopMeter,
      audioContext: meter.audioContext,
      session: new RiffSession({
        bundle,
        host: buildHost(),
        provider: observeProvider(provider, { onToolResult: guardedToolResult(id) }),
        store: new MemoryStore({ motifs: DEMO_MOTIFS }),
      }),
    };
  } catch (error) {
    meter?.stopMeter();
    await meter?.audioContext?.close().catch(() => {});
    peer?.close();
    for (const track of stream.getTracks()) track.stop();
    ui.meter.hidden = true;
    throw error;
  }
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
      // A draft update is Riff writing, not the speaker changing prompt, so it never takes the pane
      // away from one being read.
      followTake(event.takeId);
      break;

    case "take":
      // An explicit change of take is the speaker saying which prompt they are on, so the pane goes
      // with it. A null take means nothing is active and the next thing said starts a fresh one —
      // which happens right after a send, when the prompt that just went out is still worth looking
      // at, so what is on screen stays until there is something newer to follow.
      if (event.takeId) showTake(event.takeId, { pinned: false });
      else renderTakes();
      break;

    case "agent.transcript":
      showAgentTranscript(event.text, event.final);
      break;

    case "interrupted":
      // The cut-off turn never reaches a final transcript, so the entry it was filling has to be
      // released or the next thing Riff says would be appended to a sentence it abandoned.
      agentEntry = null;
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
      // Unpinned, so the pane shows what was just sent and then moves on by itself as soon as Riff
      // starts writing the next prompt.
      showTake(event.artifact.takeId, { pinned: false });
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
    // A submission can be refused — nothing captured yet, a destination that does not exist, the
    // proxy or GitHub failing. Reporting all of those as "sent" and opening the confirmation shows
    // someone a prompt that was never delivered, which is how a send gets repeated.
    if (result?.submitted !== true) {
      addEntry({
        head: "submit_prompt · not sent",
        note: result?.reason ?? result?.message ?? result?.error ?? "the host refused the submission",
        variant: "rejected",
      });
      return;
    }

    lastSubmission = result;
    addEntry({ head: "submit_prompt", note: result.message ?? "sent" });
    // Filing for real is opt-in per send, not per session. Left ticked, replaying the script would
    // open a second issue without anyone asking for one.
    ui.allowIssues.checked = false;
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

/**
 * The open prompts, and which of them is on screen.
 *
 * Hidden until there is more than one: a single draft needs no chooser, and the strip appearing is
 * itself the signal that a second prompt was started.
 *
 * A real tab set, not the look of one: exactly one tab is tabbable and the arrow keys move between
 * them, because `role="tablist"` is a promise about how the thing behaves and assistive technology
 * has no way to find out it was only decorative.
 *
 * Tabs are updated in place rather than rebuilt. Riff drafts continuously while someone talks, so
 * replacing the children here would destroy the focused tab several times a sentence and drop the
 * keyboard out of the widget mid-navigation.
 */
function renderTakes() {
  const takes = shown?.takes() ?? [];
  const activeId = shown?.book.activeId ?? null;

  ui.takes.hidden = takes.length < 2;
  ui.takesHint.hidden = ui.takes.hidden;

  if (ui.takes.hidden) {
    ui.takes.replaceChildren();
    ui.promptView.removeAttribute("aria-labelledby");
    return;
  }

  const stale = new Map([...ui.takes.children].map((tab) => [tab.dataset.takeId, tab]));
  // A take can be dropped from the book while its tab has focus, which is the one case reconciling
  // cannot preserve on its own.
  const focusedTakeId = ui.takes.contains(document.activeElement)
    ? document.activeElement.dataset.takeId
    : null;

  takes.forEach((take, index) => {
    const status = takeStatus(take, activeId);
    const selected = take.id === viewingTakeId;
    const tab = stale.get(take.id) ?? createTakeTab(take.id);
    stale.delete(take.id);

    tab.dataset.status = status;
    tab.setAttribute("aria-selected", String(selected));
    // Roving tabindex: Tab reaches the strip once, then the arrow keys move within it.
    tab.tabIndex = selected ? 0 : -1;
    tab.title = TAKE_STATUS_TITLES[status];
    // The label follows the draft: an unlabelled take is known by its title once it has one.
    tab.replaceChildren(span(take.label ?? take.title?.text ?? take.id), span(status, "take-status"));

    if (ui.takes.children[index] !== tab) ui.takes.insertBefore(tab, ui.takes.children[index] ?? null);
    if (selected) ui.promptView.setAttribute("aria-labelledby", tab.id);
  });

  for (const tab of stale.values()) tab.remove();

  if (focusedTakeId && !ui.takes.contains(document.activeElement)) {
    const selected = ui.takes.querySelector('[aria-selected="true"]');
    (ui.takes.querySelector(`[data-take-id="${focusedTakeId}"]`) ?? selected)?.focus();
  }
}

function createTakeTab(takeId) {
  const tab = document.createElement("button");
  tab.type = "button";
  tab.className = "take";
  tab.id = `take-tab-${takeId}`;
  tab.dataset.takeId = takeId;
  tab.setAttribute("role", "tab");
  tab.setAttribute("aria-controls", ui.promptView.id);
  tab.addEventListener("click", () => showTake(takeId, { pinned: true }));
  tab.addEventListener("keydown", moveBetweenTakes);
  return tab;
}

/**
 * What each status means for what can still be done with the prompt.
 *
 * A submitted or discarded take is refused by `DraftBook.switchTo`, so saying Riff will come back to
 * one would be the page promising something the engine declines to do.
 */
const TAKE_STATUS_TITLES = {
  live: "The prompt the next thing said lands in",
  parked: "Set aside. Say so and Riff comes back to it",
  sent: "Already sent. It cannot be reopened, only read",
  dropped: "Thrown away. It cannot be reopened, only read",
};

/** Arrow, Home, and End across the strip — the half of `role="tablist"` that is behavior. */
function moveBetweenTakes(event) {
  const tabs = [...ui.takes.children];
  const current = tabs.indexOf(event.currentTarget);
  const last = tabs.length - 1;

  let next;
  if (event.key === "ArrowRight" || event.key === "ArrowDown") next = current === last ? 0 : current + 1;
  else if (event.key === "ArrowLeft" || event.key === "ArrowUp") next = current === 0 ? last : current - 1;
  else if (event.key === "Home") next = 0;
  else if (event.key === "End") next = last;
  else return;

  event.preventDefault();
  // Selection follows focus, which is the expected behavior when showing a tab is this cheap.
  showTake(tabs[next].dataset.takeId, { pinned: true });
  ui.takes.querySelector('[aria-selected="true"]')?.focus();
}

/** Engine status in the viewer's terms, with the active take called out as the one being written to. */
function takeStatus(take, activeId) {
  if (take.id === activeId) return "live";
  if (take.status === "submitted") return "sent";
  if (take.status === "discarded") return "dropped";
  return "parked";
}

/**
 * Puts one take on screen. Which take Riff is writing into is unaffected.
 *
 * `pinned` records that the viewer chose this prompt, so later drafts landing elsewhere leave it
 * alone. Anything Riff drives passes `pinned: false`, which releases a previous choice.
 */
function showTake(takeId, { pinned }) {
  viewingTakeId = takeId;
  pinnedTakeId = pinned ? takeId : null;
  const artifact = takeId ? (shown?.artifact(takeId) ?? null) : null;
  renderDraft(artifact);
  setFidelity(artifact?.provenance.fidelity ?? 0);
  renderTakes();
}

/** A draft update: it moves the pane only when the viewer is not reading something else. */
function followTake(takeId) {
  if (pinnedTakeId !== null && pinnedTakeId !== takeId) renderTakes();
  // A draft landing in the prompt being read is not a reason to stop reading it, so a pin on this
  // take survives rather than being released by the update it was waiting for.
  else showTake(takeId, { pinned: pinnedTakeId === takeId });
}

function renderDraft(artifact) {
  ui.draft.replaceChildren();
  ui.markdown.textContent = artifact?.rendered ?? "";

  if (!artifact || artifact.lines.length === 0) {
    ui.draft.append(paragraph("Nothing captured yet.", "hint"));
    return;
  }

  const title = document.createElement("h3");
  title.className = "draft-title";
  title.textContent = artifact.title.text;
  ui.draft.append(title);

  for (const section of SECTIONS) {
    const lines = artifact.lines.filter((line) => line.section === section);
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

  if (artifact.context.length > 0) {
    const context = document.createElement("div");
    context.className = "context";
    for (const item of artifact.context) context.append(renderContext(item));
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

  // With both credentials on the server there is a real conversation to have against a real
  // repository, so that is what the demo opens on. Only until someone picks for themselves: moving
  // the selection under them after that is worse than starting on the mode they did not want.
  if (liveAvailable && githubAvailable && !modeChosen && !run) {
    liveInput.checked = true;
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
    // The repository goes too: left behind, the next save would resend it and quietly restore a
    // target that Forget had just cleared. `applyConfig` has already put the live one in the
    // placeholder, so nothing is lost by emptying the field.
    ui.githubRepo.value = "";
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
  shown = null;
  viewingTakeId = null;
  pinnedTakeId = null;
  ui.ledger.replaceChildren();
  ui.activity.replaceChildren();
  ui.utteranceCount.textContent = "0";
  closeSheet(ui.sent);
  setFidelity(0);
  renderTakes();
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
  input.addEventListener("change", () => {
    // Only a real click lands here — setting `checked` from script fires no `change` — so this is
    // exactly the signal `applyConfig` needs to stop choosing a mode on their behalf.
    modeChosen = true;
    ui.micLabel.textContent = defaultMicLabel();
  });
}

// Live mode needs a key on the server, so say so up front rather than failing on the first click.
const config = await fetch("/api/riff/config")
  .then((response) => response.json())
  .catch(() => ({ live: false }));

applyConfig(config);
renderTakes();
renderDraft(null);
setFidelity(0);
