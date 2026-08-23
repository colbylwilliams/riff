//! Vocabulary the transcriber gets wrong, plus the corrections that fix it.
//!
//! A term does two separate jobs, and only one of them is dangerous. Every term biases
//! transcription. Only a *corroborated* term also contributes a grounding alias, because aliases are
//! applied to both sides of a comparison: an alias for a word that is not really the same word lets
//! an invented line match different spoken words, which defeats the fidelity gate. Similarity is not
//! evidence of sameness — "cache" and "cash" are one edit apart and mean nothing alike — so the
//! agent cannot make an alias grounding-active by asserting it.

use std::collections::HashMap;

use crate::text::{normalize_token, tokenize};
use crate::types::LexiconTerm;

/// What canonicalization did to a run of tokens.
#[derive(Debug, Clone, Default)]
pub struct Canonicalized {
    /// The tokens after alias phrases were rewritten.
    pub tokens: Vec<String>,
    /// True when at least one alias was rewritten, which distinguishes a corrected line from a
    /// trimmed one.
    pub substituted: bool,
    /// Which terms were applied.
    pub applied: Vec<LexiconTerm>,
}

/// The active vocabulary for a session.
#[derive(Debug, Clone, Default)]
pub struct Lexicon {
    terms: Vec<LexiconTerm>,
    /// Which terms something outside the conversation vouched for, parallel to `terms`.
    ///
    /// Held separately from the term because it is not a property of the vocabulary — it is a
    /// property of where the vocabulary came from, and it is the only thing standing between the
    /// agent and an alias of its own invention.
    corroborated: Vec<bool>,
    /// Normalized canonical form, space joined, to the position in `terms`.
    by_key: HashMap<String, usize>,
    /// Normalized alias phrase to the canonical token sequence and the term it belongs to.
    /// Corroborated only.
    aliases: HashMap<String, (Vec<String>, String)>,
    max_alias_length: usize,
}

impl Lexicon {
    /// Seeded and host-supplied vocabulary is curated, so it is corroborated by construction.
    pub fn new<I: IntoIterator<Item = LexiconTerm>>(terms: I) -> Self {
        let mut lexicon = Self {
            max_alias_length: 1,
            ..Self::default()
        };
        for term in terms {
            lexicon.add(term);
        }
        lexicon
    }

    /// How many terms the lexicon holds.
    pub fn len(&self) -> usize {
        self.terms.len()
    }

    /// Whether the lexicon is empty.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Every term, in the order they were first added.
    pub fn terms(&self) -> &[LexiconTerm] {
        &self.terms
    }

    /// Whether something outside this conversation vouched for the term.
    ///
    /// The plain [`Lexicon::lookup`] cannot answer this: an uncorroborated term is still recorded,
    /// because it still biases transcription. Asking `lookup` instead would let the agent corroborate
    /// its own assertion by making it twice — the second call would find what the first one stored.
    pub fn is_corroborated(&self, canonical: &str) -> bool {
        let key = tokenize(canonical).join(" ");
        self.by_key
            .get(&key)
            .and_then(|index| self.corroborated.get(*index))
            .copied()
            .unwrap_or(false)
    }

    /// Adds or replaces a curated term, whose aliases affect grounding.
    pub fn add(&mut self, term: LexiconTerm) {
        self.add_with(term, true);
    }

    /// Adds or replaces a term. Aliases identical to the canonical form are ignored as no-ops.
    ///
    /// `corroborated: false` records the term for transcription biasing without letting its aliases
    /// affect grounding. That is the setting for anything the agent asserts on its own.
    pub fn add_with(&mut self, term: LexiconTerm, corroborated: bool) {
        let canonical_tokens = tokenize(&term.canonical);
        if canonical_tokens.is_empty() {
            return;
        }
        let key = canonical_tokens.join(" ");

        let merged = match self.by_key.get(&key).copied() {
            Some(index) => {
                let existing = &self.terms[index];
                let mut heard_as = existing.heard_as.clone();
                for alias in &term.heard_as {
                    if !heard_as.contains(alias) {
                        heard_as.push(alias.clone());
                    }
                }
                let merged = LexiconTerm {
                    canonical: term.canonical.clone(),
                    kind: term.kind.clone(),
                    heard_as,
                    definition: term
                        .definition
                        .clone()
                        .or_else(|| existing.definition.clone()),
                    scope: term.scope.clone().or_else(|| existing.scope.clone()),
                };
                self.terms[index] = merged.clone();
                // Sticky: curated vocabulary that the agent later re-asserts does not stop being
                // vouched for, but an assertion never promotes a term on its own.
                self.corroborated[index] |= corroborated;
                merged
            }
            None => {
                self.by_key.insert(key.clone(), self.terms.len());
                self.terms.push(term.clone());
                self.corroborated.push(corroborated);
                term
            }
        };

        if !corroborated {
            return;
        }

        for alias in &merged.heard_as {
            let alias_tokens = tokenize(alias);
            if alias_tokens.is_empty() {
                continue;
            }
            let alias_key = alias_tokens.join(" ");
            if alias_key == key {
                continue;
            }
            self.max_alias_length = self.max_alias_length.max(alias_tokens.len());
            self.aliases
                .insert(alias_key, (canonical_tokens.clone(), key.clone()));
        }
    }

    /// Looks up what the transcriber may have meant by a surface form.
    pub fn lookup(&self, heard: &str) -> Vec<LexiconTerm> {
        let key = tokenize(heard).join(" ");
        if key.is_empty() {
            return Vec::new();
        }
        if let Some(term) = self
            .by_key
            .get(&key)
            .map(|index| self.terms[*index].clone())
        {
            return vec![term];
        }
        self.aliases
            .get(&key)
            .and_then(|(_, canonical_key)| self.by_key.get(canonical_key))
            .map(|index| vec![self.terms[*index].clone()])
            .unwrap_or_default()
    }

    /// Rewrites alias phrases to canonical form, longest match first.
    pub fn canonicalize(&self, tokens: &[String]) -> Canonicalized {
        let mut out = Vec::with_capacity(tokens.len());
        let mut applied = Vec::new();
        let mut substituted = false;

        let mut index = 0;
        while index < tokens.len() {
            let mut matched = false;
            let max_length = self.max_alias_length.min(tokens.len() - index);
            for length in (1..=max_length).rev() {
                let key = tokens[index..index + length].join(" ");
                let Some((canonical_tokens, canonical_key)) = self.aliases.get(&key) else {
                    continue;
                };
                out.extend(canonical_tokens.iter().cloned());
                if let Some(term) = self.by_key.get(canonical_key) {
                    applied.push(self.terms[*term].clone());
                }
                substituted = true;
                index += length;
                matched = true;
                break;
            }
            if !matched {
                out.push(tokens[index].clone());
                index += 1;
            }
        }

        Canonicalized {
            tokens: out,
            substituted,
            applied,
        }
    }

    /// Canonicalizes text rather than an already-tokenized run.
    pub fn canonicalize_text(&self, text: &str) -> Canonicalized {
        self.canonicalize(&tokenize(text))
    }

    /// Canonical spellings for transcription biasing, most mangle-prone first.
    ///
    /// Ordering is part of the contract rather than a detail: this list is capped before it reaches
    /// the provider, so the comparator decides which terms survive, and different biasing produces
    /// different transcripts. Both scoring and tie-breaking are defined without locale rules so every
    /// binding produces the same list.
    pub fn keywords(&self, limit: usize) -> Vec<String> {
        let mut terms: Vec<&LexiconTerm> = self.terms.iter().collect();
        terms.sort_by(|a, b| {
            biasing_score(b)
                .cmp(&biasing_score(a))
                .then_with(|| compare_by_code_point(&a.canonical, &b.canonical))
        });
        terms
            .into_iter()
            .take(limit)
            .map(|term| term.canonical.clone())
            .collect()
    }

    /// Free-text biasing hint for transcription models that take a prompt instead of a keyword list.
    pub fn bias_prompt(&self, preamble: &str, limit: usize) -> String {
        let words = self.keywords(limit);
        if words.is_empty() {
            return preamble.to_owned();
        }
        format!("{preamble} {}.", words.join(", "))
    }

    /// Terms whose canonical or alias forms appear in the given text, for artifact provenance.
    pub fn terms_used_in(&self, text: &str) -> Vec<LexiconTerm> {
        let tokens = tokenize(text);
        let mut used: Vec<LexiconTerm> = Vec::new();

        let mut index = 0;
        while index < tokens.len() {
            let max_length = self.max_alias_length.min(tokens.len() - index);
            for length in (1..=max_length).rev() {
                let key = tokens[index..index + length].join(" ");
                let position = self.by_key.get(&key).copied().or_else(|| {
                    self.aliases
                        .get(&key)
                        .and_then(|(_, canonical_key)| self.by_key.get(canonical_key).copied())
                });
                if let Some(position) = position {
                    let term = &self.terms[position];
                    if !used.iter().any(|seen| seen.canonical == term.canonical) {
                        used.push(term.clone());
                    }
                    break;
                }
            }
            index += 1;
        }

        used
    }
}

/// Normalizes a spoken term for use as a lexicon key.
pub fn term_key(value: &str) -> String {
    tokenize(value)
        .iter()
        .map(|token| normalize_token(token))
        .collect::<Vec<_>>()
        .join(" ")
}

/// How close two spellings have to be before one is treated as a mishearing of the other.
const MISHEARING_SIMILARITY: f64 = 0.6;

/// Whether a surface form is plausibly a mishearing of a term, rather than a different word.
///
/// This gates the model-callable `record_term`, and it is what keeps the lexicon from being a hole
/// in the fidelity gate. Aliases are applied to both sides of a grounding comparison, so an alias
/// normally cannot make a paraphrase match — but only because it maps two spellings of the *same*
/// word. An agent that could install `{canonical: "CSV", heardAs: ["database"]}` would make an
/// invented "CSV" line match spoken "database", and the gate would pass it.
///
/// Curated vocabulary is exempt: the seed and workspace lexicons legitimately contain aliases that
/// are not orthographically close, such as "sequel" for SQL or "k8s" for Kubernetes.
pub fn is_plausible_mishearing(heard: &str, canonical: &str) -> bool {
    let a = collapse(heard);
    let b = collapse(canonical);
    if a.len() < 2 || b.len() < 2 {
        return false;
    }
    if a == b {
        return true;
    }

    let longest = a.len().max(b.len()) as f64;
    1.0 - edit_distance(&a, &b) as f64 / longest >= MISHEARING_SIMILARITY
}

/// Strips everything but letters and digits, so spacing and punctuation cannot make two spellings of
/// the same word look different.
fn collapse(value: &str) -> Vec<char> {
    tokenize(value)
        .concat()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];

    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            let substitution = previous[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            current[j] = (previous[j] + 1).min(current[j - 1] + 1).min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[b.len()]
}

/// How strongly a term should be biased toward during transcription.
///
/// Terms with recorded mishearings, acronym shapes, and multi-word names are the ones recognizers
/// actually get wrong; workspace vocabulary outranks general vocabulary because it is what this
/// speaker will say.
pub fn biasing_score(term: &LexiconTerm) -> i64 {
    let mut value = term.heard_as.len() as i64 * 2;
    if has_consecutive_capitals(&term.canonical) {
        value += 3;
    }
    if term
        .canonical
        .chars()
        .any(|character| character.is_whitespace() || matches!(character, '-' | '_' | '/'))
    {
        value += 2;
    }
    match term.scope.as_deref() {
        Some("workspace") => value += 4,
        Some("user") => value += 2,
        _ => {}
    }
    value
}

/// Two consecutive capitals, so acronyms score but ordinary CamelCase product names do not.
fn has_consecutive_capitals(value: &str) -> bool {
    let mut previous_was_upper = false;
    for character in value.chars() {
        let upper = character.is_uppercase();
        if upper && previous_was_upper {
            return true;
        }
        previous_was_upper = upper;
    }
    false
}

/// Case-insensitive comparison by code point, falling back to the raw form.
///
/// Deliberately not a locale-aware collation: its result depends on the host locale, which would
/// make the biasing vocabulary differ between two devices running the same agent.
pub fn compare_by_code_point(a: &str, b: &str) -> std::cmp::Ordering {
    compare_code_points(&a.to_lowercase(), &b.to_lowercase())
        .then_with(|| compare_code_points(a, b))
}

fn compare_code_points(left: &str, right: &str) -> std::cmp::Ordering {
    for (a, b) in left.chars().zip(right.chars()) {
        let ordering = (a as u32).cmp(&(b as u32));
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    left.chars().count().cmp(&right.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_an_alias_to_its_canonical_spelling() {
        let mut lexicon = Lexicon::default();
        lexicon.add(LexiconTerm {
            canonical: "GitHub".into(),
            kind: "product".into(),
            heard_as: vec!["get hub".into()],
            ..LexiconTerm::default()
        });

        let result = lexicon.canonicalize_text("the get hub action");
        assert_eq!(result.tokens, ["the", "github", "action"]);
        assert!(result.substituted);
    }

    #[test]
    fn will_not_let_a_term_vouch_for_itself_by_being_asserted_twice() {
        // The agent records an unknown term, then records it again. The second call must not find
        // what the first one stored and treat that as corroboration, or the whole gate is a
        // formality the agent can step around.
        let mut lexicon = Lexicon::default();
        let term = LexiconTerm {
            canonical: "CSV".into(),
            kind: "other".into(),
            heard_as: vec!["database".into()],
            ..LexiconTerm::default()
        };

        for _ in 0..2 {
            let known = lexicon.is_corroborated(&term.canonical);
            assert!(!known, "nothing outside the conversation knows this term");
            lexicon.add_with(term.clone(), known);
        }

        let result = lexicon.canonicalize_text("we need to fix the database import");
        assert!(!result.substituted, "the alias must never have taken hold");
        assert_eq!(
            result.tokens.join(" "),
            "we need to fix the database import"
        );
    }

    #[test]
    fn treats_curated_vocabulary_as_vouched_for() {
        let lexicon = Lexicon::new([LexiconTerm::new("GitHub", "product")]);
        assert!(lexicon.is_corroborated("GitHub"));
        assert!(!lexicon.is_corroborated("Quaggle"));
    }

    #[test]
    fn keeps_an_uncorroborated_alias_out_of_grounding() {
        let mut lexicon = Lexicon::default();
        lexicon.add_with(
            LexiconTerm {
                canonical: "CSV".into(),
                kind: "format".into(),
                heard_as: vec!["database".into()],
                ..LexiconTerm::default()
            },
            false,
        );

        // The term still biases transcription, but it may not rewrite anything.
        assert_eq!(lexicon.keywords(10), ["CSV"]);
        assert_eq!(
            lexicon.canonicalize_text("the database").tokens,
            ["the", "database"]
        );
    }

    #[test]
    fn treats_a_close_spelling_as_a_mishearing_and_a_different_word_as_a_different_word() {
        assert!(is_plausible_mishearing("get hub", "GitHub"));
        assert!(!is_plausible_mishearing("database", "CSV"));
    }
}
