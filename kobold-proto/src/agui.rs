//! AG-UI events, the subset Kobold speaks.
//!
//! Inert as of this commit: nothing constructs these and nothing consumes
//! them. What they are checked against is the specification's own vendored
//! payloads in `kobold/tests/fixtures/agui/`, because the reason we carry our
//! own types at all is that a published crate compiled cleanly and then
//! failed on the first event of a real stream. Compiling is not conforming.
//!
//! **Field names are renamed one at a time and never with `rename_all`.** The
//! protocol mixes conventions -- `messageId` beside `type` beside
//! `toolCallName` -- so a blanket rule is a rule that is right until it is
//! silently wrong, and a wire-name bug does not fail loudly, it drops a
//! field.
//!
//! **Internally-tagged enums defeat zero-copy borrowing, and that is fine
//! here.** `kobold-openai/src/events.rs` explains that *incoming provider*
//! events are a flat struct rather than a tagged enum precisely because
//! `#[serde(tag)]` buffers through serde's `Content`. That reasoning belongs
//! to the provider seam, where there is a socket read buffer to borrow from.
//! It does not transfer to this one: the frames here are already
//! owned-`String` types, so there is no zero-copy to lose, and the codec
//! measures at 0.35us per delta against an inter-delta gap of 4-48ms.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An opaque JSON value, for the parts of the protocol Kobold carries but
/// does not read.
///
/// **Not `sonic_rs::Value`, and that is measured rather than stylistic.**
/// `sonic_rs::Value` deserializes by asking its deserializer for raw JSON
/// through a private newtype-struct hint. Both `#[serde(tag)]` and
/// `#[serde(flatten)]` route the input through serde's buffered `Content`
/// first, which does not speak that hint, so **every event carrying one
/// fails to decode** with `invalid type: newtype struct, expected a valid
/// json`. `OwnedLazyValue` fails identically.
///
/// Serializing works, which is what makes it a trap: a type built that way
/// encodes perfectly and cannot read back a single one of the
/// specification's own payloads. Compiling is not conforming.
///
/// This goes through serde's generic data model only, so it survives the
/// buffered path. `Int` precedes `Float` so a whole number stays whole, and
/// `Null` is a value rather than an absence -- the protocol needs that,
/// because a null inside an opaque payload is data even where a null in a
/// declared field means omitted.
///
/// Integers beyond `i64` decode as `Float` and lose precision. Nothing in
/// AG-UI carries one; anything that ever did would be a token count, and
/// those have their own typed fields.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

/// Open by key, per the specification: any JSON value under any key. The
/// `ag-ui` key is reserved for the protocol's own use.
pub type Metadata = BTreeMap<String, Json>;

/// Declared on every event by the specification, and absent from
/// `ag-ui-core` entirely -- which is one of the reasons we carry our own.
///
/// **A known, deliberate deviation:** the specification *rejects* an explicit
/// `"metadata": null`. Metadata postdates the producer-side omit-don't-null
/// fix, so there is nobody to grandfather. `#[serde(default)]` accepts it as
/// absent anyway. Being more tolerant than the specification is the safe
/// direction and not worth a custom deserializer, but it is written down
/// here rather than left to be discovered.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Base {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    #[serde(rename = "rawEvent", default, skip_serializing_if = "Option::is_none")]
    pub raw_event: Option<Json>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// Per-(provider, model) token counts, as the protocol carries them.
///
/// **Not `crate::Usage`, and the two must not be conflated.** This is the
/// wire shape: camelCase, every count optional, and an array on the events
/// that carry it because a run may touch more than one model. `Usage` is
/// Kobold's context gauge and keeps its own shape. Folding one into the other
/// is a decision with a right answer per field, not a rename.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(
        rename = "inputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_tokens: Option<u32>,
    #[serde(
        rename = "outputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_tokens: Option<u32>,
    /// Reported, not derived. A `totalTokens` with neither input nor output
    /// is in the specification's own test corpus, so a fold that computes
    /// this by adding the other two answers zero for a real payload.
    #[serde(
        rename = "totalTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub total_tokens: Option<u32>,
    /// The number that would have made the 435-second turn legible. Measured
    /// on our own provider at 38 of 52 output tokens, for `17 x 23`.
    #[serde(
        rename = "reasoningTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_tokens: Option<u32>,
    #[serde(
        rename = "cachedInputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cached_input_tokens: Option<u32>,
}

/// Fold B: a run's per-model usage entries into Kobold's context gauge.
///
/// Kobold's job, not the adapter's, and the distinction is not pedantry: an
/// adapter *emits* `Vec<TokenUsage>`, so it is not the thing that can fold
/// it. Fold A -- a provider's nested report into one `TokenUsage` -- is the
/// adapter's, and that one happens in `kobold-openai`.
///
/// Three rules, each of them a fixture in `usage.json` rather than a
/// preference:
///
/// - **`None` is not zero.** A run with no usage report leaves the gauge
///   alone; returning `Usage::default()` would read as "this turn was free",
///   which is the exact failure `Usage`'s own doc comment exists to prevent.
/// - **Every entry counts.** The array is per (provider, model), so a run
///   that touched two models has two, and reading entry zero loses the
///   second in silence.
/// - **A reported total wins over a computed one.** The corpus carries an
///   entry with `totalTokens` and neither input nor output, so a fold that
///   adds the other two answers zero for a real payload. Only where no total
///   was reported are the other two summed, and where nothing at all was
///   reported the entry contributes nothing.
///
/// **A known approximation, recorded rather than hidden.** `pane.context`
/// answers *how full is the context*, and a run that touched two models does
/// not have one context to be full of. Summing gives what the run **cost**,
/// which is a different quantity wearing the same number. Showing nothing
/// instead would be worse -- a gauge that silently stops updating is stale
/// rather than wrong, and stale is the harder failure to notice -- and
/// reading entry zero is precisely what `usage.json` was built to catch. Our
/// own adapter emits one entry, so this is hypothetical today; it wants
/// revisiting if a multi-entry report ever becomes real.
pub fn fold_usage(entries: &[TokenUsage]) -> Option<crate::Usage> {
    if entries.is_empty() {
        return None;
    }
    let mut folded = crate::Usage::default();
    for e in entries {
        // Saturating, not wrapping and not panicking. These counts come from
        // whatever process is on the other end of the pipe, which under
        // AG-UI may be an agent nobody here wrote. `u32::MAX` twice is a
        // debug panic and a wrapped gauge in release -- a number reading
        // near zero for a turn that claimed to cost everything, which is the
        // worst of the three outcomes. A pegged gauge is visibly absurd.
        folded.input_tokens = folded
            .input_tokens
            .saturating_add(e.input_tokens.unwrap_or(0));
        folded.output_tokens = folded
            .output_tokens
            .saturating_add(e.output_tokens.unwrap_or(0));
        let entry_total = match e.total_tokens {
            // Preferred even where it contradicts the parts. The protocol
            // makes this the provider's own figure, and there is no way to
            // tell a provider that counts differently from one that counts
            // wrongly -- so the reported number is reported, and second-
            // guessing it here would silently disagree with the bill.
            Some(t) => t,
            None => e
                .input_tokens
                .unwrap_or(0)
                .saturating_add(e.output_tokens.unwrap_or(0)),
        };
        folded.total_tokens = folded.total_tokens.saturating_add(entry_total);
    }
    Some(folded)
}

/// What an adapter emits. **Serialize only.**
///
/// Struct variants declared inline, not newtypes over standalone structs.
/// `ag-ui-core` puts `#[serde(tag = "type")]` on the enum and the fields in
/// named structs, so serializing one of those structs directly produces a
/// payload with no `type` at all -- valid JSON that no consumer can route.
/// Here there is no `TextMessageContentEvent` type to hand a serializer, so
/// the bug is not a rule to remember or a lint to add: **the shape that
/// produces it cannot be written.**
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum Outgoing {
    #[serde(rename = "RUN_STARTED")]
    RunStarted {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "parentRunId", skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
    },
    #[serde(rename = "RUN_FINISHED")]
    RunFinished {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Vec<TokenUsage>>,
    },
    #[serde(rename = "RUN_ERROR")]
    RunError {
        #[serde(flatten)]
        base: Base,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// A run that died after billing still cost money. The
        /// specification's own corpus carries usage on a `RUN_ERROR`, and
        /// `Update::Failed` has never had anywhere to put it.
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Vec<TokenUsage>>,
    },
    #[serde(rename = "TEXT_MESSAGE_START")]
    TextMessageStart {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        /// Defaulted to "assistant" by the specification; always written,
        /// because a field that is usually omitted is a field nobody
        /// notices is wrong.
        role: String,
    },
    #[serde(rename = "TEXT_MESSAGE_CONTENT")]
    TextMessageContent {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
    },
    #[serde(rename = "TEXT_MESSAGE_END")]
    TextMessageEnd {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    #[serde(rename = "TOOL_CALL_START")]
    ToolCallStart {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolCallName")]
        tool_call_name: String,
        #[serde(rename = "parentMessageId", skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<String>,
    },
    #[serde(rename = "TOOL_CALL_ARGS")]
    ToolCallArgs {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        delta: String,
    },
    #[serde(rename = "TOOL_CALL_END")]
    ToolCallEnd {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
    },
    /// The reasoning *boundary*, which carries no content and needs no
    /// rendering decision. This pair is what turns a silent gap from
    /// indistinguishable-from-a-wedge into explained, and it is the strongest
    /// argument for the phase that is not about provider portability.
    #[serde(rename = "REASONING_START")]
    ReasoningStart {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    #[serde(rename = "REASONING_END")]
    ReasoningEnd {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    #[serde(rename = "REASONING_MESSAGE_START")]
    ReasoningMessageStart {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        role: String,
    },
    #[serde(rename = "REASONING_MESSAGE_CONTENT")]
    ReasoningMessageContent {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
    },
    #[serde(rename = "REASONING_MESSAGE_END")]
    ReasoningMessageEnd {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
    },
}

/// What Kobold parses. **Deserialize only.**
///
/// A separate type from `Outgoing`, and not for tidiness. This one needs a
/// catch-all for the two thirds of the protocol we do not model, and
/// `#[serde(other)]` gives a *unit* variant -- which an internally-tagged
/// enum serializes perfectly happily as `{"type":"Unmodelled"}`, under both
/// serde_json and sonic-rs. `"Unmodelled"` is in no version of the protocol.
/// A single enum deriving both traits would therefore reopen exactly the
/// class of hole the inline struct variants above close, one layer along.
///
/// The split also matches the traffic: Kobold only ever parses, and an
/// adapter only ever writes.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum Incoming {
    #[serde(rename = "RUN_STARTED")]
    RunStarted {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "parentRunId", default)]
        parent_run_id: Option<String>,
        /// Opaque. This is `RunAgentInput` -- the whole message array, tools
        /// and context -- echoed back to the client that sent it. Kobold has
        /// no use for its own input returned, and modelling it here would be
        /// modelling the *request* type in the reply's clothing.
        #[serde(default)]
        input: Option<Json>,
    },
    #[serde(rename = "RUN_FINISHED")]
    RunFinished {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(default)]
        usage: Option<Vec<TokenUsage>>,
        /// One of the three fields the specification still accepts an
        /// explicit `null` for, to grandfather older producers.
        ///
        /// Opaque because interrupts are deliberately not implemented:
        /// carrying the value costs nothing and loses nothing, where a
        /// modelled `Outcome` would be a taxonomy nothing reads. If
        /// interrupts are ever adopted, the data is already here.
        #[serde(default)]
        outcome: Option<Json>,
        #[serde(default)]
        result: Option<Json>,
    },
    #[serde(rename = "RUN_ERROR")]
    RunError {
        #[serde(flatten)]
        base: Base,
        message: String,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        usage: Option<Vec<TokenUsage>>,
    },
    #[serde(rename = "TEXT_MESSAGE_START")]
    TextMessageStart {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(default)]
        role: Option<String>,
    },
    #[serde(rename = "TEXT_MESSAGE_CONTENT")]
    TextMessageContent {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
    },
    #[serde(rename = "TEXT_MESSAGE_END")]
    TextMessageEnd {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    /// Consumed, never emitted. The specification's own reference server
    /// streams OpenAI through the chunk events *exclusively*, so a
    /// conforming foreign agent may speak nothing else. Every field is
    /// optional -- the corpus carries a case with none of them set.
    #[serde(rename = "TEXT_MESSAGE_CHUNK")]
    TextMessageChunk {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId", default)]
        message_id: Option<String>,
        #[serde(default)]
        role: Option<String>,
        #[serde(default)]
        delta: Option<String>,
    },
    #[serde(rename = "TOOL_CALL_START")]
    ToolCallStart {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolCallName")]
        tool_call_name: String,
        /// Accepts an explicit `null`, one of exactly three fields that do.
        #[serde(rename = "parentMessageId", default)]
        parent_message_id: Option<String>,
    },
    #[serde(rename = "TOOL_CALL_ARGS")]
    ToolCallArgs {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        delta: String,
    },
    #[serde(rename = "TOOL_CALL_END")]
    ToolCallEnd {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
    },
    #[serde(rename = "TOOL_CALL_CHUNK")]
    ToolCallChunk {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "toolCallId", default)]
        tool_call_id: Option<String>,
        #[serde(rename = "toolCallName", default)]
        tool_call_name: Option<String>,
        /// The second of the three fields that accept an explicit `null`.
        #[serde(rename = "parentMessageId", default)]
        parent_message_id: Option<String>,
        #[serde(default)]
        delta: Option<String>,
    },
    /// A tool the **agent** ran, which is `Model::server_tools` -- web search
    /// and its like. Kobold offers those today and is completely blind to
    /// them: no event is modelled, so a server-side search is invisible.
    ///
    /// Consumed, never emitted, and that asymmetry is not an oversight. A
    /// tool Kobold runs produces a result Kobold already has; it goes back as
    /// a `ToolMessage` in the next run's input, never as an event, because
    /// AG-UI events flow agent to client and Kobold is the client.
    #[serde(rename = "TOOL_CALL_RESULT")]
    ToolCallResult {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        content: String,
        #[serde(default)]
        role: Option<String>,
    },
    #[serde(rename = "REASONING_START")]
    ReasoningStart {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    #[serde(rename = "REASONING_END")]
    ReasoningEnd {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    #[serde(rename = "REASONING_MESSAGE_START")]
    ReasoningMessageStart {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(default)]
        role: Option<String>,
    },
    #[serde(rename = "REASONING_MESSAGE_CONTENT")]
    ReasoningMessageContent {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
    },
    #[serde(rename = "REASONING_MESSAGE_END")]
    ReasoningMessageEnd {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    #[serde(rename = "REASONING_MESSAGE_CHUNK")]
    ReasoningMessageChunk {
        #[serde(flatten)]
        base: Base,
        #[serde(rename = "messageId", default)]
        message_id: Option<String>,
        #[serde(default)]
        role: Option<String>,
        #[serde(default)]
        delta: Option<String>,
    },
    /// Everything in the specification we do not model, and everything the
    /// specification grows next. AG-UI is pre-1.0 and moving; without this a
    /// `STATE_DELTA` is a decode error, and the transport already holds the
    /// line that an unmodelled event must never kill the session.
    #[serde(other)]
    Unmodelled,
}
