//! Requested models and caller-reported tokens are evidence, never a verified bill.
use std::collections::BTreeMap;

use serde::{de::DeserializeOwned, Serialize};

use crate::native::{NativeCompletion, NativeDispatch, NativeFailure, NativeResume, NativeStopped};
use crate::state::Store;
use crate::{Error, Result};

mod latency;
pub mod meter;
mod pricing;

#[derive(Default, Serialize)]
struct Counts {
    requests: u64,
    completions: u64,
    stopped_without_completion: u64,
    incomplete: u64,
    dispatch_failures: u64,
    confirmed_no_child_failures: u64,
    observed_recoveries: u64,
    coordinator_completions: u64,
    native_child_completions: u64,
    reused_child_completions: u64,
    fresh_child_completions: u64,
    coordinator_usage_reported_completions: u64,
    native_child_usage_reported_completions: u64,
    usage_reported_completions: u64,
    requests_without_reported_usage: u64,
    reported_input_tokens: u64,
    reported_output_tokens: u64,
    reported_cached_input_tokens: u64,
}

fn add(target: &mut u64, value: u64) -> Result<()> {
    *target = target
        .checked_add(value)
        .ok_or_else(|| Error::Corrupt("native usage counter overflow".into()))?;
    Ok(())
}

fn artifacts<T: DeserializeOwned>(store: &Store, prefix: &str) -> Result<BTreeMap<String, T>> {
    let dir = store.artifacts_path();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(Error::io(dir, e)),
    };
    let mut records = BTreeMap::new();
    for entry in entries {
        let path = entry.map_err(|e| Error::io(&dir, e))?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(id) = name
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(".json"))
        else {
            continue;
        };
        let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
        let record = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
        records.insert(id.to_owned(), record);
    }
    Ok(records)
}

fn collect(store: &Store) -> Result<(Counts, BTreeMap<String, Counts>)> {
    let run = store.read_run()?;
    let completions = artifacts::<NativeCompletion>(store, "native-completion-")?;
    let stopped = artifacts::<NativeStopped>(store, "native-stopped-")?;
    let resumes = artifacts::<NativeResume>(store, "native-resume-")?;
    let failures = artifacts::<NativeFailure>(store, "native-failure-")?;
    // Requests are durable before results: read them last while the engine may be progressing.
    let requests = artifacts::<NativeDispatch>(store, "native-request-")?;
    for (id, completion) in &completions {
        if id != &completion.dispatch_id || !requests.contains_key(id) || !completion.agent_stopped
        {
            return Err(Error::Corrupt(format!("invalid native completion {id}")));
        }
    }
    for (id, ack) in &stopped {
        if id != &ack.dispatch_id || !requests.contains_key(id) || !ack.all_work_stopped {
            return Err(Error::Corrupt(format!(
                "invalid native stop acknowledgment {id}"
            )));
        }
    }
    for (id, failure) in &failures {
        if id != &failure.dispatch_id
            || !requests.contains_key(id)
            || failure.message.trim().is_empty()
            || completions.contains_key(id)
            || (failure.no_agent_created && stopped.contains_key(id))
        {
            return Err(Error::Corrupt(format!("invalid native failure {id}")));
        }
    }
    for (id, resume) in &resumes {
        if id != &resume.dispatch_id
            || resume.recovery_evidence.trim().is_empty()
            || !failures.get(id).is_some_and(|f| f.no_agent_created)
        {
            return Err(Error::Corrupt(format!("invalid native recovery {id}")));
        }
    }
    let (mut total, mut models) = (Counts::default(), BTreeMap::<String, Counts>::new());
    for (id, request) in requests {
        if id != request.dispatch_id
            || request.model.trim().is_empty()
            || run.as_ref().is_some_and(|run| run.run_id != request.run_id)
        {
            return Err(Error::Corrupt(format!("invalid native request {id}")));
        }
        let completion = completions.get(&id);
        if let Some(completion) = completion {
            if completion.agent_id.trim().is_empty()
                || (completion.agent_id == "coordinator" && !request.coordinator_allowed)
            {
                return Err(Error::Corrupt(format!(
                    "invalid native completion identity {id}"
                )));
            }
            if let Some(usage) = &completion.reported_usage {
                usage
                    .verify()
                    .map_err(|e| Error::Corrupt(format!("native usage {id}: {e}")))?;
            }
        }
        for counts in [&mut total, models.entry(request.model).or_default()] {
            add(&mut counts.requests, 1)?;
            add(
                &mut counts.dispatch_failures,
                u64::from(failures.contains_key(&id)),
            )?;
            add(
                &mut counts.confirmed_no_child_failures,
                u64::from(failures.get(&id).is_some_and(|f| f.no_agent_created)),
            )?;
            add(
                &mut counts.observed_recoveries,
                u64::from(resumes.contains_key(&id)),
            )?;
            let Some(completion) = completion else {
                add(&mut counts.incomplete, 1)?;
                add(
                    &mut counts.stopped_without_completion,
                    u64::from(stopped.contains_key(&id)),
                )?;
                add(&mut counts.requests_without_reported_usage, 1)?;
                continue;
            };
            add(&mut counts.completions, 1)?;
            if completion.agent_id == "coordinator" {
                add(&mut counts.coordinator_completions, 1)?;
            } else {
                add(&mut counts.native_child_completions, 1)?;
                if request.reuse_agent_id.is_some() {
                    add(&mut counts.reused_child_completions, 1)?;
                } else {
                    add(&mut counts.fresh_child_completions, 1)?;
                }
            }
            if let Some(usage) = &completion.reported_usage {
                add(&mut counts.usage_reported_completions, 1)?;
                if completion.agent_id == "coordinator" {
                    add(&mut counts.coordinator_usage_reported_completions, 1)?;
                } else {
                    add(&mut counts.native_child_usage_reported_completions, 1)?;
                }
                add(&mut counts.reported_input_tokens, usage.input_tokens)?;
                add(&mut counts.reported_output_tokens, usage.output_tokens)?;
                add(
                    &mut counts.reported_cached_input_tokens,
                    usage.cached_input_tokens,
                )?;
            } else {
                add(&mut counts.requests_without_reported_usage, 1)?;
            }
        }
    }
    Ok((total, models))
}

/// Includes every durable request, including retries and work without a completion.
pub fn summary(store: &Store) -> Result<serde_json::Value> {
    let (total, models) = collect(store)?;
    let measured = meter::summary(store)?;
    // A bad optional rate card must not strand an implementation or hide token evidence.
    let estimate = pricing::estimate(store, &measured).unwrap_or_else(|error|
        serde_json::json!({"status":"invalid_configuration","priced_subtotal":null,"error":error.to_string()}));
    let run = store.read_run()?;
    let catalog = run
        .as_ref()
        .map(|r| crate::catalog::snapshot(store, &r.run_id))
        .transpose()?;
    let selections: Vec<_> = artifacts::<NativeDispatch>(store, "native-request-")?
        .into_values()
        .map(|d| d.selection)
        .collect();
    let progress = crate::pr_review::ReviewProgress::load(store)?;
    Ok(serde_json::json!({
        "scope": "retained native requests for this run; receipts are not counted again",
        "evidence": "caller-reported tokens grouped by requested model; actual model unverified",
        "coverage": "usage_reported_completions / requests; incomplete work may consume tokens",
        "total_billed_cost": "unknown",
        "native_thread_release": "unknown; completed or interrupted work does not prove a host thread was released",
        "pool_scope": "at most three retained children for the same run and parent task; capacity is host-observed",
        "latency": latency::summary(store)?,
        "parent_relay_usage": "covered only when the parent session is explicitly attached; overlaps coordinator dispatch usage",
        "limits": "Missing usage is unknown, not zero. Session and dispatch totals overlap; never add them. Unattached sessions and work before attachment are excluded. Cached input is part of input.",
        "total": total, "by_requested_model": models,
        "observed_session_usage": measured, "cost_estimate":estimate,
        "evaluation": {
            "run_id":run.as_ref().map(|r| &r.run_id), "state":run.as_ref().map(|r| r.state.name()),
            "accepted_units":run.as_ref().map(|r| r.accepted_units.len()),
            "pr_repair_attempts":progress.as_ref().map(|p| p.repairs),
            "contract_digest":run.as_ref().and_then(|r| r.plan_digest.as_ref()),
            "reviewed_head":progress.as_ref().map(|p| &p.binding.head),
            "catalog_snapshot":catalog,
            "model_selections":selections,
            "comparison":"Compare the same task, base commit and acceptance tests across separate runs; include failures and missing usage. These observations are not a model benchmark."
        },
    }))
}

/// Mutating step/ship paths persist a readable snapshot; status remains read-only.
pub fn persist(store: &Store) -> Result<serde_json::Value> {
    let mut value = summary(store)?;
    value["observed_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    store.write_usage(
        &serde_json::to_string_pretty(&value).map_err(|e| Error::Internal(e.to_string()))?,
    )?;
    Ok(value)
}

pub fn usage_command(args: &[String]) -> Result<serde_json::Value> {
    let (action, cwd) = match args {
        [action, cwd, ..] => (action.as_str(), cwd),
        _ => return Err(Error::Rejected("usage: hwahap usage attach <repo> <session.jsonl> [--from-start] | sync <repo> | show <repo>".into())),
    };
    let git = crate::git::Git::open(std::path::Path::new(cwd))?;
    let store = Store::open(git.root())?;
    match (action, &args[2..]) {
        ("attach", [path]) => {
            meter::attach(&store, std::path::Path::new(path), false)?;
        }
        ("attach", [path, flag]) if flag == "--from-start" => {
            meter::attach(&store, std::path::Path::new(path), true)?;
        }
        ("sync", []) => {}
        ("show", []) => return summary(&store),
        _ => return Err(Error::Rejected("invalid usage command arguments".into())),
    }
    persist(&store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn put(store: &Store, kind: &str, id: &str, value: serde_json::Value) {
        store
            .write_artifact(&format!("native-{kind}-{id}.json"), &value.to_string())
            .unwrap();
    }
    pub(super) fn request(store: &Store, id: &str, model: &str) {
        put(
            store,
            "request",
            id,
            json!({"dispatch_id":id,"run_id":"run","role":"recommender","assessment":crate::delegation::test_payload().0,"decision":crate::delegation::test_payload().1,"selection":{"run_id":"run","host_session_id":"parent","role":"recommender","unit":null,"model":"m","effort":"high","requirements":{"capabilities":{},"depth":"deep"},"tools":[],"catalog_digest":"catalog","host_digest":"host","digest":"selection"},
            "profile":"deep","unit":null,"model":model,"effort":"high","cwd":"/tmp",
            "access":"read_only","coordinator_allowed":true,"prompt_digest":"x",
            "plan_digest":null,"base_head":"head","brief":"task","agent_id":null,"stop_required":false,"pool_scope":"parent","lane":"coordinator","soft_budget_secs":60,"hard_timeout_secs":180}),
        );
    }
    fn complete(store: &Store, id: &str, agent: &str, usage: serde_json::Value) {
        put(
            store,
            "completion",
            id,
            json!({"dispatch_id":id,"agent_id":agent,
            "final_message":"done","agent_stopped":true,"reported_usage":usage}),
        );
    }
    #[test]
    fn retries_partial_coverage_coordinator_and_idempotent_completion() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for (id, model) in [
            ("first", "astra"),
            ("retry", "astra"),
            ("child", "luna"),
            ("pending", "luna"),
        ] {
            request(&store, id, model);
        }
        let usage = json!({"input_tokens":100,"output_tokens":20,"cached_input_tokens":80});
        complete(&store, "retry", "coordinator", usage.clone());
        complete(&store, "retry", "coordinator", usage);
        complete(&store, "child", "agent-1", serde_json::Value::Null);
        put(
            &store,
            "stopped",
            "first",
            json!({"dispatch_id":"first","agent_id":"agent-0","all_work_stopped":true}),
        );
        let (total, models) = collect(&store).unwrap();
        assert_eq!(
            (total.requests, total.completions, total.incomplete),
            (4, 2, 2)
        );
        assert_eq!(
            (
                total.coordinator_completions,
                total.native_child_completions
            ),
            (1, 1)
        );
        assert_eq!(
            (
                total.usage_reported_completions,
                total.requests_without_reported_usage
            ),
            (1, 3)
        );
        assert_eq!(total.stopped_without_completion, 1);
        assert_eq!(
            (
                total.coordinator_usage_reported_completions,
                total.native_child_usage_reported_completions
            ),
            (1, 0)
        );
        assert_eq!(models["astra"].reported_input_tokens, 100);
        assert_eq!(
            (
                total.reported_output_tokens,
                total.reported_cached_input_tokens
            ),
            (20, 80)
        );
    }
    #[test]
    fn spawn_refusal_and_recovery_are_distinct_from_completed_work() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        request(&store, "refused", "luna");
        put(
            &store,
            "failure",
            "refused",
            json!({"dispatch_id":"refused",
            "message":"agent thread limit reached","no_agent_created":true}),
        );
        put(
            &store,
            "resume",
            "refused",
            json!({"dispatch_id":"refused",
            "recovery_evidence":"host confirmed release of owned thread 42"}),
        );
        let report = summary(&store).unwrap();
        assert_eq!(report["total"]["dispatch_failures"], 1);
        assert_eq!(report["total"]["confirmed_no_child_failures"], 1);
        assert_eq!(report["total"]["observed_recoveries"], 1);
        assert_eq!(report["total"]["native_child_completions"], 0);
        assert_eq!(report["total_billed_cost"], "unknown");
        complete(&store, "refused", "invented-child", serde_json::Value::Null);
        assert!(
            summary(&store).is_err(),
            "a refused spawn also counted as completed"
        );
    }

    #[test]
    fn bad_evidence_is_an_error_instead_of_zero_usage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        request(&store, "a", "astra");
        complete(
            &store,
            "a",
            "coordinator",
            json!({"input_tokens":1,"output_tokens":0,"cached_input_tokens":2}),
        );
        assert!(summary(&store).is_err());
        complete(
            &store,
            "a",
            "coordinator",
            json!({"input_tokens":u64::MAX,"output_tokens":0,"cached_input_tokens":0}),
        );
        request(&store, "b", "astra");
        complete(
            &store,
            "b",
            "coordinator",
            json!({"input_tokens":1,"output_tokens":0,"cached_input_tokens":0}),
        );
        assert!(summary(&store).is_err());
        store
            .write_artifact("native-completion-a.json", "{")
            .unwrap();
        assert!(summary(&store).is_err());
    }
}
