import type { Utterance } from "./types.ts";
import type { Lexicon } from "./lexicon.ts";
import type { SourceSpan } from "./grounding.ts";
import { tidyWhitespace, tokenize } from "./text.ts";

const SECRET_PATTERNS: RegExp[] = [
  /\bgithub_pat_[A-Za-z0-9_]{20,}/g,
  /\bgh[pousr]_[A-Za-z0-9]{16,}/g,
  /\bsk-[A-Za-z0-9_-]{20,}/g,
  /\bek_[A-Za-z0-9_-]{20,}/g,
  /\bAKIA[0-9A-Z]{16}\b/g,
  /\bxox[abposr]-[A-Za-z0-9-]{10,}/g,
  /\b(?:Bearer|token|api[_-]?key)\s+[A-Za-z0-9._-]{20,}/gi,
];

/** Strips credentials that can arrive through typed input before anything is stored. */
export function redactSecrets(text: string): string {
  let out = text;
  for (const pattern of SECRET_PATTERNS) out = out.replace(pattern, "[redacted]");
  return out;
}

export interface AppendUtterance {
  text: string;
  at?: string;
  source?: Utterance["source"];
  confidence?: number;
}

/**
 * Everything the speaker said, in order, as the transcriber produced it.
 *
 * This is the only source the prompt body may draw from. It is append-and-revise: a transcript can
 * be corrected while it is still streaming, but nothing is ever removed, because a superseded
 * sentence still has to be provable as theirs if it turns out they meant it after all.
 */
export class UtteranceLedger {
  #utterances: Utterance[] = [];
  #byId = new Map<string, Utterance>();
  #sequence = 0;
  #spans: SourceSpan[] | null = null;
  readonly #lexicon: Lexicon;
  readonly #windowSize: number;
  readonly #redact: boolean;

  constructor(lexicon: Lexicon, windowSize: number, redact = true) {
    this.#lexicon = lexicon;
    this.#windowSize = windowSize;
    this.#redact = redact;
  }

  get size(): number {
    return this.#utterances.length;
  }

  append(input: AppendUtterance): Utterance {
    const text = tidyWhitespace(this.#redact ? redactSecrets(input.text) : input.text);
    const utterance: Utterance = {
      id: `u${++this.#sequence}`,
      text,
      at: input.at ?? new Date().toISOString(),
      source: input.source ?? "speech",
      ...(input.confidence === undefined ? {} : { confidence: input.confidence }),
    };
    this.#utterances.push(utterance);
    this.#byId.set(utterance.id, utterance);
    this.#spans = null;
    return utterance;
  }

  /** Replaces the text of an utterance when a streaming transcript is finalized or corrected. */
  revise(id: string, text: string): Utterance | undefined {
    const utterance = this.#byId.get(id);
    if (!utterance) return undefined;
    utterance.text = tidyWhitespace(this.#redact ? redactSecrets(text) : text);
    this.#spans = null;
    return utterance;
  }

  get(id: string): Utterance | undefined {
    return this.#byId.get(id);
  }

  all(): readonly Utterance[] {
    return this.#utterances;
  }

  /** Call when the lexicon changes, so cached spans pick up the new corrections. */
  invalidate(): void {
    this.#spans = null;
  }

  /**
   * Every window of up to `windowSize` consecutive utterances, longest first. Windows exist because
   * transcription splits on pauses rather than on sentences, so one spoken sentence routinely
   * arrives as two or three utterances.
   */
  spans(): readonly SourceSpan[] {
    if (this.#spans) return this.#spans;

    const spans: SourceSpan[] = [];
    const cache = new Map<string, { tokens: string[]; rawTokens: string[] }>();

    const tokensFor = (utterance: Utterance) => {
      const cached = cache.get(utterance.id);
      if (cached) return cached;
      const rawTokens = tokenize(utterance.text).map((t) => t.norm);
      const value = { rawTokens, tokens: this.#lexicon.canonicalize(rawTokens).tokens };
      cache.set(utterance.id, value);
      return value;
    };

    for (let width = Math.min(this.#windowSize, this.#utterances.length); width >= 1; width--) {
      for (let start = 0; start + width <= this.#utterances.length; start++) {
        const members = this.#utterances.slice(start, start + width);
        const tokens: string[] = [];
        const owners: string[] = [];
        const rawTokens: string[] = [];
        for (const member of members) {
          const { tokens: memberTokens, rawTokens: memberRaw } = tokensFor(member);
          for (const token of memberTokens) {
            tokens.push(token);
            owners.push(member.id);
          }
          rawTokens.push(...memberRaw);
        }
        spans.push({ utteranceIds: members.map((m) => m.id), tokens, owners, rawTokens });
      }
    }

    this.#spans = spans;
    return spans;
  }

  /** Recent transcript, for the gist readback and for host tools that need conversational context. */
  recentText(count = 8): string {
    return this.#utterances
      .slice(-count)
      .map((u) => u.text)
      .join(" ");
  }

  toJSON(): Utterance[] {
    return this.#utterances.map((u) => ({ ...u }));
  }
}
