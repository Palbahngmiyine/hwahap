use super::{NativeDispatch, NativeProgress};
use crate::{
    engine::StepOutcome,
    error::{Error, Result},
    state::Store,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AbandonRequest {
    pub run_id: String,
    pub user_instruction: String,
    pub contract_digest: String,
}
const REQUEST: &str = "abandon-request.json";
fn validate(request: &AbandonRequest) -> Result<()> {
    if request.run_id.is_empty()
        || request.run_id.contains(['/', '\\'])
        || matches!(request.run_id.as_str(), "." | "..")
        || request.user_instruction.trim().is_empty()
    {
        return Err(Error::Rejected(
            "abandon requires the run ID and original termination instruction".into(),
        ));
    }
    Ok(())
}
pub(super) fn archived(store: &Store, request: &AbandonRequest) -> Result<Option<NativeProgress>> {
    validate(request)?;
    let directory = store.root().join("archive").join(&request.run_id);
    let path = directory.join("artifacts").join(REQUEST);
    if !directory.join("completion/events.jsonl").exists() {
        return Ok(None);
    }
    store.verify_archive(&request.run_id)?;
    let saved: AbandonRequest =
        serde_json::from_slice(&std::fs::read(&path).map_err(|e| Error::io(&path, e))?)
            .map_err(|e| Error::Corrupt(e.to_string()))?;
    if &saved != request {
        return Err(Error::Rejected(
            "abandon replay differs from its recorded request".into(),
        ));
    }
    Ok(Some(done(request)))
}
pub(super) fn request(store: &Store, request: &AbandonRequest) -> Result<()> {
    validate(request)?;
    let run = store
        .read_run()?
        .ok_or_else(|| Error::Rejected("abandon requires an active run".into()))?;
    let plan = store
        .read_plan()?
        .ok_or_else(|| Error::Rejected("abandon requires the current plan".into()))?;
    let current = plan.digest()?;
    if plan.goal_id != run.goal_id
        || plan.frozen.as_ref().is_some_and(|frozen| {
            frozen.digest != current || run.plan_digest.as_ref() != Some(&current)
        })
    {
        return Err(Error::Rejected(
            "abandon requires the intact current frozen contract".into(),
        ));
    }
    if run.run_id != request.run_id || current.to_string() != request.contract_digest {
        return Err(Error::Rejected(
            "abandon must match the current run and contract digest".into(),
        ));
    }
    crate::pr_review::save_evidence(store, REQUEST, request)?;
    let data = serde_json::to_value(request).map_err(|e| Error::Corrupt(e.to_string()))?;
    if !store
        .read_events()?
        .iter()
        .any(|e| e.kind == "abandon_requested" && e.data == data)
    {
        store.append_event(&crate::clock::SystemClock, "abandon_requested", data)?;
    }
    Ok(())
}
pub(super) fn pending(store: &Store) -> Result<Option<AbandonRequest>> {
    crate::pr_review::read_evidence(store, REQUEST)
}
pub(super) fn finish(store: &Store, request: &AbandonRequest) -> Result<NativeProgress> {
    if let Some(dispatch) = super::orphan(store)? {
        if dispatch
            .failure
            .as_ref()
            .is_some_and(|f| f.no_agent_created)
        {
            super::clear(store)?;
        } else {
            return Ok(NativeProgress {
                outcome: StepOutcome {
                    run_id: request.run_id.clone(),
                    phase: "build".into(),
                    state: "abandoning".into(),
                    next: "native_stop".into(),
                    message:
                        "Stop this dispatch and its commands, then submit its stop acknowledgment."
                            .into(),
                    plan_digest: Some(request.contract_digest.clone()),
                    pr_url: None,
                },
                dispatch: Some(NativeDispatch {
                    stop_required: true,
                    ..dispatch
                }),
            });
        }
    }
    crate::verification::require_stopped(store)?;
    let data =
        serde_json::json!({"run_id":request.run_id,"contract_digest":request.contract_digest});
    if !store
        .read_events()?
        .iter()
        .any(|e| e.kind == "abandon_stopped" && e.data == data)
    {
        store.append_event(&crate::clock::SystemClock, "abandon_stopped", data)?;
    }
    store.archive(&crate::clock::SystemClock)?;
    Ok(done(request))
}
fn done(request: &AbandonRequest) -> NativeProgress {
    NativeProgress { outcome:StepOutcome {
        run_id:request.run_id.clone(), phase:"complete".into(),state:"archived".into(),next:"completed".into(),
        message:"Run archived with its worktree and records. Start a new run with a current host observation.".into(),
        plan_digest:Some(request.contract_digest.clone()),pr_url:None,
    },dispatch:None }
}

pub(super) fn replay_stop(
    store: &Store,
    ack: &super::NativeStopped,
    parent: Option<&str>,
) -> Result<Option<NativeProgress>> {
    if ack.dispatch_id.len() != 64 || !ack.dispatch_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    for run in store.archived_run_ids()? {
        let root = store.root().join("archive").join(&run);
        let path = root
            .join("artifacts")
            .join(format!("native-stopped-{}.json", ack.dispatch_id));
        if !path.exists() {
            continue;
        }
        store.verify_archive(&run)?;
        let saved: super::NativeStopped =
            serde_json::from_slice(&std::fs::read(&path).map_err(|e| Error::io(&path, e))?)
                .map_err(|e| Error::Corrupt(e.to_string()))?;
        if serde_json::to_value(&saved).unwrap() != serde_json::to_value(ack).unwrap() {
            return Err(Error::Rejected(
                "stop replay differs from its recorded acknowledgment".into(),
            ));
        }
        let owner_path = root.join("artifacts/native-owner.json");
        let owner: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&owner_path).map_err(|e| Error::io(&owner_path, e))?,
        )
        .map_err(|e| Error::Corrupt(e.to_string()))?;
        if owner["pool_scope"].as_str() != Some(parent.unwrap_or(&run)) {
            return Err(Error::Rejected(
                "archived stop belongs to another parent task".into(),
            ));
        }
        let request_path = root.join("artifacts").join(REQUEST);
        let request: AbandonRequest = serde_json::from_slice(
            &std::fs::read(&request_path).map_err(|e| Error::io(&request_path, e))?,
        )
        .map_err(|e| Error::Corrupt(e.to_string()))?;
        return Ok(Some(done(&request)));
    }
    Ok(None)
}
