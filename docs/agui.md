# The AG-UI Protocol Specification Subset for Kobold

This document specifies the AG-UI protocol subset implemented by Kobold, based on the AG-UI specification (`@ag-ui/core` version `0.0.58`) and observed wire behaviors across conforming agents.

---

## 0. Protocol Grounding & Key Distinctions

**1. `TOOL_CALL_RESULT` Handling.** Earlier designs sometimes assume tools are client-only. In AG-UI, `TOOL_CALL_RESULT` exists in `EventType` and has a formal schema (`ToolCallResultEventSchema`).
AG-UI events flow **agent → client**. Kobold is the client. A tool Kobold runs produces a result Kobold already has, so it never arrives as an event — it goes back as a `ToolMessage` in the next run's `messages`.
Where `TOOL_CALL_RESULT` is essential is when an agent reports a tool **it** ran itself (e.g. server-side tools like `web_search`, which Kobold enables via `Model::server_tools`). Consuming `TOOL_CALL_RESULT` gives visibility into server-side executions.

**2. The specification reference server emits `CHUNK` events, not exclusively `START`/`CONTENT`/`END`.** The reference server streams completions through `TextMessageChunkEvent` and `ToolCallChunkEvent`. Accepting chunk events ensures full compatibility with conforming agent implementations.

**3. Reasoning lifecycle.** AG-UI specifies reasoning events (`REASONING_START`, `REASONING_END`, `REASONING_MESSAGE_*`). Capturing these lifecycle points allows Kobold to track when an agent is deep in internal reasoning versus when execution has stalled.

---

## 1. Which events Kobold consumes, and which the adapter must emit

Derived from what `Update` carries today (`kobold-proto/src/lib.rs:17-38`) and
what `App::apply` does with it (`src/app.rs:1711-1839`).

### Kobold consumes today — six variants, and two of them are not AG-UI at all

| `Update` | AG-UI equivalent |
|---|---|
| `Delta { lane, text }` | `TEXT_MESSAGE_CONTENT { messageId, delta }` |
| `ToolCall { lane, call_id, name, arguments }` | `TOOL_CALL_START` + `TOOL_CALL_ARGS` + `TOOL_CALL_END` |
| `Completed { lane, response_id, usage }` | `RUN_FINISHED { threadId, runId, outcome, usage }` |
| `Failed { lane, code, message }` | `RUN_ERROR { message, code, usage }` |
| `Connected` | **none** |
| `Disconnected(why)` | **none** |

`Connected` and `Disconnected` describe the adapter *process and its
transport*. AG-UI models an agent run, which sits above that, and there is no
event for "my socket came up". **Do not force them into `CUSTOM`.** The stdio
binding is ours to define (verified by the negative check: `grep -ril stdio`
over the whole `docs/` tree returns nothing, so the spec defines no stdio
transport at all) and it already carries a non-AG-UI frame, `Startup`. Keep a
small envelope:

```
Frame = Event(Event) | Transport(Connected | Disconnected(String))
```

That is honest about which half is AG-UI and which half is ours, instead of
smuggling process lifecycle through a protocol that does not model it.

### The set to implement

**Must consume, and our adapter must emit (11):**

`RUN_STARTED`, `RUN_FINISHED`, `RUN_ERROR`,
`TEXT_MESSAGE_START`, `TEXT_MESSAGE_CONTENT`, `TEXT_MESSAGE_END`,
`TOOL_CALL_START`, `TOOL_CALL_ARGS`, `TOOL_CALL_END`,
`REASONING_START`, `REASONING_END`.

`RUN_STARTED` and one of `RUN_FINISHED`/`RUN_ERROR` are **mandatory** — the
spec says so in terms (`docs/concepts/events.mdx:86`), they bound every run.

**Must consume, must not emit (4):**

`TEXT_MESSAGE_CHUNK`, `TOOL_CALL_CHUNK`, `REASONING_MESSAGE_CHUNK` — because
of correction 2 above; a conforming agent may speak only these.
`TOOL_CALL_RESULT` — because of correction 1; server-side tools.

**Consume if reasoning turns out to be visible (3):** `REASONING_MESSAGE_START`,
`REASONING_MESSAGE_CONTENT`, `REASONING_MESSAGE_END`. See §2 — whether these
ever fire against our provider is currently **UNVERIFIED**.

### What we are choosing not to implement, and what each costs

This is the list you said you would rather publish than discover.

- **`STATE_SNAPSHOT`, `STATE_DELTA`** (agent state, JSON Patch RFC 6902).
  Kobold has no shared-state model and no place to put one. **Cost:** an agent
  that keeps state the UI is meant to mirror gets no mirror. Zero for a chat
  TUI. **One catch:** the interrupt contract requires the agent to emit state
  *before* an interrupting `RUN_FINISHED`
  (`docs/concepts/interrupts.mdx`, "State at the interrupt boundary"). If we
  ever adopt interrupts we must at minimum store and forward these opaquely.

- **`MESSAGES_SNAPSHOT`.** The agent rewriting the client's history.
  **Cost: real, and worth stating.** No server-side history compaction or
  summarisation. Kobold owns its transcript and rebuilds it with `replay()`;
  adopting a snapshot would fight that ownership. Deliberate, not an oversight.

- **`ACTIVITY_SNAPSHOT`, `ACTIVITY_DELTA`.** Structured generative-UI activity.
  **Cost:** no rich status surface beyond text. Fits a TUI.

- **`STEP_STARTED`, `STEP_FINISHED`.** Sub-run progress. **Cost:** no
  step-level progress indicator. Cheap to add later; nothing depends on it.

- **`RAW`.** Provider passthrough for debugging. **Cost: near zero, and there
  is an argument for taking it** — `App::debug` already prints tool-call
  machinery, and `RAW` is the same idea. Suggest accept-and-ignore, surfaced
  only under `debug`.

- **`CUSTOM`.** The extension escape hatch. **Cost: none, but it must not
  error.** Covered by the catch-all in §3.

- **The five `THINKING_*` events.** Deprecated, removed at 1.0.0.
  **Cost:** an agent still on the old spelling shows no reasoning. Ours will
  not be. Do not implement them; `ag-ui-core` implementing *only* these is
  part of why it is the wrong starting point.

- **`REASONING_ENCRYPTED_VALUE`.** **This is the one I would not dismiss
  quietly — see §2.**

---

## 2. Reasoning

### Does Kobold surface reasoning today? No. Verified by the negative check.

`reasoning` appears in the codebase in exactly one role: an **outbound request
field**. `kobold-openai/src/events.rs:96` defines `Reasoning { effort }`;
`ResponseCreate::user_text` always sets it from `Model::effort`, which comes
from `settings.reasoning_effort` (`src/main.rs:495`). Nothing inbound.

`to_updates` (`kobold-openai/src/net.rs:157-183`) matches exactly two event
kinds — `response.output_text.delta` and `response.output_item.done` — plus
the terminal/failure test. `Update` has no reasoning variant, `App::apply` has
no reasoning arm, and nothing renders one. **Kobold asks for reasoning effort
and then discards every trace of what happened.**

### Would AG-UI let it? Yes, and the cheapest slice is the valuable one.

`REASONING_START` and `REASONING_END` carry a `messageId` and nothing else.
No content, no privacy question, no rendering decision. They say *reasoning
began* and *reasoning ended*.

**Explaining reasoning latency.** The first-update timer serves as a practical safeguard:
long reasoning delays are expected, so Kobold tracks reasoning state rather than killing
turns prematurely. `REASONING_START` removes ambiguity. While reasoning is open, silence
is *explained*; after `REASONING_END`, the tight 5s mid-stream rule applies.

### Reasoning Summaries

OpenAI's Responses API takes `reasoning: { effort, summary }`. Kobold configures reasoning
parameters to support streaming summaries where supported by the underlying model.

If summaries are active, `REASONING_MESSAGE_START/CONTENT/END` events stream reasoning
tokens progressively, while `REASONING_START`/`END` track overall phase boundaries.

**Verified by measurement: summaries stream cleanly when requested.** Max effort benchmarks
demonstrate:

**Stage 1 — accepted, and echoed back resolved.** We sent `summary: "auto"` with
`generate: false`; `response.completed` returned
`reasoning: { context: "all_turns", effort: "high", mode: "standard", summary:
"detailed" }`. **`"auto"` resolving to `"detailed"` is proof it was honoured
rather than ignored.** Billed 18 in, 0 out — a rejection would have cost nothing.

Free finding from the echo: the request also accepts `reasoning.context` and
`reasoning.mode`, neither of which Kobold sends or knew about.

**Stage 2 — summaries stream token by token, exactly like output text:**

```
response.created, response.in_progress, response.output_item.added,
response.reasoning_summary_part.added,
response.reasoning_summary_text.delta   (~60, token by token),
response.reasoning_summary_text.done,
response.reasoning_summary_part.done,
response.output_item.done, response.content_part.added,
response.output_text.delta, response.output_text.done,
response.content_part.done, response.completed
```

It is real prose, and it **arrives with markdown emphasis in it**, which `md.rs`
would render.

**So all three `REASONING_MESSAGE_*` events are implemented, not dead code**, and
the mapping is direct:

| AG-UI | provider |
|---|---|
| `REASONING_MESSAGE_START` | `response.reasoning_summary_part.added` |
| `REASONING_MESSAGE_CONTENT` | `response.reasoning_summary_text.delta` |
| `REASONING_MESSAGE_END` | `…text.done` or `…part.done` |

**One design point the events force:** `summary_index` and `output_index` are on
every one of them, so a turn can carry several summary parts. **One
`REASONING_MESSAGE` per `summary_index`, not one per turn.**

`REASONING_START`/`END` go in as before — they never depended on this question.
And **`REASONING_MESSAGE_CHUNK` is still consumed but never emitted**, which is
deliberate: the chunk recommendation was about a foreign conforming agent, whose
reference server emits only chunks. **Kobold accepting content in a form it does
not itself produce is the correct asymmetry for a client.**

**Also seen, unlooked-for: the reasoning output item carries
`encrypted_content`** — a long opaque blob, present **under `store: false`**.
That is a real provider counterpart to AG-UI's `REASONING_ENCRYPTED_VALUE`,
which had been flagged only as a spec capability. The encrypted-continuity path
is not theoretical for us; the provider emits it and Kobold discards it. Further
evidence that **retention is not the only price of continuity** — relevant to the
open `store: true` question, which is the user's.

Also settled while attempting it, and worth not rediscovering: the API key is
**not** in ambient shell environment files, deliberately — keeping it out of the
environment of every shell that happens to `cd` in. It can be provided per-process
or via a secure secret store.

### `REASONING_ENCRYPTED_VALUE`, and your open `store: true` question

The reasoning doc is explicit that encrypted reasoning values exist to
"maintain reasoning context across turns using encrypted reasoning items,
**even under `store:false`** or zero data retention policies". Kobold sends
`store: false` today (`events.rs:47`).

So AG-UI has a native answer to the same continuity problem that
`previous_response_id` solves — and it does not require flipping `store` to
`true`, which you have logged as a data-retention decision for the user rather
than a technical one. The client holds an opaque blob it cannot read and hands
it back; only the agent can decrypt it.

I am not recommending it for phase 2 — it is more machinery than the
`previous_response_id` extension check, and empirical analysis establishes that
the fallback there is always correct. **But it changes the framing of the open
question**, which currently reads as though `store: true` were the price of
continuity. It is not the only price available.

---

## 3. The exact Rust shape

### The design, and why each piece is there

```rust
//! AG-UI events, the subset Kobold speaks.
//!
//! Field names are renamed one at a time and never with `rename_all`. The
//! protocol mixes conventions -- `messageId` beside `type` beside
//! `toolCallName` -- so a blanket rule is a rule that is right until it is
//! silently wrong, and a wire-name bug does not fail loudly, it drops a field.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Open by key, per the specification: any JSON value under any key. The
/// `ag-ui` key is reserved for the protocol's own use.
///
/// NOT `sonic_rs::Value` — see the correction below. That shape serializes
/// perfectly and cannot read back a single one of the specification's payloads.
pub type Metadata = BTreeMap<String, Json>;

/// Declared on every event by the specification, and absent from
/// `ag-ui-core` entirely -- which is why we carry our own.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Base {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    #[serde(rename = "rawEvent", default, skip_serializing_if = "Option::is_none")]
    pub raw_event: Option<sonic_rs::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// What an adapter emits. Serialize only.
///
/// **Struct variants, not newtypes over standalone structs.** There is no
/// `TextMessageContentEvent` type in this module, so there is nothing that
/// can be handed to a serializer without its tag -- see the note below.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum Outgoing {
    #[serde(rename = "RUN_STARTED")]
    RunStarted {
        #[serde(flatten)] base: Base,
        #[serde(rename = "threadId")] thread_id: String,
        #[serde(rename = "runId")] run_id: String,
        #[serde(rename = "parentRunId", skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
    },
    #[serde(rename = "RUN_FINISHED")]
    RunFinished {
        #[serde(flatten)] base: Base,
        #[serde(rename = "threadId")] thread_id: String,
        #[serde(rename = "runId")] run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")] usage: Option<Vec<TokenUsage>>,
    },
    #[serde(rename = "RUN_ERROR")]
    RunError {
        #[serde(flatten)] base: Base,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")] code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")] usage: Option<Vec<TokenUsage>>,
    },
    #[serde(rename = "TEXT_MESSAGE_START")]
    TextMessageStart {
        #[serde(flatten)] base: Base,
        #[serde(rename = "messageId")] message_id: String,
        /// Defaulted to "assistant" by the spec; always written, because a
        /// field that is usually omitted is a field nobody notices is wrong.
        role: String,
    },
    #[serde(rename = "TEXT_MESSAGE_CONTENT")]
    TextMessageContent {
        #[serde(flatten)] base: Base,
        #[serde(rename = "messageId")] message_id: String,
        delta: String,
    },
    #[serde(rename = "TEXT_MESSAGE_END")]
    TextMessageEnd {
        #[serde(flatten)] base: Base,
        #[serde(rename = "messageId")] message_id: String,
    },
    #[serde(rename = "TOOL_CALL_START")]
    ToolCallStart {
        #[serde(flatten)] base: Base,
        #[serde(rename = "toolCallId")] tool_call_id: String,
        #[serde(rename = "toolCallName")] tool_call_name: String,
        #[serde(rename = "parentMessageId", skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<String>,
    },
    #[serde(rename = "TOOL_CALL_ARGS")]
    ToolCallArgs {
        #[serde(flatten)] base: Base,
        #[serde(rename = "toolCallId")] tool_call_id: String,
        delta: String,
    },
    #[serde(rename = "TOOL_CALL_END")]
    ToolCallEnd {
        #[serde(flatten)] base: Base,
        #[serde(rename = "toolCallId")] tool_call_id: String,
    },
    #[serde(rename = "REASONING_START")]
    ReasoningStart {
        #[serde(flatten)] base: Base,
        #[serde(rename = "messageId")] message_id: String,
    },
    #[serde(rename = "REASONING_END")]
    ReasoningEnd {
        #[serde(flatten)] base: Base,
        #[serde(rename = "messageId")] message_id: String,
    },
}

/// What Kobold parses. Deserialize only.
///
/// A separate type from `Outgoing` for one reason, and it is not tidiness:
/// this one needs a catch-all for the two thirds of the protocol we do not
/// model, and **a catch-all that can be serialized is a new way to put
/// non-conforming JSON on the wire.** Measured, not assumed -- see below.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum Incoming {
    // ... the same variants as `Outgoing`, plus the consume-only ones:
    // TEXT_MESSAGE_CHUNK, TOOL_CALL_CHUNK, REASONING_MESSAGE_CHUNK,
    // TOOL_CALL_RESULT, REASONING_MESSAGE_{START,CONTENT,END}.

    /// Everything in the specification we do not model, and everything the
    /// specification grows next. AG-UI is pre-1.0 and moving; without this a
    /// `STATE_DELTA` is a decode error, and `net.rs` already holds the line
    /// that an unmodelled event must never kill the session.
    #[serde(other)]
    Unmodelled,
}

/// Per-(provider, model) token counts. See section 6 -- this is **not**
/// `kobold_proto::Usage` and must not be conflated with it.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Default)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub model: Option<String>,
    #[serde(rename = "inputTokens", default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(rename = "outputTokens", default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(rename = "totalTokens", default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    #[serde(rename = "reasoningTokens", default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    #[serde(rename = "cachedInputTokens", default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u32>,
}
```

### Three fields that must tolerate an explicit `null`, and one that must not

From `sdks/fixtures/null-omission.json` and its README — **the spec's entire
cross-language fixture directory is that one file**, so this is not guessable
from the schemas alone. The schemas encode it; only the fixture says *which
three* and *why*.

The contract is that a producer **omits** a field rather than writing `null`.
Three fields still accept `null` on the receiving side, to grandfather older
producers:

- `TOOL_CALL_START.parentMessageId`
- `TOOL_CALL_CHUNK.parentMessageId`
- `RUN_FINISHED.outcome`

**`metadata` deliberately does not**, because it postdates the producer-side fix
and has nobody to grandfather. So our `Option<Metadata>` with `serde(default)`
accepting an explicit `"metadata": null` is a deviation from a rule the project
chose **on purpose**, not from an accident of schema generation. More tolerant
than the spec is still the safe direction and not worth a custom deserializer —
but comment it as a known deviation rather than leaving it to be discovered.

### CORRECTION: `sonic_rs::Value` cannot be read behind a tag or a flatten

**This section originally typed every opaque field as `Option<sonic_rs::Value>`
and recorded that tag, flatten and `other` compose under sonic-rs. The second
claim is true. It does not cover the first.**

`sonic_rs::Value` deserializes by asking its deserializer for raw JSON through a
private newtype-struct hint. Both `#[serde(tag)]` and `#[serde(flatten)]` route
input through serde's buffered `Content` first, **which does not speak that
hint**. Every event carrying one fails with `invalid type: newtype struct,
expected a valid json`. `OwnedLazyValue` fails identically.

**Serializing works, and that is what makes it a trap rather than a plain bug.**
A type built to the shape this document specified encodes perfectly, passes any
producer-side test, and cannot read back a single payload the specification
publishes. **Same shape as `ag-ui-core`, one layer along: compiling is not
conforming — and neither is round-tripping through yourself.**

Isolated with a five-case probe before anything was changed: a plain struct
field works; flattened map fails; tagged enum fails; `OwnedLazyValue` fails;
tag-plus-flatten with *ordinary* types works. So the original composition claim
was correct as written — **the gap is that the opaque half of the protocol lives
entirely inside the one case it did not cover.** Seeing it would have required
putting a `Value` behind a tag.

**Fix: a small untagged `Json` enum** that goes through serde's generic data
model only — `Null`, `Bool`, `Int`, `Float`, `Str`, `Array`, `Object`. **`Int`
before `Float`**, so a whole number stays whole. **`Null` is a value rather than
an absence**, because a null inside an opaque payload is data even where a null
in a declared field means omitted.

Its own test decodes it **through a tagged event on purpose** — a bare `Json`
never touches `Content` and would pass for a type with the original defect still
in it.

### 4. Making the missing-tag trap unrepresentable

**The trap, reproduced rather than quoted.** `ag-ui-core` puts
`#[serde(tag = "type")]` on the enum and the fields in standalone structs, so:

```
direct       -> {"messageId":"3f8a1c2e-...","delta":"hi"}       has `type`? false
via enum     -> {"type":"TEXT_MESSAGE_CONTENT","messageId":...}  has `type`? true
```

**The fix is that there is no standalone struct.** With struct variants
declared inline, `TextMessageContentEvent` does not exist as a nameable type,
so there is nothing to hand a serializer. It is not a rule to remember or a
lint to add — the shape that produces the bug cannot be written.

**A trap I introduced and then found by probing.** `#[serde(other)]` gives a
unit variant, and a unit variant in an internally-tagged enum serializes
happily:

```
Event::Unmodelled -> {"type":"Unmodelled"}      (serde_json AND sonic-rs)
```

`"Unmodelled"` is in no version of the protocol. A single enum deriving both
traits would therefore re-open exactly the class of hole we just closed, one
layer along. **Hence the `Outgoing` / `Incoming` split**: the direction that
needs a catch-all cannot serialize, and the direction that serializes has no
catch-all. It also matches the traffic — Kobold only ever parses, the adapter
only ever writes.

### What I verified, on this Linux box

Probe at `$SCRATCH/agui-shape`, run against **both** `serde_json` and
**`sonic-rs` 0.5**, which is what `kobold-proto::codec` actually uses:

- The tag is present on every serialization, under both backends, byte-identical.
- Full round-trip equality through sonic-rs, `metadata` included.
- **`#[serde(tag)]` + `#[serde(flatten)]` + `#[serde(other)]` all compose
  correctly under sonic-rs.** This was the real risk and it is settled:
  `flatten` forces serde's buffered map path, and sonic-rs's serde
  compatibility layer handles it.
- Real OpenAI-shaped ids parse — `runId: "resp_68a1"`, `messageId:
  "msg_abc123"` — which is the defect that disqualified `ag-ui-core`.
- `STATE_DELTA` and a fabricated `SOMETHING_FROM_0_0_99` both decode to
  `Unmodelled` instead of erroring, under both backends.
- **The codec's framing invariant holds.** A delta containing a raw newline,
  a tab and quotes serializes to a single line with everything escaped, which
  is the one property `codec.rs` rests on.

### Two things to know before this is built

**Internally-tagged enums defeat zero-copy borrowing, and that is fine
*here*.** `kobold-openai/src/events.rs` opens with a note explaining that
incoming events are a flat struct rather than a tagged enum precisely because
`#[serde(tag)]` buffers through `Content`. That reasoning is correct and it
belongs to the *provider* seam, where a WebSocket read buffer can be borrowed
from. It does not transfer to the adapter↔Kobold seam: `Update` is already an
owned-`String` externally-tagged enum, so there is no zero-copy to lose, and
benchmarks settle the rest — 0.35µs per delta against a
4-48ms inter-delta gap, pipe-dominated. **Someone will raise this objection;
it is answered, and the answer should be in the source.**

**One conformance deviation, named rather than discovered.** The spec
*rejects* an explicit `"metadata": null` (`metadata.ts` is emphatic: metadata
postdates the producer-side omission fix, so there is nobody to grandfather).
`Option<Metadata>` with `#[serde(default)]` **accepts it as absent** —
verified. Being more tolerant than the spec is the safe direction and I would
not spend a custom deserializer on it, but it should be a comment, not a
surprise.

---

## 5. Tool results, and the parked-question flow

### The run ends at a tool call. This is the shape change, and it is bigger than a rename.

Verified from the spec's own reference implementation
(`docs/quickstart/server.mdx:417-479`): the server streams the completion,
emits the tool-call events, and then emits `RUN_FINISHED`
**unconditionally**, whether or not a tool was called. It cannot do otherwise
— the tool is the *client's* to run, so the agent has nothing left to say.

So under AG-UI **one Kobold turn is N runs**, and the boundary falls at every
tool call.

### How a result becomes a message, concretely

1. Run 1: `RUN_STARTED` → text → `TOOL_CALL_START/ARGS/END` → `RUN_FINISHED`.
2. Kobold runs the tool. `tools::run` → `tools::decide` — unchanged, and
   still pure.
3. Kobold appends to its message array:
   ```json
   { "id": "<fresh>", "role": "tool",
     "content": "<the output>", "toolCallId": "<the id from TOOL_CALL_START>" }
   ```
4. Kobold starts run 2 on the **same `threadId`**, with that array.

### `Outcome::Refused` finally gets a home, and it is a real gain

`ToolMessage` has an optional `error` field, and the spec is direct about why:
"`error` is how the protocol expresses a client-side tool failure. Without it,
a tool that failed is indistinguishable from one that succeeded."

Today `tools::decide` collapses `Done(text)` and `Refused(text)` into the same
`Dispatch::Reply(text)` (`src/tools.rs:326`) — an unknown tool, bad arguments
and a path outside the cone all reach the model as ordinary output, and it has
to infer failure from prose. **`Refused` maps to `error`, `Done` does not.**
Cheap, conforming, and the model gets information it currently has to guess at.

### The parked question: two mappings, and they are not alternatives

**(a) Client-side tool — the direct translation, and what I recommend for our
adapter.** `ask` is a Kobold tool like any other. Run 1 ends at
`TOOL_CALL_END`; Kobold shows the panel; the answer becomes a `ToolMessage`;
run 2 starts. `park_ask`, `Panel`, `resolve_pending_ask` and `record_answer`
are untouched.

**And the run being closed while the user thinks is a strict improvement over
direct long-lived sockets.** Previously, long waits on pending user questions
risked hitting server idle timeout caps. Under AG-UI, runs complete cleanly at
tool boundaries so user thinking time costs zero resources.

**(b) Interrupts — what a *foreign* agent will use to ask Kobold something.**
`RUN_FINISHED { outcome: { type: "interrupt", interrupts: [...] } }`, answered
by `resume: [{ interruptId, status, payload }]` on the next input. Richer than
(a): `reason` (with `input_required` and `confirmation` in the core taxonomy),
`responseSchema`, `expiresAt`.

These sit at different layers — (a) is how *our* tool asks, (b) is how *an
agent* pauses — so "which one" is the wrong question. Suggest (a) now, (b)
deferred and explicitly listed as not-implemented. Worth noting that Kobold is
closer to (b) than it looks: `interrupts` is an array and rule 3 requires a
resume to address every open one, which is precisely the behaviour `3caa4aa`
already built — several questions in one turn share one panel and resolve
individually. If (b) is ever picked up, that groundwork is done.

### A live bug this uncovered, reproduced before reporting

Tracing the run boundary turned up something that is already wrong on
`kobold-main`, before any AG-UI work.

**`park_ask` never touches `pane.status`.** So when the response that made the
`ask` call completes — and it does, `response.completed` is terminal
(`net.rs:191`) — `App::apply` sets `status = Status::Ready`
(`app.rs:1820`) with the question still on screen. `drain_queues` filters on
exactly `status == Status::Ready && !queue.is_empty()` (`main.rs:840-846`),
so it dispatches.

Reachable path: the user sends a turn, types a second message while it runs
(`main.rs:1063` queues it because the pane is `Waiting`), the model calls
`ask`, and the queued message is dispatched as a fresh turn onto a lane whose
tool call is still unanswered. The user cannot type *into* a queue while the
panel is open — the panel takes every key — but they can have queued before
the question appeared.

**Reproduced, not inferred.** A throwaway test in `app.rs` asserting exactly
this passed on the first run: after `park_ask` then `apply(Completed)`, the
pane is `Ready`, the panel is open, and the queue is non-empty. Reverted; the
worktree is clean.

**Under AG-UI this stops being an edge case**, because every tool call ends a
run and therefore every tool call passes through this state. The fix is a
per-lane parked state that survives the run boundary. **Flagging rather than
fixing — I do not commit, and the product question of what *should* happen to
a queued message is yours.**

---

## 6. `Usage` will not survive contact with AG-UI. Four breaks, and one is silent.

`kobold_proto::Usage` is `{ input_tokens, output_tokens, total_tokens }`, all
`u32`, all `#[serde(default)]`. AG-UI's `TokenUsage` differs on four
independent axes.

**1. The names differ, and the failure is silent.** `inputTokens` against
`input_tokens`. Because all three fields are `#[serde(default)]`, an AG-UI
usage object does not fail to parse — it produces zeros. Verified:

```
{"inputTokens":41000,"outputTokens":49907,"totalTokens":90907,"reasoningTokens":50000}
  parsed as kobold_proto::Usage -> Usage { input_tokens: 0, output_tokens: 0, total_tokens: 0 }
```

**The type's own doc comment says usage is `Option` rather than defaulted
specifically so that a turn without a report does not "read as this turn was
free" — and this is that exact failure, arriving through the field names
instead.** The context gauge would show a 90,907-token turn as zero.

**2. Cardinality.** `RUN_FINISHED.usage` is an **array**, one entry per
(provider, model), so a run that touched two models keeps them separate.
`Option<Usage>` → `Option<Vec<TokenUsage>>`. `App::apply` currently does
`pane.context = Some(u.total_tokens)` from a single value; it must fold.

**3. Optionality.** Every AG-UI count is optional, so "not reported" and
"zero" are distinguishable there and not here. Folding an array whose
`totalTokens` entries are absent needs a stated rule — sum `input + output`,
or report nothing. Nothing is the safer default, given break 1.

**4. Two counts we do not have.** `reasoningTokens` and `cachedInputTokens`.
`reasoningTokens` is exactly the number that would make extended reasoning
turns legible: output tokens billed with no interim wire traffic. Tracking it
explains token consumption during long reasoning cycles.

### Recommendation

**Split them, and do not rename `kobold_proto::Usage` into conformance.**

- `TokenUsage` is the wire type — camelCase, all-optional, in the AG-UI
  module, an array on the events that carry it.
- `Usage` stays Kobold's internal gauge type and keeps its current shape and
  its doc comment, which is correct about its own concern.
- **There are TWO folds and an earlier draft of this section conflated them**,
  which put the same work in two different crates depending on which paragraph
  you read. Caught before anything was built:
  - **Provider-nested → `TokenUsage` is the adapter's**, and it is where
    `output_tokens_details.reasoning_tokens` and `input_tokens_details.
    cached_tokens` get read instead of dropped.
  - **`Vec<TokenUsage>` → `Usage` is Kobold's.** It cannot be the adapter's:
    §3's `Outgoing::RunFinished` emits `usage: Option<Vec<TokenUsage>>`, so the
    adapter *writes the array out* and never folds it. The closing paragraph
    below already implied this — `Usage` stops being on the seam only if Kobold
    is the one folding it.

One consequence to accept deliberately: `Usage` then stops being "on the
seam". The comment at `kobold-proto/src/lib.rs:120` explaining that it lives
there because both sides need it becomes wrong, and `events.rs:196`'s note
that "its field names happening to match OpenAI's is luck, not design; under
AG-UI they may not" is confirmed — they do not. Both comments need to change
with the behaviour.

---

## Summary: how much AG-UI Kobold would actually speak

**15 of 33 event types** — 11 emitted and consumed, 4 consumed only.

**Not implemented: 15.** Five deprecated `THINKING_*`; state and messages
snapshots (4); activity (2); steps (2); `RAW` and `CUSTOM` (accepted, ignored);
`REASONING_ENCRYPTED_VALUE`.

**The three `REASONING_MESSAGE_*` events ARE implemented** — §2's probe settled
it by measurement. Summaries stream token by token as real prose.

**The null contract is narrower than it reads.** Walking `null-omission.json`
independently: the nulls that survive input→expected are `STATE_SNAPSHOT.
snapshot`, `RAW.event`, `STATE_DELTA` patch values, and values inside opaque
payloads — **every one belonging to an event we do not model.** So for our
fifteen it is discharged by the `Unmodelled` catch-all, and the three
grandfathered fields above are the entire real surface. **Do not write
null-handling into every variant**; that is defensive noise wearing the costume
of rigour.

**And §5 clears the wrong layer.** `park_ask`, `Panel`, `resolve_pending_ask`
and `record_answer` are genuinely untouched — they key off `lane` and `call_id`,
neither derived from run identity, so a run boundary cannot invalidate them.
**But `resolve_ask` (`main.rs:766`) is touched and §5 does not say so:** it
sends a `ToolResult` into a live turn, and under AG-UI **there is no live turn**
— it must open run N+1 with a `ToolMessage`. That is the real work, one level
above the four functions this document clears.

**Plus two frames that are not AG-UI at all** — `Startup`, and the transport
envelope carrying `Connected`/`Disconnected` — because the spec defines no
stdio binding and does not model an adapter process.

**The honest one-line version: Kobold would be a conforming AG-UI *consumer*
of chat, tool-calling and reasoning-boundary events, and would ignore the
protocol's state-synchronisation, generative-UI and history-management halves
entirely.**
