import type {
  ContextItem,
  GroundingConfig,
  Line,
  Motif,
  PolicyConfig,
  Section,
  TakeStatus,
} from "./types.ts";
import { SECTIONS, isSection } from "./types.ts";
import type { GroundingChecker, SourceSpan } from "./grounding.ts";
import { tidyWhitespace } from "./text.ts";

export interface DraftOperation {
  op: "set_title" | "upsert_line" | "remove_line" | "move_line" | "attach_context" | "detach_context";
  line_id?: string;
  section?: string;
  text?: string;
  after_line_id?: string | null;
  supersedes?: string[];
  reference_id?: string;
}

export interface DraftOperationOutcome {
  op: DraftOperation["op"];
  lineId?: string;
  status: "accepted" | "rejected";
  /** Written for the model to act on: it says what to do, not just what went wrong. */
  reason?: string;
  ratio?: number;
  kind?: Line["grounding"]["kind"];
  unmatchedTokens?: string[];
  closestSource?: string;
}

const ORDER_STEP = 1000;

/** A take that has been sent or thrown away. Nothing may reopen or alter it. */
export function isTerminal(take: Take): boolean {
  return take.status === "submitted" || take.status === "discarded";
}

/** One draft prompt. A session can hold several, so a change of subject does not destroy the last one. */
export class Take {
  readonly id: string;
  label?: string;
  readonly createdAt: string;
  updatedAt: string;
  status: TakeStatus = "drafting";
  title: { text: string; origin: "derived" | "spoken" } | null = null;
  target?: string;

  #lines = new Map<string, Line>();
  #context = new Map<string, ContextItem>();
  #history: Line[] = [];
  #sequence = 0;

  constructor(id: string, createdAt: string, label?: string) {
    this.id = id;
    this.createdAt = createdAt;
    this.updatedAt = createdAt;
    if (label !== undefined) this.label = label;
  }

  nextLineId(): string {
    return `${this.id}-l${++this.#sequence}`;
  }

  /** Lines in reading order: section order first, then position within the section. */
  lines(): Line[] {
    return [...this.#lines.values()].sort(
      (a, b) => SECTIONS.indexOf(a.section) - SECTIONS.indexOf(b.section) || a.order - b.order,
    );
  }

  linesIn(section: Section): Line[] {
    return this.lines().filter((line) => line.section === section);
  }

  line(id: string): Line | undefined {
    return this.#lines.get(id);
  }

  context(): ContextItem[] {
    return [...this.#context.values()];
  }

  /** Lines replaced by a correction. Kept so a change of mind can be walked back. */
  history(): readonly Line[] {
    return this.#history;
  }

  setLine(line: Line): void {
    const existing = this.#lines.get(line.id);
    if (existing) this.#history.push({ ...existing });
    this.#lines.set(line.id, line);
  }

  removeLine(id: string): boolean {
    const existing = this.#lines.get(id);
    if (!existing) return false;
    this.#history.push({ ...existing });
    this.#lines.delete(id);
    return true;
  }

  attachContext(item: ContextItem): void {
    this.#context.set(item.referenceId, item);
  }

  detachContext(referenceId: string): boolean {
    return this.#context.delete(referenceId);
  }

  isEmpty(): boolean {
    return this.#lines.size === 0 && this.#context.size === 0;
  }

  /** Order value that places a new line immediately after `afterId` within its section. */
  orderAfter(section: Section, afterId: string | null | undefined): number {
    const siblings = this.linesIn(section);
    if (afterId === null) {
      const first = siblings[0];
      return first ? first.order - ORDER_STEP : ORDER_STEP;
    }
    if (afterId === undefined) {
      const last = siblings[siblings.length - 1];
      return last ? last.order + ORDER_STEP : ORDER_STEP;
    }
    const index = siblings.findIndex((line) => line.id === afterId);
    if (index === -1) {
      const last = siblings[siblings.length - 1];
      return last ? last.order + ORDER_STEP : ORDER_STEP;
    }
    const anchor = siblings[index];
    const next = siblings[index + 1];
    if (!anchor) return ORDER_STEP;
    return next ? (anchor.order + next.order) / 2 : anchor.order + ORDER_STEP;
  }
}

export interface ApplyContext {
  checker: GroundingChecker;
  spans: readonly SourceSpan[];
  grounding: GroundingConfig;
  /** Resolved references the agent may attach, keyed by reference id. */
  references: Map<string, ContextItem>;
  motifs: Map<string, Motif>;
  now: () => string;
}

export interface ApplyResult {
  accepted: DraftOperationOutcome[];
  rejected: DraftOperationOutcome[];
}

/**
 * Applies the agent's draft operations, refusing any line that is not made of the speaker's words.
 *
 * Rejection is the mechanism that keeps the prompt in their voice: the model proposes text, this
 * function decides whether it survives, and the rejection message tells the model exactly which
 * words it invented so it can put theirs back.
 */
export function applyDraftOperations(
  take: Take,
  operations: readonly DraftOperation[],
  ctx: ApplyContext,
): ApplyResult {
  const accepted: DraftOperationOutcome[] = [];
  const rejected: DraftOperationOutcome[] = [];
  const record = (outcome: DraftOperationOutcome) =>
    (outcome.status === "accepted" ? accepted : rejected).push(outcome);

  for (const operation of operations) {
    switch (operation.op) {
      case "set_title":
        record(applySetTitle(take, operation, ctx));
        break;
      case "upsert_line":
        record(applyUpsertLine(take, operation, ctx));
        break;
      case "remove_line":
        record(
          take.removeLine(operation.line_id ?? "")
            ? { op: operation.op, lineId: operation.line_id, status: "accepted" }
            : {
                op: operation.op,
                lineId: operation.line_id,
                status: "rejected",
                reason: `no line ${operation.line_id ?? "(missing line_id)"} in this take`,
              },
        );
        break;
      case "move_line":
        record(applyMoveLine(take, operation));
        break;
      case "attach_context": {
        const reference = operation.reference_id ? ctx.references.get(operation.reference_id) : undefined;
        record(
          reference
            ? (take.attachContext(reference), { op: operation.op, status: "accepted" })
            : {
                op: operation.op,
                status: "rejected",
                reason: `unknown reference_id ${operation.reference_id ?? "(missing)"}; call resolve_reference first and use an id it returned`,
              },
        );
        break;
      }
      case "detach_context":
        record(
          take.detachContext(operation.reference_id ?? "")
            ? { op: operation.op, status: "accepted" }
            : { op: operation.op, status: "rejected", reason: "that reference is not attached" },
        );
        break;
      default:
        record({
          op: operation.op,
          status: "rejected",
          reason: `unknown operation "${String(operation.op)}"`,
        });
    }
  }

  if (accepted.length > 0) take.updatedAt = ctx.now();
  return { accepted, rejected };
}

function applySetTitle(take: Take, operation: DraftOperation, ctx: ApplyContext): DraftOperationOutcome {
  const text = tidyWhitespace(operation.text ?? "");
  if (!text) return { op: "set_title", status: "rejected", reason: "set_title needs text" };

  const result = ctx.checker.checkTitle(text, ctx.spans);
  if (!result.ok) {
    // A title is held to a looser bar than a body line, but it may still only use words they used.
    // Accepting it as `derived` here would make the looser bar no bar at all.
    return {
      op: "set_title",
      status: "rejected",
      reason: result.reason ?? rejectionReason(result.unmatchedTokens, ctx.spans),
      ratio: result.ratio,
      unmatchedTokens: result.unmatchedTokens,
    };
  }

  take.title = { text, origin: "spoken" };
  return { op: "set_title", status: "accepted", ratio: result.ratio, kind: result.kind };
}

function applyUpsertLine(take: Take, operation: DraftOperation, ctx: ApplyContext): DraftOperationOutcome {
  const text = tidyWhitespace(operation.text ?? "");
  const section = operation.section;

  if (!isSection(section)) {
    return {
      op: "upsert_line",
      status: "rejected",
      reason: `section must be one of ${SECTIONS.join(", ")}`,
    };
  }
  if (!text) return { op: "upsert_line", status: "rejected", reason: "upsert_line needs text" };

  const mode = ctx.grounding.sections[section] ?? "strict";
  const motif = findMotif(text, ctx.motifs);

  let grounding: Line["grounding"];
  let sourceUtteranceIds: string[] = [];
  let motifId: string | undefined;

  if (mode === "motif-or-strict" && motif) {
    grounding = { ratio: 1, kind: "motif" };
    motifId = motif.id;
  } else {
    const result = ctx.checker.check(text, ctx.spans);
    if (!result.ok) {
      return {
        op: "upsert_line",
        lineId: operation.line_id,
        status: "rejected",
        reason: result.reason ?? rejectionReason(result.unmatchedTokens, ctx.spans),
        ratio: result.ratio,
        unmatchedTokens: result.unmatchedTokens,
        closestSource: closestSourceText(ctx.spans, result.sourceUtteranceIds),
      };
    }
    grounding = {
      ratio: result.ratio,
      kind: result.kind,
      ...(result.unmatchedTokens.length > 0 ? { unmatchedTokens: result.unmatchedTokens } : {}),
    };
    sourceUtteranceIds = result.sourceUtteranceIds;
  }

  const existing = operation.line_id ? take.line(operation.line_id) : undefined;
  if (operation.line_id && !existing) {
    // Accepting an unknown id would create a line outside the generated sequence, and the next
    // ordinary insert would reuse that id and silently overwrite this line.
    return {
      op: "upsert_line",
      lineId: operation.line_id,
      status: "rejected",
      reason: `no line ${operation.line_id} in this take; omit line_id to add a new one`,
    };
  }
  const id = existing?.id ?? take.nextLineId();
  const order =
    existing && operation.after_line_id === undefined
      ? existing.order
      : take.orderAfter(section, operation.after_line_id);

  const supersedes = [...new Set(operation.supersedes ?? [])].filter((target) => target !== id);
  for (const target of supersedes) take.removeLine(target);

  take.setLine({
    id,
    section,
    text,
    order,
    sourceUtteranceIds,
    ...(motifId ? { motifId } : {}),
    ...(supersedes.length > 0 ? { supersedes } : {}),
    grounding,
  });

  return { op: "upsert_line", lineId: id, status: "accepted", ratio: grounding.ratio, kind: grounding.kind };
}

function applyMoveLine(take: Take, operation: DraftOperation): DraftOperationOutcome {
  const line = operation.line_id ? take.line(operation.line_id) : undefined;
  if (!line) {
    return {
      op: "move_line",
      lineId: operation.line_id,
      status: "rejected",
      reason: `no line ${operation.line_id ?? "(missing line_id)"} in this take`,
    };
  }
  take.setLine({ ...line, order: take.orderAfter(line.section, operation.after_line_id) });
  return { op: "move_line", lineId: line.id, status: "accepted" };
}

function findMotif(text: string, motifs: Map<string, Motif>): Motif | undefined {
  const normalized = text.toLowerCase();
  for (const motif of motifs.values()) {
    if (motif.retiredAt) continue;
    if (motif.text.toLowerCase() === normalized) return motif;
  }
  return undefined;
}

function rejectionReason(unmatched: string[], spans: readonly SourceSpan[]): string {
  if (spans.length === 0) return "nothing has been said yet, so there is nothing to draw on";
  if (unmatched.length === 0) return "the words are theirs but the order is not; keep their phrasing intact";
  const shown = unmatched.slice(0, 6).map((token) => `"${token}"`).join(", ");
  const more = unmatched.length > 6 ? ` and ${unmatched.length - 6} more` : "";
  return `they did not say ${shown}${more}; use their words or ask them`;
}

function closestSourceText(spans: readonly SourceSpan[], utteranceIds: string[]): string | undefined {
  if (utteranceIds.length === 0) return undefined;
  const wanted = new Set(utteranceIds);
  const match = spans.find((span) => span.utteranceIds.some((id) => wanted.has(id)));
  return match?.tokens.join(" ");
}

/** Whether the take has everything the policy says a sendable prompt needs. */
export function isReady(take: Take, policy: PolicyConfig): boolean {
  return policy.readinessRequires.every((section) => take.linesIn(section).length > 0);
}

/** Holds every take in the session and tracks which one is being spoken into. */
export class DraftBook {
  #takes = new Map<string, Take>();
  #activeId: string | null = null;
  #sequence = 0;
  readonly #policy: PolicyConfig;
  readonly #now: () => string;

  constructor(policy: PolicyConfig, now: () => string = () => new Date().toISOString()) {
    this.#policy = policy;
    this.#now = now;
  }

  get activeId(): string | null {
    return this.#activeId;
  }

  takes(): Take[] {
    return [...this.#takes.values()];
  }

  get(id: string): Take | undefined {
    return this.#takes.get(id);
  }

  /** The take being spoken into, creating the first one on demand. */
  active(): Take {
    if (this.#activeId) {
      const take = this.#takes.get(this.#activeId);
      if (take) return take;
    }
    return this.create();
  }

  create(label?: string, carryContextFrom?: Take): Take {
    const live = this.takes().filter((take) => take.status !== "discarded" && take.status !== "submitted");
    if (live.length >= this.#policy.maxTakes) {
      const oldest = live.sort((a, b) => a.updatedAt.localeCompare(b.updatedAt))[0];
      if (oldest && oldest.isEmpty()) this.#takes.delete(oldest.id);
      else throw new Error(`too many open takes (limit ${this.#policy.maxTakes}); park or send one first`);
    }

    const take = new Take(`t${++this.#sequence}`, this.#now(), label);
    if (carryContextFrom) for (const item of carryContextFrom.context()) take.attachContext(item);
    this.#takes.set(take.id, take);
    this.#activeId = take.id;
    return take;
  }

  switchTo(id: string): Take | undefined {
    const take = this.#takes.get(id);
    if (!take) return undefined;
    // Switching to a finished take would make it the target of the next thing spoken.
    if (isTerminal(take)) throw new Error(`take "${id}" was already ${take.status}; it cannot be reopened`);
    const previous = this.#activeId ? this.#takes.get(this.#activeId) : undefined;
    if (previous && previous.id !== id && previous.status === "drafting") previous.status = "parked";
    take.status = take.status === "parked" ? "drafting" : take.status;
    this.#activeId = id;
    return take;
  }

  park(id: string): Take | undefined {
    const take = this.#takes.get(id);
    if (!take) return undefined;
    // Parking a finished take would move it out of a terminal state, and switching back would then
    // promote it to drafting — which is how every guard downstream gets bypassed.
    if (isTerminal(take)) throw new Error(`take "${id}" was already ${take.status}; it cannot be parked`);
    take.status = "parked";
    if (this.#activeId === id) {
      const next = this.takes().find((candidate) => candidate.id !== id && candidate.status === "drafting");
      this.#activeId = next?.id ?? null;
    }
    return take;
  }

  /** Leaves no take active, so the next line spoken starts a fresh one. */
  clearActive(): void {
    this.#activeId = null;
  }

  discard(id: string): boolean {
    const take = this.#takes.get(id);
    if (!take) return false;
    if (take.status === "submitted") throw new Error(`take "${id}" was already submitted`);
    take.status = "discarded";
    if (this.#activeId === id) this.#activeId = null;
    return true;
  }
}
