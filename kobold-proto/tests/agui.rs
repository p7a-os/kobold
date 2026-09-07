//! The AG-UI types against the specification's own payloads.
//!
//! `ag-ui-core` 0.1.0 compiled clean for both our targets and failed on the
//! first event of a real stream, because its id types are UUID-only and both
//! the specification's `msg-1` and a provider's `msg_abc123` are neither.
//! **Published, maintained and type-checking are all true of a crate that
//! cannot do the job**, so nothing here is asserted about our types without a
//! real payload going through them.
//!
//! The fixtures live under the `kobold` crate rather than this one because
//! that is where they were vendored, with the licence notice each file's
//! source requires. Reaching up one level is deliberate; see their README for
//! the three provenance tiers and why none substitutes for another.

use kobold_proto::agui::{self, Base, Incoming, Json, Outgoing, TokenUsage};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/agui/");

fn fixture(name: &str) -> sonic_rs::Value {
    let path = format!("{FIXTURES}{name}");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the vendored fixture {path} must be readable: {e}"));
    sonic_rs::from_str(&text).unwrap_or_else(|e| panic!("{path} is not JSON: {e}"))
}

fn json(text: &str) -> sonic_rs::Value {
    sonic_rs::from_str(text).expect("valid JSON")
}

fn encoded(event: &Outgoing) -> sonic_rs::Value {
    json(&sonic_rs::to_string(event).expect("an event is plain data"))
}

/// The AG-UI type name a decoded event corresponds to, so a case can be
/// checked against the name the specification gave it rather than against a
/// pattern written from the same assumption twice.
fn kind(event: &Incoming) -> &'static str {
    match event {
        Incoming::RunStarted { .. } => "RUN_STARTED",
        Incoming::RunFinished { .. } => "RUN_FINISHED",
        Incoming::RunError { .. } => "RUN_ERROR",
        Incoming::TextMessageStart { .. } => "TEXT_MESSAGE_START",
        Incoming::TextMessageContent { .. } => "TEXT_MESSAGE_CONTENT",
        Incoming::TextMessageEnd { .. } => "TEXT_MESSAGE_END",
        Incoming::TextMessageChunk { .. } => "TEXT_MESSAGE_CHUNK",
        Incoming::ToolCallStart { .. } => "TOOL_CALL_START",
        Incoming::ToolCallArgs { .. } => "TOOL_CALL_ARGS",
        Incoming::ToolCallEnd { .. } => "TOOL_CALL_END",
        Incoming::ToolCallChunk { .. } => "TOOL_CALL_CHUNK",
        Incoming::ToolCallResult { .. } => "TOOL_CALL_RESULT",
        Incoming::ReasoningStart { .. } => "REASONING_START",
        Incoming::ReasoningEnd { .. } => "REASONING_END",
        Incoming::ReasoningMessageStart { .. } => "REASONING_MESSAGE_START",
        Incoming::ReasoningMessageContent { .. } => "REASONING_MESSAGE_CONTENT",
        Incoming::ReasoningMessageEnd { .. } => "REASONING_MESSAGE_END",
        Incoming::ReasoningMessageChunk { .. } => "REASONING_MESSAGE_CHUNK",
        Incoming::Unmodelled => "Unmodelled",
    }
}

/// Everything in the corpus we deliberately do not model. Listed rather than
/// inferred: if a future fixture adds an event we *should* handle, it lands
/// in this set silently unless the set is written down and asserted against.
const NOT_MODELLED: &[&str] = &[
    "STEP_STARTED",
    "STEP_FINISHED",
    "STATE_SNAPSHOT",
    "STATE_DELTA",
    "MESSAGES_SNAPSHOT",
    "ACTIVITY_SNAPSHOT",
    "ACTIVITY_DELTA",
    "RAW",
    "CUSTOM",
];

fn cases(file: &str, key: &str) -> Vec<sonic_rs::Value> {
    let doc = fixture(file);
    doc.get(key)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_else(|| panic!("{file} has no {key} array"))
        .to_vec()
}

#[test]
fn every_case_in_the_specifications_corpus_decodes_to_the_event_it_names() {
    // 28 cases covering nearly every event type, not only the null contract
    // its filename suggests. The assertion is against the `type` string in
    // the payload rather than against a hand-written expectation, so a
    // variant renamed on our side cannot be "corrected" in the test to match.
    let stream = cases("null-omission.json", "stream");
    assert_eq!(stream.len(), 28, "the vendored corpus changed size");

    let mut modelled = std::collections::BTreeSet::new();
    for case in &stream {
        let name = case
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_owned();
        let input = case.get("input").expect("every case has an input");
        let declared = input
            .get("type")
            .and_then(|v| v.as_str())
            .expect("every event has a type")
            .to_owned();

        let text = sonic_rs::to_string(input).expect("re-encode");
        let got: Incoming = sonic_rs::from_str(&text)
            .unwrap_or_else(|e| panic!("{name}: {declared} failed to decode at all: {e}"));

        if NOT_MODELLED.contains(&declared.as_str()) {
            assert_eq!(
                kind(&got),
                "Unmodelled",
                "{name}: {declared} should be unmodelled"
            );
        } else {
            // The positive half, and the one that matters: without it every
            // row would be satisfied by a type that decoded the entire
            // protocol to `Unmodelled`.
            assert_eq!(kind(&got), declared, "{name}: decoded as the wrong event");
            modelled.insert(declared);
        }
    }
    // Distinct types, not cases: three of the twenty-eight are RUN_STARTED
    // and three are RUN_FINISHED.
    assert_eq!(
        modelled.len(),
        13,
        "the corpus should exercise thirteen events we model"
    );

    // And the gap the fixtures README names, pinned rather than remembered:
    // the specification's corpus carries no reasoning payloads at all beyond
    // the one chunk case. For the family we measured as load-bearing, our own
    // provider-shaped fixture is the only source there is -- so if a future
    // corpus grows one, this fails and says to go and use it.
    assert!(
        !modelled
            .iter()
            .any(|t| t.starts_with("REASONING_") && t != "REASONING_MESSAGE_CHUNK"),
        "the corpus grew a reasoning payload: {modelled:?}"
    );
    assert!(
        modelled.contains("REASONING_MESSAGE_CHUNK"),
        "the one reasoning case is gone"
    );
}

#[test]
fn what_our_adapter_writes_is_the_specifications_own_payload() {
    // The other direction, and it needs the fixture's `expected` rather than
    // its `input`: the contract is that a producer OMITS a field it has no
    // value for and never writes null for it, so `expected` is what a
    // conforming producer puts on the wire. Ours has to match it exactly --
    // an extra `"parentRunId": null` is a conformance failure that no
    // round-trip through our own types could ever notice.
    let stream = cases("null-omission.json", "stream");
    let expected = |name: &str| -> sonic_rs::Value {
        stream
            .iter()
            .find(|c| c.get("name").and_then(|v| v.as_str()) == Some(name))
            .and_then(|c| c.get("expected").cloned())
            .unwrap_or_else(|| panic!("no case named {name}"))
    };

    let b = Base::default;
    assert_eq!(
        encoded(&Outgoing::RunStarted {
            base: b(),
            thread_id: "thread_1".into(),
            run_id: "run_1".into(),
            parent_run_id: None,
        }),
        expected("run_started_without_parent_run_id_or_input")
    );
    assert_eq!(
        encoded(&Outgoing::TextMessageStart {
            base: b(),
            message_id: "msg_1".into(),
            role: "assistant".into(),
        }),
        expected("text_message_start_without_name")
    );
    assert_eq!(
        encoded(&Outgoing::TextMessageContent {
            base: b(),
            message_id: "msg_1".into(),
            delta: "Looking that up.".into(),
        }),
        expected("text_message_content")
    );
    assert_eq!(
        encoded(&Outgoing::TextMessageEnd {
            base: b(),
            message_id: "msg_1".into()
        }),
        expected("text_message_end")
    );
    assert_eq!(
        encoded(&Outgoing::ToolCallStart {
            base: b(),
            tool_call_id: "tc_1".into(),
            tool_call_name: "search".into(),
            parent_message_id: None,
        }),
        expected("tool_call_start_without_parent_message_id")
    );
    assert_eq!(
        encoded(&Outgoing::ToolCallArgs {
            base: b(),
            tool_call_id: "tc_1".into(),
            delta: r#"{"q":"ag-ui"}"#.into(),
        }),
        expected("tool_call_args")
    );
    assert_eq!(
        encoded(&Outgoing::ToolCallEnd {
            base: b(),
            tool_call_id: "tc_1".into()
        }),
        expected("tool_call_end")
    );
    assert_eq!(
        encoded(&Outgoing::RunError {
            base: b(),
            message: "upstream timed out".into(),
            code: None,
            usage: None,
        }),
        expected("run_error_without_code")
    );
    assert_eq!(
        encoded(&Outgoing::RunFinished {
            base: b(),
            thread_id: "thread_1".into(),
            run_id: "run_1".into(),
            usage: None,
        }),
        expected("run_finished_without_result_or_outcome")
    );
}

#[test]
fn an_optional_field_we_do_have_a_value_for_is_written() {
    // The partner to the test above, and it is not decoration. Every case
    // there omits its optional fields, so all nine would pass just as well
    // against a type that could not serialize an optional field at all.
    let with_parent = encoded(&Outgoing::ToolCallStart {
        base: Base::default(),
        tool_call_id: "tc_1".into(),
        tool_call_name: "search".into(),
        parent_message_id: Some("msg_1".into()),
    });
    assert_eq!(
        with_parent,
        json(
            r#"{"type":"TOOL_CALL_START","toolCallId":"tc_1",
                "toolCallName":"search","parentMessageId":"msg_1"}"#
        )
    );
}

#[test]
fn an_explicit_null_is_read_as_absent_wherever_a_field_is_optional() {
    // **This test used to claim a confinement it cannot check.** The
    // specification grandfathers exactly three fields for an explicit null,
    // and it was named for those three -- but the tolerance here is
    // *inherited, not implemented*: `#[serde(default)] Option<T>` accepts a
    // null for every optional field there is. All nine of ours do, including
    // `Base.metadata`, which the specification explicitly rejects.
    //
    // The behaviour is deliberate and stays. Being more tolerant than the
    // specification is the safe direction for a consumer, and a custom
    // deserializer to be strict about it would buy nothing. What changed is
    // the name, because a test asserting three cases under a title claiming
    // only three exist is evidence for a scope it never tested.
    //
    // The three the specification names are first, because they are the ones
    // a conforming producer may actually send.
    let start: Incoming = sonic_rs::from_str(
        r#"{"type":"TOOL_CALL_START","toolCallId":"tc_1","toolCallName":"s","parentMessageId":null}"#,
    )
    .expect("TOOL_CALL_START.parentMessageId accepts null");
    assert!(matches!(
        start,
        Incoming::ToolCallStart {
            parent_message_id: None,
            ..
        }
    ));

    let chunk: Incoming = sonic_rs::from_str(
        r#"{"type":"TOOL_CALL_CHUNK","toolCallId":"tc_1","parentMessageId":null}"#,
    )
    .expect("TOOL_CALL_CHUNK.parentMessageId accepts null");
    assert!(matches!(
        chunk,
        Incoming::ToolCallChunk {
            parent_message_id: None,
            ..
        }
    ));

    let finished: Incoming =
        sonic_rs::from_str(r#"{"type":"RUN_FINISHED","threadId":"t","runId":"r","outcome":null}"#)
            .expect("RUN_FINISHED.outcome accepts null");
    // The field, not just the variant. `outcome` is `Option<Json>` and `Json`
    // has a `Null` variant, so "absent" and "present and null" are both
    // representable -- and matching only the variant would pass for either.
    // The contract is that a null reads as absent.
    let Incoming::RunFinished { outcome, .. } = finished else {
        panic!("wrong variant")
    };
    assert_eq!(outcome, None, "an explicit null became a present null");

    // And the breadth, asserted rather than left implied by the rename: a
    // field the specification does NOT grandfather tolerates null too.
    let lenient: Incoming =
        sonic_rs::from_str(r#"{"type":"TEXT_MESSAGE_END","messageId":"m","metadata":null}"#)
            .expect("we are more tolerant than the specification, on purpose");
    let Incoming::TextMessageEnd { base, .. } = lenient else {
        panic!("wrong variant")
    };
    assert_eq!(base.metadata, None);
}

#[test]
fn an_event_the_specification_grows_next_is_not_a_decode_error() {
    // AG-UI is pre-1.0 and moving; the repository merged a PR the day before
    // this corpus was taken. An unmodelled event must never kill a session,
    // so both a real one we skip and one that does not exist yet decode.
    for raw in [
        r#"{"type":"STATE_DELTA","delta":[{"op":"replace","path":"/x","value":null}]}"#,
        r#"{"type":"SOMETHING_FROM_0_0_99","whatever":true}"#,
    ] {
        let got: Incoming = sonic_rs::from_str(raw).expect("must not error");
        assert_eq!(kind(&got), "Unmodelled");
    }
}

#[test]
fn the_base_fields_every_event_declares_survive_and_are_omitted_when_unset() {
    // **`Base` is the one place where our own evidence is a substitute for
    // the corpus rather than a supplement to it.** `timestamp` and `rawEvent`
    // appear zero times across all 28 published cases, and the two
    // `metadata` occurrences are an interrupt's own metadata, not the base
    // field. So the struct declared on every event -- and the carrier of the
    // opaque fields that broke the first implementation -- is tested here
    // against payloads we wrote, and nowhere else.
    //
    // `Base` is flattened, which is the piece most at risk of not composing:
    // `flatten` forces serde's buffered map path, and it has to compose with
    // an internal tag under sonic-rs specifically, not merely under
    // serde_json. Asserted in both directions on one event.
    let raw = r#"{"type":"TEXT_MESSAGE_END","messageId":"msg_1","timestamp":1.5,
                  "metadata":{"trace":"abc"},"rawEvent":{"provider":"openai"}}"#;
    let got: Incoming = sonic_rs::from_str(raw).expect("decode");
    let Incoming::TextMessageEnd { base, message_id } = got else {
        panic!("wrong variant")
    };
    assert_eq!(message_id, "msg_1");
    assert_eq!(base.timestamp, Some(1.5));
    assert_eq!(
        base.raw_event,
        Some(Json::Object(
            [("provider".to_owned(), Json::Str("openai".to_owned()))]
                .into_iter()
                .collect()
        ))
    );
    assert_eq!(
        base.metadata.as_ref().and_then(|m| m.get("trace")),
        Some(&Json::Str("abc".to_owned())),
        "metadata is open by key and must keep what it was given"
    );

    let out = encoded(&Outgoing::TextMessageEnd {
        base,
        message_id: "msg_1".into(),
    });
    assert_eq!(
        out,
        json(
            r#"{"type":"TEXT_MESSAGE_END","messageId":"msg_1","timestamp":1.5,
                "metadata":{"trace":"abc"},"rawEvent":{"provider":"openai"}}"#
        )
    );
    assert_eq!(
        encoded(&Outgoing::TextMessageEnd {
            base: Base::default(),
            message_id: "m".into()
        }),
        json(r#"{"type":"TEXT_MESSAGE_END","messageId":"m"}"#),
        "an unset base must add nothing at all"
    );
}

#[test]
fn an_opaque_payload_keeps_its_shape_through_the_buffered_path() {
    // `Json` exists because `sonic_rs::Value` cannot survive here -- see its
    // doc comment -- so the substitute needs its own evidence rather than
    // inheriting the confidence the thing it replaced had.
    //
    // Carried inside a tagged event on purpose. Decoding a bare `Json` would
    // not go through serde's buffered `Content` at all, which is the exact
    // path that broke, so a test that did would pass for a type with the
    // original defect still in it.
    let raw = r#"{"type":"TEXT_MESSAGE_END","messageId":"m","rawEvent":
                  {"s":"text","i":3,"f":1.5,"t":true,"n":null,
                   "a":[1,"two",null,{"deep":false}]}}"#;
    let got: Incoming = sonic_rs::from_str(raw).expect("decode");
    let Incoming::TextMessageEnd { base, .. } = got else {
        panic!("wrong variant")
    };
    let Some(Json::Object(o)) = base.raw_event.clone() else {
        panic!("not an object")
    };

    // Every shape by name, because a single assertion on the whole object
    // would be satisfied by a decoder that got two of them wrong in a way
    // that cancelled out in the re-encode below.
    assert_eq!(o.get("s"), Some(&Json::Str("text".to_owned())));
    assert_eq!(
        o.get("i"),
        Some(&Json::Int(3)),
        "a whole number must not become a float"
    );
    assert_eq!(o.get("f"), Some(&Json::Float(1.5)));
    assert_eq!(o.get("t"), Some(&Json::Bool(true)));
    assert_eq!(
        o.get("n"),
        Some(&Json::Null),
        "a null inside an opaque payload is data"
    );
    assert_eq!(
        o.get("a"),
        Some(&Json::Array(vec![
            Json::Int(1),
            Json::Str("two".to_owned()),
            Json::Null,
            Json::Object(
                [("deep".to_owned(), Json::Bool(false))]
                    .into_iter()
                    .collect()
            ),
        ]))
    );

    // And back out unchanged. `3` re-encoding as `3.0` would be a silent
    // rewrite of a payload we promised only to carry.
    let out = encoded(&Outgoing::TextMessageEnd {
        base,
        message_id: "m".into(),
    });
    assert_eq!(out, json(raw), "an opaque payload was not carried verbatim");
}

#[test]
fn every_outgoing_event_carries_its_type_tag() {
    // The `ag-ui-core` defect, asserted directly rather than trusted to the
    // shape that prevents it. There is no standalone event struct here to
    // hand a serializer, so a tagless payload should be unconstructible --
    // and this fails loudly if anyone ever adds one.
    let b = Base::default;
    let all = vec![
        Outgoing::RunStarted {
            base: b(),
            thread_id: "t".into(),
            run_id: "r".into(),
            parent_run_id: None,
        },
        Outgoing::RunFinished {
            base: b(),
            thread_id: "t".into(),
            run_id: "r".into(),
            usage: None,
        },
        Outgoing::RunError {
            base: b(),
            message: "m".into(),
            code: None,
            usage: None,
        },
        Outgoing::TextMessageStart {
            base: b(),
            message_id: "m".into(),
            role: "assistant".into(),
        },
        Outgoing::TextMessageContent {
            base: b(),
            message_id: "m".into(),
            delta: "d".into(),
        },
        Outgoing::TextMessageEnd {
            base: b(),
            message_id: "m".into(),
        },
        Outgoing::ToolCallStart {
            base: b(),
            tool_call_id: "c".into(),
            tool_call_name: "n".into(),
            parent_message_id: None,
        },
        Outgoing::ToolCallArgs {
            base: b(),
            tool_call_id: "c".into(),
            delta: "d".into(),
        },
        Outgoing::ToolCallEnd {
            base: b(),
            tool_call_id: "c".into(),
        },
        Outgoing::ReasoningStart {
            base: b(),
            message_id: "m".into(),
        },
        Outgoing::ReasoningEnd {
            base: b(),
            message_id: "m".into(),
        },
        Outgoing::ReasoningMessageStart {
            base: b(),
            message_id: "m".into(),
            role: "assistant".into(),
        },
        Outgoing::ReasoningMessageContent {
            base: b(),
            message_id: "m".into(),
            delta: "d".into(),
        },
        Outgoing::ReasoningMessageEnd {
            base: b(),
            message_id: "m".into(),
        },
    ];
    assert_eq!(all.len(), 14, "every emitted event needs a row here");
    for event in &all {
        let text = sonic_rs::to_string(event).expect("encode");
        let value = json(&text);
        let tag = value
            .get("type")
            .and_then(|v| v.as_str().map(str::to_owned));
        let tag = tag.unwrap_or_else(|| panic!("no type tag on {text}"));
        // And the tag is the protocol's spelling, not Rust's: a variant name
        // leaking through would tag the event `RunStarted`, which is valid
        // JSON that no conforming consumer can route.
        assert!(
            tag.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
            "the tag {tag} is not a protocol event name"
        );
    }
}

#[test]
fn a_usage_array_keeps_every_entry_and_reports_only_what_the_provider_counted() {
    // Three of these five cases are traps, and each one breaks a different
    // plausible fold. They are pinned on the type here so that the fold,
    // when it is written, is written against them rather than against a
    // single-entry example.
    let all = cases("usage.json", "cases");
    let usage_of = |name: &str| -> Option<Vec<TokenUsage>> {
        let case = all
            .iter()
            .find(|c| c.get("name").and_then(|v| v.as_str()) == Some(name))
            .unwrap_or_else(|| panic!("no case named {name}"));
        let text = sonic_rs::to_string(case.get("event").expect("event")).expect("re-encode");
        match sonic_rs::from_str::<Incoming>(&text).expect("decode") {
            Incoming::RunFinished { usage, .. } | Incoming::RunError { usage, .. } => usage,
            other => panic!("{name} decoded as {}", kind(&other)),
        }
    };

    let full = usage_of("run_finished_with_a_single_fully_populated_usage_entry").expect("usage");
    assert_eq!(
        full,
        vec![TokenUsage {
            provider: Some("anthropic".into()),
            model: Some("claude-sonnet-4".into()),
            input_tokens: Some(100),
            output_tokens: Some(50),
            total_tokens: Some(150),
            // The two counts `kobold_proto::Usage` has no field for. Dropping
            // reasoning_tokens throws away the one figure that explains a
            // long silent turn.
            reasoning_tokens: Some(20),
            cached_input_tokens: Some(10),
        }]
    );

    // Why usage is an array at all. A fold that reads entry [0] and stops
    // loses the second model entirely, and nothing about the shape shows it.
    let two = usage_of("run_finished_with_multiple_usage_entries_per_model").expect("usage");
    assert_eq!(two.len(), 2, "an entry was dropped");
    assert_eq!(
        two.iter().map(|u| u.model.clone()).collect::<Vec<_>>(),
        vec![Some("gpt-4o".to_owned()), Some("gpt-4o-mini".to_owned())]
    );

    // A total with neither input nor output. Any fold computing the total by
    // summing the other two answers zero for this real payload.
    let sparse = usage_of("run_finished_with_usage_alongside_outcome_and_result").expect("usage");
    assert_eq!(sparse[0].total_tokens, Some(7));
    assert_eq!(sparse[0].input_tokens, None, "absent, and not zero");
    assert_eq!(sparse[0].output_tokens, None, "absent, and not zero");

    // Billed and then failed. `Update::Failed` carries no usage at all today,
    // so this is a finding as much as a fixture.
    let failed = usage_of("run_error_with_partial_usage").expect("a failed run still cost money");
    assert_eq!(failed[0].input_tokens, Some(100));
    assert_eq!(failed[0].total_tokens, None);

    // And absent stays absent. The distinction `kobold_proto::Usage`'s own
    // doc comment exists to protect: a turn with no report must not read as
    // a turn that was free.
    assert_eq!(
        usage_of("legacy_run_finished_with_no_usage_key_at_all"),
        None
    );
}

#[test]
fn folding_usage_to_the_gauge_answers_each_of_the_corpus_traps() {
    // Fold B, against the payloads rather than against a single-entry
    // example. Three of the five cases break a different plausible fold, and
    // the point of writing this now is that the fold is written against them
    // instead of being written first and checked later.
    let all = cases("usage.json", "cases");
    let entries = |name: &str| -> Option<Vec<TokenUsage>> {
        let case = all
            .iter()
            .find(|c| c.get("name").and_then(|v| v.as_str()) == Some(name))
            .unwrap_or_else(|| panic!("no case named {name}"));
        let text = sonic_rs::to_string(case.get("event").expect("event")).expect("re-encode");
        match sonic_rs::from_str::<Incoming>(&text).expect("decode") {
            Incoming::RunFinished { usage, .. } | Incoming::RunError { usage, .. } => usage,
            other => panic!("{name} decoded as {}", kind(&other)),
        }
    };
    let fold = |name: &str| agui::fold_usage(&entries(name).unwrap_or_default());

    // Absent stays absent. A `Usage::default()` here would read as "this turn
    // was free", which is the failure the gauge type's own doc comment exists
    // to prevent -- and it is not hypothetical, it is what happens today when
    // an AG-UI usage object is parsed as `kobold_proto::Usage`.
    assert_eq!(fold("legacy_run_finished_with_no_usage_key_at_all"), None);

    let one = fold("run_finished_with_a_single_fully_populated_usage_entry").expect("usage");
    assert_eq!(
        one,
        kobold_proto::Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150
        }
    );

    // Both entries, and the totals distinguish this from reading either one
    // alone: 120 and 15 are each a plausible wrong answer, and so is 135 if
    // only one field were summed.
    let two = fold("run_finished_with_multiple_usage_entries_per_model").expect("usage");
    assert_eq!(
        two,
        kobold_proto::Usage {
            input_tokens: 110,
            output_tokens: 25,
            total_tokens: 135
        }
    );

    // A reported total with neither input nor output. Summing the other two
    // gives zero for this real payload, so the reported figure has to win.
    let sparse = fold("run_finished_with_usage_alongside_outcome_and_result").expect("usage");
    assert_eq!(
        sparse.total_tokens, 7,
        "a reported total was recomputed away"
    );

    // And the other direction: an input with no total at all, on a run that
    // failed after billing. Here there is nothing reported to prefer, so the
    // total is computed -- which is the branch the case above cannot reach.
    let failed = fold("run_error_with_partial_usage").expect("a failed run still cost money");
    assert_eq!(
        failed,
        kobold_proto::Usage {
            input_tokens: 100,
            output_tokens: 0,
            total_tokens: 100
        }
    );

    // An empty array is a report of nothing, not a report of zero.
    assert_eq!(agui::fold_usage(&[]), None);

    // The computed branch with BOTH operands non-zero, and it is constructed
    // rather than taken from the corpus because the corpus has no such entry
    // -- `run_error_with_partial_usage` reports an input and no output, so
    // `100 + 0` and `100 - 0` agree and the arithmetic above is unobserved.
    // Constructing it is fair here in a way it would not be for a
    // conformance claim: the shape is the specification's, but what is under
    // test is our fold rather than our reading of the protocol.
    assert_eq!(
        agui::fold_usage(&[TokenUsage {
            input_tokens: Some(40),
            output_tokens: Some(5),
            ..TokenUsage::default()
        }]),
        Some(kobold_proto::Usage {
            input_tokens: 40,
            output_tokens: 5,
            total_tokens: 45
        })
    );
}

#[test]
fn a_usage_array_that_is_present_and_empty_is_a_third_state_the_corpus_never_asks_about() {
    // `None`, `Some(vec![])` and `Some(non-empty)`. The middle one is legal
    // for a conforming producer and appears in neither vendored file, so
    // every other test here passes while it goes unconsidered -- and the
    // obvious fold, `usage[0]`, panics on it.
    //
    // **This is the second time in one step that the corpus simply never
    // asked the question**, after nine producer assertions that would have
    // passed against a type unable to serialize an optional field at all.
    // The failure mode of fixture-driven work is not a wrong fixture; it is
    // a question nobody's fixture poses.
    let absent: Incoming =
        sonic_rs::from_str(r#"{"type":"RUN_FINISHED","threadId":"t","runId":"r"}"#)
            .expect("decode");
    let Incoming::RunFinished { usage, .. } = absent else {
        panic!("wrong variant")
    };
    assert_eq!(usage, None, "not reported");

    let empty: Incoming =
        sonic_rs::from_str(r#"{"type":"RUN_FINISHED","threadId":"t","runId":"r","usage":[]}"#)
            .expect("decode");
    let Incoming::RunFinished { usage, .. } = empty else {
        panic!("wrong variant")
    };
    assert_eq!(usage, Some(Vec::new()), "reported, and reported as nothing");

    // The two are a different fact on the wire and the type keeps them
    // apart, which is settled and not ours to choose. What IS ours is that
    // the gauge treats them the same, because neither carries a count -- and
    // `fold_usage` reaching that answer without indexing is the point.
    assert_eq!(agui::fold_usage(&[]), None);
}

#[test]
fn a_usage_report_that_violates_its_own_contract_cannot_reach_the_gauge_as_nonsense() {
    // The counts cross a trust boundary: under AG-UI the process at the other
    // end may be an agent nobody here wrote, so "what does a sparse report
    // do" is not the same question as "what does a hostile or broken one do".
    //
    // A negative count is not representable and must not become one. `u32`
    // refuses it at the parse, which takes the whole event with it -- pinned
    // here because the alternative a defaulted field would give is a silent
    // zero, and a zero is a claim.
    let negative = sonic_rs::from_str::<Incoming>(
        r#"{"type":"RUN_FINISHED","threadId":"t","runId":"r","usage":[{"inputTokens":-5}]}"#,
    );
    assert!(negative.is_err(), "a negative token count was accepted");

    // A total that contradicts its parts is kept, deliberately. There is no
    // way to tell a provider that counts differently from one that counts
    // wrongly, and recomputing here would silently disagree with the bill.
    let contradictory = agui::fold_usage(&[TokenUsage {
        input_tokens: Some(10),
        output_tokens: Some(10),
        total_tokens: Some(999),
        ..TokenUsage::default()
    }])
    .expect("usage");
    assert_eq!(
        contradictory.total_tokens, 999,
        "the provider's own figure was overruled"
    );
    assert_eq!(
        contradictory.input_tokens, 10,
        "and the parts are still carried as reported"
    );

    // And the arithmetic saturates rather than wrapping. Two maximal entries
    // are a debug panic and, worse, a release wrap -- a gauge reading near
    // zero for a turn that claimed to cost everything. Pegged is absurd on
    // sight; wrapped is plausible and wrong.
    let huge = TokenUsage {
        input_tokens: Some(u32::MAX),
        output_tokens: Some(u32::MAX),
        total_tokens: Some(u32::MAX),
        ..TokenUsage::default()
    };
    let pegged = agui::fold_usage(&[huge.clone(), huge]).expect("usage");
    assert_eq!(pegged.total_tokens, u32::MAX);
    assert_eq!(pegged.input_tokens, u32::MAX);

    // The computed branch saturates too, which the case above cannot reach
    // because it reports a total for the other branch to prefer.
    let computed = agui::fold_usage(&[TokenUsage {
        input_tokens: Some(u32::MAX),
        output_tokens: Some(1),
        ..TokenUsage::default()
    }])
    .expect("usage");
    assert_eq!(computed.total_tokens, u32::MAX);
}

#[test]
fn a_real_provider_stream_fits_inside_what_the_specification_allows() {
    // The spec-derived fixtures test whether we read the specification right.
    // This one tests whether a real stream fits inside it: id lengths,
    // escape-heavy deltas, and non-ASCII the model produced unprompted. They
    // fail differently and neither substitutes for the other.
    let all = cases("provider-shaped.json", "cases");
    let mut checked = 0usize;
    for case in &all {
        let events: Vec<sonic_rs::Value> = match (case.get("event"), case.get("events")) {
            (Some(one), _) => vec![one.clone()],
            (None, Some(many)) => many.as_array().expect("events is an array").to_vec(),
            _ => panic!("a case with neither event nor events"),
        };
        for event in events {
            let text = sonic_rs::to_string(&event).expect("re-encode");
            let got: Incoming = sonic_rs::from_str(&text).unwrap_or_else(|e| {
                panic!("a payload observed on the wire did not decode: {e} in {text}")
            });
            assert_ne!(
                kind(&got),
                "Unmodelled",
                "a provider event fell through: {text}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 6, "every provider-shaped event must be exercised");

    // The id that disqualified `ag-ui-core`: `resp_` plus 48 hex characters,
    // neither a UUID nor hyphenated. Read back out rather than merely parsed,
    // so a type that accepted it and stored something else is caught.
    let raw = r#"{"type":"RUN_STARTED","threadId":"main",
                  "runId":"resp_04162215aeedcca3016a89342ae8cc87d1bf19f10c404d929f"}"#;
    let got: Incoming = sonic_rs::from_str(raw).expect("a real provider id must parse");
    let Incoming::RunStarted {
        run_id, thread_id, ..
    } = got
    else {
        panic!("wrong variant")
    };
    assert_eq!(
        run_id,
        "resp_04162215aeedcca3016a89342ae8cc87d1bf19f10c404d929f"
    );
    assert_eq!(thread_id, "main");
}

#[test]
fn a_reasoning_delta_with_a_newline_in_it_is_still_one_line_on_the_wire() {
    // The property the whole NDJSON framing rests on, against text observed
    // on the wire rather than an invented edge case: a real reasoning summary
    // delta carrying markdown emphasis, an embedded newline and a curly
    // apostrophe. If a raw newline could reach the pipe, the reader would
    // split one message into two fragments and neither would be valid JSON.
    let delta = "**Calculating multiplication**\n\nI\u{2019}ll";
    let event = Outgoing::ReasoningMessageContent {
        base: Base::default(),
        message_id: "rs_04162215aeedcca3016a89342b95dc87d1a2a3773352a29946".into(),
        delta: delta.to_owned(),
    };
    let line = kobold_proto::codec::encode(&event).expect("encode");
    assert_eq!(
        line.matches('\n').count(),
        1,
        "more than one newline in {line:?}"
    );
    assert!(
        line.ends_with('\n'),
        "the one newline must be the terminator"
    );

    let back: Incoming = kobold_proto::codec::decode(&line).expect("decode");
    let Incoming::ReasoningMessageContent { delta: got, .. } = back else {
        panic!("wrong variant")
    };
    assert_eq!(
        got, delta,
        "the newline and the non-ASCII must survive intact"
    );
}
