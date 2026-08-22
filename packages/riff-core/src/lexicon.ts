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
 * A term does two separate jobs, and only one of them is dangerous. Every term biases
 * transcription. Only a *corroborated* term also contributes a grounding alias, because aliases are
 * applied to both sides of a comparison: an alias for a word that is not really the same word lets
 * an invented line match different spoken words, which defeats the fidelity gate. Similarity is not
 * evidence of sameness — "cache" and "cash" are one edit apart and mean nothing alike — so the
 * agent cannot make an alias grounding-active by asserting it.
 */
export class Lexicon {
  #terms = new Map<string, LexiconTerm>();
  /** Normalized alias phrase, space joined, to the canonical token sequence. Corroborated only. */
  #aliases = new Map<string, { tokens: string[]; term: LexiconTerm }>();
  #maxAliasLength = 1;

  /** Seeded and host-supplied vocabulary is curated, so it is corroborated by construction. */
  constructor(terms: Iterable<LexiconTerm> = []) {
    for (const term of terms) this.add(term);
  }

  get size(): number {
    return this.#terms.size;
  }

  terms(): LexiconTerm[] {
    return [...this.#terms.values()];
  }

  /**
   * Adds or replaces a term. Aliases identical to the canonical form are ignored as no-ops.
   *
   * `corroborated: false` records the term for transcription biasing without letting its aliases
   * affect grounding. That is the setting for anything the agent asserts on its own.
   */
  add(term: LexiconTerm, options: { corroborated?: boolean } = {}): void {
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

    if (options.corroborated === false) return;

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
   * Canonical spellings for transcription biasing, most mangle-prone first.
   *
   * Ordering is part of the contract rather than a detail: this list is capped before it reaches the
   * provider, so the comparator decides which terms survive, and different biasing produces
   * different transcripts. Both scoring and tie-breaking are defined without locale rules so every
   * binding produces the same list.
   */
  keywords(limit = 100): string[] {
    return this.terms()
      .slice()
      .sort((a, b) => biasingScore(b) - biasingScore(a) || compareByCodePoint(a.canonical, b.canonical))
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

/**
 * Whether a surface form is plausibly a mishearing of a term, rather than a different word.
 *
 * This gates the model-callable `record_term`, and it is what keeps the lexicon from being a hole
 * in the fidelity gate. Aliases are applied to both sides of a grounding comparison, so an alias
 * normally cannot make a paraphrase match — but only because it maps two spellings of the *same*
 * word. An agent that could install `{canonical: "CSV", heardAs: ["database"]}` would make an
 * invented "CSV" line match spoken "database", and the gate would pass it.
 *
 * Curated vocabulary is exempt: the seed and workspace lexicons legitimately contain aliases that
 * are not orthographically close, such as "sequel" for SQL or "k8s" for Kubernetes.
 */
export function isPlausibleMishearing(heard: string, canonical: string): boolean {
  const collapse = (value: string): string =>
    tokenize(value)
      .map((t) => t.norm)
      .join("")
      .replace(/[^\p{L}\p{N}]/gu, "");

  const a = collapse(heard);
  const b = collapse(canonical);
  if (a.length < 2 || b.length < 2) return false;
  if (a === b) return true;

  return 1 - editDistance(a, b) / Math.max(a.length, b.length) >= MISHEARING_SIMILARITY;
}

const MISHEARING_SIMILARITY = 0.6;

function editDistance(a: string, b: string): number {
  let previous = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    const current = [i];
    for (let j = 1; j <= b.length; j++) {
      current[j] = Math.min(
        (previous[j] ?? 0) + 1,
        (current[j - 1] ?? 0) + 1,
        (previous[j - 1] ?? 0) + (a[i - 1] === b[j - 1] ? 0 : 1),
      );
    }
    previous = current;
  }
  return previous[b.length] ?? Math.max(a.length, b.length);
}

/**
 * How strongly a term should be biased toward during transcription. Terms with recorded
 * mishearings, acronym shapes, and multi-word names are the ones recognizers actually get wrong;
 * workspace vocabulary outranks general vocabulary because it is what this speaker will say.
 */
export function biasingScore(term: LexiconTerm): number {
  let value = (term.heardAs?.length ?? 0) * 2;
  // Two consecutive capitals, so acronyms score but ordinary CamelCase product names do not.
  if (/\p{Lu}\p{Lu}/u.test(term.canonical)) value += 3;
  if (/[\s\-_/]/.test(term.canonical)) value += 2;
  if (term.scope === "workspace") value += 4;
  if (term.scope === "user") value += 2;
  return value;
}

/**
 * Case-insensitive comparison by code point, falling back to the raw form.
 *
 * Deliberately not `localeCompare`: its result depends on the host locale, which would make the
 * biasing vocabulary differ between two devices running the same agent.
 */
export function compareByCodePoint(a: string, b: string): number {
  const compare = (left: string, right: string): number => {
    const leftPoints = [...left];
    const rightPoints = [...right];
    for (let i = 0; i < Math.min(leftPoints.length, rightPoints.length); i++) {
      const l = leftPoints[i]!.codePointAt(0)!;
      const r = rightPoints[i]!.codePointAt(0)!;
      if (l !== r) return l < r ? -1 : 1;
    }
    return leftPoints.length - rightPoints.length;
  };
  return compare(a.toLowerCase(), b.toLowerCase()) || compare(a, b);
}
