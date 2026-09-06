//! Stable parent ownership and pool retention across archived runs, without live agents.
#![cfg(unix)]
mod common;

use common::{Fixture, NOW};
use hwahap::native::{
    NativeCompletion, NativeHost, NativeInput, NativeRegistration, NativeSessions, NativeStopped,
};
use hwahap::profile::{Profiles, Role};
use hwahap::state::{RunState, Store};

const OWNER: &str = "stable-parent-task";

fn input() -> NativeInput {
    NativeInput {
        host_session_id: Some(OWNER.into()),
        ..Default::default()
    }
}

async fn start_run(fixture: &Fixture) {
    let engine = fixture.engine();
    engine
        .step(Some("Inspect the repository"), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn active_and_orphan_agents_reject_missing_or_different_parent_ownership() {
    let fixture = Fixture::new();
    start_run(&fixture).await;
    let host = NativeHost::default();
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let progress = host.advance(&fixture.repo, input()).await.unwrap();
            if let Some(request) = progress.dispatch {
                break request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let registered = NativeRegistration {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: "owned-worker".into(),
    };
    let mut register = input();
    register.registration = Some(registered.clone());
    host.advance(&fixture.repo, register).await.unwrap();
    let stopped = NativeStopped {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: Some("owned-worker".into()),
        all_work_stopped: true,
    };
    let pending = fixture.repo.join(".hwahap/artifacts/native-pending.json");
    let saved = std::fs::read(&pending).unwrap();
    let replacement = NativeHost::default();
    for orphaned in [false, true] {
        if orphaned {
            host.shutdown().await;
        }
        let target = if orphaned { &replacement } else { &host };
        for scope in [None, Some("another-parent".to_string())] {
            for action in 0..3 {
                let mut attempted = NativeInput {
                    host_session_id: scope.clone(),
                    ..Default::default()
                };
                match action {
                    0 => attempted.registration = Some(registered.clone()),
                    1 => {
                        attempted.completion = Some(NativeCompletion {
                            dispatch_id: request.dispatch_id.clone(),
                            agent_id: "owned-worker".into(),
                            final_message: "{}".into(),
                            agent_stopped: true,
                            reported_usage: None,
                        })
                    }
                    _ => attempted.stopped = Some(stopped.clone()),
                }
                let error = target
                    .advance(&fixture.repo, attempted)
                    .await
                    .err()
                    .expect("ownership bypass accepted");
                assert!(error.to_string().contains("another parent task"), "{error}");
                assert_eq!(std::fs::read(&pending).unwrap(), saved);
            }
        }
    }
    let mut recover = input();
    recover.stopped = Some(stopped);
    let recovered = replacement.advance(&fixture.repo, recover).await.unwrap();
    assert_eq!(recovered.outcome.next, "continue");
    assert!(!pending.exists());
    for scope in [None, Some("another-parent".into())] {
        let attempted = NativeInput {
            host_session_id: scope,
            ..Default::default()
        };
        assert!(replacement.advance(&fixture.repo, attempted).await.is_err());
    }
    assert!(!pending.exists());
}

#[tokio::test]
async fn archived_runs_keep_three_pool_identities_for_the_same_parent_task() {
    let fixture = Fixture::new();
    start_run(&fixture).await;
    let store = Store::open(&fixture.repo).unwrap();
    let first_run = store.read_run().unwrap().unwrap().run_id;
    let mut first_dispatches = Vec::new();
    for generation in 0..2 {
        for (role, agent) in [
            (Role::FactFinder, "worker"),
            (Role::UnitReviewer, "critic"),
            (Role::FinalReview, "auditor"),
        ] {
            let broker = std::sync::Arc::new(
                NativeSessions::new(store.clone(), Profiles::defaults(), 1000, 30)
                    .with_host_session_id(OWNER.into()),
            );
            let spec = hwahap::session::SessionSpec {
                cwd: fixture.repo.clone(),
                role,
                unit: None,
                prompt: "return evidence".into(),
            };
            let worker = broker.clone();
            let task = tokio::spawn(async move { worker.execute(&spec).await });
            let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if let Some(request) = broker.dispatch().unwrap() {
                        break request;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            if generation == 0 {
                first_dispatches.push(request.dispatch_id.clone());
            }
            assert_eq!(
                request.reuse_agent_id.as_deref(),
                (generation == 1).then_some(agent)
            );
            assert_eq!(request.run_id == first_run, generation == 0);
            broker
                .register(&NativeRegistration {
                    dispatch_id: request.dispatch_id.clone(),
                    agent_id: agent.into(),
                })
                .unwrap();
            broker.complete(NativeCompletion {
                final_message: serde_json::json!({"dispatch_id":request.dispatch_id,"result":{"generation":generation}}).to_string(),
                dispatch_id: request.dispatch_id, agent_id: agent.into(), agent_stopped: true, reported_usage: None,
            }).unwrap();
            task.await.unwrap().unwrap();
            broker.finish().unwrap();
        }
        if generation == 0 {
            let digest = hwahap::canonical::Digest::of_bytes(OWNER.as_bytes());
            let pool = store.root().join(format!("native-pool-{digest}.json"));
            let pool_bytes = std::fs::read(&pool).unwrap();
            let mut run = store.read_run().unwrap().unwrap();
            run.state = RunState::Blocked {
                reason: "fixture run completed for archive-boundary test".into(),
            };
            store
                .write_run(&hwahap::clock::FixedClock::new(NOW), &run)
                .unwrap();
            start_run(&fixture).await;
            assert_eq!(std::fs::read(&pool).unwrap(), pool_bytes);
            assert!(store.archived_run_ids().unwrap().contains(&first_run));
            let archived = store.root().join("archive").join(NOW.replace(':', "-"));
            for id in &first_dispatches {
                assert!(archived
                    .join("artifacts")
                    .join(format!("native-completion-{id}.json"))
                    .is_file());
            }
        }
    }
}
