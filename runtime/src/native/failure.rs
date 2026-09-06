//! Spawn refusal is a durable pause, separate from uncertain child ownership.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{clear, json, load, save, NativeDispatch};
use crate::error::{Error, Result};
use crate::state::Store;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeFailure {
    pub dispatch_id: String,
    /// Exact host error or missing capability; never infer capacity from child text.
    pub message: String,
    /// True only when the host knows spawn created no child, including no spawn attempted.
    pub no_agent_created: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeResume {
    pub dispatch_id: String,
    /// Observed host recovery, not elapsed time or a guessed free slot.
    pub recovery_evidence: String,
}

/// Caller must stop its engine continuation under the repository lock before recording.
pub fn record_failure(store: &Store, failure: &NativeFailure) -> Result<NativeDispatch> {
    let mut pending = check_failure(store, failure)?;
    // Persist the reason before changing pending. A failed write never clears ownership.
    let name = format!("native-failure-{}.json", failure.dispatch_id);
    write_exact(store, &name, failure)?;
    pending.dispatch.stop_required = !failure.no_agent_created;
    pending.dispatch.failure = Some(failure.clone());
    save(store, &pending)?;
    let outcome = if failure.no_agent_created {
        "spawn_failed"
    } else {
        "spawn_unknown"
    };
    super::timing::observe(store, &pending.dispatch, Some(outcome), None)?;
    Ok(pending.dispatch)
}

pub(super) fn check_failure(store: &Store, failure: &NativeFailure) -> Result<super::Pending> {
    let pending = load(store)?
        .ok_or_else(|| Error::Rejected("no pending native dispatch accepts failure".into()))?;
    if failure.dispatch_id != pending.dispatch.dispatch_id
        || failure.message.trim().is_empty()
        || (failure.no_agent_created
            && (pending.dispatch.agent_id.is_some() || pending.dispatch.reuse_agent_id.is_some()))
        || pending.completion.is_some()
        || pending
            .dispatch
            .failure
            .as_ref()
            .is_some_and(|saved| saved != failure)
    {
        return Err(Error::Rejected(
            "failure must match the dispatch; an existing child requires stop recovery".into(),
        ));
    }
    Ok(pending)
}

/// Each observed recovery permits one resume; the run request budget still bounds retries.
pub fn resume_failed(store: &Store, resume: &NativeResume) -> Result<()> {
    let recorded = recorded_resume(store, resume)?;
    let name = format!("native-resume-{}.json", resume.dispatch_id);
    if recorded {
        // A replay must never clear a later dispatch.
        if load(store)?.is_some_and(|p| p.dispatch.dispatch_id == resume.dispatch_id) {
            clear(store)?;
        }
        return Ok(());
    }
    resume_new(store, resume, &name)
}

pub(super) fn recorded_resume(store: &Store, resume: &NativeResume) -> Result<bool> {
    if resume.dispatch_id.len() != 64
        || !resume.dispatch_id.bytes().all(|b| b.is_ascii_hexdigit())
        || resume.recovery_evidence.trim().is_empty()
    {
        return Err(Error::Rejected(
            "resume needs an exact dispatch and observed recovery evidence".into(),
        ));
    }
    let name = format!("native-resume-{}.json", resume.dispatch_id);
    let path = store.artifacts_path().join(&name);
    if path.exists() {
        let saved: NativeResume = read(&path)?;
        if &saved != resume {
            return Err(Error::Rejected("resume replay changed its evidence".into()));
        }
        return Ok(true);
    }
    Ok(false)
}

fn resume_new(store: &Store, resume: &NativeResume, name: &str) -> Result<()> {
    let pending = load(store)?
        .ok_or_else(|| Error::Rejected("no paused native dispatch to resume".into()))?;
    if pending.dispatch.dispatch_id != resume.dispatch_id
        || pending.dispatch.agent_id.is_some()
        || pending.completion.is_some()
        || !pending
            .dispatch
            .failure
            .as_ref()
            .is_some_and(|f| f.no_agent_created)
    {
        return Err(Error::Rejected(
            "only a confirmed no-child failure can resume".into(),
        ));
    }
    for entry in std::fs::read_dir(store.artifacts_path())
        .map_err(|e| Error::io(store.artifacts_path(), e))?
    {
        let entry = entry.map_err(|e| Error::io(store.artifacts_path(), e))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("native-resume-") && name.ends_with(".json") {
            let earlier: NativeResume = read(&entry.path())?;
            if earlier.recovery_evidence.trim() == resume.recovery_evidence.trim() {
                return Err(Error::Rejected("this recovery evidence was already used; observe a new host recovery before retrying".into()));
            }
        }
    }
    write_exact(store, name, resume)?;
    clear(store)
}

fn read<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))
}

/// Recover a crash between the immutable failure write and the pending snapshot write.
pub(super) fn reconcile(store: &Store, pending: &mut super::Pending) -> Result<()> {
    let id = &pending.dispatch.dispatch_id;
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Corrupt("invalid pending native dispatch id".into()));
    }
    let path = store
        .artifacts_path()
        .join(format!("native-failure-{id}.json"));
    if !path.try_exists().map_err(|e| Error::io(&path, e))? {
        if pending.dispatch.failure.is_some() {
            return Err(Error::Corrupt("native failure evidence is missing".into()));
        }
        return Ok(());
    }
    let failure: NativeFailure = read(&path)?;
    if &failure.dispatch_id != id
        || failure.message.trim().is_empty()
        || (failure.no_agent_created
            && (pending.dispatch.agent_id.is_some() || pending.dispatch.reuse_agent_id.is_some()))
        || pending.completion.is_some()
        || pending
            .dispatch
            .failure
            .as_ref()
            .is_some_and(|saved| saved != &failure)
    {
        return Err(Error::Corrupt(
            "native failure evidence contradicts pending dispatch".into(),
        ));
    }
    pending.dispatch.stop_required = !failure.no_agent_created;
    pending.dispatch.failure = Some(failure);
    Ok(())
}

fn write_exact<T: Serialize + serde::de::DeserializeOwned + PartialEq>(
    store: &Store,
    name: &str,
    value: &T,
) -> Result<()> {
    let path = store.artifacts_path().join(name);
    if path.exists() {
        if &read::<T>(&path)? != value {
            return Err(Error::Rejected(
                "native failure/recovery evidence cannot be replaced".into(),
            ));
        }
        return Ok(());
    }
    store.write_artifact(name, &json(value)?)
}
