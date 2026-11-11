//! Shared building blocks for ttskit.
//!
//! Save file model, asset URL walking and other code that does not depend on
//! Steam or a running Tabletop Simulator instance lives here.
//!
//! # Save model
//!
//! The [`save`] module holds a save as a [`serde_json::Value`] (see [`Save`])
//! and writes it back in exactly the format Tabletop Simulator itself writes.
//! See the module documentation for why the model is value-backed and which
//! round-trip guarantees apply.

pub mod save;

pub use save::{Save, SaveError};
