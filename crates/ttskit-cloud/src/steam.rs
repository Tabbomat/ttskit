//! The real Steam Remote Storage backend.

use std::io::Read;

use steamworks::{Client, SteamAPIInitError};

use crate::{CloudBackend, CloudError, CloudFile, Quota, UgcHandle};

/// Tabletop Simulator's Steam app id.
pub const TTS_APP_ID: u32 = 286160;

/// A [`CloudBackend`] backed by the running Steam client.
///
/// Needs Steam running and logged in as an account that owns Tabletop
/// Simulator, and `steam_api64.dll` next to the executable. Not `Send`: use it
/// on the thread that created it.
///
/// `write`, `delete` and `share` are not implemented yet and return an error
/// without touching the cloud.
pub struct SteamCloud {
    client: Client,
}

impl SteamCloud {
    /// Connects to the running Steam client as Tabletop Simulator.
    ///
    /// Fails with [`CloudError::Backend`] and a user-facing message if Steam is
    /// not running, not logged in, offline, or has Steam Cloud disabled.
    pub fn connect() -> Result<SteamCloud, CloudError> {
        let client = Client::init_app(TTS_APP_ID)
            .map_err(|err| CloudError::Backend(init_error_message(&err)))?;
        if !client.user().logged_on() {
            return Err(CloudError::Backend(
                "Steam is not connected to the Steam servers (offline or not logged in)".into(),
            ));
        }
        let storage = client.remote_storage();
        if !storage.is_cloud_enabled_for_account() {
            return Err(CloudError::Backend(
                "Steam Cloud is disabled for this Steam account; enable it in Steam settings > Cloud"
                    .into(),
            ));
        }
        if !storage.is_cloud_enabled_for_app() {
            return Err(CloudError::Backend(
                "Steam Cloud is disabled for Tabletop Simulator; enable it in the game's properties in the Steam library"
                    .into(),
            ));
        }
        Ok(SteamCloud { client })
    }
}

/// Turns a Steam initialization error into a user-facing message.
fn init_error_message(err: &SteamAPIInitError) -> String {
    match err {
        SteamAPIInitError::NoSteamClient(_) => {
            "Steam is not running; start Steam and log in, then try again".into()
        }
        SteamAPIInitError::VersionMismatch(_) => {
            "the Steam client is out of date; update Steam".into()
        }
        SteamAPIInitError::FailedGeneric(msg) => {
            let msg = msg.trim();
            let detail = if msg.is_empty() {
                String::new()
            } else {
                format!(": {msg}")
            };
            format!(
                "could not connect to Steam{detail}. Make sure Steam is running and logged in \
                 as an account that owns Tabletop Simulator, and that ttskit runs as the same \
                 user (and elevation) as Steam"
            )
        }
    }
}

fn not_implemented(op: &str) -> CloudError {
    CloudError::Backend(format!("{op} is not implemented yet for the Steam backend"))
}

impl CloudBackend for SteamCloud {
    fn list(&self) -> Result<Vec<CloudFile>, CloudError> {
        let mut files: Vec<CloudFile> = self
            .client
            .remote_storage()
            .files()
            .into_iter()
            .map(|info| CloudFile {
                name: info.name,
                size: info.size,
            })
            .collect();
        files.sort_by_cached_key(|file| file.name.to_ascii_lowercase());
        Ok(files)
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, CloudError> {
        // The steamworks crate panics on interior NUL bytes.
        if name.contains('\0') {
            return Err(CloudError::InvalidName {
                name: name.to_owned(),
                reason: "contains a NUL character".into(),
            });
        }
        let file = self.client.remote_storage().file(name);
        if !file.exists() {
            return Err(CloudError::NotFound {
                name: name.to_owned(),
            });
        }
        let mut data = Vec::new();
        file.read().read_to_end(&mut data).map_err(|err| {
            CloudError::Backend(format!("failed to read cloud file {name}: {err}"))
        })?;
        Ok(data)
    }

    fn write(&mut self, _name: &str, _data: &[u8]) -> Result<(), CloudError> {
        Err(not_implemented("write"))
    }

    fn delete(&mut self, _name: &str) -> Result<(), CloudError> {
        Err(not_implemented("delete"))
    }

    fn share(&mut self, _name: &str) -> Result<UgcHandle, CloudError> {
        Err(not_implemented("share"))
    }

    fn quota(&self) -> Result<Quota, CloudError> {
        // steamworks has no wrapper for GetQuota, so call the flat API directly,
        // obtaining the interface pointer the same way the steamworks crate does.
        // The `Client` held by `self` keeps the Steam API initialized.
        let mut total = 0u64;
        let mut available = 0u64;
        // SAFETY: Steam is initialized for as long as `self.client` lives, so
        // the interface pointer is valid; both out-pointers refer to live
        // local `u64`s that Steam writes to during the call.
        let ok = unsafe {
            let rs = steamworks_sys::SteamAPI_SteamRemoteStorage_v016();
            !rs.is_null()
                && steamworks_sys::SteamAPI_ISteamRemoteStorage_GetQuota(
                    rs,
                    &mut total,
                    &mut available,
                )
        };
        if ok {
            Ok(Quota { total, available })
        } else {
            Err(CloudError::Backend(
                "Steam did not report the cloud quota".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_steam_client_says_steam_is_not_running() {
        let msg = init_error_message(&SteamAPIInitError::NoSteamClient("boom".into()));
        assert!(msg.contains("Steam is not running"), "{msg}");
    }

    #[test]
    fn version_mismatch_says_update_steam() {
        let msg = init_error_message(&SteamAPIInitError::VersionMismatch(String::new()));
        assert!(msg.contains("update Steam"), "{msg}");
    }

    #[test]
    fn generic_failure_includes_steam_message() {
        let msg = init_error_message(&SteamAPIInitError::FailedGeneric("no app id".into()));
        assert!(
            msg.starts_with("could not connect to Steam: no app id."),
            "{msg}"
        );
        assert!(msg.contains("same user (and elevation)"), "{msg}");
    }

    #[test]
    fn generic_failure_omits_empty_steam_message() {
        let msg = init_error_message(&SteamAPIInitError::FailedGeneric("  ".into()));
        assert!(
            msg.starts_with("could not connect to Steam. Make sure"),
            "{msg}"
        );
    }
}
