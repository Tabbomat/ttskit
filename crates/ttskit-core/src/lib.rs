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

pub mod asset_ref;
pub mod check;
pub mod fetch_assets;
pub mod pack;
pub mod save;
pub mod unpack;

pub use asset_ref::{AssetEntry, AssetMap, PLACEHOLDER_PREFIX, placeholder};
pub use check::{
    CheckConfig, CheckReport, Fetch, FetchError, FetchMethod, FetchRequest, FetchResponse, Outcome,
    UrlResult, check_urls,
};
pub use fetch_assets::{
    CacheDir, CacheError, Download, DownloadError, DownloadResponse, Failed, FetchConfig,
    FetchPlan, FetchReport, Fetched, Index, IndexEntry, Size, Skipped, Source, Store, StoreError,
    cache_key, detect_type, fetch_assets, fetch_assets_with_plan, plan as plan_fetch, probe_sizes,
};
pub use pack::{PackError, PackOptions, Packed, ResolvedAsset, pack};
pub use save::write::Eol;
pub use save::{
    AssetUrl, HostClass, MapProblem, ObjectView, ObjectViewMut, Occurrence, RewriteOptions,
    RewriteReport, RuleError, RuleReport, Save, SaveError, SaveView, SaveViewMut, Touched, UrlRule,
    UrlRules, parse_map_tsv,
};
pub use unpack::{
    OutputFile, UnpackError, UnpackReport, Unpacked, WriteOptions, WritePlan, unpack,
    unpack_with_assets,
};
