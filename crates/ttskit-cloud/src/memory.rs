//! An in-memory [`CloudBackend`] for tests.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::{CloudBackend, CloudError, CloudFile, Op, Quota, UgcHandle};

/// Default quota of a new [`MemoryCloud`]: 1 GiB.
const DEFAULT_QUOTA: u64 = 1 << 30;

/// First handle handed out by [`CloudBackend::share`].
const FIRST_HANDLE: u64 = 1000;

/// One recorded call to a [`MemoryCloud`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The operation.
    pub op: Op,
    /// The file name as passed, or `None` for `list` and `quota`.
    pub name: Option<String>,
}

#[derive(Debug)]
struct Entry {
    /// Spelling of the first write.
    name: String,
    data: Vec<u8>,
    handle: Option<UgcHandle>,
}

#[derive(Debug)]
struct Injection {
    op: Op,
    /// Lowercased; `None` matches every call of `op`.
    name: Option<String>,
    error: CloudError,
    /// `None` means unlimited.
    remaining: Option<usize>,
}

#[derive(Debug)]
struct State {
    /// Keyed by the ASCII-lowercased name.
    files: BTreeMap<String, Entry>,
    total: u64,
    max_file_size: Option<u64>,
    next_handle: u64,
    calls: Vec<Call>,
    injections: Vec<Injection>,
}

/// An in-memory [`CloudBackend`] with failure injection and a call log.
///
/// Names compare case-insensitively (ASCII) and keep the spelling of the first
/// write. [`list`](CloudBackend::list) sorts by the lowercased name.
///
/// Every trait call is recorded in the call log, including failed ones; the
/// builders and inspection helpers are not. Failures registered with
/// [`fail`](Self::fail) and friends are returned before the call changes
/// anything. State lives in a `RefCell`, so the type is not `Sync`.
#[derive(Debug)]
pub struct MemoryCloud {
    state: RefCell<State>,
}

impl Default for MemoryCloud {
    fn default() -> Self {
        Self::new()
    }
}

fn lower(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn len(data: &[u8]) -> u64 {
    u64::try_from(data.len()).unwrap_or(u64::MAX)
}

impl MemoryCloud {
    /// Creates an empty cloud with a 1 GiB quota and no file size limit.
    pub fn new() -> Self {
        Self {
            state: RefCell::new(State {
                files: BTreeMap::new(),
                total: DEFAULT_QUOTA,
                max_file_size: None,
                next_handle: FIRST_HANDLE,
                calls: Vec::new(),
                injections: Vec::new(),
            }),
        }
    }

    /// Sets the total quota in bytes.
    #[must_use]
    pub fn with_quota(self, total: u64) -> Self {
        self.state.borrow_mut().total = total;
        self
    }

    /// Sets the largest allowed file size in bytes.
    #[must_use]
    pub fn with_max_file_size(self, max: u64) -> Self {
        self.state.borrow_mut().max_file_size = Some(max);
        self
    }

    /// Seeds a file, replacing any file of the same name.
    ///
    /// Seeding inserts directly: it ignores the quota and size limit, is not
    /// logged and is not affected by injected failures. Seeding an empty name
    /// is allowed too.
    #[must_use]
    pub fn with_file(self, name: &str, data: impl Into<Vec<u8>>) -> Self {
        self.insert(name, data.into());
        self
    }

    fn insert(&self, name: &str, data: Vec<u8>) {
        let mut state = self.state.borrow_mut();
        match state.files.get_mut(&lower(name)) {
            Some(entry) => {
                entry.data = data;
                entry.handle = None;
            }
            None => {
                state.files.insert(
                    lower(name),
                    Entry {
                        name: name.to_owned(),
                        data,
                        handle: None,
                    },
                );
            }
        }
    }

    /// Makes every matching call fail with `error`, until
    /// [`clear_failures`](Self::clear_failures).
    ///
    /// `name` is compared case-insensitively; `None` matches all calls of
    /// `op`. `list` and `quota` have no name, so only `None` matches them.
    /// If several injections match a call, the earliest registered wins.
    pub fn fail(&self, op: Op, name: Option<&str>, error: CloudError) {
        self.add_injection(op, name, error, None);
    }

    /// Makes the next matching call fail with `error`.
    pub fn fail_once(&self, op: Op, name: Option<&str>, error: CloudError) {
        self.fail_times(op, name, error, 1);
    }

    /// Makes the next `times` matching calls fail with `error`.
    pub fn fail_times(&self, op: Op, name: Option<&str>, error: CloudError, times: usize) {
        if times > 0 {
            self.add_injection(op, name, error, Some(times));
        }
    }

    /// Removes all injected failures.
    pub fn clear_failures(&self) {
        self.state.borrow_mut().injections.clear();
    }

    fn add_injection(
        &self,
        op: Op,
        name: Option<&str>,
        error: CloudError,
        remaining: Option<usize>,
    ) {
        self.state.borrow_mut().injections.push(Injection {
            op,
            name: name.map(lower),
            error,
            remaining,
        });
    }

    /// All calls so far, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.state.borrow().calls.clone()
    }

    /// The calls that can change the cloud (`write`, `delete`, `share`).
    pub fn mutating_calls(&self) -> Vec<Call> {
        self.state
            .borrow()
            .calls
            .iter()
            .filter(|call| call.op.is_mutating())
            .cloned()
            .collect()
    }

    /// Clears the call log.
    pub fn clear_calls(&self) {
        self.state.borrow_mut().calls.clear();
    }

    /// The content of a file, without logging a call.
    pub fn contents(&self, name: &str) -> Option<Vec<u8>> {
        let state = self.state.borrow();
        state.files.get(&lower(name)).map(|e| e.data.clone())
    }

    /// The UGC handle of a shared file, without logging a call.
    pub fn handle(&self, name: &str) -> Option<UgcHandle> {
        let state = self.state.borrow();
        state.files.get(&lower(name)).and_then(|e| e.handle)
    }

    /// The stored names in list order, without logging a call.
    pub fn file_names(&self) -> Vec<String> {
        let state = self.state.borrow();
        state.files.values().map(|e| e.name.clone()).collect()
    }

    /// Logs the call and returns the matching injected failure, if any.
    fn enter(&self, op: Op, name: Option<&str>) -> Result<(), CloudError> {
        let mut state = self.state.borrow_mut();
        state.calls.push(Call {
            op,
            name: name.map(str::to_owned),
        });
        let key = name.map(lower);
        let Some(index) = state
            .injections
            .iter()
            .position(|i| i.op == op && (i.name.is_none() || i.name == key))
        else {
            return Ok(());
        };
        let injection = &mut state.injections[index];
        let error = injection.error.clone();
        if let Some(remaining) = &mut injection.remaining {
            *remaining -= 1;
            if *remaining == 0 {
                state.injections.remove(index);
            }
        }
        Err(error)
    }
}

impl State {
    fn used(&self) -> u64 {
        self.files.values().map(|e| len(&e.data)).sum()
    }
}

fn not_found(name: &str) -> CloudError {
    CloudError::NotFound {
        name: name.to_owned(),
    }
}

impl CloudBackend for MemoryCloud {
    fn list(&self) -> Result<Vec<CloudFile>, CloudError> {
        self.enter(Op::List, None)?;
        let state = self.state.borrow();
        Ok(state
            .files
            .values()
            .map(|e| CloudFile {
                name: e.name.clone(),
                size: len(&e.data),
            })
            .collect())
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, CloudError> {
        self.enter(Op::Read, Some(name))?;
        let state = self.state.borrow();
        state
            .files
            .get(&lower(name))
            .map(|e| e.data.clone())
            .ok_or_else(|| not_found(name))
    }

    fn write(&mut self, name: &str, data: &[u8]) -> Result<(), CloudError> {
        self.enter(Op::Write, Some(name))?;
        if name.is_empty() {
            return Err(CloudError::InvalidName {
                name: name.to_owned(),
                reason: "name is empty".to_owned(),
            });
        }
        let size = len(data);
        {
            let state = self.state.borrow();
            if let Some(max) = state.max_file_size
                && size > max
            {
                return Err(CloudError::FileTooLarge {
                    name: name.to_owned(),
                    size,
                    max,
                });
            }
            let old = state.files.get(&lower(name)).map_or(0, |e| len(&e.data));
            let needed = size.saturating_sub(old);
            let available = state.total.saturating_sub(state.used());
            if needed > available {
                return Err(CloudError::QuotaExceeded {
                    name: name.to_owned(),
                    needed,
                    available,
                });
            }
        }
        self.insert(name, data.to_vec());
        Ok(())
    }

    fn delete(&mut self, name: &str) -> Result<(), CloudError> {
        self.enter(Op::Delete, Some(name))?;
        let mut state = self.state.borrow_mut();
        state
            .files
            .remove(&lower(name))
            .map(|_| ())
            .ok_or_else(|| not_found(name))
    }

    fn share(&mut self, name: &str) -> Result<UgcHandle, CloudError> {
        self.enter(Op::Share, Some(name))?;
        let mut state = self.state.borrow_mut();
        let next = state.next_handle;
        let entry = state
            .files
            .get_mut(&lower(name))
            .ok_or_else(|| not_found(name))?;
        if let Some(handle) = entry.handle {
            return Ok(handle);
        }
        let handle = UgcHandle(next);
        entry.handle = Some(handle);
        state.next_handle += 1;
        Ok(handle)
    }

    fn quota(&self) -> Result<Quota, CloudError> {
        self.enter(Op::Quota, None)?;
        let state = self.state.borrow();
        Ok(Quota {
            total: state.total,
            available: state.total.saturating_sub(state.used()),
        })
    }
}
