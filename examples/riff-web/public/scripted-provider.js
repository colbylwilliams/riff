/**
 * A RealtimeProvider that replays a scripted conversation.
 *
 * It implements the same interface as the OpenAI provider and nothing more, so everything above the
 * provider seam — the ledger, the grounding check, drafts, takes, the artifact — runs for real. That
 * is what makes this a demo of Riff rather than a mockup of one: swapping this for
 * `OpenAIRealtimeProvider` changes who decides what to say, and nothing else.
 *
 * It also means the demo works with no API key, no network, and no microphone, which is the
 * difference between a demo you can give and a demo you hope works.
 */

/** 10ms of silence at 24kHz mono PCM16. Enough to drive the speaking state, nothing to hear. */
const SILENT_CHUNK = new Uint8Array(480 * 2);

const TOOL_SETTLE_TIMEOUT_MS = 5_000;

export class ScriptedProvider {
  id = "scripted";

  #script;
  #speed;
  #onStep;
  #prepareArgs;
  #prepareText;
  #listeners = new Set();
  #connection = null;
  #aborted = false;
  #pendingResponse = null;
  #responseSequence = 0;

  /**
   * @param {object} options
   * @param {Array<object>} options.script     steps to replay
   * @param {number} [options.speed]           playback multiplier; 2 runs twice as fast
   * @param {(step: object, phase: "begin" | "end") => void} [options.onStep]
   * @param {(name: string, args: object) => object} [options.prepareArgs]
   *        Last look at a call's arguments before it goes out, so a scripted step can refer to
   *        something an earlier tool returned rather than to an id baked into the script.
   * @param {(text: string) => string} [options.prepareText]
   *        The same, for a line the agent speaks, so it can name what it actually found.
   */
  constructor({ script, speed = 1, onStep, prepareArgs, prepareText } = {}) {
    this.#script = script ?? [];
    this.#speed = speed;
    this.#onStep = onStep ?? (() => {});
    this.#prepareArgs = prepareArgs ?? ((_name, args) => args);
    this.#prepareText = prepareText ?? ((text) => text);
  }

  get capabilities() {
    return {
      speechToSpeech: true,
      bargeIn: true,
      semanticTurnDetection: true,
      vocabularyBiasing: "keywords",
      inputTranscription: true,
      functionCalling: true,
      audio: {
        input: { encoding: "pcm_s16le", sampleRate: 24000, channels: 1 },
        output: { encoding: "pcm_s16le", sampleRate: 24000, channels: 1 },
      },
      maxSessionSeconds: 3600,
    };
  }

  async connect() {
    this.#aborted = false;

    this.#connection = {
      sessionId: "sess_scripted",
      model: "scripted-realtime",
      sendAudio: () => {},
      commitAudio: () => {},
      sendText: () => {},
      respondToTool: () => {},
      // The session calls this once a whole tool batch has been answered, which is exactly the
      // signal the script needs to move on without racing the dispatch.
      requestResponse: () => {
        this.#pendingResponse?.();
        this.#pendingResponse = null;
      },
      cancelResponse: () => {},
      updateSession: () => {},
      on: (listener) => {
        this.#listeners.add(listener);
        return () => this.#listeners.delete(listener);
      },
      close: async () => {
        this.#aborted = true;
        this.#emit({ type: "closed", reason: "script ended" });
      },
    };

    queueMicrotask(() =>
      this.#emit({ type: "connected", sessionId: "sess_scripted", model: "scripted-realtime" }),
    );
    return this.#connection;
  }

  /** Replays the script. Resolves when it runs out or is stopped. */
  async run() {
    for (const [index, step] of this.#script.entries()) {
      if (this.#aborted) return;
      this.#onStep(step, "begin");

      if (step.user) await this.#speak(step.user);
      else if (step.agent) await this.#respond(step.agent);
      else if (step.tools) await this.#callTools(step.tools, this.#script[index + 1]);

      this.#onStep(step, "end");
      await this.#sleep(step.pause ?? 350);
    }
  }

  stop() {
    this.#aborted = true;
    this.#pendingResponse?.();
    this.#pendingResponse = null;
  }

  /** A turn of speech: the agent yields, the words land, the ledger grows. */
  async #speak(text) {
    this.#emit({ type: "speech.started" });
    await this.#sleep(Math.min(700 + text.length * 26, 4200));
    if (this.#aborted) return;
    this.#emit({ type: "speech.stopped" });
    this.#emit({ type: "transcript.completed", itemId: `item_${this.#responseSequence}`, text });
  }

  /** A turn from the agent. The audio is silence; the state machine does not know the difference. */
  async #respond(scripted) {
    const text = this.#prepareText(scripted);
    const responseId = `resp_${++this.#responseSequence}`;
    this.#emit({ type: "response.started", responseId });
    await this.#sleep(320);

    for (const word of text.split(" ")) {
      if (this.#aborted) break;
      this.#emit({ type: "response.text.delta", responseId, delta: `${word} ` });
      this.#emit({ type: "response.audio", responseId, audio: SILENT_CHUNK });
      await this.#sleep(90);
    }

    this.#emit({ type: "response.audio.done", responseId });
    this.#emit({ type: "response.text", responseId, text });
    this.#emit({ type: "response.done", responseId });
  }

  /**
   * One turn's tool calls, delivered together the way a provider does.
   *
   * A model always answers a tool batch, even when it has nothing to say. Skipping that would leave
   * the session waiting on a continuation that never arrives, so an empty response is emitted unless
   * the script has the agent speak next.
   */
  async #callTools(calls, nextStep) {
    const settled = new Promise((resolve) => {
      this.#pendingResponse = resolve;
      setTimeout(resolve, TOOL_SETTLE_TIMEOUT_MS);
    });

    this.#emit({
      type: "tool.calls",
      calls: calls.map((call, index) => ({
        callId: `call_${this.#responseSequence}_${index}`,
        name: call.name,
        argumentsJson: JSON.stringify(this.#prepareArgs(call.name, call.args ?? {})),
      })),
    });

    await settled;
    if (this.#aborted || nextStep?.agent) return;

    const responseId = `resp_${++this.#responseSequence}`;
    this.#emit({ type: "response.started", responseId });
    this.#emit({ type: "response.done", responseId });
  }

  #sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, Math.max(0, ms / this.#speed)));
  }

  #emit(event) {
    for (const listener of [...this.#listeners]) listener(event);
  }
}
