//! Everything the speaker said, in order, as the transcriber produced it.
//!
//! This is the only source the prompt body may draw from. It is append-and-revise: a transcript can
//! be corrected while it is still streaming, but nothing is ever removed, because a superseded
//! sentence still has to be provable as theirs if it turns out they meant it after all.

use crate::grounding::SourceSpan;
use crate::lexicon::Lexicon;
use crate::text::{redact_secrets, tidy_whitespace, tokenize};
use crate::types::{Utterance, UtteranceSource};

/// The append-only record of what was said.
///
/// Only completed transcripts and typed input reach it — never environment notes, tool results, host
/// data, or anything the model says. That single-entrance rule is what makes the fidelity guarantee
/// hold, so widening it is a change to the product rather than to this file.
#[derive(Debug)]
pub struct UtteranceLedger {
    utterances: Vec<Utterance>,
    sequence: usize,
    spans: Option<Vec<SourceSpan>>,
    window_size: usize,
    redact: bool,
}

impl UtteranceLedger {
    /// A ledger that windows `window_size` utterances and, when `redact` is set, strips credentials
    /// on the way in.
    pub fn new(window_size: usize, redact: bool) -> Self {
        Self {
            utterances: Vec::new(),
            sequence: 0,
            spans: None,
            window_size,
            redact,
        }
    }

    /// How much has been said.
    pub fn len(&self) -> usize {
        self.utterances.len()
    }

    /// Whether nothing has been said yet.
    pub fn is_empty(&self) -> bool {
        self.utterances.is_empty()
    }

    /// Records something the speaker said.
    pub fn append(
        &mut self,
        text: &str,
        at: impl Into<String>,
        source: UtteranceSource,
        confidence: Option<f64>,
    ) -> Utterance {
        let cleaned = tidy_whitespace(&if self.redact {
            redact_secrets(text)
        } else {
            text.to_owned()
        });

        self.sequence += 1;
        let utterance = Utterance {
            id: format!("u{}", self.sequence),
            text: cleaned,
            at: at.into(),
            source,
            confidence,
        };
        self.utterances.push(utterance.clone());
        self.spans = None;
        utterance
    }

    /// Replaces the text of an utterance when a streaming transcript is finalized or corrected.
    pub fn revise(&mut self, id: &str, text: &str) -> Option<Utterance> {
        let cleaned = tidy_whitespace(&if self.redact {
            redact_secrets(text)
        } else {
            text.to_owned()
        });
        let utterance = self
            .utterances
            .iter_mut()
            .find(|utterance| utterance.id == id)?;
        utterance.text = cleaned;
        let revised = utterance.clone();
        self.spans = None;
        Some(revised)
    }

    /// One utterance by id.
    pub fn get(&self, id: &str) -> Option<&Utterance> {
        self.utterances.iter().find(|utterance| utterance.id == id)
    }

    /// Everything said, in order.
    pub fn all(&self) -> &[Utterance] {
        &self.utterances
    }

    /// Call when the lexicon changes, so cached spans pick up the new corrections.
    pub fn invalidate(&mut self) {
        self.spans = None;
    }

    /// Every window of up to `window_size` consecutive utterances, longest first.
    ///
    /// Windows exist because transcription splits on pauses rather than on sentences, so one spoken
    /// sentence routinely arrives as two or three utterances.
    pub fn spans(&mut self, lexicon: &Lexicon) -> &[SourceSpan] {
        if self.spans.is_none() {
            self.spans = Some(self.compute_spans(lexicon));
        }
        self.spans.as_deref().expect("just computed")
    }

    fn compute_spans(&self, lexicon: &Lexicon) -> Vec<SourceSpan> {
        let per_utterance: Vec<(Vec<String>, Vec<String>)> = self
            .utterances
            .iter()
            .map(|utterance| {
                let raw_tokens = tokenize(&utterance.text);
                let tokens = lexicon.canonicalize(&raw_tokens).tokens;
                (tokens, raw_tokens)
            })
            .collect();

        let mut spans = Vec::new();
        let mut width = self.window_size.min(self.utterances.len());
        while width >= 1 {
            for start in 0..=self.utterances.len() - width {
                let members = &self.utterances[start..start + width];
                let mut tokens = Vec::new();
                let mut owners = Vec::new();
                let mut raw_tokens = Vec::new();
                for (offset, member) in members.iter().enumerate() {
                    let (member_tokens, member_raw) = &per_utterance[start + offset];
                    tokens.extend(member_tokens.iter().cloned());
                    owners.extend(std::iter::repeat_n(member.id.clone(), member_tokens.len()));
                    raw_tokens.extend(member_raw.iter().cloned());
                }
                spans.push(SourceSpan {
                    utterance_ids: members.iter().map(|member| member.id.clone()).collect(),
                    tokens,
                    owners,
                    raw_tokens,
                });
            }
            width -= 1;
        }
        spans
    }

    /// Recent transcript, for the gist readback and for host tools that need conversational context.
    pub fn recent_text(&self, count: usize) -> String {
        let start = self.utterances.len().saturating_sub(count);
        self.utterances[start..]
            .iter()
            .map(|utterance| utterance.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> UtteranceLedger {
        UtteranceLedger::new(4, true)
    }

    #[test]
    fn windows_reach_as_far_as_the_configured_size_and_no_further() {
        let mut ledger = ledger();
        for text in ["one", "two", "three", "four", "five"] {
            ledger.append(text, "now", UtteranceSource::Speech, None);
        }
        let lexicon = Lexicon::default();
        let widest = ledger
            .spans(&lexicon)
            .iter()
            .map(|span| span.utterance_ids.len())
            .max();
        assert_eq!(widest, Some(4));
    }

    #[test]
    fn strips_credentials_before_anything_is_stored() {
        let mut ledger = ledger();
        let utterance = ledger.append(
            "the key is ghp_abcdefghijklmnopqrstuvwxyz012345",
            "now",
            UtteranceSource::Typed,
            None,
        );
        assert_eq!(utterance.text, "the key is [redacted]");
    }

    #[test]
    fn revising_a_transcript_keeps_its_place() {
        let mut ledger = ledger();
        let first = ledger.append("roll it back", "now", UtteranceSource::Speech, None);
        ledger.append("two releases", "now", UtteranceSource::Speech, None);
        ledger.revise(&first.id, "roll it forward");
        assert_eq!(ledger.all()[0].text, "roll it forward");
        assert_eq!(ledger.len(), 2);
    }
}
