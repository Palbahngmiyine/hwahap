use hwahap::{catalog::*, profile::Role};
#[test]
fn model_and_effort_names_are_replaceable_and_snapshot_bound() {
    let mut catalog = bundled();
    catalog.models[0].id = "model-a".into();
    catalog.models[0].efforts[0].name = "quick".into();
    let snapshot = CatalogSnapshot::new("run", catalog.clone()).unwrap();
    assert!(catalog.supports(
        "model-a",
        "quick",
        &catalog.role_requirements[&Role::FactFinder]
    ));
    catalog.models[0].id = "model-b".into();
    catalog.models[0].efforts[0].name = "deep".into();
    assert_ne!(snapshot, CatalogSnapshot::new("next", catalog).unwrap());
    assert_eq!(
        snapshot
            .catalog
            .candidates(&snapshot.catalog.role_requirements[&Role::FactFinder])[0],
        ("model-a", "quick")
    );
    snapshot.validate("run").unwrap();
    assert!(snapshot.validate("other").is_err());
}
#[test]
fn malformed_capabilities_and_duplicate_efforts_are_rejected() {
    let mut c = bundled();
    c.models[0].capabilities.insert("implementation".into(), 4);
    assert!(c.validate().is_err());
    let mut c = bundled();
    let duplicate = c.models[0].efforts[0].clone();
    c.models[0].efforts.push(duplicate);
    assert!(c.validate().is_err());
}

#[test]
fn catalog_file_changes_apply_only_to_new_run_snapshots() {
    use hwahap::{clock::FixedClock, state::Store};
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    std::fs::create_dir_all(store.root()).unwrap();
    let path = store.root().join("model-catalog.json");
    let mut catalog = bundled();
    catalog.models[0].id = "model-a".into();
    std::fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let a = pin(&store, &clock, "run-a").unwrap();
    catalog.models[0].id = "model-b".into();
    std::fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    assert_eq!(pin(&store, &clock, "run-a").unwrap(), a);
    assert_eq!(
        pin(&store, &clock, "run-b").unwrap().catalog.models[0].id,
        "model-b"
    );
    std::fs::write(&path, "broken json").unwrap();
    assert_eq!(snapshot(&store, "run-a").unwrap(), a);
    assert!(pin(&store, &clock, "run-c").is_err());
    std::fs::write(
        store.root().join("config.toml"),
        "catalog_path='missing.json'",
    )
    .unwrap();
    assert!(configured(&store).is_err());
}

fn observation() -> HostObservation {
    HostObservation {
        host_session_id: "parent".into(),
        observed_at: "2026-09-07T00:00:00Z".into(),
        source: "test host inventory".into(),
        parent_model: "model-a".into(),
        parent_effort: "deep".into(),
        available_slots: 2,
        models: [(
            "model-a".into(),
            ObservedModel {
                efforts: vec!["quick".into(), "deep".into()],
                tools: vec!["read".into()],
            },
        )]
        .into(),
    }
}
#[test]
fn observation_checks_freshness_identity_and_monotonic_updates() {
    use hwahap::{clock::FixedClock, state::Store};
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let observed = observation();
    let clock = FixedClock::new("2026-09-07T00:02:00Z");
    host::observe(&store, &clock, "parent", &observed).unwrap();
    assert!(host::current(&store, "parent", "2026-09-07T00:05:01Z").is_err());
    assert!(host::current(&store, "parent", "2026-09-06T23:59:59Z").is_err());
    assert!(host::current(&store, "other", "2026-09-07T00:02:00Z").is_err());
    let mut earlier = observed.clone();
    earlier.observed_at = "2026-09-06T23:59:59Z".into();
    assert!(host::observe(&store, &clock, "parent", &earlier).is_err());
    assert!(observed.available("model-a", "quick", &["read".into()]));
    assert!(!observed.available("model-a", "quick", &["write".into()]));
}

#[test]
fn selection_requires_exact_host_effort_tools_and_catalog_binding() {
    let mut catalog = bundled();
    catalog.models[0].id = "model-a".into();
    catalog.models[0].efforts[0].name = "quick".into();
    let snapshot = CatalogSnapshot::new("run", catalog).unwrap();
    let mut host = observation();
    host.models.get_mut("model-a").unwrap().tools = vec!["exec_command".into()];
    let selected =
        Selection::new(&snapshot, &host, Role::FactFinder, None, "model-a", "quick").unwrap();
    assert!(Selection::new(
        &snapshot,
        &host,
        Role::Implementer,
        None,
        "model-a",
        "quick"
    )
    .is_err());
    let mut changed = selected.clone();
    changed.effort = "deep".into();
    assert!(changed.verify().is_err());
    host.models.get_mut("model-a").unwrap().efforts.clear();
    assert!(selected.verify_observation(&host).is_err());
}

#[cfg(unix)]
mod common;
#[cfg(unix)]
fn catalog_observation(catalog: &Catalog, parent: &str, slots: u32) -> HostObservation {
    use hwahap::clock::{Clock, SystemClock};
    HostObservation {
        host_session_id: parent.into(),
        observed_at: SystemClock.now(),
        source: "native transport fixture".into(),
        parent_model: "gpt-6-astra".into(),
        parent_effort: "high".into(),
        available_slots: slots,
        models: catalog
            .models
            .iter()
            .map(|m| {
                (
                    m.id.clone(),
                    ObservedModel {
                        efforts: m.efforts.iter().map(|e| e.name.clone()).collect(),
                        tools: vec!["exec_command".into(), "apply_patch".into()],
                    },
                )
            })
            .collect(),
    }
}
#[cfg(unix)]
#[tokio::test]
async fn native_dispatch_uses_catalog_and_keeps_bound_worker() {
    use hwahap::{
        clock::SystemClock,
        native::{NativeCompletion, NativeRegistration, NativeSessions},
        session::SessionSpec,
        state::Store,
    };
    use std::sync::Arc;
    let fixture = common::Fixture::new();
    let store = Store::open(&fixture.repo).unwrap();
    let mut catalog = bundled();
    catalog.models[0].id = "model-a".into();
    catalog.models[0].efforts[0].name = "quick".into();
    std::fs::create_dir_all(store.root()).unwrap();
    let path = store.root().join("model-catalog.json");
    std::fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    fixture
        .engine()
        .start_planning("Inspect repository", true)
        .unwrap();
    common::fixture_assessments(&store);
    let observed = catalog_observation(&catalog, "parent", 1);
    host::observe(&store, &SystemClock, "parent", &observed).unwrap();
    let broker =
        Arc::new(NativeSessions::new(store.clone(), 10, 30).with_host_session_id("parent".into()));
    let spec = SessionSpec {
        assessment: None,
        cwd: fixture.repo.clone(),
        role: Role::FactFinder,
        unit: None,
        prompt: "Inspect".into(),
    };
    for index in 0..2 {
        let copy = broker.clone();
        let input = spec.clone();
        let task = tokio::spawn(async move { copy.execute(&input).await });
        let dispatch = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(d) = broker.dispatch().unwrap() {
                    break d;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!((&*dispatch.model, &*dispatch.effort), ("model-a", "quick"));
        assert_eq!(dispatch.reuse_agent_id.is_some(), index == 1);
        broker
            .register(&NativeRegistration {
                decision_digest: Some(dispatch.decision.digest.clone()),
                dispatch_id: dispatch.dispatch_id.clone(),
                agent_id: "worker-a".into(),
            })
            .unwrap();
        broker
            .complete(NativeCompletion {
                decision_digest: Some(dispatch.decision.digest.clone()),
                dispatch_id: dispatch.dispatch_id.clone(),
                agent_id: "worker-a".into(),
                agent_stopped: true,
                reported_usage: None,
                final_message: serde_json::json!({"dispatch_id":dispatch.dispatch_id,"result":{}})
                    .to_string(),
            })
            .unwrap();
        task.await.unwrap().unwrap().receipt.verify().unwrap();
        catalog.models[0].id = "model-b".into();
        std::fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    }
    let unavailable = catalog_observation(&catalog, "parent", 1);
    host::observe(&store, &SystemClock, "parent", &unavailable).unwrap();
    assert!(broker
        .execute(&spec)
        .await
        .unwrap_err()
        .to_string()
        .contains("model_unavailable"));
    assert!(broker.dispatch().unwrap().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn host_metadata_accompanies_actions_and_stale_updates_preserve_completion() {
    use hwahap::{
        native::{NativeCompletion, NativeHost, NativeInput, NativeRegistration},
        state::Store,
    };
    let fixture = common::Fixture::new();
    let host = NativeHost::default();
    let observed = catalog_observation(&bundled(), "parent", 3);
    host.advance(
        &fixture.repo,
        NativeInput {
            request: Some("Inspect repository".into()),
            host_session_id: Some("parent".into()),
            host_observation: Some(observed.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    common::fixture_assessments(&Store::open(&fixture.repo).unwrap());
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let result = host
                .advance(
                    &fixture.repo,
                    NativeInput {
                        host_session_id: Some("parent".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            if let Some(dispatch) = result.dispatch {
                break dispatch;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    host.advance(
        &fixture.repo,
        NativeInput {
            host_session_id: Some("parent".into()),
            registration: Some(NativeRegistration {
                decision_digest: Some(request.decision.digest.clone()),
                dispatch_id: request.dispatch_id.clone(),
                agent_id: "worker".into(),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let completion = NativeCompletion { decision_digest: Some(request.decision.digest.clone()),
dispatch_id:request.dispatch_id.clone(),agent_id:"worker".into(),agent_stopped:true,reported_usage:None,
        final_message:serde_json::json!({"dispatch_id":request.dispatch_id,"result":{"facts":[{"id":"F1","question":"source","answer":"seed file","sources":["src/existing.txt:1"]}]}}).to_string() };
    let mut stale = observed;
    stale.observed_at = "2000-01-01T00:00:00Z".into();
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                host_session_id: Some("parent".into()),
                host_observation: Some(stale),
                completion: Some(completion.clone()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    assert!(hwahap::native::NativeSessions::recorded_completion(
        &Store::open(&fixture.repo).unwrap(),
        &completion
    )
    .unwrap());
    host.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn abandon_is_bound_and_repeated_archive_preserves_worktree() {
    use hwahap::{
        canonical::Digest,
        clock::SystemClock,
        native::{AbandonRequest, NativeHost, NativeInput},
        state::Store,
    };
    let fixture = common::Fixture::new();
    fixture
        .engine()
        .start_planning("archive this run", true)
        .unwrap();
    let store = Store::open(&fixture.repo).unwrap();
    let mut run = store.read_run().unwrap().unwrap();
    let mut plan = store.read_plan().unwrap().unwrap();
    let digest = plan.digest().unwrap();
    plan.frozen = Some(hwahap::plan::Frozen {
        digest: digest.clone(),
        confirmed_at: "2026-09-07T00:00:00Z".into(),
        answer_text: "fixture authorization".into(),
    });
    store.write_plan(&plan).unwrap();
    run.plan_digest = Some(digest);
    run.branch = "codex/archive-test".into();
    store.write_run(&SystemClock, &run).unwrap();
    let request = AbandonRequest {
        run_id: run.run_id.clone(),
        user_instruction: "이 실행을 종료해줘.".into(),
        contract_digest: run.plan_digest.unwrap().to_string(),
    };
    std::fs::write(fixture.repo.join("user.txt"), "keep my work").unwrap();
    common::git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            &run.branch,
            store.worktree_path().to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::write(
        store.worktree_path().join("unfinished.txt"),
        "preserved edits",
    )
    .unwrap();
    let host = NativeHost::default();
    let mut wrong = request.clone();
    wrong.contract_digest = Digest::of_bytes(b"other").to_string();
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                abandon: Some(wrong),
                ..Default::default()
            }
        )
        .await
        .is_err());
    for _ in 0..2 {
        let result = host
            .advance(
                &fixture.repo,
                NativeInput {
                    abandon: Some(request.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(result.outcome.state, "archived");
    }
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("user.txt")).unwrap(),
        "keep my work"
    );
    let directory = store.root().join("archive").join(&request.run_id);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    for (path, digest) in manifest["files"].as_object().unwrap() {
        assert_eq!(
            Digest::of_bytes(&std::fs::read(directory.join(path)).unwrap()).to_string(),
            digest.as_str().unwrap()
        );
    }
    assert_eq!(
        std::fs::read_to_string(directory.join("completion/events.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(!store.archive_pending());
}

#[cfg(unix)]
#[tokio::test]
async fn interrupted_archive_reconciles_moved_files_by_manifest() {
    use hwahap::{clock::SystemClock, state::Store};
    let fixture = common::Fixture::new();
    fixture
        .engine()
        .start_planning("archive retry", true)
        .unwrap();
    let store = Store::open(&fixture.repo).unwrap();
    let run = store.read_run().unwrap().unwrap();
    store
        .write_artifact("z-proof.json", "retained evidence")
        .unwrap();
    let destination = store.root().join("archive").join(&run.run_id);
    std::fs::create_dir_all(destination.join("run.json")).unwrap();
    assert!(store.archive(&SystemClock).is_err());
    assert!(store.archive_pending());
    assert!(destination.join("events.jsonl").exists());
    std::fs::remove_dir(destination.join("run.json")).unwrap();
    store.recover().unwrap();
    assert!(!store.archive_pending());
    assert_eq!(
        std::fs::read_to_string(destination.join("artifacts/z-proof.json")).unwrap(),
        "retained evidence"
    );
    assert!(destination.join("manifest.json").exists());
    assert!(store.read_run().unwrap().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn abandon_waits_for_registered_worker_stop_and_replays_acknowledgment() {
    use hwahap::{
        clock::SystemClock,
        native::{AbandonRequest, NativeHost, NativeInput, NativeRegistration, NativeStopped},
        state::Store,
    };
    let fixture = common::Fixture::new();
    fixture
        .engine()
        .start_planning("stop worker", true)
        .unwrap();
    let store = Store::open(&fixture.repo).unwrap();
    let mut run = store.read_run().unwrap().unwrap();
    run.plan_digest = Some(store.read_plan().unwrap().unwrap().digest().unwrap());
    store.write_run(&SystemClock, &run).unwrap();
    common::fixture_native_observation(&store, &run.run_id);
    let host = NativeHost::default();
    let dispatch = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let result = host
                .advance(&fixture.repo, NativeInput::default())
                .await
                .unwrap();
            if let Some(dispatch) = result.dispatch {
                break dispatch;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    host.advance(
        &fixture.repo,
        NativeInput {
            registration: Some(NativeRegistration {
                decision_digest: Some(dispatch.decision.digest.clone()),
                dispatch_id: dispatch.dispatch_id.clone(),
                agent_id: "worker".into(),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let request = AbandonRequest {
        run_id: run.run_id.clone(),
        contract_digest: run.plan_digest.unwrap().to_string(),
        user_instruction: "종료해줘".into(),
    };
    for _ in 0..2 {
        let result = host
            .advance(
                &fixture.repo,
                NativeInput {
                    abandon: Some(request.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(result.outcome.next, "native_stop");
        assert!(store.read_run().unwrap().is_some());
    }
    let ack = NativeStopped {
        dispatch_id: dispatch.dispatch_id,
        agent_id: Some("worker".into()),
        all_work_stopped: true,
    };
    for _ in 0..2 {
        let result = host
            .advance(
                &fixture.repo,
                NativeInput {
                    stopped: Some(ack.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(result.outcome.state, "archived");
    }
    assert!(store.read_run().unwrap().is_none());
    store.verify_archive(&run.run_id).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn unfrozen_plan_abandons_with_current_plan_digest_after_capacity_refusal() {
    use hwahap::{
        clock::SystemClock,
        native::{AbandonRequest, NativeHost, NativeInput},
        state::Store,
    };
    let fixture = common::Fixture::new();
    let host = NativeHost::default();
    let parent = "planning-owner";
    host.advance(
        &fixture.repo,
        NativeInput {
            request: Some("Inspect and plan this repository".into()),
            plan_only: true,
            host_session_id: Some(parent.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let store = Store::open(&fixture.repo).unwrap();
    let run = store.read_run().unwrap().unwrap();
    assert!(run.plan_digest.is_none());
    let digest = store
        .read_plan()
        .unwrap()
        .unwrap()
        .digest()
        .unwrap()
        .to_string();
    let mut observed = common::fixture_native_observation(&store, parent);
    observed.available_slots = 0;
    hwahap::catalog::host::observe(&store, &SystemClock, parent, &observed).unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match host
                .advance(
                    &fixture.repo,
                    NativeInput {
                        host_session_id: Some(parent.into()),
                        ..Default::default()
                    },
                )
                .await
            {
                Err(error) => panic!("unexpected error: {error}"),
                Ok(result) if result.outcome.next == "delegation_wait" => {
                    break result.outcome.message
                }
                Ok(_) => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .unwrap();
    assert!(error.to_string().contains("slot_unavailable"), "{error}");
    let request = AbandonRequest {
        run_id: run.run_id.clone(),
        contract_digest: digest,
        user_instruction: "계획 실행을 종료해줘.".into(),
    };
    let mut wrong = request.clone();
    wrong.contract_digest = "stale".into();
    assert!(host
        .advance(
            &fixture.repo,
            NativeInput {
                abandon: Some(wrong),
                host_session_id: Some(parent.into()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    for _ in 0..2 {
        let result = host
            .advance(
                &fixture.repo,
                NativeInput {
                    abandon: Some(request.clone()),
                    host_session_id: Some(parent.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(result.outcome.state, "archived");
    }
    store.verify_archive(&run.run_id).unwrap();
    assert!(store.read_run().unwrap().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn adjusted_plan_abandons_current_contract_and_rejects_the_previous_digest() {
    use hwahap::{
        native::{AbandonRequest, NativeHost, NativeInput},
        state::Store,
    };
    let fixture = common::Fixture::new();
    common::git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    fixture
        .engine()
        .start_build(&hwahap::engine::BuildRequest {
            task_profiles: Default::default(),
            verification_inputs: vec![],
            user_instruction: "Build without planning".into(),
            objective: "Create output".into(),
            branch: "codex/catalog-adjust".into(),
            base_branch: "main".into(),
            full_suite: "test -f output".into(),
            units: vec![hwahap::engine::BuildUnit {
                title: "Output".into(),
                paths: vec!["output".into()],
                acceptance: "Output exists".into(),
                test_command: "test -f output".into(),
            }],
        })
        .unwrap();
    let script = common::Script::new(vec![
        common::step(
            Role::Implementer,
            common::Reply::write(
                &[("output", "done")],
                r#"{"status":"completed","summary":"wrote output"}"#,
            ),
        ),
        common::step(
            Role::UnitReviewer,
            common::Reply::say(r#"{"verdict":"pass"}"#),
        ),
        common::step(Role::UnitReviewer, common::Reply::PrAttack),
        common::step(Role::FinalReview, common::Reply::pr_defense()),
    ]);
    for expected in ["final_verifying", "pr_review", "awaiting_adjust_or_ship"] {
        assert_eq!(
            fixture
                .engine()
                .step_with(&script, None, None)
                .await
                .unwrap()
                .state,
            expected
        );
    }
    let store = Store::open(&fixture.repo).unwrap();
    let previous = store
        .read_run()
        .unwrap()
        .unwrap()
        .plan_digest
        .unwrap()
        .to_string();
    fixture
        .engine()
        .step_with(
            &script,
            None,
            Some("the documentation must mention --dry-run"),
        )
        .await
        .unwrap();
    let run = store.read_run().unwrap().unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    assert!(plan.frozen.is_none());
    assert_eq!(run.plan_digest.as_ref().unwrap().to_string(), previous);
    let current = plan.digest().unwrap().to_string();
    assert_ne!(current, previous);
    let host = NativeHost::default();
    let request = AbandonRequest {
        run_id: run.run_id.clone(),
        user_instruction: "이 조정 실행을 종료해줘.".into(),
        contract_digest: previous,
    };
    assert!(
        host.advance(
            &fixture.repo,
            NativeInput {
                abandon: Some(request.clone()),
                ..Default::default()
            }
        )
        .await
        .is_err(),
        "accepted the previous contract"
    );
    assert!(!store.artifacts_path().join("abandon-request.json").exists());
    let request = AbandonRequest {
        contract_digest: current,
        ..request
    };
    let result = host
        .advance(
            &fixture.repo,
            NativeInput {
                abandon: Some(request),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(result.outcome.state, "archived");
    store.verify_archive(&run.run_id).unwrap();
}
