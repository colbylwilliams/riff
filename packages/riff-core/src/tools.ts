import type { AgentBundle, ContextItem, Line, Motif, PromptArtifact, ToolDefinition } from "./types.ts";
import type { RiffHost, RiffStore } from "./host.ts";
import type { Lexicon } from "./lexicon.ts";
import type { UtteranceLedger } from "./ledger.ts";
import type { GroundingChecker } from "./grounding.ts";
import type { DraftOperation } from "./draft.ts";
import { DraftBook, Take, applyDraftOperations, isReady } from "./draft.ts";
import { buildArtifact, renderPrompt, summarizeDraft } from "./render.ts";
import { isPlausibleMishearing, termKey } from "./lexicon.ts";
import { validate } from "./schema.ts";
import { tidyWhitespace } from "./text.ts";

/**
 * Passed to a handler so host calls can be abandoned when Riff stops waiting, and so a handler can
 * report a result that has already come true.
 */
export interface ToolCall {
  signal?: AbortSignal;
  /**
   * Fixes the result of this call, whatever happens next.
   *
   * A call is bounded so a host that never answers cannot hold a turn open. Past the point where
   * something outside Riff has changed — a prompt the destination has taken — there is nothing left
   * to give up on, and reporting a timeout there would tell the model a prompt that went out did
   * not, inviting it to send again. A handler commits at that moment; everything after it is
   * bookkeeping that may be abandoned freely.
   */
  commit(result: unknown): void;
}

export interface ToolOutcome {
  ok: boolean;
  result: unknown;
  durationMs: number;
}

export interface ToolRuntimeEvents {
  onLexiconChanged?(): void;
  onDraftChanged?(take: Take): void;
  onTakeChanged?(take: Take | null): void;
  onSubmitted?(artifact: PromptArtifact): void;
}

export interface ToolRuntime extends ToolRuntimeEvents {
  bundle: AgentBundle;
  ledger: UtteranceLedger;
  book: DraftBook;
  lexicon: Lexicon;
  checker: GroundingChecker;
  host: RiffHost;
  store: RiffStore;
  /** References resolved this session, so the agent can attach one by id later. */
  references: Map<string, ContextItem>;
  motifs: Map<string, Motif>;
  now(): string;
  /** Render profile the session was configured with, so a preview and a submission agree. */
  renderProfile?: string;
  /**
   * Read when an artifact is built rather than stored, so a prompt submitted mid-session carries
   * the negotiated model, session id, and duration instead of whatever was known before connecting.
   */
  provenance?: () => Partial<PromptArtifact["provenance"]>;
}

export interface ToolRegistry {
  definitions: ToolDefinition[];
  dispatch(name: string, argumentsJson: string): Promise<ToolOutcome>;
}

type Handler = (args: any, runtime: ToolRuntime, call: ToolCall) => Promise<unknown> | unknown;

/**
 * Bounds a tool call. A host that never returns would otherwise hold the whole batch open, and the
 * single continuation the model is waiting for would never be requested — the conversation just
 * stops. A timeout is a result the model can act on; silence is not.
 *
 * The controller is aborted when the timeout fires, so a host that honors it stops work rather than
 * completing a submission the model has already been told failed.
 *
 * A committed result outranks both the timeout and any later failure: it says the world has already
 * changed, which no amount of giving up afterwards can undo.
 */
function withToolTimeout(
  start: (call: ToolCall) => Promise<unknown>,
  timeoutMs: number,
  name: string,
): Promise<unknown> {
  let committed: { result: unknown } | undefined;
  const controller = new AbortController();
  const call: ToolCall = {
    signal: controller.signal,
    commit: (result) => {
      committed = { result };
    },
  };

  const work = start(call);
  if (!(timeoutMs > 0)) return work;

  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      controller.abort();
      if (committed) resolve(committed.result);
      else reject(new Error(`${name} did not answer within ${timeoutMs}ms; tell them it is not responding`));
    }, timeoutMs);
    timer.unref?.();
    work.then(
      (result) => {
        clearTimeout(timer);
        resolve(result);
      },
      (error) => {
        clearTimeout(timer);
        if (committed) resolve(committed.result);
        else reject(error);
      },
    );
  });
}

export function createToolRegistry(runtime: ToolRuntime): ToolRegistry {
  const byName = new Map(runtime.bundle.tools.map((tool) => [tool.name, tool] as const));

  return {
    definitions: runtime.bundle.tools,

    async dispatch(name, argumentsJson) {
      const startedAt = Date.now();
      const finish = (ok: boolean, result: unknown): ToolOutcome => ({
        ok,
        result,
        durationMs: Date.now() - startedAt,
      });

      const definition = byName.get(name);
      if (!definition) {
        return finish(false, { error: `unknown tool "${name}"` });
      }

      let parsed: unknown;
      try {
        parsed = argumentsJson.trim() === "" ? {} : JSON.parse(argumentsJson);
      } catch (error) {
        return finish(false, { error: `arguments were not valid JSON: ${(error as Error).message}` });
      }

      const validated = validate<Record<string, unknown>>(definition.parameters, parsed);
      if (!validated.valid) {
        return finish(false, { error: "invalid arguments", details: validated.errors });
      }

      const handler = HANDLERS[name];
      if (!handler) {
        return finish(false, { error: `tool "${name}" has no implementation in this build` });
      }

      try {
        const timeoutMs = runtime.bundle.session.limits.toolTimeoutMs;
        const outcome = await withToolTimeout(
          (call) => Promise.resolve(handler(validated.value, runtime, call)),
          timeoutMs,
          name,
        );
        return finish(true, outcome ?? { ok: true });
      } catch (error) {
        return finish(false, { error: (error as Error).message });
      }
    },
  };
}

/** A motif id that no existing or retired motif is using. */
function newMotifId(motifs: Map<string, Motif>): string {
  let highest = 0;
  for (const id of motifs.keys()) {
    const parsed = /^m(\d+)$/.exec(id);
    if (parsed) highest = Math.max(highest, Number(parsed[1]));
  }
  return `m${highest + 1}`;
}

function resolveTake(runtime: ToolRuntime, takeId?: string): Take {
  if (!takeId) return runtime.book.active();
  const take = runtime.book.get(takeId);
  if (!take) throw new Error(`no take "${takeId}"`);
  return take;
}

/**
 * A take that has been sent or thrown away is finished, and naming it explicitly does not reopen it.
 * Without this, later speech could still be written into a prompt that has already gone out.
 */
function requireOpen(take: Take): Take {
  if (take.status === "submitted" || take.status === "discarded") {
    throw new Error(
      `take "${take.id}" was already ${take.status}; start a new one for anything further`,
    );
  }
  return take;
}

/** Compact view of the draft returned after every mutation, so the agent always knows line ids. */
function draftView(take: Take, runtime: ToolRuntime, includeRendered = false) {
  const grouped: Record<string, Array<{ id: string; text: string; grounding: Line["grounding"]["kind"] }>> = {};
  for (const line of take.lines()) {
    (grouped[line.section] ??= []).push({ id: line.id, text: line.text, grounding: line.grounding.kind });
  }
  return {
    take_id: take.id,
    ...(take.label ? { label: take.label } : {}),
    title: take.title?.text ?? null,
    sections: grouped,
    context: take.context().map((item) => ({
      reference_id: item.referenceId,
      identifier: item.identifier ?? item.title,
      url: item.url,
    })),
    ready: isReady(take, runtime.bundle.policy),
    ...(includeRendered
      ? {
          rendered: renderPrompt(take, {
            config: runtime.bundle.render,
            ...(runtime.renderProfile ? { profile: runtime.renderProfile } : {}),
          }),
        }
      : {}),
  };
}

function fidelityOf(take: Take, runtime: ToolRuntime): number {
  return buildArtifact(take, {
    config: runtime.bundle.render,
    ...(runtime.renderProfile ? { profile: runtime.renderProfile } : {}),
    lexicon: runtime.lexicon,
    utteranceCount: runtime.ledger.size,
    now: runtime.now(),
  }).provenance.fidelity;
}

const HANDLERS: Record<string, Handler> = {
  draft_update(args: { take_id?: string; operations: DraftOperation[] }, runtime) {
    const take = requireOpen(resolveTake(runtime, args.take_id));
    const { accepted, rejected } = applyDraftOperations(take, args.operations, {
      checker: runtime.checker,
      spans: runtime.ledger.spans(),
      grounding: runtime.bundle.grounding,
      references: runtime.references,
      motifs: runtime.motifs,
      now: runtime.now,
    });

    if (accepted.length > 0) runtime.onDraftChanged?.(take);

    return {
      accepted,
      rejected,
      fidelity: fidelityOf(take, runtime),
      draft: draftView(take, runtime),
    };
  },

  read_draft(args: { take_id?: string; include_rendered?: boolean }, runtime) {
    const take = resolveTake(runtime, args.take_id);
    return {
      ...draftView(take, runtime, args.include_rendered === true),
      gist: summarizeDraft(take),
      fidelity: fidelityOf(take, runtime),
    };
  },

  async resolve_reference(
    args: { phrase: string; kind?: string; recency?: string; actor?: string; limit?: number },
    runtime,
    call,
  ) {
    const result = await runtime.host.resolveReference({
      phrase: args.phrase,
      ...(call.signal ? { signal: call.signal } : {}),
      ...(args.kind && args.kind !== "unknown" ? { kind: args.kind } : {}),
      ...(args.recency ? { recency: args.recency as never } : {}),
      ...(args.actor ? { actor: args.actor } : {}),
      ...(args.limit ? { limit: args.limit } : {}),
      transcript: runtime.ledger.recentText(),
    });

    const candidates = result.candidates.map((candidate, index) => {
      const referenceId = candidate.referenceId || `r${runtime.references.size + index + 1}`;
      const item: ContextItem = { ...candidate, referenceId, resolvedFrom: args.phrase };
      runtime.references.set(referenceId, item);
      return {
        reference_id: referenceId,
        kind: item.kind,
        title: item.title,
        identifier: item.identifier,
        url: item.url,
        actor: item.actor,
        timestamp: item.timestamp,
        state: item.state,
        confidence: item.confidence,
      };
    });

    return {
      candidates,
      ...(candidates.length === 0
        ? { note: "nothing matched; ask them which one they mean rather than guessing" }
        : {}),
    };
  },

  async lookup_term(args: { heard: string; context?: string; kind?: string }, runtime, call) {
    const local = runtime.lexicon.lookup(args.heard).map((term) => ({ ...term, confidence: 1, source: "lexicon" }));
    const remote = await runtime.host.lookupTerm({
      heard: args.heard,
      ...(call.signal ? { signal: call.signal } : {}),
      ...(args.context ? { context: args.context } : {}),
      ...(args.kind && args.kind !== "unknown" ? { kind: args.kind } : {}),
    });

    const seen = new Set(local.map((term) => term.canonical.toLowerCase()));
    const matches = [
      ...local,
      ...remote.matches
        .filter((term) => !seen.has(term.canonical.toLowerCase()))
        .map((term) => ({ ...term, source: "host" })),
    ];

    return {
      matches,
      ...(matches.length === 0 ? { note: "unknown here; if it matters, ask them what it is" } : {}),
    };
  },

  async record_term(
    args: { canonical: string; kind: string; heard_as?: string[]; definition?: string; scope?: string },
    runtime,
    call,
  ) {
    const canonical = tidyWhitespace(args.canonical);
    if (!canonical) throw new Error("record_term needs a canonical spelling");

    // Aliases are applied to both sides of every grounding comparison, so one for a word that is
    // not really the same word lets an invented line match different spoken words. Similarity does
    // not establish sameness — "cache" and "cash" are one edit apart — so an alias only becomes
    // grounding-active when something outside this conversation confirms the term exists.
    const proposed = args.heard_as ?? [];
    const plausible = proposed.filter((heard) => isPlausibleMishearing(heard, canonical));
    const refused = proposed.filter((heard) => !plausible.includes(heard));

    const known = runtime.lexicon.lookup(canonical).some((match) => match.canonical === canonical);
    const confirmed =
      known ||
      (await runtime.host.lookupTerm({
        heard: canonical,
        ...(call.signal ? { signal: call.signal } : {}),
      })).matches.some((match) => termKey(match.canonical) === termKey(canonical));

    const term = {
      canonical,
      kind: args.kind,
      ...(plausible.length > 0 ? { heardAs: plausible } : {}),
      ...(args.definition ? { definition: args.definition } : {}),
      scope: (args.scope ?? "user") as "session" | "user" | "workspace",
    };

    runtime.lexicon.add(term, { corroborated: confirmed });
    runtime.ledger.invalidate();
    if (term.scope !== "session") await runtime.store.saveTerm(term);
    runtime.onLexiconChanged?.();

    return {
      recorded: term.canonical,
      corrections: confirmed ? plausible.length : 0,
      ...(refused.length > 0
        ? {
            refused,
            reason:
              "a correction has to be a mishearing of the same word; those are different words, so record the term without them",
          }
        : {}),
      ...(confirmed
        ? {}
        : {
            note: "nothing here knows that term, so it will help transcription but cannot be used as a spelling correction; write what they actually said",
          }),
    };
  },

  async recall_prompts(args: { query: string; recency?: string; status?: string; limit?: number }, runtime, call) {
    const result = await runtime.host.recallPrompts({
      query: args.query,
      ...(call.signal ? { signal: call.signal } : {}),
      ...(args.recency ? { recency: args.recency as never } : {}),
      ...(args.status ? { status: args.status as never } : {}),
      ...(args.limit ? { limit: args.limit } : {}),
    });
    return { prompts: result.prompts };
  },

  async motifs(
    args: { action: string; motif_id?: string; text?: string; scope?: string; applies_when?: string },
    runtime,
  ) {
    switch (args.action) {
      case "list":
        return {
          motifs: [...runtime.motifs.values()]
            .filter((motif) => !motif.retiredAt)
            .map((motif) => ({
              motif_id: motif.id,
              text: motif.text,
              scope: motif.scope,
              applies_when: motif.appliesWhen,
            })),
        };

      case "save": {
        const text = tidyWhitespace(args.text ?? "");
        if (!text) throw new Error("save needs the text of the standing instruction");

        const grounding = runtime.checker.check(text, runtime.ledger.spans());
        if (!grounding.ok) {
          return {
            saved: false,
            reason: `a motif has to be their wording; they did not say ${grounding.unmatchedTokens
              .slice(0, 6)
              .map((token) => `"${token}"`)
              .join(", ")}`,
          };
        }

        const motif: Motif = {
          // Counting active motifs reuses an id after one is retired: retire m1, reload, and the
          // next save is called m2 and silently replaces the existing m2.
          id: newMotifId(runtime.motifs),
          text,
          scope: (args.scope ?? "user") as "user" | "workspace",
          ...(args.applies_when ? { appliesWhen: args.applies_when } : {}),
          createdAt: runtime.now(),
        };
        runtime.motifs.set(motif.id, motif);
        await runtime.store.saveMotif(motif);
        return { saved: true, motif_id: motif.id };
      }

      case "attach": {
        const motif = args.motif_id ? runtime.motifs.get(args.motif_id) : undefined;
        if (!motif) throw new Error(`no motif "${args.motif_id ?? "(missing motif_id)"}"`);
        if (motif.retiredAt) throw new Error(`motif "${motif.id}" was retired; they asked to stop using it`);

        const take = requireOpen(runtime.book.active());
        if (take.lines().some((line) => line.motifId === motif.id)) {
          return { attached: false, reason: "already on this take" };
        }

        const id = take.nextLineId();
        take.setLine({
          id,
          section: "constraint",
          text: motif.text,
          order: take.orderAfter("constraint", undefined),
          sourceUtteranceIds: [],
          motifId: motif.id,
          grounding: { ratio: 1, kind: "motif" },
        });
        take.updatedAt = runtime.now();
        runtime.onDraftChanged?.(take);
        return { attached: true, line_id: id, draft: draftView(take, runtime) };
      }

      case "detach": {
        const take = requireOpen(runtime.book.active());
        const line = take.lines().find((candidate) => candidate.motifId === args.motif_id);
        if (!line) return { detached: false, reason: "not on this take" };
        take.removeLine(line.id);
        take.updatedAt = runtime.now();
        runtime.onDraftChanged?.(take);
        return { detached: true };
      }

      case "retire": {
        if (!args.motif_id) throw new Error("retire needs motif_id");
        const motif = runtime.motifs.get(args.motif_id);
        if (!motif) throw new Error(`no motif "${args.motif_id}"`);
        const at = runtime.now();
        runtime.motifs.set(motif.id, { ...motif, retiredAt: at });
        await runtime.store.retireMotif(motif.id, at);
        return { retired: true };
      }

      default:
        throw new Error(`unknown motifs action "${args.action}"`);
    }
  },

  takes(args: { action: string; take_id?: string; label?: string; carry_context?: boolean }, runtime) {
    const book: DraftBook = runtime.book;

    switch (args.action) {
      case "new": {
        const previous = book.activeId ? book.get(book.activeId) : undefined;
        const take = book.create(args.label, args.carry_context && previous ? previous : undefined);
        runtime.onTakeChanged?.(take);
        return { take_id: take.id, label: take.label ?? null, active: true };
      }

      case "switch": {
        if (!args.take_id) throw new Error("switch needs take_id");
        const take = book.switchTo(args.take_id);
        if (!take) throw new Error(`no take "${args.take_id}"`);
        runtime.onTakeChanged?.(take);
        return { take_id: take.id, draft: draftView(take, runtime) };
      }

      case "park": {
        const id = args.take_id ?? book.activeId;
        if (!id) throw new Error("there is no take to park");
        const take = book.park(id);
        if (!take) throw new Error(`no take "${id}"`);
        runtime.onTakeChanged?.(book.activeId ? (book.get(book.activeId) ?? null) : null);
        return { parked: take.id };
      }

      case "list":
        return {
          takes: book.takes().map((take) => ({
            take_id: take.id,
            label: take.label ?? null,
            status: take.status,
            active: take.id === book.activeId,
            title: take.title?.text ?? null,
            lines: take.lines().length,
            updated_at: take.updatedAt,
          })),
        };

      case "discard": {
        const id = args.take_id ?? book.activeId;
        if (!id) throw new Error("there is no take to discard");
        if (!book.discard(id)) throw new Error(`no take "${id}"`);
        runtime.onTakeChanged?.(book.activeId ? (book.get(book.activeId) ?? null) : null);
        return { discarded: id };
      }

      default:
        throw new Error(`unknown takes action "${args.action}"`);
    }
  },

  async submit_prompt(args: { take_id?: string; target?: string; keep_open?: boolean }, runtime, call) {
    const take = requireOpen(resolveTake(runtime, args.take_id));

    if (!isReady(take, runtime.bundle.policy)) {
      const missing = runtime.bundle.policy.readinessRequires.filter(
        (section) => take.linesIn(section).length === 0,
      );
      return {
        submitted: false,
        reason: `nothing to send yet: no ${missing.join(" or ")} captured. Ask them what they want done.`,
      };
    }

    if (args.target) take.target = args.target;

    // The take stays where the speaker left it across the await, and only the artifact the host
    // receives is marked ready. Moving the take first would strand it in `ready` when this handler
    // is abandoned at its deadline, and any rollback afterwards has to guess what to put back —
    // which is how a parked take gets resurrected as drafting by a send that never happened.
    const artifact: PromptArtifact = {
      ...buildArtifact(take, {
        config: runtime.bundle.render,
        ...(runtime.renderProfile ? { profile: runtime.renderProfile } : {}),
        lexicon: runtime.lexicon,
        utteranceCount: runtime.ledger.size,
        now: runtime.now(),
        ...(runtime.provenance ? { provenance: runtime.provenance() } : {}),
      }),
      status: "ready",
    };

    // No rollback on failure: the take was never moved, so it is already exactly as they left it.
    const result = await runtime.host.submitPrompt(artifact, {
      ...(args.target ? { target: args.target } : {}),
      ...(args.keep_open ? { keepOpen: args.keep_open } : {}),
      ...(call.signal ? { signal: call.signal } : {}),
    });

    const report = (warning?: string) => ({
      submitted: result.submitted,
      prompt_id: result.promptId ?? artifact.id,
      destination: result.destination,
      url: result.url,
      ...(result.message ? { message: result.message } : {}),
      ...(warning ? { warning } : {}),
    });

    // A refusal needs no rollback either: the take was never moved out of where they left it, and
    // nothing outside Riff changed, so there is nothing to commit.
    if (!result.submitted) return report();

    // The destination has the prompt. Everything that records that fact happens before the store is
    // awaited, and the answer is committed, so a save that never returns can neither strand the
    // take nor turn a delivered prompt into a timeout the model would act on by sending it twice.
    const stoodDown = runtime.book.markSubmitted(take.id, args.keep_open === true);
    const stored: PromptArtifact = { ...artifact, status: take.status, submittedAt: runtime.now() };

    runtime.onSubmitted?.(stored);
    if (stoodDown) runtime.onTakeChanged?.(null);
    call.commit(report());

    // Reporting a failed save as a failed submission would invite a retry that sends it twice, so
    // the send is reported as what it is.
    try {
      await runtime.store.saveArtifact(stored);
    } catch (error) {
      return report(`it was sent, but saving a copy failed: ${(error as Error).message}`);
    }

    return report();
  },
};
