//! Client for the Tabletop Simulator External Editor API.
//!
//! Unofficial; not affiliated with Berserk Games. See `docs/editor-api.md`
//! for the observed wire behaviour.

mod message;

pub use message::{CodecError, Incoming, Outgoing, ScriptState};
