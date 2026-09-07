//! Wire types for the Responses WebSocket API.
//!
//! Incoming events are one flat struct rather than a `#[serde(tag = "type")]`
//! enum on purpose: serde's internally-tagged enums buffer through `Content`,
//! which defeats zero-copy borrowing. A flat struct with borrowed `&str`
//! fields parses in one pass with no allocation, and unknown event types cost
//! nothing because unrecognised fields are simply ignored.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

// ---------- outgoing ----------

#[derive(Serialize)]
pub struct ResponseCreate<'a> {
    /// Always "response.create".
    #[serde(rename = "type")]
    pub kind: &'a str,
    /// Omit for the implicit default lane; an empty string is not valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<&'a str>,
    pub model: &'a str,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<&'a str>,
    pub input: Vec<InputItem<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a [ToolDef<'a>]>,
    /// `false` warms request state without generating output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate: Option<bool>,
}

impl<'a> ResponseCreate<'a> {
    pub fn user_text(
        model: &'a str,
        stream_id: Option<&'a str>,
        text: &'a str,
        effort: &'a str,
    ) -> Self {
        Self {
            kind: "response.create",
            stream_id,
            model,
            store: false,
            previous_response_id: None,
            input: vec![InputItem::message("user", text)],
            reasoning: Some(Reasoning {
                effort,
                summary: "auto",
            }),
            tools: None,
            generate: None,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum InputItem<'a> {
    Message {
        #[serde(rename = "type")]
        kind: &'a str,
        role: &'a str,
        content: Vec<ContentPart<'a>>,
    },
    /// Tool result. `call_id` must match the originating `function_call`.
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'a str,
        call_id: &'a str,
        output: String,
    },
}

impl<'a> InputItem<'a> {
    pub fn message(role: &'a str, text: &'a str) -> Self {
        // Content part type is role-dependent: an assistant turn replayed as
        // input must be `output_text`. Sending `input_text` for an assistant
        // message is rejected, and the server drops the connection rather than
        // returning a per-lane error.
        let kind = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        InputItem::Message {
            kind: "message",
            role,
            content: vec![ContentPart { kind, text }],
        }
    }

    pub fn tool_output(call_id: &'a str, output: String) -> Self {
        InputItem::FunctionCallOutput {
            kind: "function_call_output",
            call_id,
            output,
        }
    }
}

#[derive(Serialize)]
pub struct Reasoning<'a> {
    /// `none` | `low` | `medium` | `high` | `xhigh` | `max`. `none` is the
    /// latency baseline; the API defaults to `medium` when omitted.
    pub effort: &'a str,
    /// Ask for reasoning summaries, which is what makes a long think
    /// *visible* rather than 435 seconds of nothing.
    ///
    /// Kobold sent `effort` alone for its whole life, so it had never asked
    /// for summaries -- which is a complete explanation of that silence
    /// without needing the provider to be incapable of it. Measured: `"auto"`
    /// is accepted and echoed back resolved to `"detailed"`, and the
    /// summaries then stream token by token exactly like output text.
    ///
    /// `"auto"` rather than a fixed level so the provider picks what suits
    /// the model, and so a model with no summaries degrades to today's
    /// behaviour rather than an error.
    pub summary: &'a str,
}

#[derive(Serialize)]
pub struct ContentPart<'a> {
    #[serde(rename = "type")]
    pub kind: &'a str,
    pub text: &'a str,
}

/// A tool named in a request.
///
/// Two shapes, because two things are being described. A function is one we
/// run: it needs a name, a description and a schema, and the model's call comes
/// back to us. A server-side tool is one the API runs itself -- web search and
/// its like -- and it is named and nothing else, because we neither implement
/// it nor ever see the call.
///
/// Untagged so both serialise as the flat objects the API expects: the `type`
/// field is the discriminator on the wire, and an outer tag would nest them.
#[derive(Serialize)]
#[serde(untagged)]
pub enum ToolDef<'a> {
    Function {
        #[serde(rename = "type")]
        kind: &'a str,
        name: &'a str,
        description: &'a str,
        parameters: sonic_rs::Value,
    },
    /// `{"type": "web_search"}`. Whatever the API offers, passed through by
    /// name rather than enumerated here: a fixed list would need editing every
    /// time the API grows one, and would refuse a tool it simply had not heard
    /// of yet.
    Server {
        #[serde(rename = "type")]
        kind: &'a str,
    },
}

impl<'a> ToolDef<'a> {
    pub fn function(name: &'a str, description: &'a str, parameters: sonic_rs::Value) -> Self {
        ToolDef::Function {
            kind: "function",
            name,
            description,
            parameters,
        }
    }

    pub fn server(kind: &'a str) -> Self {
        ToolDef::Server { kind }
    }
}

// ---------- incoming ----------

#[derive(Deserialize, Debug)]
pub struct Event<'a> {
    #[serde(rename = "type", borrow)]
    pub kind: &'a str,
    /// Absent on the implicit default lane, and on connection-scoped errors.
    #[serde(default, borrow)]
    pub stream_id: Option<&'a str>,
    /// Present on `response.output_text.delta`.
    ///
    /// `Cow`, not `&str`: any delta containing an escape sequence (a newline,
    /// a quote, a `\uXXXX`) cannot be borrowed out of the read buffer, it has
    /// to be unescaped into owned storage. With `&str` those deltas fail to
    /// deserialize and the text is silently lost. Borrowing still happens for
    /// the escape-free majority.
    #[serde(default, borrow)]
    pub delta: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    pub response: Option<ResponseRef<'a>>,
    #[serde(default, borrow)]
    pub item: Option<OutputItem<'a>>,
    #[serde(default, borrow)]
    pub error: Option<ApiError<'a>>,
    #[serde(default, borrow)]
    pub call_id: Option<&'a str>,
    /// Complete arguments on `response.function_call_arguments.done`. Always
    /// escaped in practice: it is JSON nested inside a JSON string.
    #[serde(default, borrow)]
    pub arguments: Option<Cow<'a, str>>,
    /// Which reasoning summary part an event belongs to.
    ///
    /// On every `response.reasoning_summary_*` event, and it has to be read
    /// rather than assumed: a single turn can produce several summary parts,
    /// so keying a reasoning message on the lane alone would concatenate them
    /// into one and lose the boundaries the provider went to the trouble of
    /// sending.
    #[serde(default)]
    pub summary_index: Option<u32>,
}

#[derive(Deserialize, Debug)]
pub struct ResponseRef<'a> {
    #[serde(borrow)]
    pub id: &'a str,
    /// Sent on `response.completed`, and null on every event before it, which
    /// is why the whole thing is optional rather than just its fields.
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// What a turn cost, in the provider's own shape.
///
/// **This used to be `kobold_proto::Usage` re-exported, and that was the bug.**
/// The three flat counts happen to share their names with Kobold's gauge, so
/// they parsed and the totals were right -- while `input_tokens_details` and
/// `output_tokens_details` were dropped as unknown fields, in silence.
/// Measured on the wire: a two-sentence arithmetic turn reported
/// `output_tokens: 52` of which `reasoning_tokens: 38`. **Three quarters of
/// that turn was thinking and Kobold could not say so.**
///
/// The seam comment on `kobold_proto::Usage` said its field names matching
/// OpenAI's was luck rather than design, and that the mapping would be this
/// crate's job under AG-UI. It is, and this is it.
#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    /// `Option`, not a defaulted struct: an absent details object means the
    /// provider said nothing, and a zero would claim it counted none. That is
    /// the same distinction `Usage` itself is `Option` for one level up.
    #[serde(default)]
    pub input_tokens_details: Option<InputTokenDetails>,
    #[serde(default)]
    pub output_tokens_details: Option<OutputTokenDetails>,
}

#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputTokenDetails {
    #[serde(default)]
    pub cached_tokens: u32,
    #[serde(default)]
    pub cache_write_tokens: u32,
}

#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputTokenDetails {
    /// The number that explains a long silent turn. AG-UI has a field for it
    /// and Kobold's gauge does not, which is why the fold happens here.
    #[serde(default)]
    pub reasoning_tokens: u32,
}

impl Usage {
    /// Fold A: the provider's nested report into AG-UI's flat per-model
    /// entry. The adapter's job, and the whole reason an adapter exists.
    ///
    /// The three counts are `Some` because this provider always reports all
    /// three on `response.completed`; the two detail counts are `Some` only
    /// when their object was actually present, so "not reported" survives as
    /// something different from zero.
    pub fn to_agui(self, model: &str) -> kobold_proto::agui::TokenUsage {
        kobold_proto::agui::TokenUsage {
            provider: Some("openai".to_owned()),
            model: Some(model.to_owned()),
            input_tokens: Some(self.input_tokens),
            output_tokens: Some(self.output_tokens),
            total_tokens: Some(self.total_tokens),
            reasoning_tokens: self.output_tokens_details.map(|d| d.reasoning_tokens),
            cached_input_tokens: self.input_tokens_details.map(|d| d.cached_tokens),
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct OutputItem<'a> {
    #[serde(rename = "type", borrow)]
    pub kind: &'a str,
    #[serde(default, borrow)]
    pub call_id: Option<&'a str>,
    #[serde(default, borrow)]
    pub name: Option<&'a str>,
    #[serde(default, borrow)]
    pub arguments: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Debug)]
pub struct ApiError<'a> {
    #[serde(default, borrow)]
    pub code: Option<&'a str>,
    #[serde(default, borrow)]
    pub message: Option<Cow<'a, str>>,
}

impl<'a> Event<'a> {
    /// A lane is finished, successfully or not, on exactly these four.
    /// Miss one and the read loop hangs forever.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.kind,
            "response.completed" | "response.failed" | "response.incomplete" | "error"
        )
    }

    pub fn is_failure(&self) -> bool {
        matches!(
            self.kind,
            "response.failed" | "response.incomplete" | "error"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_of(tools: &[ToolDef]) -> String {
        crate::json::to_string(tools).expect("serialise")
    }

    fn event(kind: &str) -> String {
        format!(r#"{{"type":"{kind}"}}"#)
    }

    #[test]
    fn every_way_a_turn_can_end_is_recognised_as_the_end() {
        // Miss one and the read loop waits forever for something that is
        // never coming, which is the worst shape of bug here: no error, no
        // output, just a conversation that stops.
        for kind in [
            "response.completed",
            "response.failed",
            "response.incomplete",
            "error",
        ] {
            let raw = event(kind);
            let ev: Event = crate::json::from_slice(raw.as_bytes()).expect("parse");
            assert!(
                ev.is_terminal(),
                "{kind} ends a turn and was not recognised"
            );
        }
        // And the ones that must not end it, or the reply is cut off mid-word.
        for kind in [
            "response.created",
            "response.in_progress",
            "response.output_text.delta",
            "response.output_item.done",
        ] {
            let raw = event(kind);
            let ev: Event = crate::json::from_slice(raw.as_bytes()).expect("parse");
            assert!(
                !ev.is_terminal(),
                "{kind} does not end a turn but was treated as if it did"
            );
        }
    }

    #[test]
    fn a_failure_is_distinguished_from_an_ordinary_finish() {
        // They are both terminal; only one of them means the answer is not
        // coming, and the UI reports them differently.
        for kind in ["response.failed", "response.incomplete", "error"] {
            let raw = event(kind);
            let ev: Event = crate::json::from_slice(raw.as_bytes()).expect("parse");
            assert!(ev.is_failure(), "{kind} is a failure");
            assert!(ev.is_terminal(), "{kind} also ends the turn");
        }
        let raw = event("response.completed");
        let ev: Event = crate::json::from_slice(raw.as_bytes()).expect("parse");
        assert!(!ev.is_failure(), "a completed turn is not a failure");
    }

    #[test]
    fn an_assistant_turn_is_replayed_as_output_text_and_a_user_turn_as_input() {
        // Not cosmetic: the server rejects `input_text` on an assistant
        // message and drops the connection rather than returning a per-lane
        // error, so getting this backwards ends the session.
        let assistant =
            crate::json::to_string(&InputItem::message("assistant", "hi")).expect("serialise");
        assert!(assistant.contains(r#""type":"output_text""#), "{assistant}");

        for role in ["user", "system", "developer"] {
            let other = crate::json::to_string(&InputItem::message(role, "hi")).expect("serialise");
            assert!(
                other.contains(r#""type":"input_text""#),
                "{role} should send input_text: {other}"
            );
        }
    }

    #[test]
    fn a_function_tool_carries_its_schema_and_a_server_tool_carries_only_a_type() {
        // The shapes are different because the responsibilities are: one we
        // run and must describe, the other the API runs and we only name.
        let params: sonic_rs::Value = sonic_rs::from_str(r#"{"type":"object"}"#).expect("schema");
        let out = json_of(&[
            ToolDef::function("file_read", "read a file", params),
            ToolDef::server("web_search"),
        ]);
        assert!(out.contains(r#""type":"function""#), "{out}");
        assert!(out.contains(r#""name":"file_read""#), "{out}");
        assert!(out.contains(r#""description":"read a file""#), "{out}");
        assert!(out.contains(r#""type":"web_search""#), "{out}");
    }

    #[test]
    fn a_server_tool_serialises_flat_with_no_wrapper_and_no_empty_fields() {
        // Untagged is the whole point: a tagged enum would nest it under a
        // variant name, and the API would see a tool it does not recognise.
        let out = json_of(&[ToolDef::server("web_search")]);
        assert_eq!(out, r#"[{"type":"web_search"}]"#);
    }

    /// The `usage` object exactly as this provider sent it on 2026-08-22, for
    /// a prompt asking for `17 x 23`. Copied from the wire, not composed:
    /// a hand-built payload is written from the same understanding the code
    /// is, so it agrees with a wrong one.
    const OBSERVED_USAGE: &str = r#"{"input_tokens":18,
        "input_tokens_details":{"cache_write_tokens":0,"cached_tokens":0},
        "output_tokens":52,
        "output_tokens_details":{"reasoning_tokens":38},
        "total_tokens":70}"#;

    #[test]
    fn the_nested_token_details_are_read_rather_than_dropped() {
        // The bug this type exists to fix, pinned against the payload that
        // exposed it. The three flat counts share their names with Kobold's
        // gauge, so they always parsed and the totals were always right --
        // and both nested objects went to unknown-field silence. 38 of 52
        // output tokens were reasoning, and Kobold could not say so.
        let u: Usage = crate::json::from_slice(OBSERVED_USAGE.as_bytes()).expect("real payload");
        assert_eq!(u.input_tokens, 18);
        assert_eq!(u.output_tokens, 52);
        assert_eq!(u.total_tokens, 70);
        assert_eq!(
            u.output_tokens_details.map(|d| d.reasoning_tokens),
            Some(38),
            "the count that explains a silent turn was dropped"
        );
        assert_eq!(u.input_tokens_details.map(|d| d.cached_tokens), Some(0));
    }

    #[test]
    fn a_usage_report_without_details_says_nothing_rather_than_zero() {
        // The partner, and the reason the details are `Option` rather than a
        // defaulted struct. Without this, the test above is satisfied by a
        // type that reports `Some(0)` for a provider that said nothing --
        // which is the same "absent read as zero" mistake one level down.
        let u: Usage =
            crate::json::from_slice(br#"{"input_tokens":18,"output_tokens":52,"total_tokens":70}"#)
                .expect("a report with no details is legal");
        assert_eq!(u.total_tokens, 70);
        assert_eq!(
            u.output_tokens_details, None,
            "silence became a count of zero"
        );
        assert_eq!(u.input_tokens_details, None);
    }

    #[test]
    fn fold_a_carries_every_count_the_provider_reported_into_the_wire_type() {
        // `kobold_proto::Usage` has three fields and this has five. Folding
        // to AG-UI is where the other two survive, and it is the adapter's
        // job precisely because only the adapter knows this provider's shape.
        let u: Usage = crate::json::from_slice(OBSERVED_USAGE.as_bytes()).expect("real payload");
        assert_eq!(
            u.to_agui("gpt-5.6-luna"),
            kobold_proto::agui::TokenUsage {
                provider: Some("openai".to_owned()),
                model: Some("gpt-5.6-luna".to_owned()),
                input_tokens: Some(18),
                output_tokens: Some(52),
                total_tokens: Some(70),
                reasoning_tokens: Some(38),
                cached_input_tokens: Some(0),
            }
        );

        // And an absent detail object stays absent across the fold, rather
        // than being filled in with the zero the struct would default to.
        let bare: Usage =
            crate::json::from_slice(br#"{"input_tokens":1,"output_tokens":2,"total_tokens":3}"#)
                .expect("decode");
        let folded = bare.to_agui("m");
        assert_eq!(folded.reasoning_tokens, None);
        assert_eq!(folded.cached_input_tokens, None);
        assert_eq!(
            folded.total_tokens,
            Some(3),
            "the counts it did report still cross"
        );
    }

    #[test]
    fn any_server_tool_name_passes_through_including_ones_we_have_not_heard_of() {
        // Deliberately not an enum of known tools. The API grows these, and a
        // fixed list would refuse a tool for the sole reason that this code
        // predates it.
        let out = json_of(&[ToolDef::server("some_tool_invented_next_year")]);
        assert!(out.contains("some_tool_invented_next_year"), "{out}");
    }
}
