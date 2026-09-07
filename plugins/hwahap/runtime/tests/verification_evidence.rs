use hwahap::{canonical::Digest, clock::FixedClock, state::Store, verification::*};

#[test]
fn t11_declared_inputs_bind_content_existence_and_repository_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let declared = vec!["input.txt".into()];
    let missing = inputs::digest(dir.path(), &declared).unwrap();
    std::fs::write(dir.path().join("input.txt"), "first").unwrap();
    let first = inputs::digest(dir.path(), &declared).unwrap();
    std::fs::write(dir.path().join("input.txt"), "second").unwrap();
    assert_ne!(missing, first);
    assert_ne!(first, inputs::digest(dir.path(), &declared).unwrap());
    assert!(inputs::digest(dir.path(), &["../outside".into()]).is_err());
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        assert!(inputs::digest(dir.path(), &["escape".into()]).is_err());
        assert!(inputs::digest(dir.path(), &["escape/missing".into()]).is_err());
    }
}

fn request() -> VerificationRequest {
    VerificationRequest {
        run_id: "run".into(),
        unit_id: Some("U1".into()),
        test_id: Some("T1".into()),
        kind: Kind::FinalUnit,
        contract: Digest::of_bytes(b"contract"),
        command: "true".into(),
        cwd: "/test".into(),
        head: "a".repeat(40),
        tree: "b".repeat(40),
        code: Digest::of_bytes(b"code"),
        inputs: Digest::of_bytes(b"inputs"),
        environment: Some(Digest::of_bytes(b"environment")),
    }
}

#[test]
fn t12_t14_replay_recovers_completed_evidence_and_rejects_stale_or_missing_output() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let request = request();
    let started = start(&store, &clock, request.clone()).unwrap();
    assert!(start(&store, &clock, request.clone()).is_err());
    let done = record_verification(
        &store,
        &clock,
        started.clone(),
        Status::Passed,
        Some(0),
        "ok",
    )
    .unwrap();
    let count = store.read_events().unwrap().len();
    assert_eq!(
        record_verification(
            &store,
            &clock,
            started.clone(),
            Status::Passed,
            Some(0),
            "ok"
        )
        .unwrap(),
        done
    );
    assert_eq!(store.read_events().unwrap().len(), count);
    assert!(record_verification(&store, &clock, started, Status::Failed, Some(1), "bad").is_err());
    std::fs::remove_file(store.root().join("verification.json")).unwrap();
    assert_eq!(
        require_current_verification(&store, &request).unwrap(),
        done
    );
    assert!(store.root().join("verification.json").is_file());
    let mut stale = request;
    stale.head = "c".repeat(40);
    assert!(require_current_verification(&store, &stale).is_err());
    std::fs::remove_file(
        store
            .artifacts_path()
            .join(format!("verification-{}.output", done.id)),
    )
    .unwrap();
    assert!(recover_verifications(&store).is_err());
}

#[test]
fn t11_latest_failed_attempt_never_reuses_an_older_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let request = request();
    for (status, code) in [(Status::Passed, Some(0)), (Status::Failed, Some(1))] {
        let record = start(&store, &clock, request.clone()).unwrap();
        record_verification(&store, &clock, record, status, code, "output").unwrap();
    }
    assert!(require_current_verification(&store, &request).is_err());
    assert_eq!(
        recover_verifications(&store)
            .unwrap()
            .values()
            .map(|r| r.attempt)
            .max(),
        Some(2)
    );
}

#[cfg(unix)]
mod common;

#[cfg(unix)]
#[tokio::test]
async fn t14_interrupted_command_requires_matching_stop_acknowledgment() {
    use hwahap::{
        engine::{BuildRequest, BuildUnit},
        native::{NativeHost, NativeInput},
    };
    let f = common::Fixture::new();
    common::git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    f.engine()
        .start_build(&BuildRequest {
            task_profiles: Default::default(),
            verification_inputs: vec![],
            user_instruction: "Build the contract".into(),
            objective: "verify recovery".into(),
            base_branch: "main".into(),
            branch: "codex/recovery".into(),
            full_suite: "true".into(),
            units: vec![BuildUnit {
                title: "unit".into(),
                acceptance: "ready".into(),
                paths: vec!["feature".into()],
                test_command: "true".into(),
            }],
        })
        .unwrap();
    let store = Store::open(&f.repo).unwrap();
    let mut req = request();
    req.run_id = store.read_run().unwrap().unwrap().run_id;
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let record = start(&store, &clock, req.clone()).unwrap();
    assert!(require_stopped(&store).is_err());
    let host = NativeHost::default();
    let mut recovery = Recovery {
        run_id: req.run_id,
        verification_id: record.id,
        all_work_stopped: false,
        evidence: "host observed termination of owned process".into(),
    };
    assert!(host
        .advance(
            &f.repo,
            NativeInput {
                verification_recovery: Some(recovery.clone()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    recovery.all_work_stopped = true;
    host.advance(
        &f.repo,
        NativeInput {
            verification_recovery: Some(recovery.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    require_stopped(&store).unwrap();
    host.advance(
        &f.repo,
        NativeInput {
            verification_recovery: Some(recovery),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(recover_verifications(&store)
        .unwrap()
        .values()
        .all(|r| r.status == Status::Interrupted));
}

#[test]
fn t16_v4_run_is_rejected_without_changing_saved_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    std::fs::create_dir_all(store.root()).unwrap();
    let mut run = hwahap::state::Run {
        schema: hwahap::plan::SCHEMA.into(),
        run_id: "legacy".into(),
        goal_id: "legacy".into(),
        revision: 1,
        state: hwahap::state::RunState::PlanReady,
        accepted_units: vec![],
        accepted_fingerprints: Default::default(),
        plan_digest: None,
        branch: String::new(),
        reviewed_head: None,
        seq: 0,
    };
    store
        .write_run(&FixedClock::new("2026-09-07T00:00:00Z"), &run)
        .unwrap();
    run.schema = "hwahap/v4".into();
    let bytes = serde_json::to_string(&run).unwrap();
    std::fs::write(store.root().join("run.json"), &bytes).unwrap();
    assert!(store
        .recover()
        .unwrap_err()
        .to_string()
        .contains("only supports hwahap/v5"));
    assert_eq!(
        std::fs::read_to_string(store.root().join("run.json")).unwrap(),
        bytes
    );
}

#[test]
fn t14_snapshot_before_completion_recovers_and_ahead_snapshot_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let started = start(&store, &clock, request()).unwrap();
    let path = store.root().join("verification.json");
    let before = std::fs::read(&path).unwrap();
    // Output persisted but no completion event: outcome is still unresolved.
    store
        .write_artifact(&format!("verification-{}.output", started.id), "ok")
        .unwrap();
    assert_eq!(
        recover_verifications(&store).unwrap()[&started.id].status,
        Status::Started
    );
    let mut forged = started.clone();
    forged.status = Status::Passed;
    forged.exit_code = Some(0);
    forged.output_digest = Some(Digest::of_bytes(b"ok"));
    std::fs::write(
        &path,
        serde_json::to_vec(&std::collections::BTreeMap::from([(
            forged.id.clone(),
            forged,
        )]))
        .unwrap(),
    )
    .unwrap();
    assert!(recover_verifications(&store)
        .unwrap_err()
        .to_string()
        .contains("snapshot"));
    std::fs::write(&path, &before).unwrap();
    let done = record_verification(
        &store,
        &clock,
        started.clone(),
        Status::Passed,
        Some(0),
        "ok",
    )
    .unwrap();
    // Completion event persisted before snapshot replacement.
    std::fs::write(&path, before).unwrap();
    assert_eq!(recover_verifications(&store).unwrap()[&started.id], done);
    let recovered: std::collections::BTreeMap<String, VerificationRecord> =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(recovered[&started.id], done);
}

#[cfg(unix)]
#[test]
fn t11_inputs_are_preserved_in_build_and_change_the_unit_contract() {
    use hwahap::engine::{BuildRequest, BuildUnit};
    let mut input = BuildRequest {
        task_profiles: Default::default(),
        user_instruction: "implement".into(),
        objective: "fixture".into(),
        base_branch: "main".into(),
        branch: "codex/inputs".into(),
        full_suite: "true".into(),
        verification_inputs: vec!["ignored-fixture".into()],
        units: vec![BuildUnit {
            title: "unit".into(),
            acceptance: "ready".into(),
            paths: vec!["src/".into()],
            test_command: "true".into(),
        }],
    };
    let plan = input.plan("inputs", &"a".repeat(40)).unwrap();
    assert_eq!(plan.verification_inputs, input.verification_inputs);
    input.verification_inputs.clear();
    assert_ne!(
        plan.unit_fingerprint("U1").unwrap(),
        input
            .plan("inputs", &"a".repeat(40))
            .unwrap()
            .unit_fingerprint("U1")
            .unwrap()
    );
    input.verification_inputs = vec!["../outside".into()];
    assert!(input.plan("inputs", &"a".repeat(40)).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn t13_t17_final_checks_every_unit_and_ship_rejects_changed_declared_inputs() {
    use common::{git, step, Fixture, Reply, Script};
    use hwahap::{
        engine::{BuildRequest, BuildUnit},
        profile::Role,
    };
    let f = Fixture::new();
    std::fs::write(f.repo.join(".gitignore"), ".hwahap/\ninput.fixture\n").unwrap();
    git(&f.repo, &["add", ".gitignore"]);
    git(&f.repo, &["commit", "-qm", "ignore declared input"]);
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let command = "test -f src/existing.txt && test \"$(cat input.fixture)\" = one";
    let input = BuildRequest {
        task_profiles: Default::default(),
        user_instruction: "Implement both units".into(),
        objective: "two independent files".into(),
        base_branch: "main".into(),
        branch: "codex/evidence".into(),
        full_suite: "true".into(),
        verification_inputs: vec!["input.fixture".into()],
        units: ["first", "second"]
            .into_iter()
            .map(|name| BuildUnit {
                title: name.into(),
                acceptance: format!("{name} exists"),
                paths: vec![name.into()],
                test_command: command.into(),
            })
            .collect(),
    };
    let engine = f.engine();
    engine.start_build(&input).unwrap();
    std::fs::write(f.worktree().join("input.fixture"), "one").unwrap();
    let mut steps = Vec::new();
    for name in ["first", "second"] {
        steps.push(step(
            Role::Implementer,
            Reply::write(
                &[(name, "ready")],
                r#"{"status":"completed","summary":"created","conflict":null}"#,
            ),
        ));
        steps.push(step(
            Role::UnitReviewer,
            Reply::say(r#"{"verdict":"pass","findings":[]}"#),
        ));
    }
    let script = Script::new(steps);
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "final_verifying"
    );
    assert_eq!(script.remaining(), 0);
    engine.step_with(&script, None, None).await.unwrap();
    let store = Store::open(&f.repo).unwrap();
    let records = recover_verifications(&store).unwrap();
    let finals: Vec<_> = records
        .values()
        .filter(|r| r.request.kind == Kind::FinalUnit)
        .collect();
    assert_eq!(finals.len(), 2);
    assert!(finals
        .iter()
        .all(|r| r.status == Status::Passed && r.request.command == command));
    assert_ne!(finals[0].request.unit_id, finals[1].request.unit_id);
    assert_eq!(
        records
            .values()
            .filter(|r| r.request.kind == Kind::FullSuite)
            .count(),
        1
    );
    let head = git(&f.worktree(), &["rev-parse", "HEAD"]);
    assert!(finals.iter().all(|r| r.request.head == head));
    let events = store.read_events().unwrap();
    for implementation in events
        .iter()
        .filter(|e| e.kind == "implementation_completed")
    {
        for id in implementation.data["verification_ids"].as_array().unwrap() {
            assert_eq!(
                records[id.as_str().unwrap()].request.tree,
                implementation.data["tree"].as_str().unwrap()
            );
        }
    }
    let reviews = Script::new(vec![
        step(Role::UnitReviewer, Reply::PrAttack),
        step(Role::FinalReview, Reply::pr_defense()),
    ]);
    assert_eq!(
        engine.step_with(&reviews, None, None).await.unwrap().state,
        "awaiting_adjust_or_ship"
    );
    std::fs::write(f.worktree().join("input.fixture"), "changed").unwrap();
    assert_eq!(git(&f.worktree(), &["status", "--porcelain"]), "");
    let ship = format!(
        "SHIP {}",
        store
            .read_plan()
            .unwrap()
            .unwrap()
            .digest()
            .unwrap()
            .challenge()
    );
    let error = engine.ship(&ship).unwrap_err().to_string();
    assert!(error.contains("verification evidence"), "{error}");
    assert_eq!(
        store.read_run().unwrap().unwrap().state.name(),
        "awaiting_adjust_or_ship"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn t11_mutating_tests_and_timeouts_preserve_failure_and_never_accept_work() {
    use common::{git, step, Fixture, Reply, Script};
    use hwahap::{
        engine::{BuildRequest, BuildUnit},
        profile::Role,
    };
    for command in [
        "printf changed > feature",
        "git reset --quiet HEAD -- feature",
        "printf changed >> input.fixture",
        "sleep 3",
    ] {
        let f = Fixture::new();
        std::fs::write(f.repo.join(".gitignore"), ".hwahap/\ninput.fixture\n").unwrap();
        git(&f.repo, &["add", ".gitignore"]);
        git(&f.repo, &["commit", "-qm", "ignore declared input"]);
        git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::create_dir_all(f.repo.join(".hwahap")).unwrap();
        std::fs::write(
            f.repo.join(".hwahap/config.toml"),
            "[limits]\ntest_timeout_secs=1\n",
        )
        .unwrap();
        let engine = f.engine();
        engine
            .start_build(&BuildRequest {
                task_profiles: Default::default(),
                verification_inputs: vec!["input.fixture".into()],
                user_instruction: "Implement".into(),
                objective: "mutation failure".into(),
                base_branch: "main".into(),
                branch: "codex/mutation".into(),
                full_suite: "true".into(),
                units: vec![BuildUnit {
                    title: "feature".into(),
                    acceptance: "ready".into(),
                    paths: vec!["feature".into()],
                    test_command: command.into(),
                }],
            })
            .unwrap();
        let script = Script::new(
            [Role::Implementer, Role::Rework]
                .into_iter()
                .map(|role| {
                    step(
                        role,
                        Reply::write(
                            &[("feature", "ready")],
                            r#"{"status":"completed","summary":"created","conflict":null}"#,
                        ),
                    )
                })
                .collect(),
        );
        let head = git(&f.worktree(), &["rev-parse", "HEAD"]);
        let result = engine.step_with(&script, None, None).await.unwrap();
        assert_eq!(result.state, "blocked", "{command}: {}", result.message);
        assert_eq!(git(&f.worktree(), &["rev-parse", "HEAD"]), head);
        let store = Store::open(&f.repo).unwrap();
        let records = recover_verifications(&store).unwrap();
        assert_eq!(records.len(), 2, "{command}");
        assert!(records.values().all(|r| r.status == Status::Failed));
        assert!(store.read_run().unwrap().unwrap().accepted_units.is_empty());
        assert_eq!(script.remaining(), 0);
        assert_eq!(
            store
                .read_events()
                .unwrap()
                .iter()
                .filter(|e| e.kind == "verification_process")
                .count(),
            2
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn t14_cancelled_live_command_remains_unresolved_until_owned_stop_recovery() {
    use common::{git, step, Fixture, Reply, Script};
    use hwahap::{
        engine::{BuildRequest, BuildUnit},
        native::{NativeHost, NativeInput},
        profile::Role,
    };
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let engine = f.engine();
    engine
        .start_build(&BuildRequest {
            task_profiles: Default::default(),
            verification_inputs: vec![],
            user_instruction: "Implement".into(),
            objective: "cancel command".into(),
            base_branch: "main".into(),
            branch: "codex/cancel".into(),
            full_suite: "true".into(),
            units: vec![BuildUnit {
                title: "feature".into(),
                acceptance: "ready".into(),
                paths: vec!["feature".into()],
                test_command: "sleep 30".into(),
            }],
        })
        .unwrap();
    let script = Script::new(vec![step(
        Role::Implementer,
        Reply::write(
            &[("feature", "ready")],
            r#"{"status":"completed","summary":"created","conflict":null}"#,
        ),
    )]);
    let task = tokio::spawn(async move { engine.step_with(&script, None, None).await });
    let store = Store::open(&f.repo).unwrap();
    let mut process = None;
    for _ in 0..500 {
        process = store
            .read_events()
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "verification_process");
        if process.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let process = process.expect("live command journal event");
    let pid = process.data["pid"].as_u64().unwrap().to_string();
    let mut alive = true;
    for _ in 0..100 {
        alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if !alive {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!alive, "owned process survived cancellation");
    let records = recover_verifications(&store).unwrap();
    let record = records.values().next().unwrap();
    assert_eq!(record.status, Status::Started);
    assert!(require_stopped(&store).is_err());
    let host = NativeHost::default();
    let recovery = Recovery {
        run_id: record.request.run_id.clone(),
        verification_id: record.id.clone(),
        all_work_stopped: true,
        evidence: format!("owned process {pid} exited after cancellation"),
    };
    host.advance(
        &f.repo,
        NativeInput {
            verification_recovery: Some(recovery),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    require_stopped(&store).unwrap();
    assert_eq!(
        recover_verifications(&store).unwrap()[&record.id].status,
        Status::Interrupted
    );
    assert!(require_current_verification(&store, &record.request).is_err());
}

#[cfg(unix)]
#[test]
fn t11_symlink_target_permissions_invalidate_current_evidence() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("fixture");
    std::fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink("fixture", dir.path().join("fixture-link")).unwrap();
    let declared = vec!["fixture-link".into()];
    let mut req = request();
    req.inputs = inputs::digest(dir.path(), &declared).unwrap();
    let store = Store::open(dir.path()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let record = start(&store, &clock, req.clone()).unwrap();
    record_verification(&store, &clock, record, Status::Passed, Some(0), "ok").unwrap();
    require_current_verification(&store, &req).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    let changed = inputs::digest(dir.path(), &declared).unwrap();
    assert_ne!(changed, req.inputs);
    req.inputs = changed;
    assert!(require_current_verification(&store, &req).is_err());
}

#[cfg(unix)]
#[test]
fn t11_reset_preserves_declared_literal_paths_and_symlink_targets_only() {
    use common::{git, Fixture};
    let f = Fixture::new();
    std::fs::write(f.repo.join(".gitignore"), ".hwahap/\ncached/\ncache-link\n").unwrap();
    git(&f.repo, &["add", ".gitignore"]);
    git(&f.repo, &["commit", "-qm", "ignored inputs"]);
    std::fs::create_dir_all(f.repo.join("cached")).unwrap();
    std::fs::write(f.repo.join("cached/keep [1].fixture"), "required").unwrap();
    std::fs::write(f.repo.join("cached/keep 1.fixture"), "disposable").unwrap();
    std::os::unix::fs::symlink("cached", f.repo.join("cache-link")).unwrap();
    std::fs::write(f.repo.join("extra"), "disposable").unwrap();
    let repo = hwahap::git::Git::open(&f.repo).unwrap();
    repo.reset_preserving_inputs(&f.repo, "HEAD", &["cache-link/keep [1].fixture".into()])
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(f.repo.join("cache-link/keep [1].fixture")).unwrap(),
        "required"
    );
    assert!(!f.repo.join("cached/keep 1.fixture").exists());
    assert!(!f.repo.join("extra").exists());
}

#[cfg(unix)]
#[test]
fn t11_cleanup_preserves_every_link_in_a_declared_symlink_chain() {
    use common::{git, Fixture};
    for directory in [false, true] {
        let f = Fixture::new();
        std::fs::write(
            f.repo.join(".gitignore"),
            ".hwahap/\nalias\nmiddle\nfixture\n",
        )
        .unwrap();
        git(&f.repo, &["add", ".gitignore"]);
        git(&f.repo, &["commit", "-qm", "chained inputs"]);
        let (file, input) = if directory {
            std::fs::create_dir(f.repo.join("fixture")).unwrap();
            std::fs::write(f.repo.join("fixture/disposable"), "output").unwrap();
            ("fixture/input", "alias/input")
        } else {
            ("fixture", "alias")
        };
        std::fs::write(f.repo.join(file), "required").unwrap();
        std::os::unix::fs::symlink("fixture", f.repo.join("middle")).unwrap();
        std::os::unix::fs::symlink("middle", f.repo.join("alias")).unwrap();
        let declared = vec![input.into()];
        let before = inputs::digest(&f.repo, &declared).unwrap();
        hwahap::git::Git::open(&f.repo)
            .unwrap()
            .reset_preserving_inputs(&f.repo, "HEAD", &declared)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(f.repo.join(input)).unwrap(),
            "required"
        );
        assert_eq!(before, inputs::digest(&f.repo, &declared).unwrap());
        assert!(!f.repo.join("fixture/disposable").exists());
    }
}

#[cfg(unix)]
#[test]
fn t11_intermediate_link_destination_changes_the_input_digest() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    for name in ["first", "second"] {
        std::fs::write(dir.path().join(name), "identical").unwrap();
    }
    symlink("first", dir.path().join("middle")).unwrap();
    symlink("middle", dir.path().join("alias")).unwrap();
    let declared = vec!["alias".into()];
    let before = inputs::digest(dir.path(), &declared).unwrap();
    std::fs::remove_file(dir.path().join("middle")).unwrap();
    symlink("second", dir.path().join("middle")).unwrap();
    assert_ne!(before, inputs::digest(dir.path(), &declared).unwrap());
}

#[test]
fn reuse_requires_explicit_environment_and_latest_intact_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let request = request();
    let record = start(&store, &clock, request.clone()).unwrap();
    assert!(reusable_pass(&store, &request).is_err());
    let passed =
        record_verification(&store, &clock, record, Status::Passed, Some(0), "passed").unwrap();
    assert_eq!(
        reusable_pass(&store, &request).unwrap(),
        Some((passed.id.clone(), "passed".into()))
    );
    for field in [
        "head",
        "tree",
        "code",
        "inputs",
        "environment",
        "command",
        "contract",
    ] {
        let mut changed = serde_json::to_value(&request).unwrap();
        changed[field] = if matches!(field, "code" | "inputs" | "environment" | "contract") {
            serde_json::to_value(Digest::of_bytes(b"changed")).unwrap()
        } else {
            serde_json::json!("changed")
        };
        assert!(
            reusable_pass(&store, &serde_json::from_value(changed).unwrap())
                .unwrap()
                .is_none()
        );
    }
    let mut unknown = request.clone();
    unknown.environment = None;
    assert!(reusable_pass(&store, &unknown).unwrap().is_none());
    let retry = start(&store, &clock, request.clone()).unwrap();
    record_verification(&store, &clock, retry, Status::Failed, Some(1), "failed").unwrap();
    assert!(reusable_pass(&store, &request).unwrap().is_none());
    std::fs::write(
        store
            .artifacts_path()
            .join(format!("verification-{}.output", passed.id)),
        "tampered",
    )
    .unwrap();
    assert!(reusable_pass(&store, &request).is_err());
}

#[test]
fn reuse_configuration_requires_an_environment_revision() {
    use hwahap::config::Config;
    assert!(!Config::default().verification.reuse_passed);
    assert!(Config::parse("[verification]\nreuse_passed=true").is_err());
    assert!(
        Config::parse(
            "[verification]\nreuse_passed=true\nenvironment_revision='toolchain-fixture-v1'"
        )
        .unwrap()
        .verification
        .reuse_passed
    );
    assert_ne!(environment_digest("v1"), environment_digest("v2"));
}

#[test]
fn preflight_success_is_never_reused_for_external_recovery_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let mut request = request();
    request.kind = Kind::Preflight;
    let started = start(&store, &clock, request.clone()).unwrap();
    record_verification(
        &store,
        &clock,
        started,
        Status::Passed,
        Some(0),
        "recovered",
    )
    .unwrap();
    assert!(reusable_pass(&store, &request).unwrap().is_none());
}
