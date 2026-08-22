//! Text normalization shared by the grounding check, the lexicon, and rendering.
//!
//! Must stay behaviourally identical to the other bindings; the conformance suite is what proves
//! it. The patterns the other bindings express as regular expressions are written out by hand here
//! because the engine takes no third-party dependencies and the standard library has no regex.

/// Splits text into comparable tokens, already normalized.
///
/// Digits, dotted identifiers, and hyphenated words stay whole because `owner/repo#412`, `v1.2.3`,
/// and `cherry-pick` are single things when someone says them.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for character in text.chars() {
        // A token starts on a letter or digit and may then carry the punctuation that holds
        // `owner/repo`, `v1.2.3`, and `cherry-pick` together.
        let continues =
            !current.is_empty() && matches!(character, '\'' | '\u{2019}' | '_' | '.' | '-');
        if character.is_alphanumeric() || continues {
            current.push(character);
        } else {
            push_normalized(&mut current, &mut tokens);
        }
    }
    push_normalized(&mut current, &mut tokens);
    tokens
}

fn push_normalized(current: &mut String, tokens: &mut Vec<String>) {
    if !current.is_empty() {
        let normalized = normalize_token(current);
        if !normalized.is_empty() {
            tokens.push(normalized);
        }
        current.clear();
    }
}

/// Lowercases, folds curly apostrophes, and trims punctuation that survived tokenization.
pub fn normalize_token(raw: &str) -> String {
    let lowered: String = raw
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character == '\u{2019}' {
                '\''
            } else {
                character
            }
        })
        .collect();

    let trimmed = lowered
        .trim_start_matches(['.', '_', '-'])
        .trim_end_matches(['.', '_', '-', '\'']);
    trimmed.to_owned()
}

/// Collapses whitespace and trims. Used before a line is stored, never to change wording.
pub fn tidy_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How many comparable tokens the text holds.
pub fn count_words(text: &str) -> usize {
    tokenize(text).len()
}

/// Single-token filler plus the multi-word filler phrases, split the way the checker needs them.
///
/// Phrases are matched by the caller, which needs positional context.
#[derive(Debug, Clone, Default)]
pub struct FillerMatcher {
    /// Filler that is one token, which the grounding check treats as ignorable wherever it appears.
    pub single: std::collections::HashSet<String>,
    /// Multi-token filler phrases, longest first.
    pub phrases: Vec<Vec<String>>,
}

impl FillerMatcher {
    /// Splits a filler list into single tokens and phrases.
    pub fn new(filler: &[String]) -> Self {
        let mut single = std::collections::HashSet::new();
        let mut phrases: Vec<Vec<String>> = Vec::new();
        for entry in filler {
            let tokens = tokenize(entry);
            match tokens.len() {
                1 => {
                    single.insert(tokens.into_iter().next().expect("length checked"));
                }
                0 => {}
                _ => phrases.push(tokens),
            }
        }
        phrases.sort_by_key(|phrase| std::cmp::Reverse(phrase.len()));
        Self { single, phrases }
    }
}

/// Strips credentials that can arrive through typed input before anything is stored.
///
/// A pattern-based backstop rather than a complete defense: extend the patterns when a new
/// credential shape shows up, and never move the call site downstream of the ledger.
pub fn redact_secrets(text: &str) -> String {
    let mut out = text.to_owned();
    for pattern in SECRET_PATTERNS {
        out = redact(&out, pattern);
    }
    out
}

const REDACTED: &str = "[redacted]";

/// A credential shape: a literal prefix at a word boundary, then a run of permitted characters.
struct SecretPattern {
    /// Alternatives for the prefix, matched in order. Empty `word_prefix` means a literal prefix.
    prefix: Prefix,
    /// Characters the secret body may contain.
    body: CharClass,
    /// How many body characters are needed for this to be a credential rather than a word.
    min_body: usize,
    /// Whether the match must end on a word boundary.
    ends_on_boundary: bool,
    /// Whether the body length is exactly `min_body` rather than at least it.
    exact_body: bool,
}

enum Prefix {
    /// A literal, such as `github_pat_`.
    Literal(&'static str),
    /// A literal with one character from a set in the middle, such as `gh[pousr]_`.
    Templated {
        before: &'static str,
        one_of: &'static str,
        after: &'static str,
    },
    /// One of several keywords, case-insensitive, followed by whitespace: `Bearer <secret>`.
    KeywordThenSpace(&'static [&'static str]),
}

#[derive(Clone, Copy)]
enum CharClass {
    /// `[A-Za-z0-9_]`
    AlphanumericUnderscore,
    /// `[A-Za-z0-9]`
    Alphanumeric,
    /// `[A-Za-z0-9_-]`
    AlphanumericUnderscoreDash,
    /// `[A-Za-z0-9-]`
    AlphanumericDash,
    /// `[0-9A-Z]`
    UppercaseAlphanumeric,
    /// `[A-Za-z0-9._-]`
    Token,
}

impl CharClass {
    fn matches(self, byte: u8) -> bool {
        match self {
            CharClass::AlphanumericUnderscore => byte.is_ascii_alphanumeric() || byte == b'_',
            CharClass::Alphanumeric => byte.is_ascii_alphanumeric(),
            CharClass::AlphanumericUnderscoreDash => {
                byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
            }
            CharClass::AlphanumericDash => byte.is_ascii_alphanumeric() || byte == b'-',
            CharClass::UppercaseAlphanumeric => byte.is_ascii_digit() || byte.is_ascii_uppercase(),
            CharClass::Token => byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'),
        }
    }
}

const SECRET_PATTERNS: &[SecretPattern] = &[
    SecretPattern {
        prefix: Prefix::Literal("github_pat_"),
        body: CharClass::AlphanumericUnderscore,
        min_body: 20,
        ends_on_boundary: false,
        exact_body: false,
    },
    SecretPattern {
        prefix: Prefix::Templated {
            before: "gh",
            one_of: "pousr",
            after: "_",
        },
        body: CharClass::Alphanumeric,
        min_body: 16,
        ends_on_boundary: false,
        exact_body: false,
    },
    SecretPattern {
        prefix: Prefix::Literal("sk-"),
        body: CharClass::AlphanumericUnderscoreDash,
        min_body: 20,
        ends_on_boundary: false,
        exact_body: false,
    },
    SecretPattern {
        prefix: Prefix::Literal("ek_"),
        body: CharClass::AlphanumericUnderscoreDash,
        min_body: 20,
        ends_on_boundary: false,
        exact_body: false,
    },
    SecretPattern {
        prefix: Prefix::Literal("AKIA"),
        body: CharClass::UppercaseAlphanumeric,
        min_body: 16,
        ends_on_boundary: true,
        exact_body: true,
    },
    SecretPattern {
        prefix: Prefix::Templated {
            before: "xox",
            one_of: "abposr",
            after: "-",
        },
        body: CharClass::AlphanumericDash,
        min_body: 10,
        ends_on_boundary: false,
        exact_body: false,
    },
    SecretPattern {
        prefix: Prefix::KeywordThenSpace(&["bearer", "token", "api_key", "api-key", "apikey"]),
        body: CharClass::Token,
        min_body: 20,
        ends_on_boundary: false,
        exact_body: false,
    },
];

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The next character starting at `at`, or `None` at the end or on a byte that starts nothing.
///
/// The width comes from the lead byte so this stays constant time: validating the rest of the input
/// on every whitespace character would make redaction quadratic in the length of a pasted line.
fn next_char(bytes: &[u8], at: usize) -> Option<char> {
    let width = match *bytes.get(at)? {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return None,
    };
    std::str::from_utf8(bytes.get(at..at + width)?)
        .ok()?
        .chars()
        .next()
}

/// Whitespace as the shared `\s` pattern means it: everything Unicode calls white space, plus the
/// byte order mark, which `char::is_whitespace` excludes and the pattern does not.
fn is_pattern_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}

/// Every pattern starts at a word boundary, so a credential shape inside a longer identifier is
/// left alone rather than half-redacted.
fn at_word_boundary(bytes: &[u8], at: usize) -> bool {
    at == 0 || !is_word_byte(bytes[at - 1])
}

fn redact(text: &str, pattern: &SecretPattern) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;

    while at < bytes.len() {
        match match_at(bytes, at, pattern) {
            Some(end) => {
                out.push_str(&text[copied..at]);
                out.push_str(REDACTED);
                copied = end;
                at = end;
            }
            None => at += 1,
        }
    }

    out.push_str(&text[copied..]);
    out
}

fn match_at(bytes: &[u8], at: usize, pattern: &SecretPattern) -> Option<usize> {
    if !at_word_boundary(bytes, at) {
        return None;
    }

    let body_start = match &pattern.prefix {
        Prefix::Literal(literal) => {
            if !bytes[at..].starts_with(literal.as_bytes()) {
                return None;
            }
            at + literal.len()
        }
        Prefix::Templated {
            before,
            one_of,
            after,
        } => {
            let middle = at + before.len();
            if !bytes[at..].starts_with(before.as_bytes())
                || !bytes
                    .get(middle)
                    .is_some_and(|byte| one_of.as_bytes().contains(byte))
                || !bytes[middle + 1..].starts_with(after.as_bytes())
            {
                return None;
            }
            middle + 1 + after.len()
        }
        Prefix::KeywordThenSpace(keywords) => {
            let keyword = keywords.iter().find(|keyword| {
                bytes.len() >= at + keyword.len()
                    && bytes[at..at + keyword.len()].eq_ignore_ascii_case(keyword.as_bytes())
            })?;
            let mut after = at + keyword.len();
            let space_start = after;
            // Decoded as characters rather than bytes because the separator is `\s` in the shared
            // pattern: a credential pasted out of a rendered page arrives behind a non-breaking
            // space, and tidy_whitespace would then fold it into a clean, readable, unredacted line.
            while let Some(character) = next_char(bytes, after) {
                if !is_pattern_whitespace(character) {
                    break;
                }
                after += character.len_utf8();
            }
            if after == space_start {
                return None;
            }
            after
        }
    };

    let mut end = body_start;
    while end < bytes.len() && pattern.body.matches(bytes[end]) {
        end += 1;
        if pattern.exact_body && end - body_start == pattern.min_body {
            break;
        }
    }

    if end - body_start < pattern.min_body {
        return None;
    }
    if pattern.ends_on_boundary && bytes.get(end).copied().is_some_and(is_word_byte) {
        return None;
    }

    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_compound_identifiers_whole() {
        assert_eq!(
            tokenize("owner/repo#412 v1.2.3 cherry-pick"),
            ["owner", "repo", "412", "v1.2.3", "cherry-pick"]
        );
    }

    #[test]
    fn folds_case_and_curly_apostrophes() {
        assert_eq!(tokenize("Don\u{2019}t"), ["don't"]);
    }

    #[test]
    fn drops_punctuation_that_survived_tokenization() {
        assert_eq!(tokenize("hello, world."), ["hello", "world"]);
        assert_eq!(normalize_token("__weird__"), "weird");
    }

    #[test]
    fn redacts_credentials_that_arrive_by_keyboard() {
        let redacted = redact_secrets(
            "use ghp_abcdefghijklmnopqrstuvwxyz012345 and sk-abcdefghijklmnopqrstuvwx today",
        );
        assert_eq!(redacted, "use [redacted] and [redacted] today");
    }

    #[test]
    fn redacts_a_labelled_credential() {
        assert_eq!(
            redact_secrets("Authorization: Bearer abcdefghijklmnopqrstuvwxyz"),
            "Authorization: [redacted]"
        );
        assert_eq!(
            redact_secrets("api-key   abcdefghijklmnopqrstuvwxyz"),
            "[redacted]"
        );
    }

    #[test]
    fn redacts_a_credential_behind_the_whitespace_a_paste_produces() {
        // A token copied out of a rendered page arrives behind a non-breaking space. Missing it
        // would be worse than a no-op: `tidy_whitespace` runs afterwards and would fold the line
        // into a clean, readable, unredacted credential on its way into the ledger.
        for separator in ['\u{a0}', '\u{2009}', '\u{3000}', '\u{feff}', '\u{b}'] {
            let text = format!("Bearer{separator}abcdefghijklmnopqrstuvwxyz");
            assert_eq!(
                redact_secrets(&text),
                "[redacted]",
                "U+{:04X} separated the credential",
                separator as u32
            );
        }
    }

    #[test]
    fn leaves_ordinary_words_alone() {
        // "ask-" contains "sk-", but not at a word boundary, and a short suffix is not a secret.
        assert_eq!(
            redact_secrets("ask-me-about-the-token"),
            "ask-me-about-the-token"
        );
        assert_eq!(redact_secrets("token short"), "token short");
    }

    #[test]
    fn requires_an_access_key_id_to_end_where_the_shape_ends() {
        assert_eq!(redact_secrets("AKIAIOSFODNN7EXAMPLE"), "[redacted]");
        assert_eq!(
            redact_secrets("AKIAIOSFODNN7EXAMPLEEXTRA"),
            "AKIAIOSFODNN7EXAMPLEEXTRA"
        );
    }
}
