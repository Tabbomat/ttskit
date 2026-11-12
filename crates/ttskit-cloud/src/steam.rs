//! The real Steam Remote Storage backend.

use std::ffi::CString;
use std::io::Read;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

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
/// `write` accepts files up to [`MAX_FILE_SIZE`]. `share` waits up to 30
/// seconds for Steam's answer. `delete` removes the file from the cloud and
/// from Steam's local copy.
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

/// Largest file Steam's `FileWrite` accepts in one call:
/// `k_unMaxCloudFileChunkSize`, 100 MiB.
pub const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// How long `share` waits for Steam's answer.
const SHARE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often `share` checks for Steam's answer.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Rejects files larger than [`MAX_FILE_SIZE`].
fn check_size(name: &str, size: u64) -> Result<(), CloudError> {
    if size > MAX_FILE_SIZE {
        return Err(CloudError::FileTooLarge {
            name: name.to_owned(),
            size,
            max: MAX_FILE_SIZE,
        });
    }
    Ok(())
}

/// The name as a C string; the Steam API cannot take empty names or names
/// with NUL characters.
fn c_name(name: &str) -> Result<CString, CloudError> {
    let invalid = |reason: &str| CloudError::InvalidName {
        name: name.to_owned(),
        reason: reason.to_owned(),
    };
    if name.is_empty() {
        return Err(invalid("name is empty"));
    }
    CString::new(name).map_err(|_| invalid("contains a NUL character"))
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

    fn write(&mut self, name: &str, data: &[u8]) -> Result<(), CloudError> {
        let c_name = c_name(name)?;
        let size = u64::try_from(data.len()).unwrap_or(u64::MAX);
        check_size(name, size)?;
        // The size fits in an i32: it is at most MAX_FILE_SIZE (100 MiB).
        let len = i32::try_from(data.len()).map_err(|_| CloudError::FileTooLarge {
            name: name.to_owned(),
            size,
            max: MAX_FILE_SIZE,
        })?;
        // steamworks only offers a stream writer whose close result is
        // ignored, so call the synchronous FileWrite directly, obtaining the
        // interface pointer the same way the steamworks crate does.
        // SAFETY: Steam is initialized for as long as `self.client` lives, so
        // the interface pointer is valid; `c_name` is a NUL-terminated string
        // and `data` is valid for `len` bytes for the duration of the call.
        let ok = unsafe {
            let rs = steamworks_sys::SteamAPI_SteamRemoteStorage_v016();
            !rs.is_null()
                && steamworks_sys::SteamAPI_ISteamRemoteStorage_FileWrite(
                    rs,
                    c_name.as_ptr(),
                    data.as_ptr().cast(),
                    len,
                )
        };
        if ok {
            return Ok(());
        }
        // Steam gives no reason; report a quota problem if that explains it.
        if let Ok(quota) = self.quota() {
            let old = self
                .client
                .remote_storage()
                .files()
                .into_iter()
                .find(|f| f.name.eq_ignore_ascii_case(name))
                .map_or(0, |f| f.size);
            let needed = size.saturating_sub(old);
            if needed > quota.available {
                return Err(CloudError::QuotaExceeded {
                    name: name.to_owned(),
                    needed,
                    available: quota.available,
                });
            }
        }
        Err(CloudError::Backend(format!(
            "Steam refused to write cloud file {name}"
        )))
    }

    fn delete(&mut self, name: &str) -> Result<(), CloudError> {
        // The steamworks crate panics on interior NUL bytes.
        c_name(name)?;
        let file = self.client.remote_storage().file(name);
        if !file.exists() {
            return Err(CloudError::NotFound {
                name: name.to_owned(),
            });
        }
        if file.delete() {
            Ok(())
        } else {
            Err(CloudError::Backend(format!(
                "Steam refused to delete cloud file {name}"
            )))
        }
    }

    fn share(&mut self, name: &str) -> Result<UgcHandle, CloudError> {
        c_name(name)?;
        let file = self.client.remote_storage().file(name);
        if !file.exists() {
            return Err(CloudError::NotFound {
                name: name.to_owned(),
            });
        }
        let (tx, rx) = mpsc::channel();
        file.share(move |result| {
            let _ = tx.send(result);
        });
        let deadline = Instant::now() + SHARE_TIMEOUT;
        loop {
            self.client.run_callbacks();
            match rx.try_recv() {
                Ok(Ok(handle)) => return Ok(UgcHandle(handle)),
                Ok(Err(err)) => {
                    return Err(CloudError::Backend(format!(
                        "Steam could not share cloud file {name}: {err}"
                    )));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(CloudError::Backend(format!(
                        "Steam dropped the share request for cloud file {name}"
                    )));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if Instant::now() >= deadline {
                return Err(CloudError::Backend(format!(
                    "Steam did not answer the share request for cloud file {name} within {} seconds",
                    SHARE_TIMEOUT.as_secs()
                )));
            }
            thread::sleep(POLL_INTERVAL);
        }
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
    fn size_limit_is_inclusive() {
        assert!(check_size("a", MAX_FILE_SIZE).is_ok());
        assert_eq!(
            check_size("a", MAX_FILE_SIZE + 1),
            Err(CloudError::FileTooLarge {
                name: "a".into(),
                size: MAX_FILE_SIZE + 1,
                max: MAX_FILE_SIZE,
            })
        );
    }

    #[test]
    fn empty_and_nul_names_are_invalid() {
        assert!(matches!(c_name(""), Err(CloudError::InvalidName { .. })));
        assert!(matches!(
            c_name("a\0b"),
            Err(CloudError::InvalidName { .. })
        ));
        assert_eq!(c_name("AB_c.png").unwrap().as_bytes(), b"AB_c.png");
    }

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
