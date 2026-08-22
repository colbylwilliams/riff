import type { LexiconTerm } from "./types.ts";
import { normalizeToken, tokenize } from "./text.ts";

export interface CanonicalizeResult {
  tokens: string[];
  /** True when at least one alias was rewritten, which distinguishes a corrected line from a trimmed one. */
  substituted: boolean;
  applied: LexiconTerm[];
}

/**
 * Vocabulary the transcriber gets wrong, plus the corrections that fix it.
 *
 * The lexicon is applied to both sides of every grounding comparison, so an alias can never make a
 * paraphrase look grounded — it only lets "get hub" and "GitHub" recognize each other. Canonical
 * forms are also compiled into transcription biasing hints at connect time.
 */
export class Lexicon {
  #terms = new Map<string, LexiconTerm>();
  /** Normalized alias phrase, space joined, to the canonical token sequence. */
  #aliases = new Map<string, { tokens: string[]; term: LexiconTerm }>();
  #maxAliasLength = 1;

  constructor(terms: Iterable<LexiconTerm> = []) {
    for (const term of terms) this.add(term);
  }

  get size(): number {
    return this.#terms.size;
  }

  terms(): LexiconTerm[] {
    return [...this.#terms.values()];
  }

  /** Adds or replaces a term. Aliases identical to the canonical form are ignored as no-ops. */
  add(term: LexiconTerm): void {
    const canonicalTokens = tokenize(term.canonical).map((t) => t.norm);
    if (canonicalTokens.length === 0) return;
    const key = canonicalTokens.join(" ");

    const existing = this.#terms.get(key);
    const merged: LexiconTerm = existing
      ? {
          ...existing,
          ...term,
          heardAs: [...new Set([...(existing.heardAs ?? []), ...(term.heardAs ?? [])])],
        }
      : term;
    this.#terms.set(key, merged);

    for (const alias of merged.heardAs ?? []) {
      const aliasTokens = tokenize(alias).map((t) => t.norm);
      if (aliasTokens.length === 0) continue;
      const aliasKey = aliasTokens.join(" ");
      if (aliasKey === key) continue;
      this.#aliases.set(aliasKey, { tokens: canonicalTokens, term: merged });
      this.#maxAliasLength = Math.max(this.#maxAliasLength, aliasTokens.length);
    }
  }

  /** Looks up what the transcriber may have meant by a surface form. */
  lookup(heard: string): LexiconTerm[] {
    const key = tokenize(heard)
      .map((t) => t.norm)
      .join(" ");
    if (key.length === 0) return [];
    const direct = this.#terms.get(key);
    if (direct) return [direct];
    const alias = this.#aliases.get(key);
    return alias ? [alias.term] : [];
  }

  /** Rewrites alias phrases to canonical form, longest match first. */
  canonicalize(tokens: readonly string[]): CanonicalizeResult {
    const out: string[] = [];
    const applied: LexiconTerm[] = [];
    let substituted = false;

    for (let i = 0; i < tokens.length; ) {
      let matched = false;
      const maxLength = Math.min(this.#maxAliasLength, tokens.length - i);
      for (let length = maxLength; length >= 1; length--) {
        const key = tokens.slice(i, i + length).join(" ");
        const alias = this.#aliases.get(key);
        if (!alias) continue;
        out.push(...alias.tokens);
        applied.push(alias.term);
        substituted = true;
        i += length;
        matched = true;
        break;
      }
      if (!matched) {
        const token = tokens[i];
        if (token !== undefined) out.push(token);
        i += 1;
      }
    }

    return { tokens: out, substituted, applied };
  }

  canonicalizeText(text: string): CanonicalizeResult {
    return this.canonicalize(tokenize(text).map((t) => t.norm));
  }

  /**
   * Canonical spellings for transcription biasing. Multi-word and acronym-shaped terms come first
   * because those are the ones recognizers mangle; providers cap the list.
   */
  keywords(limit = 100): string[] {
    const score = (term: LexiconTerm): number => {
      let value = (term.heardAs?.length ?? 0) * 2;
      if (/[A-Z]{2,}/.test(term.canonical)) value += 3;
      if (/\s|[-_/]/.test(term.canonical)) value += 2;
      if (term.scope === "workspace") value += 4;
      if (term.scope === "user") value += 2;
      return value;
    };
    return this.terms()
      .slice()
      .sort((a, b) => score(b) - score(a) || a.canonical.localeCompare(b.canonical))
      .slice(0, limit)
      .map((t) => t.canonical);
  }

  /** Free-text biasing hint for transcription models that take a prompt instead of a keyword list. */
  biasPrompt(preamble: string, limit = 100): string {
    const words = this.keywords(limit);
    if (words.length === 0) return preamble;
    return `${preamble} ${words.join(", ")}.`;
  }

  /** Terms whose canonical or alias forms appear in the given text, for artifact provenance. */
  termsUsedIn(text: string): LexiconTerm[] {
    const tokens = tokenize(text).map((t) => t.norm);
    const used = new Map<string, LexiconTerm>();
    for (let i = 0; i < tokens.length; i++) {
      const maxLength = Math.min(this.#maxAliasLength, tokens.length - i);
      for (let length = maxLength; length >= 1; length--) {
        const key = tokens.slice(i, i + length).join(" ");
        const term = this.#terms.get(key) ?? this.#aliases.get(key)?.term;
        if (term) {
          used.set(term.canonical, term);
          break;
        }
      }
    }
    return [...used.values()];
  }

  clone(): Lexicon {
    return new Lexicon(this.terms());
  }
}

/** Normalizes a spoken term for use as a lexicon key. */
export function termKey(value: string): string {
  return tokenize(value)
    .map((t) => normalizeToken(t.raw))
    .join(" ");
}
