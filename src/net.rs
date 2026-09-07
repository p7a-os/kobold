//! Where the rest of Kobold reaches for the protocol types.
//!
//! Kobold does no networking any more -- a provider adapter does, in its own
//! process -- so the name survives only because every call site in Kobold
//! uses it and renaming them is churn without a reader benefit. What is left
//! is the vocabulary Kobold and an adapter share, which lives in
//! `kobold-proto` because neither side may own it.

pub use kobold_proto::agui::{self, Incoming};
pub use kobold_proto::{Command, IncomingFrame, Model, Startup, Transport, Usage};
