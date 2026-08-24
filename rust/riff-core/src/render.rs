//! Turning a take into the Markdown the downstream agent receives, and into the artifact that
//! leaves the session.

use crate::draft::Take;
use crate::error::{Result, RiffError};
use crate::lexicon::Lexicon;
use crate::text::{count_words, tidy_whitespace};
use crate::types::{
    ArtifactTerm, ContextItem, GroundingKind, Line, PromptArtifact, Provenance, RenderConfig,
    RenderDisposition, SECTIONS, Section, Title, TitleOrigin,
};

/// Which render profile to use.
#[derive(Debug, Clone, Copy)]
pub struct RenderOptions<'a> {
    /// The profiles and labels the bundle ships with.
    pub config: &'a RenderConfig,
    /// Overrides `config.profile` when a host wants a different shape for a specific destination.
    pub profile: Option<&'a str>,
}

impl<'a> RenderOptions<'a> {
    /// The profile a render will actually use.
    pub fn name(&self) -> &'a str {
        self.profile.unwrap_or(&self.config.profile)
    }
}

/// Renders a take as the Markdown the downstream agent receives.
pub fn render_prompt(take: &Take, options: RenderOptions<'_>) -> Result<String> {
    let name = options.name();
    let profile = options
        .config
        .profile(name)
        .ok_or_else(|| RiffError::bundle(format!("unknown render profile \"{name}\"")))?;

    let disposition = |key: &str, fallback: RenderDisposition| {
        profile
            .iter()
            .find(|(section, _)| section == key)
            .map_or(fallback, |(_, disposition)| *disposition)
    };

    let mut blocks: Vec<String> = Vec::new();
    if let Some(title) = &take.title
        && disposition("title", RenderDisposition::Omit) == RenderDisposition::H1
    {
        blocks.push(format!("# {}", title.text));
    }

    for section in SECTIONS {
        let lines = take.lines_in(section);
        if lines.is_empty() {
            continue;
        }
        if let Some(block) = render_section(
            section,
            &lines,
            disposition(section.as_str(), RenderDisposition::Paragraphs),
            options.config,
        ) {
            blocks.push(block);
        }
    }

    let context = take.context();
    if !context.is_empty()
        && let Some(block) = render_context(
            context,
            disposition("context", RenderDisposition::LabeledList),
            options.config,
        )
    {
        blocks.push(block);
    }

    Ok(blocks.join("\n\n").trim().to_owned())
}

fn render_section(
    section: Section,
    lines: &[Line],
    disposition: RenderDisposition,
    config: &RenderConfig,
) -> Option<String> {
    let label = config.label(section.as_str());
    let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();

    match disposition {
        RenderDisposition::Omit => None,
        RenderDisposition::Paragraphs => Some(
            texts
                .iter()
                .map(|text| with_terminal_punctuation(text))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        RenderDisposition::LabeledList => {
            let mut block = vec![format!("**{label}**")];
            block.extend(texts.iter().map(|text| format!("- {text}")));
            Some(block.join("\n"))
        }
        RenderDisposition::SectionList => {
            let mut block = vec![format!("## {label}"), String::new()];
            block.extend(texts.iter().map(|text| format!("- {text}")));
            Some(block.join("\n"))
        }
        RenderDisposition::H1 => Some(format!("# {}", texts.join(" "))),
        RenderDisposition::Lines => Some(texts.join("\n")),
    }
}

fn render_context(
    items: &[ContextItem],
    disposition: RenderDisposition,
    config: &RenderConfig,
) -> Option<String> {
    if disposition == RenderDisposition::Omit {
        return None;
    }
    let label = config.label("context");
    let entries: Vec<String> = items
        .iter()
        .map(|item| format!("- {}", describe_context(item)))
        .collect();

    Some(if disposition == RenderDisposition::SectionList {
        let mut block = vec![format!("## {label}"), String::new()];
        block.extend(entries);
        block.join("\n")
    } else {
        let mut block = vec![format!("**{label}**")];
        block.extend(entries);
        block.join("\n")
    })
}

fn describe_context(item: &ContextItem) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(item.identifier.clone().unwrap_or_else(|| {
        if item.title.is_empty() {
            item.kind.clone()
        } else {
            item.title.clone()
        }
    }));
    if item.identifier.is_some() && !item.title.is_empty() {
        parts.push(format!("\"{}\"", item.title));
    }
    if let Some(state) = &item.state {
        parts.push(format!("({state})"));
    }
    if let Some(actor) = &item.actor {
        parts.push(format!("by {actor}"));
    }
    if let Some(url) = &item.url {
        parts.push(format!("— {url}"));
    }
    if let Some(resolved_from) = &item.resolved_from {
        parts.push(format!("— referred to as \"{resolved_from}\""));
    }
    parts.join(" ")
}

fn with_terminal_punctuation(text: &str) -> String {
    let trimmed = tidy_whitespace(text);
    if trimmed.ends_with(['.', '!', '?', ':', ';']) {
        trimmed
    } else {
        format!("{trimmed}.")
    }
}

/// What an artifact needs beyond the take itself.
pub struct BuildArtifactOptions<'a> {
    /// Which render profile the rendered prompt uses.
    pub render: RenderOptions<'a>,
    /// The vocabulary, so the artifact can carry the terms its body uses.
    pub lexicon: &'a Lexicon,
    /// How much was said in the session.
    pub utterance_count: usize,
    /// The build timestamp, which is part of the artifact's identity.
    pub now: &'a str,
    /// Where the prompt came from, filled in by the session.
    pub provenance: Provenance,
}

/// Turns a take into the artifact that leaves the session.
///
/// Fidelity is a token-weighted share of the body that is provably the speaker's, so a consumer can
/// tell at a glance whether a prompt was captured or composed, without reading it.
pub fn build_artifact(take: &Take, options: BuildArtifactOptions<'_>) -> Result<PromptArtifact> {
    let lines = take.lines();
    let rendered = render_prompt(take, options.render)?;

    let mut body_tokens = 0usize;
    let mut grounded_tokens = 0.0f64;
    for line in &lines {
        let words = count_words(&line.text);
        body_tokens += words;
        grounded_tokens += words as f64
            * if line.grounding.kind == GroundingKind::Derived {
                0.0
            } else {
                line.grounding.ratio
            };
    }

    let fidelity = if body_tokens == 0 {
        0.0
    } else {
        (grounded_tokens / body_tokens as f64 * 1000.0).round() / 1000.0
    };

    let body = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let terms = options
        .lexicon
        .terms_used_in(&body)
        .into_iter()
        .map(|term| ArtifactTerm {
            canonical: term.canonical,
            kind: term.kind,
            heard_as: term.heard_as,
            definition: term.definition,
        })
        .collect();

    let mut provenance = options.provenance;
    provenance.fidelity = fidelity;
    provenance.utterance_count = options.utterance_count;
    provenance.body_tokens = body_tokens;
    provenance.agent_authored_tokens = (body_tokens as f64 - grounded_tokens).round() as i64;

    Ok(PromptArtifact {
        id: format!("{}-{}", take.id, options.now),
        take_id: take.id.clone(),
        label: take.label.clone(),
        created_at: take.created_at.clone(),
        updated_at: take.updated_at.clone(),
        title: take.title.clone().unwrap_or_else(|| Title {
            text: fallback_title(take),
            origin: TitleOrigin::Derived,
        }),
        lines,
        context: take.context().to_vec(),
        terms,
        provenance,
        rendered,
        target: take.target.clone(),
        status: Some(take.status),
        submitted_at: None,
    })
}

fn fallback_title(take: &Take) -> String {
    let first = take
        .lines_in(Section::Intent)
        .into_iter()
        .next()
        .or_else(|| take.lines().into_iter().next());
    let Some(first) = first else {
        return "Untitled".to_owned();
    };
    let mut words = first
        .text
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    // Exactly one, matching the anchored single-character replacement the other bindings use. An
    // intent trailing off in an ellipsis derives "Fix this.." everywhere, or nowhere.
    if words.ends_with([',', '.', ';', ':']) {
        words.pop();
    }
    words
}

/// A one or two sentence account of what the draft covers, for when they ask how it is looking.
///
/// This is the agent's own summary and never becomes part of the prompt.
pub fn summarize_draft(take: &Take) -> String {
    let counts: Vec<(Section, usize)> = SECTIONS
        .iter()
        .map(|section| (*section, take.lines_in(*section).len()))
        .filter(|(_, count)| *count > 0)
        .collect();
    if counts.is_empty() {
        return "Nothing captured yet.".to_owned();
    }

    let described = counts
        .iter()
        .map(|(section, count)| {
            format!(
                "{count} {}{}",
                section.as_str().replace('_', " "),
                if *count == 1 { "" } else { "s" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    let context = take.context();
    let attached = if context.is_empty() {
        String::new()
    } else {
        format!(
            " Attached: {}.",
            context
                .iter()
                .map(|item| item
                    .identifier
                    .clone()
                    .unwrap_or_else(|| item.title.clone()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let intent = take.lines_in(Section::Intent).into_iter().next();
    let opening = intent.map_or(String::new(), |line| format!("{} ", line.text));
    format!("{opening}Captured {described}.{attached}")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft::Anchor;
    use crate::types::{GroundingKind, LineGrounding};

    fn take_with(intent: &str) -> Take {
        let mut take = Take::new("t1", "1970-01-01T00:00:00.000Z", None);
        let id = take.next_line_id();
        let order = take.order_after(Section::Intent, &Anchor::End);
        take.set_line(Line {
            id,
            section: Section::Intent,
            text: intent.to_owned(),
            order,
            source_utterance_ids: Vec::new(),
            motif_id: None,
            supersedes: Vec::new(),
            grounding: LineGrounding::full(GroundingKind::Verbatim),
        });
        take
    }

    #[test]
    fn a_derived_title_drops_one_trailing_mark_not_a_run_of_them() {
        // The other bindings apply an anchored single-character replacement, so a title has to lose
        // exactly one mark here too or the same draft yields different artifacts.
        assert_eq!(
            fallback_title(&take_with("fix the uploader.")),
            "fix the uploader"
        );
        assert_eq!(
            fallback_title(&take_with("fix the uploader...")),
            "fix the uploader.."
        );
        assert_eq!(
            fallback_title(&take_with("fix the uploader,")),
            "fix the uploader"
        );
        assert_eq!(
            fallback_title(&take_with("fix the uploader")),
            "fix the uploader"
        );
    }

    #[test]
    fn a_derived_title_keeps_the_first_eight_words() {
        let take = take_with("one two three four five six seven eight nine ten");
        assert_eq!(
            fallback_title(&take),
            "one two three four five six seven eight"
        );
    }

    #[test]
    fn an_empty_take_is_untitled() {
        assert_eq!(
            fallback_title(&Take::new("t1", "1970-01-01T00:00:00.000Z", None)),
            "Untitled"
        );
    }
}
