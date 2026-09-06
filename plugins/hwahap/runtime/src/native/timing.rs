//! Host-observed intervals include queue/spawn/relay, not pure model execution time.
//! These observations never authorize completion. Callers serialize writes under their run lock.
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::profile::Role;
use crate::state::Store;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeTiming {
    pub dispatch_id: String,
    pub role: String,
    pub prompt_bytes: u64,
    pub soft_budget_secs: u64,
    pub hard_timeout_secs: u64,
    pub offered_at_ms: i64,
    pub registered_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub outcome: Option<String>,
    pub output_bytes: Option<u64>,
}

pub fn soft_budget(role: Role) -> u64 {
    match role {
        Role::FactFinder
        | Role::ColdConsumer
        | Role::PlanCritic
        | Role::UnitReviewer
        | Role::FailureDiagnosis => 60,
        Role::Implementer | Role::Rework | Role::FinalReview => 120,
        Role::Recommender | Role::PlanSynthesis | Role::ConflictReplan => 90,
    }
}

fn name(id: &str) -> Result<String> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Rejected("invalid native timing dispatch id".into()));
    }
    Ok(format!("native-timing-{id}.json"))
}

pub fn read(store: &Store, id: &str) -> Result<Option<NativeTiming>> {
    let path = store.artifacts_path().join(name(id)?);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(&path, e)),
    };
    let value: NativeTiming = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
    if value.dispatch_id != id {
        return Err(Error::Corrupt("native timing dispatch mismatch".into()));
    }
    Ok(Some(value))
}

fn save(store: &Store, value: &NativeTiming) -> Result<()> {
    store.write_artifact(&name(&value.dispatch_id)?, &super::json(value)?)
}

pub fn begin(
    store: &Store,
    id: &str,
    role: Role,
    prompt_bytes: u64,
    soft: u64,
    hard: u64,
) -> Result<()> {
    if let Some(saved) = read(store, id)? {
        if saved.role != role.as_str()
            || saved.prompt_bytes != prompt_bytes
            || saved.soft_budget_secs != soft.min(hard)
            || saved.hard_timeout_secs != hard
        {
            return Err(Error::Rejected(
                "native timing replay changed its request".into(),
            ));
        }
        return Ok(());
    }
    save(
        store,
        &NativeTiming {
            dispatch_id: id.into(),
            role: role.as_str().into(),
            prompt_bytes,
            soft_budget_secs: soft.min(hard),
            hard_timeout_secs: hard,
            offered_at_ms: chrono::Utc::now().timestamp_millis(),
            registered_at_ms: None,
            finished_at_ms: None,
            outcome: None,
            output_bytes: None,
        },
    )
}

pub fn registered(store: &Store, id: &str) -> Result<()> {
    stamp(store, id, chrono::Utc::now().timestamp_millis(), None)
}

pub fn finish(store: &Store, id: &str, outcome: &str, output_bytes: Option<u64>) -> Result<()> {
    stamp(
        store,
        id,
        chrono::Utc::now().timestamp_millis(),
        Some((outcome, output_bytes)),
    )
}

fn stamp(store: &Store, id: &str, now: i64, terminal: Option<(&str, Option<u64>)>) -> Result<()> {
    let mut value =
        read(store, id)?.ok_or_else(|| Error::Rejected("native timing was not offered".into()))?;
    if value.finished_at_ms.is_some() {
        return Ok(());
    }
    if let Some((outcome, bytes)) = terminal {
        if outcome.trim().is_empty() {
            return Err(Error::Rejected("native timing outcome is empty".into()));
        }
        value.finished_at_ms = Some(now);
        value.outcome = Some(outcome.into());
        value.output_bytes = bytes;
    } else if value.registered_at_ms.is_none() {
        value.registered_at_ms = Some(now);
    } else {
        return Ok(());
    }
    save(store, &value)
}

/// Every dispatch must have the timing record created before it was offered.
pub(super) fn observe(
    store: &Store,
    dispatch: &super::NativeDispatch,
    outcome: Option<&str>,
    bytes: Option<u64>,
) -> Result<()> {
    if read(store, &dispatch.dispatch_id)?.is_none() {
        return Err(Error::Corrupt("native dispatch timing is missing".into()));
    }
    match outcome {
        Some(outcome) => finish(store, &dispatch.dispatch_id, outcome, bytes),
        None => registered(store, &dispatch.dispatch_id),
    }
}

pub fn elapsed_since_offer(store: &Store, id: &str) -> Result<Option<u64>> {
    Ok(read(store, id)?
        .and_then(|v| elapsed_at(v.offered_at_ms, chrono::Utc::now().timestamp_millis())))
}

pub fn elapsed_since_registration(store: &Store, id: &str) -> Result<Option<u64>> {
    Ok(read(store, id)?
        .and_then(|v| v.registered_at_ms)
        .and_then(|start| elapsed_at(start, chrono::Utc::now().timestamp_millis())))
}

fn elapsed_at(offered: i64, now: i64) -> Option<u64> {
    now.checked_sub(offered)
        .and_then(|ms| u64::try_from(ms).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_rejects_missing_timing_for_every_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let id = "b".repeat(64);
        let dispatch: super::super::NativeDispatch =
            serde_json::from_value(serde_json::json!({
                "dispatch_id": id, "run_id": "r", "role": "fact_finder","assessment":crate::delegation::test_payload().0,"decision":crate::delegation::test_payload().1,"selection":{"run_id":"run","host_session_id":"parent","role":"recommender","unit":null,"model":"m","effort":"high","requirements":{"capabilities":{},"depth":"deep"},"tools":[],"catalog_digest":"catalog","host_digest":"host","digest":"selection"}, "profile": "economy",
                "model": "m", "effort": "medium", "cwd": "/tmp", "access": "read_only",
                "coordinator_allowed": false, "prompt_digest": "p", "base_head": "h",
                "brief": "facts", "stop_required": false, "pool_scope":"parent", "lane":"worker", "soft_budget_secs":60, "hard_timeout_secs":180
            }))
            .unwrap();
        assert!(observe(&store, &dispatch, None, None).is_err());
        begin(&store, &id, Role::FactFinder, 5, 60, 180).unwrap();
        observe(&store, &dispatch, None, None).unwrap();
        observe(&store, &dispatch, Some("completed"), Some(7)).unwrap();
        let saved = read(&store, &id).unwrap().unwrap();
        assert!(saved.registered_at_ms.is_some());
        assert_eq!(saved.outcome.as_deref(), Some("completed"));
        assert_eq!(saved.output_bytes, Some(7));
        store.write_artifact(&name(&id).unwrap(), "broken").unwrap();
        assert!(observe(&store, &dispatch, Some("stopped"), None).is_err());
    }

    #[test]
    fn replay_preserves_first_observations_and_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let id = "a".repeat(64);
        begin(&store, &id, Role::Implementer, 42, 120, 90).unwrap();
        let offered = read(&store, &id).unwrap().unwrap();
        assert_eq!(offered.soft_budget_secs, 90);
        assert!(offered.outcome.is_none());
        let t = offered.offered_at_ms;
        stamp(&store, &id, t + 10, None).unwrap();
        stamp(&store, &id, t + 20, None).unwrap();
        stamp(&store, &id, t + 30, Some(("timeout", None))).unwrap();
        stamp(&store, &id, t + 40, Some(("completed", Some(99)))).unwrap();
        begin(&store, &id, Role::Implementer, 42, 120, 90).unwrap();
        let saved = read(&store, &id).unwrap().unwrap();
        assert_eq!(saved.offered_at_ms, offered.offered_at_ms);
        assert_eq!(saved.registered_at_ms, Some(t + 10));
        assert_eq!(saved.finished_at_ms, Some(t + 30));
        assert_eq!(saved.outcome.as_deref(), Some("timeout"));
        assert_eq!(saved.output_bytes, None);
        assert!(begin(&store, &id, Role::Implementer, 43, 120, 90).is_err());
        assert_eq!(elapsed_at(10, 25), Some(15));
        assert_eq!(elapsed_at(25, 10), None);
        assert_eq!(elapsed_at(i64::MIN, i64::MAX), None);
    }
}
