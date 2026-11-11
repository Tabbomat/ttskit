//! Steam Cloud access for ttskit, an unofficial toolkit for Tabletop Simulator
//! mod development (not affiliated with Berserk Games).
//!
//! Everything that reads or changes the user's cloud goes through the
//! [`CloudBackend`] trait. The real backend talks to Steam Remote Storage and
//! is only built with the `steam` feature, so builds without it never need the
//! Steamworks SDK or `steam_api64.dll`. With the feature, `SteamCloud` is that
//! backend. Tests use [`MemoryCloud`], an in-memory backend with failure
//! injection and a call log, and never need Steam.

mod backend;
mod backup;
mod index;
mod memory;
#[cfg(feature = "steam")]
mod steam;

pub use backend::{CloudBackend, CloudError, CloudFile, Op, Quota, UgcHandle};
pub use backup::{
    Backup, BackupError, BackupFile, BackupStore, RestoreAction, RestoreOutcome, RestorePlan,
    RestoreStep, SaveOutcome, plan_restore, restore,
};
pub use index::{
    CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, CloudIndex, GAME_INTERNAL_FILES, IndexEntry, IndexError,
    UgcUrl, is_game_internal, normalize_folder,
};
pub use memory::{Call, MemoryCloud};
#[cfg(feature = "steam")]
pub use steam::{SteamCloud, TTS_APP_ID};
