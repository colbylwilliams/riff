//! Behavior the conformance cases cannot express.
//!
//! The cases in `core/conformance` pin what every binding must decide; these pin how this binding
//! wires those decisions together — the single-entrance ledger rule, one continuation per batch of
//! tool calls, interruption, credential stripping, and a take that has been sent staying sent.

mod support;

use std::sync::Arc;

use riff_core::{
    AgentBundle, ContextItem, Destination, HostEnvironment, Json, MemoryStore, RiffEvent,
    RiffSession, RiffSessionOptions, SessionState, SystemClock, TakeStatus, json_object,
};
use support::{Call, FakeProvider, InstantClock, RecordingHost, StalledHost, block_on};

struct Harness {
    session: RiffSession,
    provider: Arc<FakeProvider>,
    host: Arc<RecordingHost>,
    store: Arc<MemoryStore>,
}

impl Harness {
    fn start() -> Harness {
        Harness::with(
            Arc::new(RecordingHost::default()),
            Arc::new(SystemClock::new()),
        )
    }

    fn with(host: Arc<RecordingHost>, clock: Arc<dyn riff_core::Clock>) -> Harness {
        let provider = Arc::new(FakeProvider::default());
        let store = Arc::new(MemoryStore::default());
        let bundle = Arc::new(AgentBundle::bundled().expect("the vendored bundle must load"));

        let mut options = RiffSessionOptions::new(bundle, provider.clone());
        options.host = host.clone();
        options.store = store.clone();
        options.clock = clock;

        let mut session = RiffSession::new(options);
        block_on(session.start()).expect("the fake provider always connects");

        Harness {
            session,
            provider,
            host,
            store,
        }
    }

    /// Steps the session once, which is enough because every test queues an event first.
    fn say(&mut self, text: &str) {
        self.provider.connection().say(text);
        self.step();
    }

    fn step(&mut self) {
        block_on(self.session.step());
    }

    fn call(&mut self, name: &str, arguments: Json) -> Json {
        let id = self.provider.connection().call_tool(name, arguments);
        self.step();
        self.provider.connection().result_for(&id)
    }

    fn calls(&self) -> Vec<Call> {
        self.provider.connection().calls()
    }
}

/// One `upsert_line` operation, which is what most of these tests are made of.
fn upsert(section: &str, text: &str) -> Json {
    Json::Object(json_object! {
        "operations" => vec![Json::Object(json_object! {
            "op" => "upsert_line",
            "section" => section,
            "text" => text,
        })],
    })
}

#[test]
fn hands_the_model_the_composed_instructions_the_tools_and_the_vocabulary() {
    let harness = Harness::start();
    let request = harness.provider.request();

    assert_eq!(request.instructions, harness.session.bundle.instructions);
    assert_eq!(request.tools.len(), harness.session.bundle.tools.len());
    assert!(
        request.vocabulary.contains(&"GitHub".to_owned()),
        "the seed lexicon must reach the transcriber"
    );
    assert_eq!(harness.session.state(), SessionState::Listening);
}

#[test]
fn records_what_was_said_and_lets_it_be_drafted_verbatim() {
    let mut harness = Harness::start();
    harness.say("the login page is broken on Safari");

    let result = harness.call(
        "draft_update",
        upsert("intent", "the login page is broken on Safari"),
    );

    assert_eq!(
        result.get("rejected").map(Json::array_or_empty),
        Some(&[][..])
    );
    assert_eq!(
        result
            .get("accepted")
            .map(|accepted| accepted.array_or_empty().len()),
        Some(1)
    );
    assert_eq!(result.get("fidelity").and_then(Json::as_f64), Some(1.0));

    let artifact = harness.session.artifact().expect("a take is active");
    assert_eq!(artifact.lines.len(), 1);
    assert_eq!(artifact.lines[0].text, "the login page is broken on Safari");
    assert!(
        artifact
            .rendered
            .contains("the login page is broken on Safari.")
    );
}

#[test]
fn rejects_a_paraphrase_and_tells_the_model_which_words_it_invented() {
    let mut harness = Harness::start();
    harness.say("the login page is busted on Safari");

    let result = harness.call(
        "draft_update",
        upsert(
            "intent",
            "the sign-in screen is not working correctly in Safari",
        ),
    );

    let rejected = result
        .get("rejected")
        .map(Json::array_or_empty)
        .unwrap_or(&[]);
    assert_eq!(rejected.len(), 1);
    let unmatched: Vec<&str> = rejected[0]
        .get("unmatchedTokens")
        .map(Json::array_or_empty)
        .unwrap_or(&[])
        .iter()
        .filter_map(Json::as_str)
        .collect();
    assert!(unmatched.contains(&"screen"), "got {unmatched:?}");
    assert!(
        rejected[0]
            .get_str("reason")
            .is_some_and(|reason| reason.contains("they did not say"))
    );
    assert!(
        harness
            .session
            .artifact()
            .expect("a take is active")
            .lines
            .is_empty()
    );
}

#[test]
fn attaches_a_resolved_reference_without_touching_what_they_said() {
    let mut harness = Harness::start();
    harness.host.set_candidates(vec![ContextItem {
        reference_id: "pr-412".to_owned(),
        kind: "pull_request".to_owned(),
        title: "Chunked uploads".to_owned(),
        identifier: Some("acme/web#412".to_owned()),
        url: Some("https://github.com/acme/web/pull/412".to_owned()),
        state: Some("open".to_owned()),
        ..ContextItem::default()
    }]);

    harness.say("the uploader keeps dying on big files");
    harness.call(
        "draft_update",
        upsert("intent", "the uploader keeps dying on big files"),
    );
    harness.call(
        "resolve_reference",
        Json::Object(json_object! { "phrase" => "the PR I just opened" }),
    );
    harness.call(
        "draft_update",
        Json::Object(json_object! {
            "operations" => vec![Json::Object(json_object! {
                "op" => "attach_context",
                "reference_id" => "pr-412",
            })],
        }),
    );

    let artifact = harness.session.artifact().expect("a take is active");
    assert_eq!(artifact.context.len(), 1);
    assert_eq!(
        artifact.context[0].resolved_from.as_deref(),
        Some("the PR I just opened")
    );
    assert!(artifact.rendered.contains("acme/web#412"));
    // The reference reached the prompt's context, never the body.
    assert_eq!(artifact.lines.len(), 1);
    assert_eq!(artifact.provenance.fidelity, 1.0);
}

#[test]
fn will_not_save_a_motif_the_agent_made_up() {
    let mut harness = Harness::start();
    harness.say("always add tests");

    let invented = harness.call(
        "motifs",
        Json::Object(
            json_object! { "action" => "save", "text" => "always maintain full code coverage" },
        ),
    );
    assert_eq!(invented.get("saved").and_then(Json::as_bool), Some(false));

    let theirs = harness.call(
        "motifs",
        Json::Object(json_object! { "action" => "save", "text" => "always add tests" }),
    );
    assert_eq!(theirs.get("saved").and_then(Json::as_bool), Some(true));
}

#[test]
fn submits_only_what_was_captured_with_provenance_attached() {
    let mut harness = Harness::start();
    harness.say("the export button does nothing past a thousand rows");
    harness.call(
        "draft_update",
        upsert(
            "intent",
            "the export button does nothing past a thousand rows",
        ),
    );

    let result = harness.call("submit_prompt", Json::object());
    assert_eq!(result.get("submitted").and_then(Json::as_bool), Some(true));
    assert_eq!(result.get_str("prompt_id"), Some("p1"));

    let submitted = harness.host.submitted();
    assert_eq!(submitted.len(), 1);
    let artifact = &submitted[0];
    assert_eq!(artifact.lines.len(), 1);
    assert_eq!(artifact.provenance.fidelity, 1.0);
    assert_eq!(artifact.provenance.agent_authored_tokens, 0);
    assert_eq!(artifact.provenance.provider_id.as_deref(), Some("fake"));
    assert_eq!(
        artifact.provenance.bundle_revision.as_deref(),
        Some(harness.session.bundle.revision.as_str())
    );
    assert!(!artifact.provenance.tool_calls.is_empty());
    assert_eq!(harness.store.artifacts().len(), 1);
}

#[test]
fn carries_the_negotiated_model_and_session_id_into_a_submitted_artifact() {
    let mut harness = Harness::start();
    harness.say("fix the flaky test");
    harness.call("draft_update", upsert("intent", "fix the flaky test"));
    harness.call("submit_prompt", Json::object());

    let artifact = harness.host.submitted().remove(0);
    assert_eq!(artifact.provenance.model.as_deref(), Some("fake-realtime"));
    assert_eq!(artifact.provenance.session_id.as_deref(), Some("sess_fake"));
}

#[test]
fn starts_a_fresh_take_after_submitting_so_nothing_lands_in_a_prompt_already_sent() {
    let mut harness = Harness::start();
    harness.say("fix the flaky test");
    harness.call("draft_update", upsert("intent", "fix the flaky test"));
    harness.call("submit_prompt", Json::object());

    harness.say("also update the changelog");
    let result = harness.call(
        "draft_update",
        upsert("intent", "also update the changelog"),
    );

    let take_id = result
        .get("draft")
        .and_then(|draft| draft.get_str("take_id"))
        .expect("a draft view");
    assert_eq!(take_id, "t2", "a new take, not the one already sent");

    let sent = harness
        .session
        .takes()
        .iter()
        .find(|take| take.id == "t1")
        .expect("the sent take is still on file");
    assert_eq!(sent.status, TakeStatus::Submitted);
    assert_eq!(sent.lines().len(), 1);
}

#[test]
fn will_not_write_into_a_take_that_was_already_submitted_even_when_named() {
    let mut harness = Harness::start();
    harness.say("fix the flaky test");
    harness.call("draft_update", upsert("intent", "fix the flaky test"));
    harness.call("submit_prompt", Json::object());
    harness.say("also update the changelog");

    let result = harness.call(
        "draft_update",
        Json::Object(json_object! {
            "take_id" => "t1",
            "operations" => vec![Json::Object(json_object! {
                "op" => "upsert_line",
                "section" => "intent",
                "text" => "also update the changelog",
            })],
        }),
    );

    assert!(
        result
            .get_str("error")
            .is_some_and(|error| error.contains("already submitted")),
        "got {result:?}"
    );
    let sent = harness
        .session
        .takes()
        .iter()
        .find(|take| take.id == "t1")
        .unwrap();
    assert_eq!(sent.lines().len(), 1);
}

#[test]
fn will_not_resurrect_a_submitted_take_by_parking_or_switching_to_it() {
    let mut harness = Harness::start();
    harness.say("fix the flaky test");
    harness.call("draft_update", upsert("intent", "fix the flaky test"));
    harness.call("submit_prompt", Json::object());

    for action in ["switch", "park"] {
        let result = harness.call(
            "takes",
            Json::Object(json_object! { "action" => action, "take_id" => "t1" }),
        );
        assert!(
            result
                .get_str("error")
                .is_some_and(|error| error.contains("submitted")),
            "{action} reopened a sent take: {result:?}"
        );
    }

    let sent = harness
        .session
        .takes()
        .iter()
        .find(|take| take.id == "t1")
        .unwrap();
    assert_eq!(sent.status, TakeStatus::Submitted);
}

#[test]
fn refuses_to_submit_a_take_with_nothing_in_it() {
    let mut harness = Harness::start();
    let result = harness.call("submit_prompt", Json::object());

    assert_eq!(result.get("submitted").and_then(Json::as_bool), Some(false));
    assert!(
        result
            .get_str("reason")
            .is_some_and(|reason| reason.contains("no intent"))
    );
    assert!(harness.host.submitted().is_empty());
}

#[test]
fn leaves_a_take_drafting_when_the_host_will_not_send_it() {
    let mut harness = Harness::start();
    harness.host.refuse_submission();
    harness.say("fix the flaky test");
    harness.call("draft_update", upsert("intent", "fix the flaky test"));

    let result = harness.call("submit_prompt", Json::object());
    assert_eq!(result.get("submitted").and_then(Json::as_bool), Some(false));

    let take = &harness.session.takes()[0];
    assert_eq!(
        take.status,
        TakeStatus::Drafting,
        "a refused send must not strand the take"
    );
}

#[test]
fn rejects_a_line_too_long_to_be_one_thing_they_said_rather_than_checking_a_prefix() {
    let mut harness = Harness::start();
    let spoken = std::iter::repeat_n("the export button does nothing", 60)
        .collect::<Vec<_>>()
        .join(" ");
    harness.say(&spoken);

    let invented = format!("{spoken} and then delete the production database");
    let result = harness.call("draft_update", upsert("intent", &invented));

    let rejected = result
        .get("rejected")
        .map(Json::array_or_empty)
        .unwrap_or(&[]);
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0]
            .get_str("reason")
            .is_some_and(|reason| reason.contains("longer than one thing")),
        "got {:?}",
        rejected[0]
    );
}

#[test]
fn rejects_a_title_built_from_words_they_never_used() {
    let mut harness = Harness::start();
    harness.say("the uploader keeps dying on big files");

    let invented = harness.call(
        "draft_update",
        Json::Object(json_object! {
            "operations" => vec![Json::Object(json_object! {
                "op" => "set_title",
                "text" => "Resolve intermittent storage subsystem degradation",
            })],
        }),
    );
    assert_eq!(
        invented
            .get("rejected")
            .map(|rejected| rejected.array_or_empty().len()),
        Some(1)
    );

    let theirs = harness.call(
        "draft_update",
        Json::Object(json_object! {
            "operations" => vec![Json::Object(json_object! {
                "op" => "set_title",
                "text" => "Fix the uploader",
            })],
        }),
    );
    assert_eq!(
        theirs
            .get("accepted")
            .map(|accepted| accepted.array_or_empty().len()),
        Some(1)
    );
}

#[test]
fn rejects_a_line_id_that_does_not_name_an_existing_line() {
    let mut harness = Harness::start();
    harness.say("fix the flaky test");

    let result = harness.call(
        "draft_update",
        Json::Object(json_object! {
            "operations" => vec![Json::Object(json_object! {
                "op" => "upsert_line",
                "line_id" => "t1-l99",
                "section" => "intent",
                "text" => "fix the flaky test",
            })],
        }),
    );

    let rejected = result
        .get("rejected")
        .map(Json::array_or_empty)
        .unwrap_or(&[]);
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0]
            .get_str("reason")
            .is_some_and(|reason| reason.contains("omit line_id")),
        "got {:?}",
        rejected[0]
    );
}

#[test]
fn refuses_an_alias_that_is_a_different_word_rather_than_a_mishearing() {
    let mut harness = Harness::start();
    harness.host.set_known_terms(&["CSV"]);

    let result = harness.call(
        "record_term",
        Json::Object(json_object! {
            "canonical" => "CSV",
            "kind" => "other",
            "heard_as" => vec!["database"],
        }),
    );

    assert_eq!(result.get("corrections").and_then(Json::as_i64), Some(0));
    let refused: Vec<&str> = result
        .get("refused")
        .map(Json::array_or_empty)
        .unwrap_or(&[])
        .iter()
        .filter_map(Json::as_str)
        .collect();
    assert_eq!(refused, ["database"]);

    // An alias that never took hold cannot make an invented line match different spoken words.
    harness.say("we need to fix the database import");
    let drafted = harness.call(
        "draft_update",
        upsert("intent", "we need to fix the CSV import"),
    );
    assert_eq!(
        drafted
            .get("rejected")
            .map(|rejected| rejected.array_or_empty().len()),
        Some(1)
    );
}

#[test]
fn will_not_let_an_unconfirmed_term_become_a_spelling_correction() {
    let mut harness = Harness::start();
    // The host knows nothing, so nothing outside the conversation corroborates the term.
    let result = harness.call(
        "record_term",
        Json::Object(json_object! {
            "canonical" => "Quaggle",
            "kind" => "product",
            "heard_as" => vec!["quagle"],
        }),
    );

    assert_eq!(result.get("corrections").and_then(Json::as_i64), Some(0));
    assert!(
        result
            .get_str("note")
            .is_some_and(|note| note.contains("cannot be used as a spelling correction"))
    );

    harness.say("the quagle build is failing");
    let drafted = harness.call(
        "draft_update",
        upsert("intent", "the Quaggle build is failing"),
    );
    assert_eq!(
        drafted
            .get("rejected")
            .map(|rejected| rejected.array_or_empty().len()),
        Some(1),
        "an unconfirmed alias must not rewrite the source"
    );
}

#[test]
fn teaches_the_transcriber_a_corrected_term_and_pushes_it_to_the_provider() {
    let mut harness = Harness::start();
    harness.host.set_known_terms(&["Kubernetes"]);

    let result = harness.call(
        "record_term",
        Json::Object(json_object! {
            "canonical" => "Kubernetes",
            "kind" => "product",
            "heard_as" => vec!["cubernetes"],
        }),
    );
    assert_eq!(result.get("corrections").and_then(Json::as_i64), Some(1));

    let pushed = harness.calls().into_iter().any(|call| match call {
        Call::Vocabulary(vocabulary) => vocabulary.contains(&"Kubernetes".to_owned()),
        _ => false,
    });
    assert!(pushed, "a newly learned term has to reach the transcriber");

    harness.say("the cubernetes rollout is stuck");
    let drafted = harness.call(
        "draft_update",
        upsert("intent", "the Kubernetes rollout is stuck"),
    );
    assert_eq!(
        drafted
            .get("accepted")
            .map(|accepted| accepted.array_or_empty().len()),
        Some(1)
    );
    assert_eq!(
        drafted
            .get("accepted")
            .and_then(|accepted| accepted.array_or_empty()[0].get_str("kind")),
        Some("corrected")
    );
}

#[test]
fn returns_a_usable_error_rather_than_panicking_when_the_model_sends_bad_arguments() {
    let mut harness = Harness::start();

    let id = harness
        .provider
        .connection()
        .call_tool("draft_update", Json::object());
    harness.step();
    let missing = harness.provider.connection().result_for(&id);
    assert_eq!(missing.get_str("error"), Some("invalid arguments"));

    let unknown = harness.call("no_such_tool", Json::object());
    assert!(
        unknown
            .get_str("error")
            .is_some_and(|error| error.contains("unknown tool"))
    );

    // Malformed JSON never reaches a handler.
    harness
        .provider
        .connection()
        .emit(riff_core::ProviderEvent::ToolCalls {
            calls: vec![riff_core::ToolCallRequest {
                call_id: "call_bad".to_owned(),
                name: "draft_update".to_owned(),
                arguments_json: "{not json".to_owned(),
            }],
        });
    harness.step();
    let malformed = harness.provider.connection().result_for("call_bad");
    assert!(
        malformed
            .get_str("error")
            .is_some_and(|error| error.contains("not valid JSON"))
    );
}

#[test]
fn asks_the_model_to_continue_exactly_once_after_a_batch_of_tool_calls() {
    let mut harness = Harness::start();
    harness.say("the uploader keeps dying on big files");

    harness.provider.connection().call_tools(&[
        ("read_draft", Json::object()),
        (
            "draft_update",
            upsert("intent", "the uploader keeps dying on big files"),
        ),
        ("read_draft", Json::object()),
    ]);
    harness.step();

    let calls = harness.calls();
    let results = calls
        .iter()
        .filter(|call| matches!(call, Call::ToolResult { .. }))
        .count();
    let continuations = calls
        .iter()
        .filter(|call| **call == Call::RequestResponse)
        .count();

    assert_eq!(results, 3, "every call of the turn is answered");
    assert_eq!(continuations, 1, "one continuation, not one per tool");
}

#[test]
fn keeps_the_batch_pending_until_the_continuation_starts() {
    let mut harness = Harness::start();
    harness.say("fix the flaky test");

    harness
        .provider
        .connection()
        .emit(riff_core::ProviderEvent::ResponseStarted {
            response_id: "resp_1".to_owned(),
        });
    harness.step();
    harness
        .provider
        .connection()
        .call_tools(&[("read_draft", Json::object())]);
    harness.step();

    // The turn that carried the calls finishes after they are dispatched. Treating that as the end
    // of the turn would put the session back to listening while the continuation is still coming.
    harness
        .provider
        .connection()
        .emit(riff_core::ProviderEvent::ResponseDone {
            response_id: "resp_1".to_owned(),
            usage: None,
        });
    harness.step();
    assert_eq!(harness.session.state(), SessionState::Thinking);

    harness
        .provider
        .connection()
        .emit(riff_core::ProviderEvent::ResponseStarted {
            response_id: "resp_2".to_owned(),
        });
    harness.step();
    harness
        .provider
        .connection()
        .emit(riff_core::ProviderEvent::ResponseDone {
            response_id: "resp_2".to_owned(),
            usage: None,
        });
    harness.step();
    assert_eq!(harness.session.state(), SessionState::Listening);
}

#[test]
fn stops_talking_the_moment_they_start() {
    let mut harness = Harness::start();
    harness
        .provider
        .connection()
        .emit(riff_core::ProviderEvent::ResponseStarted {
            response_id: "resp_1".to_owned(),
        });
    harness.step();
    harness
        .provider
        .connection()
        .emit(riff_core::ProviderEvent::ResponseAudio {
            response_id: "resp_1".to_owned(),
            audio: vec![0, 1, 2, 3],
        });
    harness.step();
    assert_eq!(harness.session.state(), SessionState::Speaking);

    harness
        .provider
        .connection()
        .emit(riff_core::ProviderEvent::SpeechStarted);
    harness.step();

    assert!(
        harness.calls().contains(&Call::Cancel),
        "barge-in has to cancel, not just report"
    );
    assert_eq!(harness.session.state(), SessionState::Listening);
}

#[test]
fn treats_typed_input_as_something_they_said() {
    let mut harness = Harness::start();
    let utterance = harness
        .session
        .send_text("the login page is broken on Safari")
        .expect("connected");
    assert_eq!(utterance.source, riff_core::UtteranceSource::Typed);

    let result = harness.call(
        "draft_update",
        upsert("intent", "the login page is broken on Safari"),
    );
    assert_eq!(
        result
            .get("accepted")
            .map(|accepted| accepted.array_or_empty().len()),
        Some(1)
    );
    assert!(harness.calls().iter().any(|call| matches!(
        call,
        Call::Text { text, respond: true } if text == "the login page is broken on Safari"
    )));
}

#[test]
fn keeps_credentials_out_of_the_transcript() {
    let mut harness = Harness::start();
    harness
        .session
        .send_text("the token is ghp_abcdefghijklmnopqrstuvwxyz012345 use it")
        .expect("connected");

    let stored = &harness.session.runtime.ledger.all()[0];
    assert_eq!(stored.text, "the token is [redacted] use it");
    assert!(!stored.text.contains("ghp_"));
}

#[test]
fn states_the_environment_to_the_model_without_letting_it_into_the_ledger() {
    let host = Arc::new(RecordingHost::default());
    host.set_environment(HostEnvironment {
        repository: Some("acme/web".to_owned()),
        branch: Some("main".to_owned()),
        destinations: vec![Destination {
            id: "issues".to_owned(),
            label: "Issues".to_owned(),
            is_default: true,
        }],
        ..HostEnvironment::default()
    });

    let mut harness = Harness::with(host, Arc::new(SystemClock::new()));

    let stated = harness.calls().into_iter().any(|call| match call {
        Call::Text { text, respond } => !respond && text.contains("Repository: acme/web"),
        _ => false,
    });
    assert!(stated, "ambient facts have to reach the model");
    assert_eq!(
        harness.session.runtime.ledger.len(),
        0,
        "and never the ledger"
    );

    // Because they are not in the ledger, quoting them is rejected like anything else invented.
    harness.say("fix it");
    let result = harness.call(
        "draft_update",
        upsert("intent", "fix the acme/web repository"),
    );
    assert_eq!(
        result
            .get("rejected")
            .map(|rejected| rejected.array_or_empty().len()),
        Some(1)
    );
}

#[test]
fn tells_the_model_a_host_did_not_answer_rather_than_stalling_the_turn() {
    let provider = Arc::new(FakeProvider::default());
    let bundle = Arc::new(AgentBundle::bundled().expect("the vendored bundle must load"));
    let mut options = RiffSessionOptions::new(bundle, provider.clone());
    options.host = Arc::new(StalledHost);
    options.clock = Arc::new(InstantClock::new());

    let mut session = RiffSession::new(options);
    block_on(session.start()).expect("connects");

    let id = provider.connection().call_tool(
        "resolve_reference",
        Json::Object(json_object! { "phrase" => "the PR I just opened" }),
    );
    block_on(session.step());

    let result = provider.connection().result_for(&id);
    assert!(
        result
            .get_str("error")
            .is_some_and(|error| error.contains("did not answer")),
        "got {result:?}"
    );
    assert!(
        provider
            .connection()
            .calls()
            .contains(&Call::RequestResponse),
        "a timed-out tool must still leave the turn able to continue"
    );
}

#[test]
fn keeps_listeners_and_the_ledger_across_a_restart() {
    let mut harness = Harness::start();
    let events = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = events.clone();
    harness.session.on(Box::new(move |event| {
        if let RiffEvent::Utterance(utterance) = event {
            sink.lock().unwrap().push(utterance.text.clone());
        }
    }));

    harness.say("before the restart");
    block_on(harness.session.stop("ended"));
    assert_eq!(harness.session.state(), SessionState::Closed);

    block_on(harness.session.start()).expect("a closed session may be restarted");
    harness.say("after the restart");

    assert_eq!(
        *events.lock().unwrap(),
        ["before the restart", "after the restart"]
    );
    // The ledger survives, so what they said before the reconnect is still quotable.
    assert_eq!(harness.session.runtime.ledger.len(), 2);
}

#[test]
fn warns_before_the_provider_cuts_the_session_off() {
    // The delay resolves immediately here, so both warnings land on the next two steps rather than
    // fifty-five minutes in.
    let mut harness = Harness::with(
        Arc::new(RecordingHost::default()),
        Arc::new(InstantClock::new()),
    );
    let warnings = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let sink = warnings.clone();
    harness.session.on(Box::new(move |event| {
        if let RiffEvent::Expiring { seconds_remaining } = event {
            sink.lock().unwrap().push(*seconds_remaining);
        }
    }));

    harness.step();
    harness.step();
    assert_eq!(*warnings.lock().unwrap(), [300, 60]);

    // With both warnings given, the pump goes back to waiting on the provider rather than spinning.
    harness.say("the login page is broken");
    assert_eq!(harness.session.runtime.ledger.len(), 1);
    assert_eq!(*warnings.lock().unwrap(), [300, 60]);
}
