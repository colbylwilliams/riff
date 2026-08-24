//! Runs the language-neutral conformance suite in `core/conformance`.
//!
//! The TypeScript and Swift engines run the identical cases. That is what makes "the same agent
//! everywhere" a checkable claim rather than an aspiration: a prompt captured on a phone and one
//! captured at a desk are held to the same standard and render to the same bytes.

use std::sync::LazyLock;

use riff_core::{
    AgentBundle, Anchor, GroundingChecker, Json, Lexicon, LexiconTerm, Line, LineGrounding,
    RenderOptions, Section, Take, UtteranceLedger, UtteranceSource, render_prompt,
};

static BUNDLE: LazyLock<AgentBundle> =
    LazyLock::new(|| AgentBundle::bundled().expect("the vendored bundle must load"));

/// The cases are mirrored out of `core/conformance/cases` by `npm run bundle`, so a binding that
/// cannot run Node still proves it satisfies them.
fn load(name: &str) -> Json {
    let path = format!("{}/tests/resources/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("conformance case file {path} is missing: {error}"));
    Json::parse(&text).unwrap_or_else(|error| panic!("{path} is not valid JSON: {error}"))
}

fn cases(suite: &Json) -> Vec<Json> {
    suite
        .get("cases")
        .map(Json::array_or_empty)
        .unwrap_or(&[])
        .to_vec()
}

fn terms(value: Option<&Json>) -> Vec<LexiconTerm> {
    value
        .map(Json::array_or_empty)
        .unwrap_or(&[])
        .iter()
        .map(LexiconTerm::from_json)
        .collect()
}

fn strings(value: Option<&Json>) -> Vec<String> {
    value
        .map(Json::array_or_empty)
        .unwrap_or(&[])
        .iter()
        .filter_map(|entry| entry.as_str().map(str::to_owned))
        .collect()
}

#[test]
fn the_suite_is_pinned_to_the_thresholds_the_agent_actually_ships_with() {
    let suite = load("grounding.json");
    let defaults = suite.get("defaults").expect("defaults");

    assert_eq!(
        defaults.get("threshold").and_then(Json::as_f64),
        Some(BUNDLE.grounding.threshold)
    );
    assert_eq!(
        defaults.get("titleThreshold").and_then(Json::as_f64),
        BUNDLE.grounding.title_threshold
    );
    assert_eq!(
        defaults.get("windowSize").and_then(Json::as_usize),
        Some(BUNDLE.grounding.window_size)
    );
}

#[test]
fn grounding() {
    let suite = load("grounding.json");
    let mut failures: Vec<String> = Vec::new();

    for case in cases(&suite) {
        let id = case.get_str("id").unwrap_or_default();
        let detail = case.get_str("description").unwrap_or_default();
        let mut fail = |message: String| failures.push(format!("{id}: {detail}\n    {message}"));

        let lexicon = Lexicon::new(terms(case.get("lexicon")));
        let mut ledger = UtteranceLedger::new(BUNDLE.grounding.window_size, false);
        for text in strings(case.get("utterances")) {
            ledger.append(
                &text,
                "1970-01-01T00:00:00.000Z",
                UtteranceSource::Speech,
                None,
            );
        }

        let checker = GroundingChecker::new(&BUNDLE.grounding);
        let spans = ledger.spans(&lexicon).to_vec();
        let candidate = case.get_str("candidate").unwrap_or_default();
        let result = if case.get_str("mode") == Some("title") {
            checker.check_title(candidate, &spans, &lexicon)
        } else {
            checker.check(candidate, &spans, &lexicon)
        };

        let expect = case.get("expect").cloned().unwrap_or_else(Json::object);
        let expect_ok = expect.get("ok").and_then(Json::as_bool).unwrap_or(false);
        if result.ok != expect_ok {
            fail(format!(
                "expected ok={expect_ok}, got ok={} (ratio {}, kind {})",
                result.ok,
                result.ratio,
                result.kind.as_str()
            ));
        }
        if let Some(ratio) = expect.get("ratio").and_then(Json::as_f64)
            && result.ratio != ratio
        {
            fail(format!("expected ratio {ratio}, got {}", result.ratio));
        }
        if let Some(kind) = expect.get_str("kind")
            && result.kind.as_str() != kind
        {
            fail(format!(
                "expected kind {kind}, got {}",
                result.kind.as_str()
            ));
        }
        for token in strings(expect.get("unmatchedIncludes")) {
            if !result.unmatched_tokens.contains(&token) {
                fail(format!("expected \"{token}\" to be reported as not said"));
            }
        }
        if let Some(sources) = expect.get("sourceUtterances") {
            let mut expected = strings(Some(sources));
            expected.sort();
            let mut actual = result.source_utterance_ids.clone();
            actual.sort();
            if actual != expected {
                fail(format!("expected sources {expected:?}, got {actual:?}"));
            }
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn biasing_vocabulary() {
    let suite = load("lexicon.json");
    let mut failures: Vec<String> = Vec::new();

    for case in cases(&suite) {
        let lexicon = Lexicon::new(terms(case.get("terms")));
        let limit = case.get("limit").and_then(Json::as_usize).unwrap_or(100);
        let expected = strings(case.get("expect"));
        let actual = lexicon.keywords(limit);
        if actual != expected {
            failures.push(format!(
                "{}: {}\n    expected {expected:?}, got {actual:?}",
                case.get_str("id").unwrap_or_default(),
                case.get_str("description").unwrap_or_default(),
            ));
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn rendering() {
    let suite = load("render.json");
    let mut failures: Vec<String> = Vec::new();

    for case in cases(&suite) {
        let source = case.get("take").cloned().unwrap_or_else(Json::object);
        let mut take = Take::new("t1", "2026-01-01T00:00:00.000Z", None);
        if let Some(title) = source.get_str("title") {
            take.title = Some(riff_core::Title {
                text: title.to_owned(),
                origin: riff_core::TitleOrigin::Spoken,
            });
        }

        for line in source.get("lines").map(Json::array_or_empty).unwrap_or(&[]) {
            let Some(section) = line.get_str("section").and_then(Section::parse) else {
                continue;
            };
            let id = take.next_line_id();
            let order = take.order_after(section, &Anchor::End);
            take.set_line(Line {
                id,
                section,
                text: line.get_str("text").unwrap_or_default().to_owned(),
                order,
                source_utterance_ids: Vec::new(),
                motif_id: None,
                supersedes: Vec::new(),
                grounding: LineGrounding::full(riff_core::GroundingKind::Verbatim),
            });
        }

        for item in source
            .get("context")
            .map(Json::array_or_empty)
            .unwrap_or(&[])
        {
            take.attach_context(riff_core::ContextItem {
                reference_id: item.get_str("referenceId").unwrap_or_default().to_owned(),
                kind: item.get_str("kind").unwrap_or_default().to_owned(),
                title: item.get_str("title").unwrap_or_default().to_owned(),
                identifier: item.get_str("identifier").map(str::to_owned),
                url: item.get_str("url").map(str::to_owned),
                actor: item.get_str("actor").map(str::to_owned),
                state: item.get_str("state").map(str::to_owned),
                resolved_from: item.get_str("resolvedFrom").map(str::to_owned),
                ..riff_core::ContextItem::default()
            });
        }

        let rendered = render_prompt(
            &take,
            RenderOptions {
                config: &BUNDLE.render,
                profile: case.get_str("profile"),
            },
        )
        .expect("every conformance profile is defined in the bundle");

        let expected = case.get_str("expect").unwrap_or_default();
        if rendered != expected {
            failures.push(format!(
                "{}: {}\n    expected:\n{expected}\n    got:\n{rendered}",
                case.get_str("id").unwrap_or_default(),
                case.get_str("description").unwrap_or_default(),
            ));
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
