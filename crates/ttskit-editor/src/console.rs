//! The game's console output: `print` text and Lua errors.
//!
//! The game sends no timestamp, and a `print` carries no information about
//! which object produced it (observed for `print`, `printToAll`,
//! `broadcastToAll` and `log`: only `{message}`). A Lua error carries the GUID
//! of the object whose script failed.

use crate::exec::LuaError;
use crate::message::Incoming;

/// One console line from the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleEvent {
    Print { message: String },
    Error(LuaError),
}

impl ConsoleEvent {
    /// The console event in `message`, or `None` for every other message kind
    /// (scripts, saves, custom messages, ...).
    pub fn from_incoming(message: Incoming) -> Option<Self> {
        match message {
            Incoming::Print { message } => Some(Self::Print { message }),
            Incoming::Error {
                error,
                guid,
                error_message_prefix,
            } => Some(Self::Error(LuaError {
                guid,
                error,
                prefix: error_message_prefix,
            })),
            _ => None,
        }
    }
}
