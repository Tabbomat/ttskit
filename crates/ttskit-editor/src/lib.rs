//! Client for the Tabletop Simulator External Editor API.
//!
//! Unofficial; not affiliated with Berserk Games. See `docs/editor-api.md`
//! for the observed wire behaviour.

pub mod fake;
mod message;
mod scripts;
mod transport;

pub use message::{CodecError, Incoming, Outgoing, ScriptState};
pub use scripts::{Desired, FieldChange, ObjectChange, PlanError, ScriptPlan, fetch_scripts, plan};
pub use transport::{
    DEFAULT_TIMEOUT, EDITOR_PORT, EditorError, GAME_PORT, Listener, MAX_MESSAGE_BYTES, send,
};
