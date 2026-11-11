//! The [`CloudBackend`] trait and the types it uses.

use std::fmt;

/// Access to the files in the user's Tabletop Simulator Steam Cloud.
///
/// Calls are synchronous; a backend that needs to wait for Steam callbacks
/// does so inside the call.
///
/// # File names
///
/// Names are the flat Steam Remote Storage names (the game stores assets as
/// `<SHA1>_<name>`, plus its index files). Names compare case-insensitively
/// (ASCII), as Steam Cloud does on Windows: writing `a.png` over an existing
/// `A.png` replaces that file, and the stored spelling stays `A.png`.
pub trait CloudBackend {
    /// Lists all files, sorted by name.
    fn list(&self) -> Result<Vec<CloudFile>, CloudError>;

    /// Reads the whole content of a file.
    fn read(&self, name: &str) -> Result<Vec<u8>, CloudError>;

    /// Creates a file or replaces its content.
    fn write(&mut self, name: &str, data: &[u8]) -> Result<(), CloudError>;

    /// Deletes a file.
    fn delete(&mut self, name: &str) -> Result<(), CloudError>;

    /// Shares a file publicly and returns its UGC handle, which is part of the
    /// file's download URL.
    fn share(&mut self, name: &str) -> Result<UgcHandle, CloudError>;

    /// Returns the storage quota.
    fn quota(&self) -> Result<Quota, CloudError>;
}

/// A file in the cloud.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CloudFile {
    /// The file name as stored.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
}

/// The handle Steam returns when a file is shared.
///
/// Displayed as a decimal number, the form used in download URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UgcHandle(pub u64);

impl fmt::Display for UgcHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Cloud storage quota in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    /// Total space.
    pub total: u64,
    /// Space still free.
    pub available: u64,
}

impl Quota {
    /// Space in use.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
}

/// A [`CloudBackend`] operation, for logging calls and injecting failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    /// [`CloudBackend::list`]
    List,
    /// [`CloudBackend::read`]
    Read,
    /// [`CloudBackend::write`]
    Write,
    /// [`CloudBackend::delete`]
    Delete,
    /// [`CloudBackend::share`]
    Share,
    /// [`CloudBackend::quota`]
    Quota,
}

impl Op {
    /// Whether the operation can change the cloud.
    pub fn is_mutating(self) -> bool {
        matches!(self, Op::Write | Op::Delete | Op::Share)
    }
}

/// Errors returned by a [`CloudBackend`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CloudError {
    /// The file does not exist.
    #[error("cloud file not found: {name}")]
    NotFound {
        /// The requested name.
        name: String,
    },
    /// Writing the file would exceed the cloud quota.
    #[error("not enough cloud space for {name}: {needed} bytes needed, {available} available")]
    QuotaExceeded {
        /// The file being written.
        name: String,
        /// Additional bytes the write needs.
        needed: u64,
        /// Bytes still free.
        available: u64,
    },
    /// The file is larger than a single cloud file may be.
    #[error("cloud file {name} is too large: {size} bytes, at most {max} allowed")]
    FileTooLarge {
        /// The file being written.
        name: String,
        /// Its size in bytes.
        size: u64,
        /// The largest allowed size in bytes.
        max: u64,
    },
    /// The name cannot be used as a cloud file name.
    #[error("invalid cloud file name {name:?}: {reason}")]
    InvalidName {
        /// The rejected name.
        name: String,
        /// Why it was rejected.
        reason: String,
    },
    /// Any other failure reported by the backend.
    #[error("cloud backend error: {0}")]
    Backend(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ugc_handle_displays_as_decimal() {
        assert_eq!(UgcHandle(1_234_567_890_123).to_string(), "1234567890123");
    }

    #[test]
    fn quota_used_is_total_minus_available() {
        let quota = Quota {
            total: 100,
            available: 30,
        };
        assert_eq!(quota.used(), 70);
    }

    #[test]
    fn only_write_delete_and_share_are_mutating() {
        let mutating: Vec<Op> = [
            Op::List,
            Op::Read,
            Op::Write,
            Op::Delete,
            Op::Share,
            Op::Quota,
        ]
        .into_iter()
        .filter(|op| op.is_mutating())
        .collect();
        assert_eq!(mutating, [Op::Write, Op::Delete, Op::Share]);
    }

    #[test]
    fn backend_is_object_safe() {
        fn _takes(_: &mut dyn CloudBackend) {}
    }
}
