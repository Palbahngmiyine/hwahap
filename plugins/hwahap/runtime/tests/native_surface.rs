//! Real Git and durable storage with a controlled host; no model access is implied.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use hwahap::engine::Engine;
use hwahap::native::{
    acknowledge_stopped, orphan, NativeCompletion, NativeDispatch, NativeRegistration,
    NativeSessions, NativeStopped,
};
use hwahap::profile::{Profiles, Role};
use hwahap::session::SessionSpec;
use hwahap::state::Store;

#[tokio::test]
async fn starting_request_and_user_reply_are_rejected_before_any_state_is_created() {
    use hwahap::native::{NativeHost, NativeInput};

    for plan_only in [false, true] {
        for reply in ["Planning only; do not implement", ""] {
            let temp = tempfile::tempdir().unwrap();
            let host = NativeHost::default();
            let error = host
                .advance(
                    temp.path(),
                    NativeInput {
                        request: Some("Implement log output".into()),
                        user_input: Some(reply.into()),
                        plan_only,
                        host_session_id: Some("input-validation-parent".into()),
                        ..Default::default()
                    },
                )
                .await
                .err()
                .expect("neither message may be silently dropped");
            assert!(
                error.to_string().contains("request and user_input"),
                "{error}"
            );
            assert!(!temp.path().join(".hwahap").exists());
            host.shutdown().await;
        }
    }
}

async fn fixture(
    max_calls: u64,
    timeout: u64,
) -> (tempfile::TempDir, Arc<NativeSessions>, SessionSpec) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("README.md"), "# Minimal repository\n").unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "test@example.invalid"],
        vec!["config", "user.name", "Native Test"],
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Ignore runtime state without changing the repository's .gitignore.
    std::fs::write(temp.path().join(".git/info/exclude"), "/.hwahap/\n").unwrap();
    Engine::open(temp.path())
        .unwrap()
        .step(Some("Inspect this repository"), None)
        .await
        .unwrap();
    let broker = Arc::new(NativeSessions::new(
        Store::open(temp.path()).unwrap(),
        Profiles::defaults(),
        max_calls,
        timeout,
    ));
    let spec = SessionSpec {
        cwd: temp.path().into(),
        role: Role::FactFinder,
        unit: None,
        prompt: "Return facts".into(),
    };
    (temp, broker, spec)
}

async fn dispatch(broker: &NativeSessions) -> NativeDispatch {
    tokio::time::timeout(Duration::from_secs(5), async {
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
async fn registered_result_is_durable_bound_and_consumed_once() {
    let (temp, broker, spec) = fixture(1, 30).await;
    let worker = broker.clone();
    let task = tokio::spawn(async move { worker.execute(&spec).await });
    let request = dispatch(&broker).await;
    let store = Store::open(temp.path()).unwrap();
    assert!(orphan(&store).unwrap().unwrap().stop_required);
    assert!(request.reuse_agent_id.is_none());
    assert_eq!(request.lane, hwahap::native::NativeLane::Worker);
    assert!(!request.base_head.is_empty());
    let mut completion = NativeCompletion {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: "child-1".into(),
        final_message: serde_json::json!({"dispatch_id":request.dispatch_id,"result":{"facts":[]}})
            .to_string(),
        agent_stopped: true,
        reported_usage: None,
    };
    assert!(
        broker.complete(completion.clone()).is_err(),
        "unregistered agent accepted"
    );
    broker
        .register(&NativeRegistration {
            dispatch_id: request.dispatch_id.clone(),
            agent_id: "child-1".into(),
        })
        .unwrap();
    completion.agent_id = "other-child".into();
    assert!(broker.complete(completion.clone()).is_err());
    completion.agent_id = "child-1".into();
    let mut unbound = completion.clone();
    unbound.final_message = r#"{"facts":[]}"#.into();
    assert!(
        broker.complete(unbound).is_err(),
        "fresh children must bind their result too"
    );
    broker.complete(completion.clone()).unwrap();
    broker.complete(completion.clone()).unwrap();
    completion.final_message = "changed".into();
    assert!(broker.complete(completion).is_err());
    let outcome = task.await.unwrap().unwrap();
    assert_eq!(outcome.final_message, r#"{"facts":[]}"#);
    assert!(store
        .artifacts_path()
        .join(format!("native-completion-{}.json", request.dispatch_id))
        .exists());
    broker.finish().unwrap();
    assert!(orphan(&store).unwrap().is_none());
    let spec = SessionSpec {
        cwd: temp.path().into(),
        role: Role::FactFinder,
        unit: None,
        prompt: "again".into(),
    };
    assert!(broker
        .execute(&spec)
        .await
        .unwrap_err()
        .to_string()
        .contains("budget"));
}

#[tokio::test]
async fn completion_write_failures_do_not_deliver_and_exact_retry_delivers_once() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    for fail_pending in [false, true] {
        let (temp, broker, spec) = fixture(5, 30).await;
        let store = Store::open(temp.path()).unwrap();
        let mut execution = Box::pin(broker.execute(&spec));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(execution.as_mut().poll(&mut cx).is_pending());
        let request = broker.dispatch().unwrap().unwrap();
        broker
            .register(&NativeRegistration {
                dispatch_id: request.dispatch_id.clone(),
                agent_id: "child-1".into(),
            })
            .unwrap();
        let completion = NativeCompletion {
            dispatch_id: request.dispatch_id.clone(),
            agent_id: "child-1".into(),
            final_message:
                serde_json::json!({"dispatch_id":request.dispatch_id,"result":{"facts":[]}})
                    .to_string(),
            agent_stopped: true,
            reported_usage: None,
        };
        let result_path = store
            .artifacts_path()
            .join(format!("native-completion-{}.json", request.dispatch_id));
        let pending_path = store.artifacts_path().join("native-pending.json");
        let blocked_path = if fail_pending {
            &pending_path
        } else {
            &result_path
        };
        if blocked_path.exists() {
            std::fs::remove_file(blocked_path).unwrap();
        }
        std::fs::create_dir(blocked_path).unwrap();
        assert!(broker.complete(completion.clone()).is_err());
        assert!(
            execution.as_mut().poll(&mut cx).is_pending(),
            "delivered after failed persistence"
        );
        assert_eq!(result_path.is_file(), fail_pending);
        std::fs::remove_dir(blocked_path).unwrap();

        broker.complete(completion.clone()).unwrap();
        let outcome = match execution.as_mut().poll(&mut cx) {
            Poll::Ready(Ok(outcome)) => outcome,
            other => panic!("retry did not deliver: {other:?}"),
        };
        assert_eq!(outcome.final_message, r#"{"facts":[]}"#);
        broker.complete(completion.clone()).unwrap();
        let saved: NativeCompletion =
            serde_json::from_slice(&std::fs::read(&result_path).unwrap()).unwrap();
        assert_eq!(saved, completion);
        let pending: serde_json::Value =
            serde_json::from_slice(&std::fs::read(pending_path).unwrap()).unwrap();
        assert_eq!(
            pending["completion"],
            serde_json::to_value(&completion).unwrap()
        );
        broker.finish().unwrap();
        assert!(orphan(&store).unwrap().is_none());
        assert!(broker.dispatch().unwrap().is_none());
    }
}

#[tokio::test]
async fn restart_requires_exact_stop_acknowledgment_before_recovery() {
    let (temp, broker, spec) = fixture(5, 30).await;
    let worker = broker.clone();
    let task = tokio::spawn(async move { worker.execute(&spec).await });
    let request = dispatch(&broker).await;
    broker
        .register(&NativeRegistration {
            dispatch_id: request.dispatch_id.clone(),
            agent_id: "child-1".into(),
        })
        .unwrap();
    task.abort();
    let _ = task.await;
    let store = Store::open(temp.path()).unwrap();
    let orphan = orphan(&store).unwrap().unwrap();
    assert_eq!(orphan.agent_id.as_deref(), Some("child-1"));
    let mut ack = NativeStopped {
        dispatch_id: request.dispatch_id,
        agent_id: None,
        all_work_stopped: true,
    };
    assert!(acknowledge_stopped(&store, &ack).is_err());
    ack.agent_id = Some("child-1".into());
    ack.all_work_stopped = false;
    assert!(acknowledge_stopped(&store, &ack).is_err());
    ack.all_work_stopped = true;
    acknowledge_stopped(&store, &ack).unwrap();
    assert!(hwahap::native::orphan(&store).unwrap().is_none());
}

#[tokio::test]
async fn deadline_does_not_claim_the_child_was_stopped() {
    let (temp, broker, spec) = fixture(5, 0).await;
    assert!(broker
        .execute(&spec)
        .await
        .unwrap_err()
        .to_string()
        .contains("deadline"));
    assert!(broker.dispatch().unwrap().unwrap().stop_required);
    assert!(broker.finish().is_err());
    assert!(orphan(&Store::open(temp.path()).unwrap())
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn timing_immediate_completion_does_not_wait_for_the_hard_deadline() {
    let (temp, broker, spec) = fixture(5, 180).await;
    let worker = broker.clone();
    let task = tokio::spawn(async move { worker.execute(&spec).await });
    let request = dispatch(&broker).await;
    assert_eq!(request.hard_timeout_secs, 180);
    broker
        .register(&NativeRegistration {
            dispatch_id: request.dispatch_id.clone(),
            agent_id: "fast-child".into(),
        })
        .unwrap();
    let message = r#"{"facts":[]}"#;
    broker
        .complete(NativeCompletion {
            dispatch_id: request.dispatch_id.clone(),
            agent_id: "fast-child".into(),
            final_message: serde_json::json!({"dispatch_id":request.dispatch_id,"result":serde_json::from_str::<serde_json::Value>(message).unwrap()}).to_string(),
            agent_stopped: true,
            reported_usage: None,
        })
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.final_message, message);
    let store = Store::open(temp.path()).unwrap();
    let timing = hwahap::native::timing::read(&store, &request.dispatch_id)
        .unwrap()
        .unwrap();
    assert_eq!(timing.outcome.as_deref(), Some("completed"));
    assert_eq!(timing.output_bytes, Some(serde_json::json!({"dispatch_id":request.dispatch_id,"result":serde_json::from_str::<serde_json::Value>(message).unwrap()}).to_string().len() as u64));
    assert!(timing.registered_at_ms.unwrap() >= timing.offered_at_ms);
    assert!(timing.finished_at_ms.unwrap() >= timing.registered_at_ms.unwrap());
    broker.finish().unwrap();
    assert!(orphan(&store).unwrap().is_none());
}

#[tokio::test]
async fn timing_short_deadline_keeps_a_registered_child_behind_the_stop_barrier() {
    let (temp, broker, spec) = fixture(5, 1).await;
    let worker = broker.clone();
    let task = tokio::spawn(async move { worker.execute(&spec).await });
    let request = dispatch(&broker).await;
    broker
        .register(&NativeRegistration {
            dispatch_id: request.dispatch_id.clone(),
            agent_id: "slow-child".into(),
        })
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("deadline"), "{error}");
    let store = Store::open(temp.path()).unwrap();
    let progress = hwahap::native::NativeHost::default()
        .status(temp.path())
        .await
        .unwrap();
    assert_eq!(progress.outcome.next, "native_stop");
    assert_eq!(
        progress.dispatch.unwrap().agent_id.as_deref(),
        Some("slow-child")
    );
    let timing = hwahap::native::timing::read(&store, &request.dispatch_id)
        .unwrap()
        .unwrap();
    assert_eq!(timing.outcome.as_deref(), Some("deadline"));
    assert!(timing.registered_at_ms.is_some());
    assert!(timing.finished_at_ms.is_some());
    assert!(timing.output_bytes.is_none());
    assert!(!store
        .artifacts_path()
        .join(format!("native-completion-{}.json", request.dispatch_id))
        .exists());
    assert!(broker.finish().is_err());
    let mut ack = NativeStopped {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: None,
        all_work_stopped: true,
    };
    assert!(acknowledge_stopped(&store, &ack).is_err());
    ack.agent_id = Some("slow-child".into());
    acknowledge_stopped(&store, &ack).unwrap();
    assert!(orphan(&store).unwrap().is_none());
    assert_eq!(
        hwahap::native::timing::read(&store, &request.dispatch_id)
            .unwrap()
            .unwrap(),
        timing
    );
}

#[tokio::test]
async fn timing_spawn_failure_records_no_registration_or_completion() {
    let (temp, broker, spec) = fixture(5, 180).await;
    let worker = broker.clone();
    let task = tokio::spawn(async move { worker.execute(&spec).await });
    let request = dispatch(&broker).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let store = Store::open(temp.path()).unwrap();
    hwahap::native::record_failure(
        &store,
        &hwahap::native::NativeFailure {
            dispatch_id: request.dispatch_id.clone(),
            message: "agent thread limit reached".into(),
            no_agent_created: true,
        },
    )
    .unwrap();
    let timing = hwahap::native::timing::read(&store, &request.dispatch_id)
        .unwrap()
        .unwrap();
    assert_eq!(timing.outcome.as_deref(), Some("spawn_failed"));
    assert!(timing.registered_at_ms.is_none());
    assert!(timing.finished_at_ms.unwrap() >= timing.offered_at_ms);
    assert!(timing.output_bytes.is_none());
    assert!(!store
        .artifacts_path()
        .join(format!("native-completion-{}.json", request.dispatch_id))
        .exists());
    let progress = hwahap::native::NativeHost::default()
        .status(temp.path())
        .await
        .unwrap();
    assert_eq!(progress.outcome.next, "native_paused");
    assert!(progress.dispatch.unwrap().agent_id.is_none());
}

#[tokio::test]
async fn coordinator_is_limited_to_astra_planning_roles() {
    let (_temp, broker, mut spec) = fixture(5, 30).await;
    spec.role = Role::Recommender;
    let worker = broker.clone();
    let task = tokio::spawn(async move { worker.execute(&spec).await });
    let request = dispatch(&broker).await;
    assert!(request.coordinator_allowed);
    broker
        .register(&NativeRegistration {
            dispatch_id: request.dispatch_id.clone(),
            agent_id: "coordinator".into(),
        })
        .unwrap();
    for text in [
        r#"{"recommendations":[]}"#.to_owned(),
        serde_json::json!({"dispatch_id":"stale", "result":{"recommendations":[]}}).to_string(),
    ] {
        assert!(broker
            .complete(NativeCompletion {
                dispatch_id: request.dispatch_id.clone(),
                agent_id: "coordinator".into(),
                final_message: text,
                agent_stopped: true,
                reported_usage: None,
            })
            .is_err());
    }
    let envelope = serde_json::json!({"dispatch_id":request.dispatch_id,
        "result":{"recommendations":[]}})
    .to_string();
    broker
        .complete(NativeCompletion {
            dispatch_id: request.dispatch_id,
            agent_id: "coordinator".into(),
            final_message: envelope,
            agent_stopped: true,
            reported_usage: None,
        })
        .unwrap();
    assert_eq!(
        task.await.unwrap().unwrap().final_message,
        r#"{"recommendations":[]}"#
    );
    broker.finish().unwrap();
    let (_temp, broker, spec) = fixture(5, 30).await;
    let worker = broker.clone();
    let task = tokio::spawn(async move { worker.execute(&spec).await });
    let request = dispatch(&broker).await;
    assert!(!request.coordinator_allowed);
    assert!(broker
        .register(&NativeRegistration {
            dispatch_id: request.dispatch_id,
            agent_id: "coordinator".into()
        })
        .is_err());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn incompatible_coordinator_model_is_rejected_before_dispatch() {
    let (temp, _, mut spec) = fixture(5, 30).await;
    spec.role = Role::Recommender;
    let profiles = Profiles::from_toml(
        "[profiles.economy]\nmodel = 'gpt-5.6-luna'\neffort = 'medium'\n\
         [profiles.critic]\nmodel = 'gpt-6-astra'\neffort = 'high'\n\
         [profiles.deep]\nmodel = 'other-model'\neffort = 'high'\n",
    )
    .unwrap();
    let store = Store::open(temp.path()).unwrap();
    let broker = NativeSessions::new(store.clone(), profiles, 5, 30);
    let error = broker.execute(&spec).await.unwrap_err().to_string();
    assert!(error.contains("Astra parent"), "{error}");
    assert!(broker.dispatch().unwrap().is_none());
    assert!(orphan(&store).unwrap().is_none());
    assert_eq!(
        hwahap::cost::summary(&store).unwrap()["total"]["requests"],
        0
    );
}

async fn host_dispatch(
    host: &hwahap::native::NativeHost,
    root: &std::path::Path,
) -> NativeDispatch {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = host
                .advance(root, hwahap::native::NativeInput::default())
                .await
                .unwrap();
            if let Some(dispatch) = result.dispatch {
                break dispatch;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn host_polling_preserves_one_dispatch_and_excludes_a_second_server() {
    use hwahap::native::{NativeHost, NativeInput};
    let (temp, _broker, _spec) = fixture(5, 30).await;
    let host = NativeHost::default();
    let request = host_dispatch(&host, temp.path()).await;
    let again = host
        .advance(temp.path(), NativeInput::default())
        .await
        .unwrap()
        .dispatch
        .unwrap();
    assert_eq!(request.dispatch_id, again.dispatch_id);
    assert!(NativeHost::default()
        .advance(temp.path(), NativeInput::default())
        .await
        .is_err());
    assert!(host.ship(temp.path(), "anything").await.is_err());
    assert!(host
        .advance(
            temp.path(),
            NativeInput {
                user_input: Some("change it".into()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    host.advance(
        temp.path(),
        NativeInput {
            registration: Some(NativeRegistration {
                dispatch_id: request.dispatch_id.clone(),
                agent_id: "native-1".into(),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        host.status(temp.path()).await.unwrap().outcome.next,
        "native_wait"
    );
    drop(host);
    tokio::task::yield_now().await;
    let replacement = NativeHost::default();
    let result = replacement
        .advance(temp.path(), NativeInput::default())
        .await
        .unwrap();
    assert_eq!(result.outcome.next, "native_stop");
    assert_eq!(result.dispatch.unwrap().dispatch_id, request.dispatch_id);
    replacement
        .advance(
            temp.path(),
            NativeInput {
                stopped: Some(NativeStopped {
                    dispatch_id: request.dispatch_id,
                    agent_id: Some("native-1".into()),
                    all_work_stopped: true,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(orphan(&Store::open(temp.path()).unwrap())
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn stop_ack_keeps_lock_and_pending_until_task_cancellation_finishes() {
    use hwahap::native::{NativeHost, NativeInput, RepoLock};
    use std::future::Future;
    use std::task::{Context, Waker};

    let (temp, _broker, _spec) = fixture(5, 30).await;
    let store = Store::open(temp.path()).unwrap();
    let host = NativeHost::default();
    let request = host_dispatch(&host, temp.path()).await;
    host.advance(
        temp.path(),
        NativeInput {
            registration: Some(NativeRegistration {
                dispatch_id: request.dispatch_id.clone(),
                agent_id: "child-1".into(),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let stopped_path = store
        .artifacts_path()
        .join(format!("native-stopped-{}.json", request.dispatch_id));
    let mut stopping = Box::pin(host.advance(
        temp.path(),
        NativeInput {
            stopped: Some(NativeStopped {
                dispatch_id: request.dispatch_id.clone(),
                agent_id: Some("child-1".into()),
                all_work_stopped: true,
            }),
            ..Default::default()
        },
    ));
    // This current-thread runtime cannot deliver cancellation until we yield to the task.
    assert!(stopping
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    assert!(RepoLock::acquire(temp.path()).is_err());
    assert_eq!(
        orphan(&store).unwrap().unwrap().dispatch_id,
        request.dispatch_id
    );
    assert!(!stopped_path.exists());

    stopping.await.unwrap();
    assert!(orphan(&store).unwrap().is_none());
    assert!(stopped_path.is_file());
    RepoLock::acquire(temp.path()).unwrap();
}

#[tokio::test]
async fn host_consumes_registered_output_and_accepts_identical_replay() {
    use hwahap::native::{NativeHost, NativeInput};
    let (temp, _broker, _spec) = fixture(5, 30).await;
    let host = NativeHost::default();
    let request = host_dispatch(&host, temp.path()).await;
    host.advance(
        temp.path(),
        NativeInput {
            registration: Some(NativeRegistration {
                dispatch_id: request.dispatch_id.clone(),
                agent_id: "native-1".into(),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let completion = NativeCompletion { dispatch_id: request.dispatch_id.clone(), agent_id: "native-1".into(), final_message: serde_json::json!({"dispatch_id":request.dispatch_id,"result":serde_json::from_str::<serde_json::Value>(r#"{"facts":[{"id":"F1","question":"what exists?","answer":"README.md exists","sources":["README.md:1"]}]}"#).unwrap()}).to_string(), agent_stopped: true, reported_usage: None };
    host.advance(
        temp.path(),
        NativeInput {
            completion: Some(completion.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tokio::task::yield_now().await;
    host.advance(
        temp.path(),
        NativeInput {
            completion: Some(completion),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let count = std::fs::read_dir(Store::open(temp.path()).unwrap().artifacts_path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("native-completion-")
        })
        .count();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn native_fact_finder_ignores_runtime_writes_but_detects_user_file_changes() {
    use hwahap::native::{NativeHost, NativeInput};
    for changed_path in [None, Some(".hwahap-other/new.txt"), Some("new.txt")] {
        let (temp, _broker, _spec) = fixture(5, 30).await;
        assert!(!temp.path().join(".gitignore").exists());
        let host = NativeHost::default();
        let request = host_dispatch(&host, temp.path()).await;
        assert_eq!(request.role, "fact_finder");
        host.advance(
            temp.path(),
            NativeInput {
                registration: Some(NativeRegistration {
                    dispatch_id: request.dispatch_id.clone(),
                    agent_id: "reader".into(),
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        if let Some(path) = changed_path {
            let path = temp.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "unexpected child change").unwrap();
        }
        let mut progress = host.advance(temp.path(), NativeInput {
            completion: Some(NativeCompletion {
                dispatch_id: request.dispatch_id.clone(), agent_id: "reader".into(),
                final_message: serde_json::json!({"dispatch_id":request.dispatch_id,"result":serde_json::from_str::<serde_json::Value>(r#"{"facts":[{"id":"F1","question":"what exists?","answer":"README.md","sources":["README.md:1"]}]}"#).unwrap()}).to_string(),
                agent_stopped: true, reported_usage: None,
            }), ..Default::default()
        }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while progress.dispatch.is_none() && progress.outcome.next != "blocked" {
                tokio::task::yield_now().await;
                progress = host
                    .advance(temp.path(), NativeInput::default())
                    .await
                    .unwrap();
            }
        })
        .await
        .unwrap();
        if changed_path.is_some() {
            assert_eq!(progress.outcome.next, "blocked");
            assert!(progress.outcome.message.contains("read-only fact_finder"));
        } else {
            assert_eq!(
                progress.outcome.next, "native_dispatch",
                "{}",
                progress.outcome.message
            );
            assert_eq!(progress.dispatch.unwrap().role, "recommender");
        }
    }
}

#[tokio::test]
async fn fingerprint_excludes_only_root_runtime_and_observes_untracked_modes() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, _broker, _spec) = fixture(5, 30).await;
    let git = hwahap::git::Git::open(temp.path()).unwrap();
    let baseline = git.fingerprint(temp.path()).unwrap();
    Store::open(temp.path())
        .unwrap()
        .write_artifact("host-write.json", "{}")
        .unwrap();
    assert_eq!(git.fingerprint(temp.path()).unwrap(), baseline);

    let nested = temp.path().join("nested/.hwahap/user.txt");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::write(&nested, "user-owned nested directory").unwrap();
    let with_nested = git.fingerprint(temp.path()).unwrap();
    assert_ne!(with_nested, baseline);
    let mode = std::fs::metadata(&nested).unwrap().permissions().mode();
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(mode ^ 0o100)).unwrap();
    assert_ne!(git.fingerprint(temp.path()).unwrap(), with_nested);
}
