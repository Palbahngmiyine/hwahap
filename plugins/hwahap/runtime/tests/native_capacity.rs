//! Capacity recovery uses durable native dispatches and real Git, without live models.
#![cfg(unix)]
mod common;

use common::{git, Fixture};
use hwahap::native::{
    NativeCompletion, NativeDispatch, NativeFailure, NativeHost, NativeInput, NativeRegistration,
    NativeResume, NativeStopped,
};

async fn setup() -> (Fixture, NativeHost, NativeDispatch) {
    let fixture = Fixture::new();
    fixture
        .engine()
        .step(Some("Inspect this repository"), None)
        .await
        .unwrap();
    let host = NativeHost::default();
    let request = dispatch(&host, &fixture).await;
    (fixture, host, request)
}

async fn dispatch(host: &NativeHost, fixture: &Fixture) -> NativeDispatch {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let progress = host
                .advance(&fixture.repo, NativeInput::default())
                .await
                .unwrap();
            if progress.outcome.next == "native_dispatch" {
                break progress.dispatch.unwrap();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

fn failure(request: &NativeDispatch, no_agent_created: bool) -> NativeFailure {
    NativeFailure {
        dispatch_id: request.dispatch_id.clone(),
        message: "agent thread limit reached".into(),
        no_agent_created,
    }
}

fn resume(request: &NativeDispatch) -> NativeInput {
    NativeInput {
        resume: Some(NativeResume {
            dispatch_id: request.dispatch_id.clone(),
            recovery_evidence: format!(
                "host capacity check after {} reports a free slot",
                request.dispatch_id
            ),
        }),
        ..Default::default()
    }
}

fn requests(fixture: &Fixture) -> usize {
    fixture
        .repo
        .join(".hwahap/artifacts")
        .read_dir()
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("native-request-")
        })
        .count()
}

#[tokio::test]
async fn confirmed_no_child_failure_survives_restart_and_requires_fresh_recovery_evidence() {
    let (fixture, host, mut request) = setup().await;
    let run = std::fs::read(fixture.repo.join(".hwahap/run.json")).unwrap();
    let head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    let failed = failure(&request, true);
    for _ in 0..2 {
        let paused = host
            .advance(
                &fixture.repo,
                NativeInput {
                    dispatch_failure: Some(failed.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(paused.outcome.next, "native_paused");
        assert_eq!(
            paused.dispatch.unwrap().failure.unwrap().message,
            failed.message
        );
    }
    let mut altered = failed.clone();
    altered.message = "different failure".into();
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                dispatch_failure: Some(altered),
                ..Default::default()
            }
        )
        .await
        .is_err());
    host.shutdown().await;
    drop(host);
    let host = NativeHost::default();
    for _ in 0..3 {
        assert_eq!(
            host.status(&fixture.repo).await.unwrap().outcome.next,
            "native_paused"
        );
        assert_eq!(
            host.advance(&fixture.repo, NativeInput::default())
                .await
                .unwrap()
                .outcome
                .next,
            "native_paused"
        );
    }
    assert_eq!(requests(&fixture), 1);
    assert_eq!(
        std::fs::read(fixture.repo.join(".hwahap/run.json")).unwrap(),
        run
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), head);
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                request: Some("replace it".into()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    let evidence = resume(&request).resume.unwrap().recovery_evidence;
    let mut blank = resume(&request);
    blank.resume.as_mut().unwrap().recovery_evidence = " ".into();
    assert!(host.advance(&fixture.repo, blank).await.is_err());
    for expected in 2..=3 {
        assert_eq!(
            host.advance(&fixture.repo, resume(&request))
                .await
                .unwrap()
                .outcome
                .next,
            "continue"
        );
        let next = dispatch(&host, &fixture).await;
        assert_eq!(next.run_id, request.run_id);
        assert_ne!(next.dispatch_id, request.dispatch_id);
        assert_eq!(requests(&fixture), expected);
        request = next;
        host.advance(
            &fixture.repo,
            NativeInput {
                dispatch_failure: Some(failure(&request, true)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let mut reused = resume(&request);
    reused.resume.as_mut().unwrap().recovery_evidence = evidence;
    assert!(host.advance(&fixture.repo, reused).await.is_err());
    assert_eq!(requests(&fixture), 3);
    host.shutdown().await;
}

#[tokio::test]
async fn uncertain_dispatch_failure_requires_exact_stop_ack_before_recovery() {
    let (fixture, host, request) = setup().await;
    let stopped = host
        .advance(
            &fixture.repo,
            NativeInput {
                dispatch_failure: Some(failure(&request, false)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(stopped.outcome.next, "native_stop");
    assert!(stopped.dispatch.unwrap().stop_required);
    assert!(host.advance(&fixture.repo, resume(&request)).await.is_err());
    let mut ack = NativeStopped {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: None,
        all_work_stopped: false,
    };
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                stopped: Some(ack.clone()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    ack.all_work_stopped = true;
    ack.dispatch_id = "wrong".into();
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                stopped: Some(ack.clone()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    ack.dispatch_id = request.dispatch_id.clone();
    let recovered = host
        .advance(
            &fixture.repo,
            NativeInput {
                stopped: Some(ack),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered.outcome.next, "continue");
    host.shutdown().await;
}

#[tokio::test]
async fn failure_actions_reject_wrong_dispatch_registered_child_and_mixed_inputs() {
    let (fixture, host, request) = setup().await;
    let mut wrong = failure(&request, true);
    wrong.dispatch_id = "wrong".into();
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                dispatch_failure: Some(wrong),
                ..Default::default()
            }
        )
        .await
        .is_err());
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                dispatch_failure: Some(failure(&request, true)),
                request: Some("replacement".into()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    let mut mixed = resume(&request);
    mixed.dispatch_failure = Some(failure(&request, true));
    assert!(host.advance(&fixture.repo, mixed).await.is_err());
    host.advance(
        &fixture.repo,
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
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                dispatch_failure: Some(failure(&request, true)),
                ..Default::default()
            }
        )
        .await
        .is_err());
    let pending = host.status(&fixture.repo).await.unwrap();
    assert_eq!(pending.outcome.next, "native_wait");
    assert_eq!(
        pending.dispatch.unwrap().agent_id.as_deref(),
        Some("child-1")
    );
    host.shutdown().await;
}

#[tokio::test]
async fn failure_evidence_recovers_a_crash_before_the_pending_pause_write() {
    let (fixture, host, request) = setup().await;
    let path = fixture.repo.join(".hwahap/artifacts/native-pending.json");
    let before = std::fs::read(&path).unwrap();
    host.advance(
        &fixture.repo,
        NativeInput {
            dispatch_failure: Some(failure(&request, true)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    host.shutdown().await;
    drop(host);
    // Failure evidence reached disk, but the pending snapshot write did not survive the crash.
    std::fs::write(&path, before).unwrap();
    let replacement = NativeHost::default();
    for progress in [
        replacement.status(&fixture.repo).await.unwrap(),
        replacement
            .advance(&fixture.repo, NativeInput::default())
            .await
            .unwrap(),
    ] {
        assert_eq!(progress.outcome.next, "native_paused");
        let failed = progress.dispatch.unwrap();
        assert!(failed.failure.unwrap().no_agent_created);
    }
    let ack = NativeStopped {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: None,
        all_work_stopped: true,
    };
    assert!(replacement
        .advance(
            &fixture.repo,
            NativeInput {
                stopped: Some(ack),
                ..Default::default()
            }
        )
        .await
        .is_err());
    assert_eq!(
        replacement
            .advance(&fixture.repo, resume(&request))
            .await
            .unwrap()
            .outcome
            .next,
        "continue"
    );
    replacement.shutdown().await;
}

fn failed_input(request: &NativeDispatch) -> NativeInput {
    NativeInput {
        dispatch_failure: Some(failure(request, true)),
        ..Default::default()
    }
}

#[tokio::test]
async fn recommender_capacity_recovery_preserves_completed_fact_finding() {
    let (fixture, host, first) = setup().await;
    let registered = NativeRegistration {
        dispatch_id: first.dispatch_id.clone(),
        agent_id: "fact-child".into(),
    };
    host.advance(
        &fixture.repo,
        NativeInput {
            registration: Some(registered),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let facts = r#"{"facts":[{"id":"F1","question":"what exists?","answer":"one seed file","sources":["src/existing.txt:1"]}]}"#;
    let completion = NativeCompletion {
        dispatch_id: first.dispatch_id.clone(),
        agent_id: "fact-child".into(),
        final_message: serde_json::json!({"dispatch_id":first.dispatch_id,"result":serde_json::from_str::<serde_json::Value>(facts).unwrap()}).to_string(),
        agent_stopped: true,
        reported_usage: None,
    };
    host.advance(
        &fixture.repo,
        NativeInput {
            completion: Some(completion),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let recommender = dispatch(&host, &fixture).await;
    assert_eq!(recommender.role, "recommender");
    let store = hwahap::state::Store::open(&fixture.repo).unwrap();
    let saved = serde_json::to_value(store.read_plan().unwrap().unwrap().facts).unwrap();
    assert_eq!(saved[0]["id"], "F1");
    assert_eq!(saved[0]["sources"][0], "src/existing.txt:1");
    host.advance(&fixture.repo, failed_input(&recommender))
        .await
        .unwrap();
    host.shutdown().await;
    let replacement = NativeHost::default();
    replacement
        .advance(&fixture.repo, resume(&recommender))
        .await
        .unwrap();
    let retried = dispatch(&replacement, &fixture).await;
    assert_eq!(retried.role, "recommender");
    assert_eq!(retried.run_id, first.run_id);
    assert_eq!(requests(&fixture), 3);
    assert_eq!(
        serde_json::to_value(store.read_plan().unwrap().unwrap().facts).unwrap(),
        saved
    );
    replacement.shutdown().await;
}

#[tokio::test]
async fn failure_and_resume_write_errors_preserve_ownership_and_exact_replays() {
    let (fixture, host, request) = setup().await;
    let artifacts = fixture.repo.join(".hwahap/artifacts");
    let pending = artifacts.join("native-pending.json");
    let before = std::fs::read(&pending).unwrap();
    let failure_path = artifacts.join(format!("native-failure-{}.json", request.dispatch_id));
    std::fs::create_dir(&failure_path).unwrap();
    assert!(host
        .advance(&fixture.repo, failed_input(&request))
        .await
        .is_err());
    assert_eq!(std::fs::read(&pending).unwrap(), before);
    assert!(!artifacts
        .join(format!("native-completion-{}.json", request.dispatch_id))
        .exists());
    std::fs::remove_dir(&failure_path).unwrap();
    let paused = host
        .advance(&fixture.repo, failed_input(&request))
        .await
        .unwrap();
    assert_eq!(paused.outcome.next, "native_paused");
    let paused_snapshot = std::fs::read(&pending).unwrap();
    let resume_path = artifacts.join(format!("native-resume-{}.json", request.dispatch_id));
    std::fs::create_dir(&resume_path).unwrap();
    assert!(host.advance(&fixture.repo, resume(&request)).await.is_err());
    assert_eq!(std::fs::read(&pending).unwrap(), paused_snapshot);
    assert_eq!(
        host.status(&fixture.repo).await.unwrap().outcome.next,
        "native_paused"
    );
    std::fs::remove_dir(&resume_path).unwrap();
    host.advance(&fixture.repo, resume(&request)).await.unwrap();
    assert!(resume_path.is_file());
    host.shutdown().await;
    // Recovery evidence reached disk, but clearing the pending pause did not survive a crash.
    std::fs::write(&pending, paused_snapshot).unwrap();
    let replacement = NativeHost::default();
    for _ in 0..2 {
        assert_eq!(
            replacement
                .advance(&fixture.repo, resume(&request))
                .await
                .unwrap()
                .outcome
                .next,
            "continue"
        );
        assert!(!pending.exists());
        assert_eq!(requests(&fixture), 1);
    }
    let next = dispatch(&replacement, &fixture).await;
    replacement
        .advance(&fixture.repo, resume(&request))
        .await
        .unwrap();
    assert_eq!(
        replacement
            .status(&fixture.repo)
            .await
            .unwrap()
            .dispatch
            .unwrap()
            .dispatch_id,
        next.dispatch_id
    );
    assert_eq!(requests(&fixture), 2);
    replacement.shutdown().await;
}

#[tokio::test]
async fn explicit_capacity_recoveries_still_exhaust_the_durable_request_budget() {
    let fixture = Fixture::new();
    fixture
        .engine()
        .step(Some("Inspect this repository"), None)
        .await
        .unwrap();
    std::fs::write(
        fixture.repo.join(".hwahap/config.toml"),
        "[limits]\nnative_max_calls = 2\n",
    )
    .unwrap();
    let mut host = NativeHost::default();
    let mut run_id = String::new();
    for expected in 1..=2 {
        let request = dispatch(&host, &fixture).await;
        assert_eq!(requests(&fixture), expected);
        assert!(request.agent_id.is_none());
        if run_id.is_empty() {
            run_id = request.run_id.clone();
        }
        assert_eq!(request.run_id, run_id);
        host.advance(&fixture.repo, failed_input(&request))
            .await
            .unwrap();
        host.advance(&fixture.repo, resume(&request)).await.unwrap();
        // A replacement host must count earlier dispatches, including failed spawn requests.
        host.shutdown().await;
        host = NativeHost::default();
    }
    let blocked = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let progress = host
                .advance(&fixture.repo, NativeInput::default())
                .await
                .unwrap();
            assert!(
                progress.dispatch.is_none(),
                "a third request escaped the budget"
            );
            if progress.outcome.next == "blocked" {
                break progress.outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        blocked.message.contains("budget 2 is exhausted"),
        "{}",
        blocked.message
    );
    assert_eq!(blocked.run_id, run_id);
    assert_eq!(requests(&fixture), 2);
    assert_eq!(
        host.status(&fixture.repo).await.unwrap().outcome.state,
        "blocked"
    );
    let evidence =
        hwahap::cost::summary(&hwahap::state::Store::open(&fixture.repo).unwrap()).unwrap();
    assert_eq!(evidence["total"]["completions"], 0);
    assert_eq!(evidence["total"]["dispatch_failures"], 2);
    host.shutdown().await;
}
