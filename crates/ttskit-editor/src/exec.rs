//! Executing Lua in the game and collecting what comes back.
//!
//! The game links no request ID to its answers, so everything that arrives
//! after the request is attributed to it: a return value (ID 5), `print`
//! output (ID 2) and Lua errors (ID 3). Observed behaviour on a real game: a
//! `return` statement did not produce an ID 5 message, so [`execute_lua`] does
//! not depend on one; it stops at a return value or error, otherwise after a
//! quiet period.

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::Value;
use tokio::time::{Instant, timeout_at};

use crate::message::{Incoming, Outgoing};
use crate::transport::{EditorError, MessageSource, send};

/// The GUID of the Global script.
pub const GLOBAL_GUID: &str = "-1";

/// A Lua error reported by the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaError {
    pub guid: String,
    pub error: String,
    pub prefix: String,
}

/// Everything the game reported for one execution.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExecOutcome {
    /// The value from an ID 5 message, if the game sent one.
    pub return_value: Option<Value>,
    /// `print` output, in arrival order. The game's own prints are included.
    pub prints: Vec<String>,
    pub errors: Vec<LuaError>,
}

/// Run `script` in the context of `guid` ([`GLOBAL_GUID`] for Global).
///
/// `limit` bounds connecting and sending, and the total time spent collecting
/// replies. Collecting also ends after `quiet` without any message, and
/// immediately on a return value or an error. Running into `limit` while
/// messages keep arriving is not an error; the outcome so far is returned.
pub async fn execute_lua(
    listener: &impl MessageSource,
    game: SocketAddr,
    guid: &str,
    script: &str,
    limit: Duration,
    quiet: Duration,
) -> Result<ExecOutcome, EditorError> {
    let request = Outgoing::ExecuteLua {
        guid: guid.to_owned(),
        script: script.to_owned(),
    };
    send(game, &request, limit).await?;
    let mut outcome = ExecOutcome::default();
    let deadline = Instant::now() + limit;
    loop {
        let until = (Instant::now() + quiet).min(deadline);
        match timeout_at(until, listener.next_message(limit)).await {
            Err(_) => return Ok(outcome),
            Ok(Ok(Incoming::Return { return_value })) => {
                outcome.return_value = Some(return_value);
                return Ok(outcome);
            }
            Ok(Ok(Incoming::Print { message })) => outcome.prints.push(message),
            Ok(Ok(Incoming::Error {
                error,
                guid,
                error_message_prefix,
            })) => {
                outcome.errors.push(LuaError {
                    guid,
                    error,
                    prefix: error_message_prefix,
                });
                return Ok(outcome);
            }
            // Unrelated or broken messages must not end the wait.
            Ok(Ok(_)) | Ok(Err(EditorError::Codec(_))) => {}
            Ok(Err(e)) => return Err(e),
        }
    }
}

/// Send a custom message to the game's `onExternalMessage` handler.
///
/// The game silently drops a message that is not a JSON object, so anything
/// else is refused here instead of being sent into the void.
pub async fn send_custom(
    game: SocketAddr,
    message: Value,
    limit: Duration,
) -> Result<(), EditorError> {
    if !message.is_object() {
        return Err(EditorError::CustomNotObject);
    }
    send(
        game,
        &Outgoing::Custom {
            custom_message: message,
        },
        limit,
    )
    .await
}
