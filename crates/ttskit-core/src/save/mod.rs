//! Lossless Tabletop Simulator save model.
//!
//! A [`Save`] wraps a [`serde_json::Value`] and writes it back in the exact
//! format Tabletop Simulator itself writes (see [`mod@write`]), so a save written
//! by the game round-trips byte for byte.
//!
//! # Why the model is value-backed
//!
//! - The official save-file-format page is a class listing without versioning
//!   or stability guarantees, so a fixed set of structs cannot be trusted to
//!   cover every save.
//! - Its own example contains fields that are not in its class listing.
//! - `#[serde(flatten)]`, the usual way to keep unknown fields in typed
//!   structs, conflicts with serde_json's `arbitrary_precision` feature, which
//!   is needed to keep number text intact.
//!
//! Holding the raw value means unknown fields pass through untouched, key order
//! is preserved (`preserve_order`) and numbers keep their original text
//! (`arbitrary_precision`), for example `-0.0`, `1.0` or integers that do not
//! fit into 64 bits. Typed views over the value are layered on top separately.
//!
//! # Round-trip guarantees and limits
//!
//! - A byte-exact round trip is guaranteed for saves written by Tabletop
//!   Simulator and for anything ttskit itself wrote.
//! - Duplicate keys in an object collapse; the last one wins.
//! - Input written by third-party tools (LF newlines, a trailing newline,
//!   `\u003c`-style or ASCII-only escaping, compact JSON) is not reproduced
//!   byte-exactly. It is written in the Tabletop Simulator format and
//!   represents an identical object: same keys in the same order, same values,
//!   same number text.
//! - Number digits are kept verbatim, but serde_json normalizes the exponent
//!   while parsing: `E`/`e` becomes `e` and a missing sign becomes `+`. The
//!   writer emits `E` with a sign (`1E-05`, `1E+20`) like .NET, so numbers
//!   written by Tabletop Simulator are unchanged.
//! - A leading UTF-8 byte order mark on input is accepted and dropped.

pub mod assets;
pub mod view;
pub mod write;

use std::io::Write;
use std::str::FromStr;

use serde_json::{Map, Value};

pub use assets::{AssetUrl, HostClass};
pub use view::{ObjectView, ObjectViewMut, SaveView, SaveViewMut};

/// Errors produced while reading or writing a save.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The input is not valid JSON.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The top-level JSON value is not an object.
    #[error("save must be a JSON object at the top level")]
    NotAnObject,
    /// Writing failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// A Tabletop Simulator save, held losslessly as a JSON object.
#[derive(Debug, Clone, PartialEq)]
pub struct Save {
    value: Value,
}

impl Save {
    /// Parses a save from bytes. A leading UTF-8 BOM is accepted and dropped.
    pub fn from_slice(bytes: &[u8]) -> Result<Save, SaveError> {
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        Save::from_value(serde_json::from_slice(bytes)?)
    }

    /// Wraps an already parsed value; it must be a JSON object.
    pub fn from_value(value: Value) -> Result<Save, SaveError> {
        if value.is_object() {
            Ok(Save { value })
        } else {
            Err(SaveError::NotAnObject)
        }
    }

    /// Serializes the save in Tabletop Simulator format.
    pub fn to_vec(&self) -> Vec<u8> {
        write::to_tts_vec(&self.value)
    }

    /// Writes the save in Tabletop Simulator format to `out`.
    pub fn write_to<W: Write>(&self, out: W) -> Result<(), SaveError> {
        write::write_value(&self.value, out)?;
        Ok(())
    }

    /// The underlying JSON value (always an object).
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// The top-level JSON object.
    pub fn map(&self) -> &Map<String, Value> {
        match &self.value {
            Value::Object(map) => map,
            // `value` is private, set to an object by `from_value`, and
            // `map_mut` cannot change its variant.
            _ => unreachable!("Save invariant: top level is an object"),
        }
    }

    /// Mutable access to the top-level JSON object.
    ///
    /// Returns the map rather than the value so the top level cannot be
    /// replaced by a non-object.
    pub fn map_mut(&mut self) -> &mut Map<String, Value> {
        match &mut self.value {
            Value::Object(map) => map,
            _ => unreachable!("Save invariant: top level is an object"),
        }
    }

    /// A read-only typed view of the save.
    pub fn view(&self) -> SaveView<'_> {
        SaveView::new(self.map())
    }

    /// A mutable typed view of the save.
    pub fn view_mut(&mut self) -> SaveViewMut<'_> {
        SaveViewMut::new(self.map_mut())
    }

    /// Consumes the save and returns the underlying value.
    pub fn into_value(self) -> Value {
        self.value
    }
}

impl FromStr for Save {
    type Err = SaveError;

    fn from_str(s: &str) -> Result<Save, SaveError> {
        Save::from_slice(s.as_bytes())
    }
}

impl TryFrom<Value> for Save {
    type Error = SaveError;

    fn try_from(value: Value) -> Result<Save, SaveError> {
        Save::from_value(value)
    }
}
