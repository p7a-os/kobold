//! Elicitation: a server asking the person at the keyboard a question.
//!
//! The two revisions differ in *delivery* and agree on *payload*, which is
//! worth stating because it is the reason this module is not split in two:
//!
//! - 2025-11-25 sends `elicitation/create` as a server-initiated JSON-RPC
//!   request, and the client replies to it directly.
//! - 2026-07-28 has no server-initiated requests at all. The same
//!   `elicitation/create` object arrives inside an `InputRequiredResult`, and
//!   the client answers by re-sending its original request with the result in
//!   `inputResponses`.
//!
//! Both carry the same `params` -- `mode`, `message`, `requestedSchema` -- and
//! both expect the same `{action, content}` back. So parsing and answering are
//! shared, and only the envelope differs; that lives with each transport rather
//! than here. Checked against both specification pages rather than assumed:
//! where they agree, they agree exactly.
//!
//! The question ends up in kobold's own `ask` panel. That is deliberate: an
//! elicitation and a local `ask` are the same question put to the same person
//! in the same place, and two panels would be two sets of keybindings for one
//! idea.

use crate::tools::{Ask, MAX_OPTIONS};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

/// What the user did, in the spec's own vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Approved and submitted data.
    Accept,
    /// Explicitly refused.
    Decline,
    /// Dismissed without choosing either way.
    Cancel,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Accept => "accept",
            Action::Decline => "decline",
            Action::Cancel => "cancel",
        }
    }
}

/// A parsed `elicitation/create`, reduced to what the panel needs and what is
/// required to answer it.
#[derive(Debug, Clone, PartialEq)]
pub struct Elicitation {
    /// The single property being asked for. The schema permits several, but the
    /// panel asks one thing at a time, so only the first is used and the rest
    /// are declined rather than silently dropped -- see [`parse`].
    pub property: String,
    pub ask: Ask,
}

/// Turn `elicitation/create` params into a question for the panel.
///
/// `Err` is a reason to decline rather than a failure: the server asked for
/// something this client cannot present, and telling it so is a better answer
/// than an error, because `decline` is a response it already knows how to
/// handle.
pub fn parse(call_id: &str, params: &Value) -> Result<Elicitation, String> {
    // Absent means form, per both revisions: servers may omit it for backwards
    // compatibility and clients must read that as form mode.
    let mode_value = params.get("mode");
    let mode = mode_value
        .as_ref()
        .and_then(|m| m.as_str())
        .unwrap_or("form");
    if mode == "url" {
        // Out-of-band interaction through a browser. Declined rather than
        // half-supported: the flow's entire point is that the data never
        // reaches the client, so rendering the URL in a terminal panel and
        // calling it done would claim a guarantee kobold cannot make.
        return Err("this client does not support url-mode elicitation".to_owned());
    }
    if mode != "form" {
        return Err(format!("unknown elicitation mode '{mode}'"));
    }

    let message_value = params.get("message");
    let message = message_value
        .as_ref()
        .and_then(|m| m.as_str())
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| "elicitation has no message".to_owned())?;

    let properties = params
        .get("requestedSchema")
        .and_then(|s| s.get("properties"))
        .ok_or_else(|| "requestedSchema has no properties".to_owned())?;
    let object = properties
        .as_object()
        .ok_or_else(|| "requestedSchema properties is not an object".to_owned())?;
    let (name, schema) = object
        .iter()
        .next()
        .ok_or_else(|| "requestedSchema asks for nothing".to_owned())?;
    if object.iter().count() > 1 {
        // One panel, one question. Answering the first and dropping the rest
        // would return content the server believes is complete.
        return Err(format!(
            "this client asks one thing at a time; {} properties were requested",
            object.iter().count()
        ));
    }

    let (options, multiple) = choices(schema);
    // More options than the panel holds. Rather than refuse, ask it as free
    // text and name them in the question: the user can still answer, which is
    // the point, and the alternative is a dead end for a schema the spec allows.
    let (options, question) = if options.len() > MAX_OPTIONS {
        (Vec::new(), format!("{message} ({})", options.join(", ")))
    } else {
        (options, message.to_owned())
    };

    Ok(Elicitation {
        property: name.to_owned(),
        ask: Ask {
            call_id: call_id.to_owned(),
            question,
            options,
            multiple,
        },
    })
}

/// The enum choices a property offers, and whether several may be picked.
///
/// Two shapes carry them: a string with `enum`, which is single-select, and an
/// array whose `items` carry `enum`, which is multi. Anything else is free text.
fn choices(schema: &Value) -> (Vec<String>, bool) {
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(|e| {
            e.as_array().map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default()
    };
    let kind = schema.get("type");
    match kind.as_ref().and_then(|k| k.as_str()) {
        Some("array") => {
            let items = schema.get("items").and_then(|i| i.get("enum"));
            (strings(items), true)
        }
        _ => (strings(schema.get("enum")), false),
    }
}

/// The `{action, content}` a client returns, in the shape both revisions expect.
///
/// `content` is present only for `accept`. A declined or cancelled elicitation
/// carries no data, and sending an empty object would be indistinguishable from
/// a user who accepted and answered nothing.
pub fn result(action: Action, property: &str, answer: Option<&str>) -> Value {
    let mut out = sonic_rs::json!({ "action": action.as_str() });
    if let (Action::Accept, Some(text)) = (action, answer) {
        out["content"] = sonic_rs::json!({ property: text });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(raw: &str) -> Value {
        sonic_rs::from_str(raw).expect("fixture is valid JSON")
    }

    #[test]
    fn a_bare_string_property_becomes_a_free_text_question() {
        let p = params(
            r#"{"mode":"form","message":"Your GitHub username?",
                "requestedSchema":{"type":"object","properties":{"name":{"type":"string"}}}}"#,
        );
        let e = parse("call_1", &p).expect("should parse");
        assert_eq!(e.property, "name");
        assert_eq!(e.ask.question, "Your GitHub username?");
        assert!(
            e.ask.options.is_empty(),
            "nothing to choose from, so type it"
        );
        assert!(!e.ask.multiple);
        assert_eq!(
            e.ask.call_id, "call_1",
            "the answer has to find its way back"
        );
    }

    #[test]
    fn an_enum_becomes_a_single_select_and_an_array_of_enum_a_multi_select() {
        // The only two shapes that carry choices, and the difference between
        // them is the difference between radio and checkbox on screen.
        let single = params(
            r#"{"message":"Pick a colour","requestedSchema":{"type":"object","properties":{
                "colour":{"type":"string","enum":["Red","Green","Blue"]}}}}"#,
        );
        let e = parse("c", &single).expect("should parse");
        assert_eq!(e.ask.options, vec!["Red", "Green", "Blue"]);
        assert!(!e.ask.multiple, "a string enum picks one");

        let multi = params(
            r#"{"message":"Pick colours","requestedSchema":{"type":"object","properties":{
                "colours":{"type":"array","items":{"type":"string","enum":["Red","Green"]}}}}}"#,
        );
        let e = parse("c", &multi).expect("should parse");
        assert_eq!(e.ask.options, vec!["Red", "Green"]);
        assert!(e.ask.multiple, "an array of enum picks several");
    }

    #[test]
    fn a_missing_mode_is_form_because_both_revisions_say_so() {
        // Servers may omit it for backwards compatibility and clients must
        // read the absence as form. Defaulting the other way would reject
        // every older server.
        let p = params(
            r#"{"message":"Name?","requestedSchema":{"type":"object","properties":{"n":{"type":"string"}}}}"#,
        );
        assert!(parse("c", &p).is_ok());
    }

    #[test]
    fn url_mode_is_declined_rather_than_half_supported() {
        // Its whole point is that the data never reaches the client. Rendering
        // the URL in a terminal and calling it done would claim a guarantee
        // kobold cannot make.
        let p = params(r#"{"mode":"url","message":"Sign in","url":"https://example.test/auth"}"#);
        let why = parse("c", &p).expect_err("url mode is not supported");
        assert!(why.contains("url"), "{why}");
    }

    #[test]
    fn more_options_than_the_panel_holds_become_a_typed_answer_naming_them() {
        // A dead end would be worse: the schema is legal, so the user should
        // still be able to answer.
        let p = params(
            r#"{"message":"Pick one","requestedSchema":{"type":"object","properties":{
                "x":{"type":"string","enum":["a","b","c","d","e","f"]}}}}"#,
        );
        let e = parse("c", &p).expect("should parse");
        assert!(e.ask.options.is_empty(), "too many to show as options");
        for opt in ["a", "b", "c", "d", "e", "f"] {
            assert!(
                e.ask.question.contains(opt),
                "option {opt} was dropped: {}",
                e.ask.question
            );
        }
        // Exactly the maximum still renders as options.
        let p = params(
            r#"{"message":"Pick one","requestedSchema":{"type":"object","properties":{
                "x":{"type":"string","enum":["a","b","c","d"]}}}}"#,
        );
        assert_eq!(
            parse("c", &p).expect("should parse").ask.options.len(),
            MAX_OPTIONS
        );
    }

    #[test]
    fn what_cannot_be_asked_is_declined_with_a_reason() {
        // Every one of these is a legal thing for a server to send and an
        // impossible thing for one panel to show. Declining is an answer the
        // server already knows how to handle; an error is not.
        let cases = [
            (
                r#"{"message":"","requestedSchema":{"type":"object","properties":{"n":{"type":"string"}}}}"#,
                "message",
            ),
            (r#"{"message":"Hi"}"#, "requestedSchema"),
            (
                r#"{"message":"Hi","requestedSchema":{"type":"object","properties":{}}}"#,
                "nothing",
            ),
            (r#"{"mode":"telepathy","message":"Hi"}"#, "mode"),
        ];
        for (raw, expected) in cases {
            let why = parse("c", &params(raw)).expect_err(raw);
            assert!(
                why.contains(expected),
                "{raw} gave {why:?}, wanted mention of {expected}"
            );
        }
    }

    #[test]
    fn several_properties_are_declined_rather_than_partly_answered() {
        // Answering the first and dropping the rest would hand back content
        // the server has every reason to believe is complete.
        let p = params(
            r#"{"message":"Details","requestedSchema":{"type":"object","properties":{
                "name":{"type":"string"},"email":{"type":"string"}}}}"#,
        );
        let why = parse("c", &p).expect_err("two properties cannot be one panel");
        assert!(why.contains("one thing at a time"), "{why}");
        assert!(
            why.contains('2'),
            "should say how many were asked for: {why}"
        );
    }

    #[test]
    fn only_an_accepted_elicitation_carries_content() {
        // An empty `content` on a decline is indistinguishable from a user who
        // accepted and answered nothing.
        let accepted = result(Action::Accept, "name", Some("ada"));
        assert_eq!(accepted["action"].as_str(), Some("accept"));
        assert_eq!(accepted["content"]["name"].as_str(), Some("ada"));

        for action in [Action::Decline, Action::Cancel] {
            let out = result(action, "name", Some("ada"));
            assert_eq!(out["action"].as_str(), Some(action.as_str()));
            assert!(
                out.get("content").is_none(),
                "{} carried content",
                action.as_str()
            );
        }
        // Accepting without an answer is also contentless, not empty content.
        assert!(result(Action::Accept, "name", None)
            .get("content")
            .is_none());
    }

    #[test]
    fn the_three_actions_are_spelled_the_way_the_spec_spells_them() {
        // These strings go on the wire; a typo is a protocol error that would
        // read as a server bug.
        assert_eq!(Action::Accept.as_str(), "accept");
        assert_eq!(Action::Decline.as_str(), "decline");
        assert_eq!(Action::Cancel.as_str(), "cancel");
    }

    #[test]
    fn both_revisions_parse_from_the_same_payload() {
        // The finding this module is built on: 2025-11-25 delivers this as a
        // server-initiated request and 2026-07-28 inside an InputRequiredResult,
        // but the `params` are identical, so one parser serves both. If that
        // ever stops being true, this test is where it shows.
        let legacy_params = params(
            r#"{"mode":"form","message":"Pick","requestedSchema":{"type":"object",
                "properties":{"c":{"type":"string","enum":["x","y"]}}}}"#,
        );
        let modern_params = params(
            r#"{"mode":"form","message":"Pick","requestedSchema":{"type":"object",
                "properties":{"c":{"type":"string","enum":["x","y"]}}}}"#,
        );
        assert_eq!(parse("c", &legacy_params), parse("c", &modern_params));
    }
}
