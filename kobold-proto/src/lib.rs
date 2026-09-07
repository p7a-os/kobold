//! What Kobold and a provider adapter say to each other.
//!
//! Kobold owns the conversation, the terminal and the tools; an adapter owns
//! one provider's wire format. These are the types that cross between them,
//! and they are in their own crate because neither side may depend on the
//! other -- an adapter must not pull in the TUI, and Kobold must not pull in
//! a provider.
//!
//! Everything here is on the wire, so a change to it is a protocol change
//! rather than a refactor.

pub mod agui;
pub mod codec;
pub mod northbound;

use serde::{Deserialize, Serialize};

/// One line on the adapter -> Kobold pipe, as an adapter writes it.
///
/// **Serialize only, and `IncomingFrame` is its counterpart.** The AG-UI
/// event types split by direction because the reading side needs a catch-all
/// and a catch-all that can be serialized is a new way to put non-conforming
/// JSON on the wire. That split reaches the envelope: a frame that could both
/// write and read would reopen the hole one layer up.
///
/// Two halves, kept apart on purpose. `Event` is about a turn and is what a
/// provider protocol describes; `Transport` is about the adapter's own
/// process and connection, which no provider protocol models -- AG-UI has no
/// event for "my socket came up", and the stdio binding is ours to define.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub enum OutgoingFrame {
    Event {
        /// **Which pane the event belongs to, and it is ours rather than
        /// AG-UI's.**
        ///
        /// Only `RUN_STARTED` and `RUN_FINISHED` carry a `threadId`; every
        /// text, tool and reasoning event carries none, and `RUN_ERROR`
        /// carries neither a thread nor a message. AG-UI assumes one event
        /// stream per run. Kobold multiplexes lanes -- splits, forks, several
        /// turns in flight -- over one adapter process, and nothing in the
        /// protocol was meant to carry that.
        ///
        /// So it goes in the envelope, which §1 of `docs/agui.md` establishes
        /// is ours to define and which already carries two non-AG-UI frames.
        /// The same argument put `Connected` and `Disconnected` in
        /// `Transport` rather than smuggling them through `CUSTOM`:
        /// **multiplexing is a property of our transport, not of any agent
        /// run.** A foreign agent that speaks one run per connection simply
        /// always reports the same lane, so this forecloses nothing.
        lane: String,
        event: agui::Outgoing,
    },
    Transport(Transport),
}

/// The same line as Kobold reads it. **Deserialize only** -- see
/// `OutgoingFrame`.
///
/// **The size difference between the variants is deliberate, not an
/// oversight.** `Event` is around 240 bytes against `Transport`'s handful,
/// and clippy's suggestion is to box the large one -- but `Event` is the
/// overwhelmingly common frame and `Transport` arrives twice in a session,
/// so boxing would add an allocation per delta to shrink a variant almost
/// nothing is. The channel that carries these is bounded at 256, which is
/// about 60 KB either way.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum IncomingFrame {
    Event { lane: String, event: agui::Incoming },
    Transport(Transport),
}

/// The adapter's connection, which belongs to no turn.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Transport {
    Connected,
    Disconnected(String),
}

/// Kobold -> adapter.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Command {
    Send {
        lane: String,
        text: String,
        previous_response_id: Option<String>,
        /// Prior turns as (role, text). Sent only when there is no
        /// `previous_response_id`: after a rewind or fork the chain has to be
        /// rebuilt, and with `store: false` the old response id is very likely
        /// gone from the connection-local cache.
        replay: Vec<(String, String)>,
    },
    /// What a tool produced, on its way back to the model.
    ToolResult {
        lane: String,
        call_id: String,
        output: String,
        /// Whether the call failed rather than answered.
        ///
        /// AG-UI's `ToolMessage` carries this, and the specification is
        /// direct that without it a tool that failed is indistinguishable
        /// from one that succeeded. It crosses the seam because only Kobold
        /// knows -- it is the one that ran the tool -- and only an adapter
        /// knows whether its provider has anywhere to put it.
        #[serde(default)]
        error: bool,
    },
    /// Cancel an in-flight turn on the specified lane.
    Cancel {
        lane: String,
    },
    Quit,
}

/// Everything an adapter is told once, before any `Command`, and never
/// again.
///
/// Deliberately not a `Command` variant. As a variant it could arrive at any
/// time, which would mean specifying what a mid-stream reconfiguration does
/// to turns already in flight -- a state machine nobody wants and nothing
/// needs. As a distinct frame, "exactly once, before anything else" is the
/// shape rather than a rule someone has to remember.
///
/// **When something eventually does need to change, respawn the adapter
/// rather than adding a `Configure`.** Spawn measures at 301us, so a restart
/// is free, and it keeps the invariant at "one adapter, one configuration,
/// for its whole life". Nothing changes model or effort mid-session today --
/// both are set once from config at startup, and the slash commands are
/// `/voice`, `/quit` and `/help`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Startup {
    /// On stdin rather than in the environment, from the start rather than
    /// retrofitted: an environment is readable from `/proc/PID/environ` by
    /// the same user, is inherited by any grandchild the adapter spawns, and
    /// lands in crash dumps. Same class as the MCP key leak.
    pub api_key: String,
    pub model: Model,
    /// Where to reach Kobold's egress broker, as a path **inside the
    /// sandbox**, or `None` for "connect directly".
    ///
    /// It travels with the credential rather than in the environment for the
    /// same reason the credential does, and because it must arrive before the
    /// first connection attempt -- an adapter that dialled once directly and
    /// learned about the broker afterwards would already have leaked the fact
    /// that it was running.
    ///
    /// **Kobold decides this, and the allowlist behind it, and the adapter
    /// cannot influence either.** All the adapter is told is where to ask.
    #[serde(default)]
    pub egress: Option<String>,
}

/// What the adapter should ask its provider for, and what it may offer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Model {
    pub name: String,
    pub effort: String,
    /// Tools the provider runs itself, by type, e.g. `web_search`. Named
    /// rather than described, because we neither implement them nor see the
    /// calls.
    pub server_tools: Vec<String>,
    /// Tools Kobold runs, as (name, description, JSON schema).
    ///
    /// One list, not two. Kobold knows which of these is a local tool and
    /// which came from an MCP server; the adapter does not need to, because
    /// to the provider they are the same thing and every call comes back the
    /// same way. **MCP stays entirely in Kobold** -- client, transports and
    /// dispatch -- and the adapter only describes these to the provider and
    /// reports the calls back.
    pub tools: Vec<(String, String, String)>,
}

/// What a turn cost, as Kobold's context gauge holds it.
///
/// **No longer on the seam, and the comment that said it was is why this one
/// spells out where it now sits.** An adapter emits `agui::TokenUsage` -- an
/// array, camelCase, every count optional, with two counts this has no field
/// for -- so nothing of this type crosses the pipe any more. `kobold-openai`
/// has its own `Usage` for its provider's nested shape and never touches
/// this one.
///
/// Two folds, and only one of them is the adapter's. Provider-nested into
/// `TokenUsage` is the adapter's, because only it knows that
/// `reasoning_tokens` hides under `output_tokens_details`. `Vec<TokenUsage>`
/// into this is Kobold's, because an adapter *emits* the array and so cannot
/// be the thing that folds it -- see `agui::fold_usage`.
///
/// It stays in this crate for now only because `fold_usage` returns it and
/// lives beside the wire type it folds from. Moving both into `kobold` is a
/// tidy nobody is blocked on.
///
/// `input_tokens` counts the whole conversation the server was given, not just
/// the newest message, so the totals here describe a turn rather than
/// accumulating across them: adding them up would count the same history once
/// per turn.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    /// The two above, summed by the server. Carried rather than recomputed so
    /// a future field that counts toward it is not silently dropped.
    #[serde(default)]
    pub total_tokens: u32,
}
