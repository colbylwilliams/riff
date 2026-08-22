import type { AgentBundle, ContextItem, Motif, PromptArtifact, Utterance } from "./types.ts";
import type { RiffHost, RiffStore } from "./host.ts";
import type { ProviderEvent, RealtimeConnection, RealtimeProvider, ToolCallRequest } from "./provider.ts";
import type { SessionOverrides } from "./bundle.ts";
import type { Take } from "./draft.ts";
import { MemoryStore, NullHost } from "./host.ts";
import { DraftBook, isReady } from "./draft.ts";
import { Lexicon } from "./lexicon.ts";
import { UtteranceLedger } from "./ledger.ts";
import { createGroundingChecker } from "./grounding.ts";
import { createToolRegistry } from "./tools.ts";
import { buildArtifact, summarizeDraft } from "./render.ts";
import { withOverrides } from "./bundle.ts";

export type SessionState =
  | "idle"
  | "connecting"
  | "listening"
  | "thinking"
  | "speaking"
  | "closing"
  | "closed"
  | "failed";

export type RiffEvent =
  | { type: "state"; state: SessionState; previous: SessionState }
  | { type: "utterance"; utterance: Utterance }
  | { type: "draft"; takeId: string; ready: boolean; fidelity: number; gist: string }
  | { type: "take"; takeId: string | null }
  | { type: "agent.transcript"; text: string; final: boolean }
  | { type: "agent.audio"; audio: Uint8Array }
  | { type: "interrupted" }
  | { type: "tool"; name: string; ok: boolean; durationMs: number }
  | { type: "submitted"; artifact: PromptArtifact }
  | { type: "expiring"; secondsRemaining: number }
  | { type: "error"; error: { code: string; message: string; retryable: boolean } }
  | { type: "closed"; reason?: string };

export interface RiffSessionOptions {
  bundle: AgentBundle;
  provider: RealtimeProvider;
  host?: RiffHost;
  store?: RiffStore;
  overrides?: SessionOverrides;
  /** Which render profile the artifact uses. Defaults to the bundle's. */
  renderProfile?: string;
  now?: () => string;
  signal?: AbortSignal;
}

const EXPIRY_WARNINGS_SECONDS = [300, 60];

/**
 * One conversation, from the first word to a submitted prompt.
 *
 * The session owns the pieces that have to agree with each other: what was heard, what has been
 * drafted from it, and what the agent is allowed to do next. Provider events flow in, tool calls
 * flow back out, and the ledger stays the single place the prompt body can come from — which is
 * what makes "in their own words" a property of the system rather than a request in a prompt.
 */
export class RiffSession {
  readonly bundle: AgentBundle;
  readonly ledger: UtteranceLedger;
  readonly book: DraftBook;
  readonly lexicon: Lexicon;

  #provider: RealtimeProvider;
  #host: RiffHost;
  #store: RiffStore;
  #options: RiffSessionOptions;
  #connection: RealtimeConnection | null = null;
  #unsubscribe: (() => void) | null = null;

  #state: SessionState = "idle";
  #listeners = new Set<(event: RiffEvent) => void>();
  #references = new Map<string, ContextItem>();
  #motifs = new Map<string, Motif>();
  #registry: ReturnType<typeof createToolRegistry> | null = null;
  #toolsInFlight = false;
  #toolLog: Array<{ name: string; at: string; durationMs?: number; ok?: boolean }> = [];
  #agentTranscript = "";
  #startedAt = 0;
  #timers: ReturnType<typeof setTimeout>[] = [];
  #now: () => string;

  constructor(options: RiffSessionOptions) {
    this.#options = options;
    this.bundle = options.bundle;
    this.#provider = options.provider;
    this.#host = options.host ?? new NullHost();
    this.#store = options.store ?? new MemoryStore();
    this.#now = options.now ?? (() => new Date().toISOString());

    this.lexicon = new Lexicon(this.bundle.lexicon.terms);
    this.ledger = new UtteranceLedger(
      this.lexicon,
      this.bundle.grounding.windowSize,
      this.bundle.policy.redactSecretsFromTranscript !== false,
    );
    this.book = new DraftBook(this.bundle.policy, this.#now);
  }

  get state(): SessionState {
    return this.#state;
  }

  get sessionId(): string | undefined {
    return this.#connection?.sessionId;
  }

  on(listener: (event: RiffEvent) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  async start(): Promise<void> {
    if (this.#state !== "idle" && this.#state !== "closed" && this.#state !== "failed") {
      throw new Error(`session already ${this.#state}`);
    }
    this.#setState("connecting");

    try {
      for (const term of await this.#store.loadLexicon()) this.lexicon.add(term);
      // Includes retired motifs when the store keeps them, so their ids are never reissued.
      for (const motif of await this.#store.listMotifs()) this.#motifs.set(motif.id, motif);

      const environment = (await this.#host.environment?.()) ?? {};
      for (const term of environment.vocabulary ?? []) this.lexicon.add(term);
      this.ledger.invalidate();

      const checker = createGroundingChecker(this.bundle.grounding, this.lexicon);
      this.#registry = createToolRegistry({
        bundle: this.bundle,
        ledger: this.ledger,
        book: this.book,
        lexicon: this.lexicon,
        checker,
        host: this.#host,
        store: this.#store,
        references: this.#references,
        motifs: this.#motifs,
        now: this.#now,
        ...(this.#options.renderProfile ? { renderProfile: this.#options.renderProfile } : {}),
        provenance: () => this.#provenance(),
        onLexiconChanged: () => {
          this.#connection?.updateSession({ vocabulary: this.#vocabulary() });
        },
        onDraftChanged: (take) => this.#emitDraft(take),
        onTakeChanged: (take) => this.#emit({ type: "take", takeId: take?.id ?? null }),
        onSubmitted: (artifact) => this.#emit({ type: "submitted", artifact }),
      });

      const session = withOverrides(this.bundle.session, this.#options.overrides);
      const connection = await this.#provider.connect({
        instructions: this.bundle.instructions,
        tools: this.bundle.tools,
        session,
        vocabulary: this.#vocabulary(),
        ...(this.#options.signal ? { signal: this.#options.signal } : {}),
      });

      this.#connection = connection;
      this.#unsubscribe = connection.on((event) => this.#handle(event));
      this.#startedAt = Date.now();
      this.#scheduleExpiryWarnings(session.limits.maxSessionSeconds);

      const note = describeEnvironment(environment);
      if (note) connection.sendText(note, { respond: false });

      this.#setState("listening");
    } catch (error) {
      this.#setState("failed");
      this.#emit({
        type: "error",
        error: { code: "connect_failed", message: (error as Error).message, retryable: true },
      });
      throw error;
    }
  }

  async stop(reason = "ended"): Promise<void> {
    if (this.#state === "closed" || this.#state === "idle") return;
    this.#setState("closing");
    for (const timer of this.#timers) clearTimeout(timer);
    this.#timers = [];
    this.#unsubscribe?.();
    this.#unsubscribe = null;
    await this.#connection?.close(reason);
    this.#connection = null;
    this.#setState("closed");
    this.#emit({ type: "closed", reason });
  }

  /** Captured microphone audio, in the format the provider advertised. */
  sendAudio(chunk: Uint8Array): void {
    this.#requireConnection().sendAudio(chunk);
  }

  /**
   * Typed input, treated exactly like speech: it lands in the ledger and can be quoted in the
   * prompt. Someone switching to the keyboard mid-thought should not lose the ability to be quoted.
   */
  sendText(text: string): Utterance {
    const connection = this.#requireConnection();
    const utterance = this.ledger.append({ text, at: this.#now(), source: "typed" });
    this.#emit({ type: "utterance", utterance });
    connection.sendText(utterance.text, { respond: true });
    return utterance;
  }

  /** Cuts the agent off. Called when the speaker starts talking over it. */
  interrupt(): void {
    if (this.#state !== "speaking" && this.#state !== "thinking") return;
    this.#connection?.cancelResponse();
    this.#emit({ type: "interrupted" });
    this.#setState("listening");
  }

  /** The active take as it stands right now. Always safe to read mid-conversation. */
  artifact(): PromptArtifact | null {
    const id = this.book.activeId;
    const take = id ? this.book.get(id) : undefined;
    return take ? this.#buildArtifact(take) : null;
  }

  takes(): Take[] {
    return this.book.takes();
  }

  #buildArtifact(take: Take): PromptArtifact {
    return buildArtifact(take, {
      config: this.bundle.render,
      ...(this.#options.renderProfile ? { profile: this.#options.renderProfile } : {}),
      lexicon: this.lexicon,
      utteranceCount: this.ledger.size,
      now: this.#now(),
      provenance: this.#provenance(),
    });
  }

  #provenance(): Partial<PromptArtifact["provenance"]> {
    return {
      agentVersion: this.bundle.version,
      bundleRevision: this.bundle.revision,
      providerId: this.#provider.id,
      ...(this.#connection ? { model: this.#connection.model, sessionId: this.#connection.sessionId } : {}),
      ...(this.#startedAt ? { durationMs: Date.now() - this.#startedAt } : {}),
      toolCalls: this.#toolLog,
    };
  }

  #vocabulary(): string[] {
    const biasing = this.bundle.session.transcription.biasing;
    if (!biasing?.enabled) return [];
    return this.lexicon.keywords(biasing.maxKeywords);
  }

  #requireConnection(): RealtimeConnection {
    if (!this.#connection) throw new Error("session is not connected");
    return this.#connection;
  }

  #handle(event: ProviderEvent): void {
    switch (event.type) {
      case "connected":
        this.#setState("listening");
        break;

      case "speech.started":
        // Automatic barge-in has to do everything an explicit interrupt does. Emitting the event
        // without cancelling leaves buffered output playing over whoever just started talking.
        this.interrupt();
        this.#setState("listening");
        break;

      case "transcript.completed": {
        const text = event.text.trim();
        if (text.length === 0) break;
        const utterance = this.ledger.append({
          text,
          at: this.#now(),
          source: "speech",
          ...(event.confidence === undefined ? {} : { confidence: event.confidence }),
        });
        this.#emit({ type: "utterance", utterance });
        break;
      }

      case "transcript.failed":
        this.#emit({
          type: "error",
          error: { code: "transcription_failed", message: event.reason, retryable: true },
        });
        break;

      case "response.started":
        // The continuation for a tool batch has begun, so the batch is no longer pending.
        this.#toolsInFlight = false;
        this.#agentTranscript = "";
        this.#setState("thinking");
        break;

      case "response.audio":
        if (this.#state !== "speaking") this.#setState("speaking");
        this.#emit({ type: "agent.audio", audio: event.audio });
        break;

      case "response.text.delta":
        this.#agentTranscript += event.delta;
        this.#emit({ type: "agent.transcript", text: this.#agentTranscript, final: false });
        break;

      case "response.text":
        this.#agentTranscript = event.text;
        this.#emit({ type: "agent.transcript", text: event.text, final: true });
        break;

      case "tool.calls":
        // Set on receipt rather than inside the dispatch, so the guard below does not depend on
        // whether provider events are delivered while the dispatch is still running.
        this.#toolsInFlight = true;
        void this.#runTools(event.calls);
        break;

      case "response.done":
      case "response.cancelled":
        if (!this.#toolsInFlight) this.#setState("listening");
        break;

      case "error":
        this.#toolsInFlight = false;
        this.#emit({
          type: "error",
          error: { code: event.error.code, message: event.error.message, retryable: event.error.retryable },
        });
        if (!event.error.retryable) this.#setState("failed");
        break;

      case "closed":
        this.#setState("closed");
        this.#emit({ type: "closed", ...(event.reason ? { reason: event.reason } : {}) });
        break;

      default:
        break;
    }
  }

  /**
   * Runs every tool call of one model turn, then asks for exactly one continuation.
   *
   * Providers deliver a turn's calls together, so they are dispatched together. Asking the model to
   * continue once per call instead would produce one spoken reply per tool, which sounds like the
   * agent stuttering.
   */
  async #runTools(calls: readonly ToolCallRequest[]): Promise<void> {
    const registry = this.#registry;
    const connection = this.#connection;
    if (!registry || !connection || calls.length === 0) return;

    try {
      await Promise.all(
        calls.map(async ({ callId, name, argumentsJson }) => {
          const at = this.#now();

          let outcome: Awaited<ReturnType<typeof registry.dispatch>>;
          try {
            outcome = await registry.dispatch(name, argumentsJson);
          } catch (error) {
            outcome = { ok: false, result: { error: (error as Error).message }, durationMs: 0 };
          }

          this.#toolLog.push({ name, at, durationMs: outcome.durationMs, ok: outcome.ok });
          this.#emit({ type: "tool", name, ok: outcome.ok, durationMs: outcome.durationMs });

          try {
            connection.respondToTool(callId, JSON.stringify(outcome.result));
          } catch (error) {
            this.#emit({
              type: "error",
              error: { code: "tool_response_failed", message: (error as Error).message, retryable: true },
            });
          }
        }),
      );
    } catch (error) {
      this.#toolsInFlight = false;
      throw error;
    }

    connection.requestResponse();
  }

  #emitDraft(take: Take): void {
    const artifact = this.#buildArtifact(take);
    this.#emit({
      type: "draft",
      takeId: take.id,
      ready: isReady(take, this.bundle.policy),
      fidelity: artifact.provenance.fidelity,
      gist: summarizeDraft(take),
    });
  }

  #scheduleExpiryWarnings(maxSessionSeconds: number): void {
    for (const remaining of EXPIRY_WARNINGS_SECONDS) {
      const delay = (maxSessionSeconds - remaining) * 1000;
      if (delay <= 0) continue;
      const timer = setTimeout(() => this.#emit({ type: "expiring", secondsRemaining: remaining }), delay);
      timer.unref?.();
      this.#timers.push(timer);
    }
  }

  #setState(state: SessionState): void {
    if (state === this.#state) return;
    const previous = this.#state;
    this.#state = state;
    this.#emit({ type: "state", state, previous });
  }

  #emit(event: RiffEvent): void {
    for (const listener of [...this.#listeners]) {
      try {
        listener(event);
      } catch {
        // A misbehaving UI listener must not take the conversation down.
      }
    }
  }
}

/**
 * Ambient facts stated once at connect time so references resolve without anyone being asked.
 *
 * This is injected into the model's context, not into the ledger, so none of it can end up quoted
 * in the prompt as though the speaker had said it.
 */
function describeEnvironment(environment: {
  workspace?: string;
  repository?: string;
  branch?: string;
  user?: { login?: string; name?: string };
  destinations?: Array<{ id: string; label: string; default?: boolean }>;
  recent?: ContextItem[];
}): string | null {
  const facts: string[] = [];
  if (environment.repository) facts.push(`Repository: ${environment.repository}`);
  if (environment.branch) facts.push(`Branch: ${environment.branch}`);
  if (environment.workspace) facts.push(`Workspace: ${environment.workspace}`);
  if (environment.user?.login) facts.push(`Speaker: ${environment.user.name ?? environment.user.login}`);
  if (environment.destinations?.length) {
    facts.push(
      `Destinations: ${environment.destinations
        .map((destination) => `${destination.id}${destination.default ? " (default)" : ""}`)
        .join(", ")}`,
    );
  }
  if (environment.recent?.length) {
    facts.push(
      `Recently touched: ${environment.recent
        .slice(0, 5)
        .map((item) => item.identifier ?? item.title)
        .join(", ")}`,
    );
  }

  if (facts.length === 0) return null;
  return `[context, not spoken by the user, never quote this in the prompt]\n${facts.join("\n")}`;
}
