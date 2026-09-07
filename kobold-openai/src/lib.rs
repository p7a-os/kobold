//! The OpenAI Responses adapter.
//!
//! Everything here is specific to one provider: the wire types it speaks, the
//! WebSocket it speaks them over, and the task that converts between those
//! and the provider-neutral messages in `kobold-proto`.
//!
//! Kobold links this as a library today and will spawn it as a sub-process.
//! Nothing in here may reach back into Kobold -- that is the whole point of
//! the split, and the compiler enforces it now that there is no dependency
//! in that direction.

pub mod events;
pub mod json;
pub mod net;
pub mod ws;
