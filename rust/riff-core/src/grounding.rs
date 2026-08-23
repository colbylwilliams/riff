//! Decides whether a line the agent wants to put in the prompt is actually made of the speaker's
//! words.
//!
//! The permitted edit is deletion: drop filler, drop false starts, drop whole sentences, fix a
//! misheard word. That makes a grounded line an ordered subsequence of something the speaker said,
//! modulo lexicon corrections and a small set of connective tokens. Longest common subsequence
//! measures exactly that, and because it is order sensitive it also catches words being shuffled
//! inside a sentence, which reads as a paraphrase even when every word is theirs.

use std::collections::{HashMap, HashSet};

use crate::lexicon::Lexicon;
use crate::text::{FillerMatcher, tokenize};
use crate::types::{GroundingConfig, GroundingKind, GroundingResult};

/// A stretch of what the speaker said that a candidate line can be checked against.
///
/// Spans cover several consecutive utterances so a sentence split across transcription boundaries
/// still matches.
#[derive(Debug, Clone)]
pub struct SourceSpan {
    /// Which utterances the span covers.
    pub utterance_ids: Vec<String>,
    /// Canonicalized, normalized tokens.
    pub tokens: Vec<String>,
    /// Utterance id each token came from, parallel to `tokens`.
    pub owners: Vec<String>,
    /// Tokens before lexicon canonicalization, for detecting an exact verbatim match.
    pub raw_tokens: Vec<String>,
}

/// Longer than anything one person says in a turn. A candidate past this is a composed paragraph,
/// not a quotation.
const MAX_CANDIDATE_TOKENS: usize = 240;

/// The fidelity gate.
#[derive(Debug, Clone)]
pub struct GroundingChecker {
    free: HashSet<String>,
    filler: FillerMatcher,
    threshold: f64,
    title_threshold: f64,
}

impl GroundingChecker {
    /// Builds a checker from the thresholds and vocabulary the bundle ships with.
    pub fn new(config: &GroundingConfig) -> Self {
        Self {
            free: config
                .free_tokens
                .iter()
                .map(|token| token.to_lowercase())
                .collect(),
            filler: FillerMatcher::new(&config.filler),
            threshold: config.threshold,
            title_threshold: config.title_threshold.unwrap_or(config.threshold),
        }
    }

    /// Whether a body line is made of the speaker's words.
    pub fn check(
        &self,
        candidate: &str,
        spans: &[SourceSpan],
        lexicon: &Lexicon,
    ) -> GroundingResult {
        self.evaluate(candidate, spans, lexicon, self.threshold, false)
    }

    /// Looser check for the title, which is a label rather than part of the request.
    pub fn check_title(
        &self,
        candidate: &str,
        spans: &[SourceSpan],
        lexicon: &Lexicon,
    ) -> GroundingResult {
        self.evaluate(candidate, spans, lexicon, self.title_threshold, true)
    }

    fn ignorable(&self, token: &str) -> bool {
        self.free.contains(token) || self.filler.single.contains(token)
    }

    fn evaluate(
        &self,
        candidate: &str,
        spans: &[SourceSpan],
        lexicon: &Lexicon,
        threshold: f64,
        allow_derived: bool,
    ) -> GroundingResult {
        let raw_tokens = tokenize(candidate);
        let fallback_kind = if allow_derived {
            GroundingKind::Derived
        } else {
            GroundingKind::Trimmed
        };

        // Truncating here would check a prefix and let the caller store the whole string, so
        // anything invented past the limit would be recorded as fully grounded. The gate has to see
        // every token.
        if raw_tokens.len() > MAX_CANDIDATE_TOKENS {
            return GroundingResult {
                ok: false,
                ratio: 0.0,
                kind: fallback_kind,
                source_utterance_ids: Vec::new(),
                unmatched_tokens: Vec::new(),
                reason: Some(format!(
                    "that line is {} words, which is longer than one thing someone says; split it into separate lines",
                    raw_tokens.len()
                )),
            };
        }

        let canonicalized = lexicon.canonicalize(&raw_tokens);
        let cand_tokens = canonicalized.tokens;

        let substantive: Vec<String> = cand_tokens
            .iter()
            .filter(|token| !self.ignorable(token))
            .cloned()
            .collect();
        let total_substantive = substantive.len();

        let empty = GroundingResult {
            ok: false,
            ratio: 0.0,
            kind: fallback_kind,
            source_utterance_ids: Vec::new(),
            unmatched_tokens: substantive.clone(),
            reason: None,
        };

        if total_substantive == 0 || spans.is_empty() {
            return empty;
        }

        let cand_counts = count_tokens(&substantive);

        let mut best: Option<Best<'_>> = None;
        for span in spans {
            if span.tokens.is_empty() {
                continue;
            }
            // Multiset containment upper-bounds the ordered match, so this prunes without false
            // negatives.
            if containment(&cand_counts, &span.tokens, total_substantive) < threshold {
                continue;
            }

            let pairs = longest_common_subsequence(&cand_tokens, &span.tokens);

            let mut matched_cand_indexes = HashSet::new();
            let mut matched_owners = HashSet::new();
            let mut matched_substantive = 0usize;
            for (cand_index, span_index) in pairs {
                matched_cand_indexes.insert(cand_index);
                if !self.ignorable(&cand_tokens[cand_index]) {
                    matched_substantive += 1;
                }
                if let Some(owner) = span.owners.get(span_index) {
                    matched_owners.insert(owner.clone());
                }
            }

            let ratio = matched_substantive as f64 / total_substantive as f64;
            if best.as_ref().is_none_or(|current| ratio > current.ratio) {
                best = Some(Best {
                    ratio,
                    span,
                    matched_cand_indexes,
                    matched_owners,
                });
            }
            if ratio == 1.0 {
                break;
            }
        }

        let Some(best) = best else { return empty };

        let unmatched_tokens: Vec<String> = cand_tokens
            .iter()
            .enumerate()
            .filter(|(index, token)| {
                !self.ignorable(token) && !best.matched_cand_indexes.contains(index)
            })
            .map(|(_, token)| token.clone())
            .collect();
        let source_utterance_ids: Vec<String> = best
            .span
            .utterance_ids
            .iter()
            .filter(|id| best.matched_owners.contains(*id))
            .cloned()
            .collect();

        // The lexicon usually corrects the source rather than the candidate: the transcriber wrote
        // "get hub" and the agent wrote "GitHub". Comparing the match with and without
        // canonicalization is what tells a corrected line apart from a merely trimmed one.
        let raw_substantive_total = raw_tokens
            .iter()
            .filter(|token| !self.ignorable(token))
            .count();
        let raw_matched_substantive =
            longest_common_subsequence(&raw_tokens, &best.span.raw_tokens)
                .into_iter()
                .filter(|(cand_index, _)| !self.ignorable(&raw_tokens[*cand_index]))
                .count();
        let raw_ratio = if raw_substantive_total == 0 {
            0.0
        } else {
            raw_matched_substantive as f64 / raw_substantive_total as f64
        };
        let lexicon_helped = canonicalized.substituted || raw_ratio < best.ratio;

        let kind = if best.ratio < threshold {
            fallback_kind
        } else if lexicon_helped {
            GroundingKind::Corrected
        } else if raw_tokens == best.span.raw_tokens {
            GroundingKind::Verbatim
        } else {
            GroundingKind::Trimmed
        };

        GroundingResult {
            ok: best.ratio >= threshold,
            ratio: round(best.ratio),
            kind,
            source_utterance_ids,
            unmatched_tokens,
            reason: None,
        }
    }
}

struct Best<'a> {
    ratio: f64,
    span: &'a SourceSpan,
    matched_cand_indexes: HashSet<usize>,
    matched_owners: HashSet<String>,
}

fn count_tokens(tokens: &[String]) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for token in tokens {
        *counts.entry(token.as_str()).or_insert(0) += 1;
    }
    counts
}

/// Fraction of the candidate's meaningful tokens present in the span, counting repeats.
fn containment(cand_counts: &HashMap<&str, usize>, span_tokens: &[String], total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let span_counts = count_tokens(span_tokens);
    let mut shared = 0usize;
    for (token, count) in cand_counts {
        shared += (*count).min(span_counts.get(token).copied().unwrap_or(0));
    }
    shared as f64 / total as f64
}

/// Matched index pairs of the longest common subsequence, in O(min) memory.
///
/// A full DP table would be quadratic, which is why earlier versions capped the source span and
/// aligned only a window of it. Every one of those caps was wrong in a way nobody could see: a cap
/// on the candidate let invented text through, and a cap on the source rejected lines that were
/// entirely the speaker's. Hirschberg's divide and conquer gives the same alignment while holding
/// only two rows at a time, so the whole span is always compared and there is no cap to be wrong
/// about.
pub fn longest_common_subsequence(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    align(a, 0, a.len(), b, 0, b.len(), &mut pairs);
    pairs
}

fn align(
    a: &[String],
    a_start: usize,
    a_end: usize,
    b: &[String],
    b_start: usize,
    b_end: usize,
    out: &mut Vec<(usize, usize)>,
) {
    if a_end == a_start || b_end == b_start {
        return;
    }

    if a_end - a_start == 1 {
        if let Some(offset) = b[b_start..b_end]
            .iter()
            .position(|token| *token == a[a_start])
        {
            out.push((a_start, b_start + offset));
        }
        return;
    }

    let a_mid = a_start + (a_end - a_start) / 2;
    let forward = lcs_row(a, a_start, a_mid, b, b_start, b_end, false);
    let backward = lcs_row(a, a_mid, a_end, b, b_start, b_end, true);

    // Split the source where the two halves together match the most. Ties take the leftmost split so
    // every binding chooses the same alignment among equally long ones.
    let mut best_score = 0u32;
    let mut best_split = b_start;
    let mut first = true;
    for j in b_start..=b_end {
        let score = forward[j - b_start] + backward[b_end - j];
        if first || score > best_score {
            best_score = score;
            best_split = j;
            first = false;
        }
    }

    align(a, a_start, a_mid, b, b_start, best_split, out);
    align(a, a_mid, a_end, b, best_split, b_end, out);
}

/// Final DP row of the LCS lengths for one half, walked forwards or backwards.
fn lcs_row(
    a: &[String],
    a_start: usize,
    a_end: usize,
    b: &[String],
    b_start: usize,
    b_end: usize,
    reversed: bool,
) -> Vec<u32> {
    let width = b_end - b_start + 1;
    let mut previous = vec![0u32; width];
    let mut current = vec![0u32; width];

    for i in a_start..a_end {
        let left = if reversed {
            &a[a_end - 1 - (i - a_start)]
        } else {
            &a[i]
        };
        current[0] = 0;
        for j in 1..width {
            let right = if reversed {
                &b[b_end - j]
            } else {
                &b[b_start + j - 1]
            };
            current[j] = if left == right {
                previous[j - 1] + 1
            } else {
                previous[j].max(current[j - 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_ordered_alignment() {
        let a: Vec<String> = "a b c d".split(' ').map(str::to_owned).collect();
        let b: Vec<String> = "x a y b z d".split(' ').map(str::to_owned).collect();
        let pairs = longest_common_subsequence(&a, &b);
        assert_eq!(pairs, [(0, 1), (1, 3), (3, 5)]);
    }

    #[test]
    fn reports_no_alignment_when_nothing_lines_up() {
        let a: Vec<String> = vec!["a".into()];
        let b: Vec<String> = vec!["b".into()];
        assert!(longest_common_subsequence(&a, &b).is_empty());
    }
}
