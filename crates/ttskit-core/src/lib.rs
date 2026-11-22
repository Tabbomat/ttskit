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

pub mod check;
pub mod fetch_assets;
pub mod pack;
pub mod save;
pub mod unpack;

pub use check::{
    CheckConfig, CheckReport, Fetch, FetchError, FetchMethod, FetchRequest, FetchResponse, Outcome,
    UrlResult, check_urls,
};
pub use fetch_assets::{
    Download, DownloadError, DownloadResponse, Failed, FetchConfig, FetchPlan, FetchReport,
    Fetched, Index, IndexEntry, Skipped, Store, StoreError, detect_type, fetch_assets,
    fetch_assets_with_plan, plan as plan_fetch,
};
pub use pack::{PackError, PackOptions, Packed, pack};
pub use save::write::Eol;
pub use save::{
    AssetUrl, HostClass, ObjectView, ObjectViewMut, Occurrence, Save, SaveError, SaveView,
    SaveViewMut,
};
pub use unpack::{
    OutputFile, UnpackError, UnpackReport, Unpacked, WriteOptions, WritePlan, unpack,
};
