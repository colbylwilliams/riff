/** Text normalization shared by the grounding check, the lexicon, and rendering. */

export interface Token {
  /** The token as it appeared. */
  raw: string;
  /** Lowercased, punctuation stripped. What comparisons use. */
  norm: string;
  /** Index into the original string, so a match can be traced back to the source text. */
  offset: number;
}

const WORD = /[\p{L}\p{N}][\p{L}\p{N}'’_.-]*/gu;

/**
 * Splits text into comparable tokens. Digits, dotted identifiers, and hyphenated words stay whole
 * because `owner/repo#412`, `v1.2.3`, and `cherry-pick` are single things when someone says them.
 */
export function tokenize(text: string): Token[] {
  const tokens: Token[] = [];
  for (const match of text.matchAll(WORD)) {
    const raw = match[0];
    const norm = normalizeToken(raw);
    if (norm.length > 0) tokens.push({ raw, norm, offset: match.index });
  }
  return tokens;
}

/** Lowercases, folds curly apostrophes, and trims punctuation that survived tokenization. */
export function normalizeToken(raw: string): string {
  return raw
    .toLowerCase()
    .replace(/[’]/g, "'")
    .replace(/^[._-]+|[._'-]+$/g, "");
}

export function normalizeText(text: string): string {
  return tokenize(text)
    .map((t) => t.norm)
    .join(" ");
}

/** Collapses whitespace and trims. Used before a line is stored, never to change wording. */
export function tidyWhitespace(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

export function countWords(text: string): number {
  return tokenize(text).length;
}

/**
 * Builds a lookup of single-token filler plus the first tokens of multi-word filler phrases.
 * Phrases are matched by the caller, which needs positional context.
 */
export function buildFillerMatcher(filler: string[]): {
  single: Set<string>;
  phrases: string[][];
} {
  const single = new Set<string>();
  const phrases: string[][] = [];
  for (const entry of filler) {
    const tokens = tokenize(entry).map((t) => t.norm);
    if (tokens.length === 1 && tokens[0]) single.add(tokens[0]);
    else if (tokens.length > 1) phrases.push(tokens);
  }
  phrases.sort((a, b) => b.length - a.length);
  return { single, phrases };
}

/** Escapes a string for safe inclusion in a Markdown table cell or inline span. */
export function escapeMarkdown(text: string): string {
  return text.replace(/([\\`*_{}[\]()#+\-.!|])/g, "\\$1");
}
