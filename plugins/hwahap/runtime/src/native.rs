//! Native Codex dispatches. The host owns agents; Hwahap owns durable progress.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::session::TokenUsage;
use crate::state::Store;

mod broker;
pub use broker::NativeSessions;
mod abandon;
pub use abandon::AbandonRequest;
mod host;
pub use host::{NativeHost, NativeInput, NativeProgress};
mod failure;
pub use failure::{record_failure, resume_failed, NativeFailure, NativeResume};
mod pool;
pub use pool::NativeLane;
mod reply;
pub mod timing;

const PENDING: &str = "native-pending.json";

/// One exact request for a retained child or the parent coordinator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeDispatch {
    pub dispatch_id: String,
    pub selection: crate::catalog::Selection,
    pub run_id: String,
    pub role: String,
    pub profile: String,
    pub unit: Option<String>,
    pub model: String,
    pub effort: String,
    pub cwd: String,
    pub access: String,
    /// Only an already-Astra host may perform these planning roles directly.
    pub coordinator_allowed: bool,
    pub prompt_digest: String,
    pub plan_digest: Option<String>,
    pub base_head: String,
    pub brief: String,
    pub agent_id: Option<String>,
    /// True after a deadline or process restart: stop the child before acknowledging recovery.
    pub stop_required: bool,
    /// Host-reported spawn failure; no-child failures pause without an imaginary stop.
    pub failure: Option<NativeFailure>,
    /// Stable parent task identity; pool ownership never crosses this boundary.
    pub pool_scope: String,
    pub lane: NativeLane,
    /// Register this identity before sending exactly one follow-up turn.
    pub reuse_agent_id: Option<String>,
    pub soft_budget_secs: u64,
    pub hard_timeout_secs: u64,
}

/// Persist the child identity immediately after spawn, before waiting for its answer.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRegistration {
    pub dispatch_id: String,
    pub agent_id: String,
}

/// Agent text is untrusted input. Only real host tool usage may populate reported_usage.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeCompletion {
    pub dispatch_id: String,
    pub agent_id: String,
    pub final_message: String,
    pub agent_stopped: bool,
    #[serde(default)]
    pub reported_usage: Option<TokenUsage>,
}

/// Explicit host confirmation after it has stopped an orphan and its remaining work.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeStopped {
    pub dispatch_id: String,
    pub agent_id: Option<String>,
    pub all_work_stopped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Pending {
    pub dispatch: NativeDispatch,
    pub completion: Option<NativeCompletion>,
}

pub(super) fn save(store: &Store, pending: &Pending) -> Result<()> {
    store.write_artifact(PENDING, &json(pending)?)
}

fn json(value: &impl Serialize) -> Result<String> {
    serde_json::to_string_pretty(value).map_err(|e| Error::Internal(e.to_string()))
}

pub(super) fn load(store: &Store) -> Result<Option<Pending>> {
    let path = store.artifacts_path().join(PENDING);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let mut pending: Pending = serde_json::from_str(&text)
                .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
            failure::reconcile(store, &mut pending)?;
            Ok(Some(pending))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, e)),
    }
}

pub fn orphan(store: &Store) -> Result<Option<NativeDispatch>> {
    Ok(load(store)?.map(|pending| NativeDispatch {
        stop_required: !pending
            .dispatch
            .failure
            .as_ref()
            .is_some_and(|f| f.no_agent_created),
        ..pending.dispatch
    }))
}

/// Validate without writes before the host cancels its continuation.
pub(super) fn check_stopped(store: &Store, ack: &NativeStopped) -> Result<Pending> {
    let mut pending =
        load(store)?.ok_or_else(|| Error::Rejected("no native dispatch to stop".into()))?;
    let expected = pending
        .dispatch
        .agent_id
        .as_ref()
        .or(pending.dispatch.reuse_agent_id.as_ref());
    if !ack.all_work_stopped
        || pending
            .dispatch
            .failure
            .as_ref()
            .is_some_and(|f| f.no_agent_created)
        || ack.dispatch_id != pending.dispatch.dispatch_id
        || expected.is_some_and(|id| ack.agent_id.as_ref() != Some(id))
        || ack.agent_id.as_ref().is_some_and(|id| {
            id.trim().is_empty() || (id == "coordinator" && !pending.dispatch.coordinator_allowed)
        })
    {
        return Err(Error::Rejected(
            "stop acknowledgment does not match the pending native dispatch".into(),
        ));
    }
    if let Some(id) = &ack.agent_id {
        pool::check_registration(store, &pending.dispatch, id)?;
        pending.dispatch.agent_id = Some(id.clone());
    }
    Ok(pending)
}

pub fn acknowledge_stopped(store: &Store, ack: &NativeStopped) -> Result<()> {
    let pending = check_stopped(store, ack)?;
    // Preserve a discovered child before stop evidence or pool writes can fail.
    save(store, &pending)?;
    store.write_artifact(
        &format!("native-stopped-{}.json", ack.dispatch_id),
        &json(ack)?,
    )?;
    pool::stopped(store, &pending.dispatch)?;
    timing::observe(store, &pending.dispatch, Some("stopped"), None)?;
    clear(store)
}

pub(super) fn clear(store: &Store) -> Result<()> {
    let path = store.artifacts_path().join(PENDING);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// An OS lock survives task cancellation and is released automatically on process death.
pub struct RepoLock {
    _file: std::fs::File,
}

impl RepoLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        let dir = root.join(".hwahap");
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path: PathBuf = dir.join("native.lock");
        let _file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: flock only observes this live descriptor and stores no Rust pointer.
            if unsafe { libc::flock(_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(Error::Rejected(format!(
                    "another Hwahap process owns {}",
                    root.display()
                )));
            }
            Ok(Self { _file })
        }
        #[cfg(not(unix))]
        Err(Error::Rejected(
            "native runs require a supported OS file lock".into(),
        ))
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: this descriptor remains live until the File field is dropped.
            unsafe {
                libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn independent_lock_owners_are_excluded() {
        let temp = tempfile::tempdir().unwrap();
        let lock = RepoLock::acquire(temp.path()).unwrap();
        assert!(RepoLock::acquire(temp.path()).is_err());
        drop(lock);
        RepoLock::acquire(temp.path()).unwrap();
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platforms_reject_native_execution() {
        let temp = tempfile::tempdir().unwrap();
        assert!(matches!(
            RepoLock::acquire(temp.path()),
            Err(Error::Rejected(message))
                if message == "native runs require a supported OS file lock"
        ));
    }

    #[test]
    fn no_pending_is_read_only_and_cannot_be_acknowledged() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        assert!(orphan(&store).unwrap().is_none());
        assert!(!store.root().exists());
        assert!(acknowledge_stopped(
            &store,
            &NativeStopped {
                dispatch_id: "unknown".into(),
                agent_id: None,
                all_work_stopped: true,
            }
        )
        .is_err());
    }
}
