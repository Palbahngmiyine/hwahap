//! A controlled host never releases its three child slots; all later work must reuse them.
#![cfg(unix)]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use common::Fixture;
use hwahap::native::{
    NativeCompletion, NativeDispatch, NativeLane, NativeRegistration, NativeSessions,
};
use hwahap::profile::{Profiles, Role};
use hwahap::session::SessionSpec;
use hwahap::state::Store;

async fn pending(broker: &NativeSessions) -> NativeDispatch {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(dispatch) = broker.dispatch().unwrap() {
                break dispatch;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn three_occupied_child_slots_complete_three_hundred_jobs_without_replacement_spawns() {
    let fixture = Fixture::new();
    fixture
        .engine()
        .step(Some("Inspect this repository"), None)
        .await
        .unwrap();
    let mut occupied = BTreeMap::new();
    let mut dispatch_ids = BTreeSet::new();
    let (mut spawns, mut followups) = (0, 0);
    let mut pool_scope = None;
    for unit in 1..=100 {
        for (role, lane, label, model) in [
            (
                Role::FactFinder,
                NativeLane::Worker,
                "worker",
                "gpt-5.6-luna",
            ),
            (
                Role::UnitReviewer,
                NativeLane::Critic,
                "critic",
                "gpt-6-astra",
            ),
            (
                Role::FinalReview,
                NativeLane::Auditor,
                "auditor",
                "gpt-6-astra",
            ),
        ] {
            // Recreate the broker for every job so in-memory reuse cannot satisfy this test.
            let broker = Arc::new(
                NativeSessions::new(
                    Store::open(&fixture.repo).unwrap(),
                    Profiles::defaults(),
                    1000,
                    30,
                )
                .with_host_session_id("controlled-parent-task".to_string()),
            );
            let spec = SessionSpec {
                cwd: fixture.repo.clone(),
                role,
                unit: Some(format!("U{unit}")),
                prompt: format!("Perform {label} job for U{unit}"),
            };
            let worker = broker.clone();
            let task = tokio::spawn(async move { worker.execute(&spec).await });
            let request = pending(&broker).await;
            assert!(dispatch_ids.insert(request.dispatch_id.clone()));
            assert_eq!(request.lane, lane);
            assert_eq!(request.model, model);
            assert!(!request.pool_scope.is_empty());
            assert_eq!(
                pool_scope.get_or_insert(request.pool_scope.clone()),
                &request.pool_scope
            );
            assert!(
                request.agent_id.is_none(),
                "each job needs explicit registration"
            );
            let agent_id = match &request.reuse_agent_id {
                Some(id) => {
                    assert_eq!(occupied.get(label), Some(id));
                    followups += 1;
                    id.clone()
                }
                None => {
                    assert!(
                        occupied.len() < 3,
                        "controlled host has no fourth thread slot"
                    );
                    assert!(!occupied.contains_key(label));
                    let id = format!("controlled-{label}");
                    occupied.insert(label, id.clone());
                    spawns += 1;
                    id
                }
            };
            broker
                .register(&NativeRegistration {
                    dispatch_id: request.dispatch_id.clone(),
                    agent_id: agent_id.clone(),
                })
                .unwrap();
            let result = serde_json::json!({"unit":unit,"role":label,"verdict":"pass"});
            let final_message =
                serde_json::json!({"dispatch_id":request.dispatch_id,"result":result}).to_string();
            broker
                .complete(NativeCompletion {
                    dispatch_id: request.dispatch_id,
                    agent_id,
                    final_message,
                    agent_stopped: true,
                    reported_usage: None,
                })
                .unwrap();
            let outcome = task.await.unwrap().unwrap();
            assert_eq!(
                outcome.final_message,
                result.to_string(),
                "the envelope must be unwrapped exactly"
            );
            broker.finish().unwrap();
        }
    }
    assert_eq!(occupied.len(), 3);
    assert_eq!(spawns, 3);
    assert_eq!(followups, 297);
    assert_eq!(dispatch_ids.len(), 300);
    assert_eq!(occupied.values().collect::<BTreeSet<_>>().len(), 3);
    let artifacts = Store::open(&fixture.repo).unwrap().artifacts_path();
    let count = artifacts
        .read_dir()
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("native-completion-")
        })
        .count();
    assert_eq!(
        count, 300,
        "every job must have durable completion evidence"
    );
}

type Job = tokio::task::JoinHandle<hwahap::error::Result<hwahap::session::SessionOutcome>>;

async fn start_job(fixture: &Fixture, role: Role) -> (Arc<NativeSessions>, NativeDispatch, Job) {
    let broker = Arc::new(
        NativeSessions::new(
            Store::open(&fixture.repo).unwrap(),
            Profiles::defaults(),
            1000,
            30,
        )
        .with_host_session_id("guard-parent-task".into()),
    );
    let spec = SessionSpec {
        cwd: fixture.repo.clone(),
        role,
        unit: None,
        prompt: "return evidence".into(),
    };
    let worker = broker.clone();
    let task = tokio::spawn(async move { worker.execute(&spec).await });
    let request = pending(&broker).await;
    (broker, request, task)
}

fn registration(request: &NativeDispatch, agent: &str) -> NativeRegistration {
    NativeRegistration {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: agent.into(),
    }
}

fn completion(request: &NativeDispatch, agent: &str) -> NativeCompletion {
    NativeCompletion {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: agent.into(),
        final_message:
            serde_json::json!({"dispatch_id":request.dispatch_id,"result":{"done":true}})
                .to_string(),
        agent_stopped: true,
        reported_usage: None,
    }
}

async fn finish_job(broker: &NativeSessions, request: &NativeDispatch, task: Job, agent: &str) {
    broker.register(&registration(request, agent)).unwrap();
    broker.complete(completion(request, agent)).unwrap();
    assert_eq!(
        task.await.unwrap().unwrap().final_message,
        r#"{"done":true}"#
    );
    broker.finish().unwrap();
}

async fn pool_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture
        .engine()
        .step(Some("Inspect this repository"), None)
        .await
        .unwrap();
    fixture
}

#[tokio::test]
async fn pool_cross_lane_registration_cannot_poison_the_pending_identity() {
    let fixture = pool_fixture().await;
    let (broker, worker, task) = start_job(&fixture, Role::FactFinder).await;
    finish_job(&broker, &worker, task, "author").await;
    let (broker, critic, task) = start_job(&fixture, Role::UnitReviewer).await;
    let path = Store::open(&fixture.repo)
        .unwrap()
        .artifacts_path()
        .join("native-pending.json");
    let before = std::fs::read(&path).unwrap();
    assert!(broker.register(&registration(&critic, "author")).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(broker.dispatch().unwrap().unwrap().agent_id.is_none());
    finish_job(&broker, &critic, task, "reviewer").await;
}

#[tokio::test]
async fn pool_reused_child_rejects_stale_envelopes_and_unbound_plaintext() {
    let fixture = pool_fixture().await;
    let (broker, first, task) = start_job(&fixture, Role::FactFinder).await;
    finish_job(&broker, &first, task, "worker").await;
    let (broker, reused, task) = start_job(&fixture, Role::FactFinder).await;
    assert_eq!(reused.reuse_agent_id.as_deref(), Some("worker"));
    broker.register(&registration(&reused, "worker")).unwrap();
    for text in [
        r#"{"done":true}"#.into(),
        completion(&first, "worker").final_message,
    ] {
        let mut stale = completion(&reused, "worker");
        stale.final_message = text;
        assert!(broker.complete(stale).is_err());
        assert!(!task.is_finished());
        let file = format!("native-completion-{}.json", reused.dispatch_id);
        assert!(!Store::open(&fixture.repo)
            .unwrap()
            .artifacts_path()
            .join(file)
            .exists());
    }
    finish_job(&broker, &reused, task, "worker").await;
}

#[tokio::test]
async fn pool_write_failures_preserve_pending_identity_and_allow_exact_recovery() {
    use std::os::unix::fs::PermissionsExt;
    for completing in [false, true] {
        let fixture = pool_fixture().await;
        let (broker, request, task) = start_job(&fixture, Role::FactFinder).await;
        let registered = registration(&request, "worker");
        if completing {
            broker.register(&registered).unwrap();
        }
        let state = fixture.repo.join(".hwahap");
        let permissions = std::fs::metadata(&state).unwrap().permissions();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap();
        let failed = if completing {
            broker.complete(completion(&request, "worker"))
        } else {
            broker.register(&registered)
        };
        std::fs::set_permissions(&state, permissions).unwrap();
        assert!(failed.is_err(), "pool persistence unexpectedly succeeded");
        assert!(
            !task.is_finished(),
            "result delivered before the pool write"
        );
        let pending: serde_json::Value = serde_json::from_slice(
            &std::fs::read(state.join("artifacts/native-pending.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(pending["dispatch"]["agent_id"], "worker");
        assert_eq!(pending["completion"].is_object(), completing);
        finish_job(&broker, &request, task, "worker").await;
        let (next_broker, next, next_task) = start_job(&fixture, Role::FactFinder).await;
        assert_eq!(next.reuse_agent_id.as_deref(), Some("worker"));
        finish_job(&next_broker, &next, next_task, "worker").await;
    }
}

#[tokio::test]
async fn pool_model_or_effort_changes_refuse_reuse_and_replacement() {
    let fixture = pool_fixture().await;
    let (broker, first, task) = start_job(&fixture, Role::FactFinder).await;
    finish_job(&broker, &first, task, "worker").await;
    let artifacts = Store::open(&fixture.repo).unwrap().artifacts_path();
    let before = artifacts.read_dir().unwrap().count();
    for (model, effort) in [("other-model", "medium"), ("gpt-5.6-luna", "high")] {
        let config = format!("[profiles.economy]\nmodel = {model:?}\neffort = {effort:?}\n[profiles.critic]\nmodel = \"gpt-6-astra\"\neffort = \"high\"\n[profiles.deep]\nmodel = \"gpt-6-astra\"\neffort = \"high\"\n");
        let broker = NativeSessions::new(
            Store::open(&fixture.repo).unwrap(),
            Profiles::from_toml(&config).unwrap(),
            1000,
            30,
        )
        .with_host_session_id("guard-parent-task".into());
        let spec = SessionSpec {
            cwd: fixture.repo.clone(),
            role: Role::FactFinder,
            unit: None,
            prompt: "return evidence".into(),
        };
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), broker.execute(&spec))
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            error.to_string().contains("model/effort changed"),
            "{error}"
        );
        assert!(broker.dispatch().unwrap().is_none());
        assert_eq!(artifacts.read_dir().unwrap().count(), before);
    }
}

#[tokio::test]
async fn unregistered_orphan_retains_discovered_child_without_cross_lane_reuse() {
    use hwahap::native::{NativeHost, NativeInput, NativeStopped};
    let fixture = pool_fixture().await;
    let (broker, worker, task) = start_job(&fixture, Role::FactFinder).await;
    finish_job(&broker, &worker, task, "author").await;
    let (broker, orphan, task) = start_job(&fixture, Role::FinalReview).await;
    // The host created this auditor, then lost its continuation before registration.
    task.abort();
    let _ = task.await;
    drop(broker);
    let store = Store::open(&fixture.repo).unwrap();
    let pending = store.artifacts_path().join("native-pending.json");
    let before = std::fs::read(&pending).unwrap();
    let host = NativeHost::default();
    let stopped = |request: &NativeDispatch, id: Option<&str>| NativeInput {
        host_session_id: Some("guard-parent-task".into()),
        stopped: Some(NativeStopped {
            dispatch_id: request.dispatch_id.clone(),
            agent_id: id.map(str::to_owned),
            all_work_stopped: true,
        }),
        ..Default::default()
    };
    assert!(host
        .advance(&fixture.repo, stopped(&orphan, Some("author")))
        .await
        .is_err());
    assert_eq!(std::fs::read(&pending).unwrap(), before);
    let record = store
        .artifacts_path()
        .join(format!("native-stopped-{}.json", orphan.dispatch_id));
    std::fs::create_dir(&record).unwrap();
    assert!(host
        .advance(&fixture.repo, stopped(&orphan, Some("auditor")))
        .await
        .is_err());
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
    assert_eq!(
        saved["dispatch"]["agent_id"], "auditor",
        "discovered identity was lost when stop evidence failed"
    );
    std::fs::remove_dir(&record).unwrap();
    host.advance(&fixture.repo, stopped(&orphan, Some("auditor")))
        .await
        .unwrap();
    let (broker, reused, task) = start_job(&fixture, Role::FinalReview).await;
    assert_eq!(reused.reuse_agent_id.as_deref(), Some("auditor"));
    task.abort();
    let _ = task.await;
    drop(broker);
    let before = std::fs::read(&pending).unwrap();
    for id in [None, Some("another-child")] {
        assert!(host
            .advance(&fixture.repo, stopped(&reused, id))
            .await
            .is_err());
        assert_eq!(std::fs::read(&pending).unwrap(), before);
    }
    host.advance(&fixture.repo, stopped(&reused, Some("auditor")))
        .await
        .unwrap();
    let (broker, next, task) = start_job(&fixture, Role::FinalReview).await;
    assert_eq!(next.reuse_agent_id.as_deref(), Some("auditor"));
    finish_job(&broker, &next, task, "auditor").await;
}
