import type { GroundingConfig, GroundingResult } from "./types.ts";
import type { Lexicon } from "./lexicon.ts";
import { buildFillerMatcher, tokenize } from "./text.ts";

/**
 * A stretch of what the speaker said that a candidate line can be checked against. Spans cover
 * several consecutive utterances so a sentence split across transcription boundaries still matches.
 */
export interface SourceSpan {
  utteranceIds: string[];
  /** Canonicalized, normalized tokens. */
  tokens: string[];
  /** Utterance id each token came from, parallel to `tokens`. */
  owners: string[];
  /** Tokens before lexicon canonicalization, for detecting an exact verbatim match. */
  rawTokens: string[];
}

export interface GroundingChecker {
  check(candidate: string, spans: readonly SourceSpan[]): GroundingResult;
  /** Looser check for the title, which is a label rather than part of the request. */
  checkTitle(candidate: string, spans: readonly SourceSpan[]): GroundingResult;
}

const MAX_CANDIDATE_TOKENS = 240;
const MAX_SPAN_TOKENS = 600;

/**
 * Decides whether a line the agent wants to put in the prompt is actually made of the speaker's
 * words.
 *
 * The permitted edit is deletion: drop filler, drop false starts, drop whole sentences, fix a
 * misheard word. That makes a grounded line an ordered subsequence of something the speaker said,
 * modulo lexicon corrections and a small set of connective tokens. Longest common subsequence
 * measures exactly that, and because it is order sensitive it also catches words being shuffled
 * inside a sentence, which reads as a paraphrase even when every word is theirs.
 */
export function createGroundingChecker(config: GroundingConfig, lexicon: Lexicon): GroundingChecker {
  const free = new Set(config.freeTokens.map((t) => t.toLowerCase()));
  const filler = buildFillerMatcher(config.filler);
  const ignorable = (token: string): boolean => free.has(token) || filler.single.has(token);

  const evaluate = (candidate: string, spans: readonly SourceSpan[], threshold: number, allowDerived: boolean): GroundingResult => {
    const rawTokens = tokenize(candidate)
      .map((t) => t.norm)
      .slice(0, MAX_CANDIDATE_TOKENS);
    const { tokens: candTokens, substituted } = lexicon.canonicalize(rawTokens);

    const substantiveIndexes = candTokens.map((t, i) => (ignorable(t) ? -1 : i)).filter((i) => i >= 0);
    const totalSubstantive = substantiveIndexes.length;

    const empty: GroundingResult = {
      ok: false,
      ratio: 0,
      kind: allowDerived ? "derived" : "trimmed",
      sourceUtteranceIds: [],
      unmatchedTokens: candTokens.filter((t) => !ignorable(t)),
    };

    if (totalSubstantive === 0 || spans.length === 0) {
      return allowDerived && totalSubstantive === 0
        ? { ...empty, ok: false }
        : empty;
    }

    const candCounts = countTokens(candTokens.filter((t) => !ignorable(t)));

    let best: {
      ratio: number;
      span: SourceSpan;
      matchedCandIndexes: Set<number>;
      matchedOwners: Set<string>;
    } | null = null;

    for (const span of spans) {
      if (span.tokens.length === 0) continue;
      // Multiset containment upper-bounds the ordered match, so this prunes without false negatives.
      if (containment(candCounts, span.tokens, totalSubstantive) < threshold) continue;

      const spanTokens = span.tokens.length > MAX_SPAN_TOKENS ? span.tokens.slice(-MAX_SPAN_TOKENS) : span.tokens;
      const offset = span.tokens.length - spanTokens.length;
      const pairs = longestCommonSubsequence(candTokens, spanTokens);

      const matchedCandIndexes = new Set<number>();
      const matchedOwners = new Set<string>();
      let matchedSubstantive = 0;
      for (const [candIndex, spanIndex] of pairs) {
        matchedCandIndexes.add(candIndex);
        const token = candTokens[candIndex];
        if (token !== undefined && !ignorable(token)) matchedSubstantive += 1;
        const owner = span.owners[spanIndex + offset];
        if (owner !== undefined) matchedOwners.add(owner);
      }

      const ratio = matchedSubstantive / totalSubstantive;
      if (!best || ratio > best.ratio) best = { ratio, span, matchedCandIndexes, matchedOwners };
      if (ratio === 1) break;
    }

    if (!best) {
      return empty;
    }

    const unmatchedTokens = candTokens.filter((token, i) => !ignorable(token) && !best.matchedCandIndexes.has(i));
    const sourceUtteranceIds = best.span.utteranceIds.filter((id) => best.matchedOwners.has(id));

    // The lexicon usually corrects the source rather than the candidate: the transcriber wrote
    // "get hub" and the agent wrote "GitHub". Comparing the match with and without canonicalization
    // is what tells a corrected line apart from a merely trimmed one.
    const rawSubstantiveTotal = rawTokens.filter((token) => !ignorable(token)).length;
    let rawMatchedSubstantive = 0;
    for (const [candIndex] of longestCommonSubsequence(rawTokens, best.span.rawTokens)) {
      const token = rawTokens[candIndex];
      if (token !== undefined && !ignorable(token)) rawMatchedSubstantive += 1;
    }
    const rawRatio = rawSubstantiveTotal === 0 ? 0 : rawMatchedSubstantive / rawSubstantiveTotal;
    const lexiconHelped = substituted || rawRatio < best.ratio;

    let kind: GroundingResult["kind"];
    if (best.ratio < threshold) {
      kind = allowDerived ? "derived" : "trimmed";
    } else if (lexiconHelped) {
      kind = "corrected";
    } else if (sameSequence(rawTokens, best.span.rawTokens)) {
      kind = "verbatim";
    } else {
      kind = "trimmed";
    }

    return {
      ok: best.ratio >= threshold,
      ratio: round(best.ratio),
      kind,
      sourceUtteranceIds,
      unmatchedTokens,
    };
  };

  return {
    check: (candidate, spans) => evaluate(candidate, spans, config.threshold, false),
    checkTitle: (candidate, spans) =>
      evaluate(candidate, spans, config.titleThreshold ?? config.threshold, true),
  };
}

function countTokens(tokens: readonly string[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const token of tokens) counts.set(token, (counts.get(token) ?? 0) + 1);
  return counts;
}

/** Fraction of the candidate's meaningful tokens present in the span, counting repeats. */
function containment(candCounts: Map<string, number>, spanTokens: readonly string[], total: number): number {
  if (total === 0) return 0;
  const spanCounts = countTokens(spanTokens);
  let shared = 0;
  for (const [token, count] of candCounts) {
    shared += Math.min(count, spanCounts.get(token) ?? 0);
  }
  return shared / total;
}

/** Returns matched index pairs of the longest common subsequence of two token sequences. */
function longestCommonSubsequence(a: readonly string[], b: readonly string[]): Array<[number, number]> {
  const rows = a.length;
  const cols = b.length;
  if (rows === 0 || cols === 0) return [];

  const width = cols + 1;
  const table = new Uint32Array((rows + 1) * width);

  for (let i = rows - 1; i >= 0; i--) {
    for (let j = cols - 1; j >= 0; j--) {
      table[i * width + j] =
        a[i] === b[j]
          ? (table[(i + 1) * width + (j + 1)] ?? 0) + 1
          : Math.max(table[(i + 1) * width + j] ?? 0, table[i * width + (j + 1)] ?? 0);
    }
  }

  const pairs: Array<[number, number]> = [];
  let i = 0;
  let j = 0;
  while (i < rows && j < cols) {
    if (a[i] === b[j]) {
      pairs.push([i, j]);
      i += 1;
      j += 1;
    } else if ((table[(i + 1) * width + j] ?? 0) >= (table[i * width + (j + 1)] ?? 0)) {
      i += 1;
    } else {
      j += 1;
    }
  }
  return pairs;
}

function sameSequence(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((token, i) => token === b[i]);
}

function round(value: number): number {
  return Math.round(value * 1000) / 1000;
}
