use hwahap::{
    catalog::{Depth, Requirements},
    delegation::*,
    profile::Role,
};
fn assessment() -> TaskAssessment {
    TaskAssessment {
        run_id: "run".into(),
        unit: Some("U1".into()),
        role: Role::Implementer,
        contract_digest: "contract".into(),
        requirements: Requirements {
            capabilities: [("implementation".into(), 1)].into(),
            depth: Depth::Routine,
        },
        risk: Risk {
            failure_cost: Some(0),
            reversibility: Some(0),
            blast_radius: Some(0),
        },
        topology: Topology {
            predecessors: vec![],
            coupling: Coupling::Independent,
            shared_resources: vec![],
            writer_owner: Some("U1".into()),
            separable: true,
            write_paths: vec!["output".into()],
        },
        evidence: vec!["isolated fixture worktree".into()],
        recovery: None,
    }
}
#[test]
fn risk_is_not_averaged_and_unknown_axes_wait() {
    let mut a = assessment();
    assert!(!a.risk.high().unwrap());
    for index in 0..3 {
        let mut risk = a.risk.clone();
        match index {
            0 => risk.failure_cost = Some(2),
            1 => risk.reversibility = Some(2),
            _ => risk.blast_radius = Some(2),
        };
        assert!(risk.high().unwrap());
    }
    a.risk.failure_cost = None;
    assert!(a
        .validate()
        .unwrap_err()
        .to_string()
        .contains("assessment_missing"));
}
#[test]
fn requirements_merge_each_capability_and_deeper_reasoning() {
    let mut a = assessment();
    a.risk.blast_radius = Some(2);
    let baseline = Requirements {
        capabilities: [
            ("implementation".into(), 2),
            ("repository_analysis".into(), 1),
        ]
        .into(),
        depth: Depth::Routine,
    };
    let result = a.merged(&baseline).unwrap();
    assert_eq!(result.depth, Depth::Focused);
    assert_eq!(result.capabilities["implementation"], 2);
    assert_eq!(result.capabilities["repository_analysis"], 1);
    a.role = Role::UnitReviewer;
    assert_eq!(a.merged(&baseline).unwrap().depth, Depth::Deep);
    let first = a.digest().unwrap();
    a.topology.shared_resources.push("store".into());
    assert_ne!(first, a.digest().unwrap());
}
fn policy_fixture() -> (
    hwahap::catalog::CatalogSnapshot,
    hwahap::catalog::HostObservation,
    Context,
) {
    let snapshot =
        hwahap::catalog::CatalogSnapshot::new("run", hwahap::catalog::bundled()).unwrap();
    let host = hwahap::catalog::HostObservation {
        host_session_id: "parent".into(),
        observed_at: "2026-09-07T00:00:00Z".into(),
        source: "fixture inventory".into(),
        parent_model: "gpt-6-astra".into(),
        parent_effort: "high".into(),
        available_slots: 3,
        models: snapshot
            .catalog
            .models
            .iter()
            .map(|m| {
                (
                    m.id.clone(),
                    hwahap::catalog::ObservedModel {
                        efforts: m.efforts.iter().map(|e| e.name.clone()).collect(),
                        tools: vec!["exec_command".into(), "apply_patch".into()],
                    },
                )
            })
            .collect(),
    };
    let context = Context {
        run_id: "run".into(),
        contract_digest: "contract".into(),
        role: Role::Implementer,
        unit: Some("U1".into()),
        completed: Default::default(),
        write_paths: vec!["output".into()],
        shared_state: false,
        writer_available: true,
        bound: None,
        reviewer_bindings: Default::default(),
        free_slots: 3,
        preflight_verified: false,
    };
    (snapshot, host, context)
}
#[test]
fn policy_routes_risk_shared_state_dependencies_and_bound_capability() {
    let (snapshot, host, mut context) = policy_fixture();
    let mut a = assessment();
    let ordinary = decide(&snapshot, &host, Some(&a), &context).unwrap();
    assert_eq!(ordinary.route, Route::Worker);
    assert_eq!(ordinary.selection.unwrap().model, "gpt-5.6-luna");
    a.risk.failure_cost = Some(2);
    assert_eq!(
        decide(&snapshot, &host, Some(&a), &context).unwrap().route,
        Route::Wait
    );
    context.preflight_verified = true;
    assert_eq!(
        decide(&snapshot, &host, Some(&a), &context).unwrap().route,
        Route::Coordinator
    );
    a.risk.failure_cost = Some(0);
    a.topology.coupling = Coupling::Shared;
    assert_eq!(
        decide(&snapshot, &host, Some(&a), &context)
            .unwrap()
            .reason_codes,
        vec!["shared_state"]
    );
    a.topology.coupling = Coupling::Independent;
    a.topology.predecessors.push("U0".into());
    assert_eq!(
        decide(&snapshot, &host, Some(&a), &context).unwrap().route,
        Route::Wait
    );
    context.completed.insert("U0".into());
    context.bound = Some(("gpt-5.6-luna".into(), "medium".into()));
    a.requirements
        .capabilities
        .insert("cross_module_reasoning".into(), 3);
    a.requirements.depth = Depth::Deep;
    let difficult = decide(&snapshot, &host, Some(&a), &context).unwrap();
    assert_eq!(difficult.route, Route::Coordinator);
    assert_eq!(
        difficult.reason_codes,
        vec!["bound_capability_insufficient"]
    );
    let mut weak = host.clone();
    weak.parent_model = "gpt-5.6-luna".into();
    weak.parent_effort = "medium".into();
    assert_eq!(
        decide(&snapshot, &weak, Some(&a), &context).unwrap().route,
        Route::Wait
    );
}
#[test]
fn reviewers_keep_independent_lanes_and_missing_inventory_waits() {
    let (snapshot, mut host, mut context) = policy_fixture();
    let mut a = assessment();
    a.role = Role::UnitReviewer;
    context.role = a.role;
    a.risk.failure_cost = Some(2);
    let reviewer = decide(&snapshot, &host, Some(&a), &context).unwrap();
    assert_eq!(reviewer.route, Route::Worker);
    assert_eq!(reviewer.lane, hwahap::native::NativeLane::Critic);
    context.free_slots = 0;
    assert_eq!(
        decide(&snapshot, &host, Some(&a), &context)
            .unwrap()
            .reason_codes,
        vec!["slot_unavailable"]
    );
    context.free_slots = 3;
    host.models.remove("gpt-6-astra");
    assert_eq!(
        decide(&snapshot, &host, Some(&a), &context)
            .unwrap()
            .reason_codes,
        vec!["model_unavailable"]
    );
    assert_eq!(
        decide(&snapshot, &host, None, &context)
            .unwrap()
            .reason_codes,
        vec!["assessment_missing"]
    );
}

mod common;
use hwahap::{
    native::{NativeCompletion, NativeLane, NativeRegistration, NativeSessions},
    session::SessionSpec,
    state::Store,
};
use std::sync::Arc;
fn native_fixture(shared: bool) -> (common::Fixture, Store, TaskAssessment) {
    let f = common::Fixture::new();
    f.engine()
        .start_planning("delegation fixture", true)
        .unwrap();
    let store = Store::open(&f.repo).unwrap();
    let mut plan = store.read_plan().unwrap().unwrap();
    plan.units = (1..=2)
        .map(|n| hwahap::plan::Unit {
            id: format!("U{n}"),
            title: "fixture".into(),
            paths: vec![if shared {
                "output".into()
            } else {
                format!("output-{n}")
            }],
            acceptance_ids: vec![],
            depends_on: vec![],
            probe: false,
        })
        .collect();
    store.write_plan(&plan).unwrap();
    common::fixture_observation(&store, "parent");
    let mut a = assessment();
    a.run_id = store.read_run().unwrap().unwrap().run_id;
    a.contract_digest = plan.digest().unwrap().to_string();
    a.topology.write_paths = plan.units[0].paths.clone();
    (f, store, a)
}
async fn dispatch_job(
    f: &common::Fixture,
    store: &Store,
    a: &TaskAssessment,
    expected: NativeLane,
    model: &str,
    agent: &str,
) -> String {
    hwahap::delegation::store::record(store, a).unwrap();
    let broker =
        Arc::new(NativeSessions::new(store.clone(), 20, 10).with_host_session_id("parent".into()));
    let spec = SessionSpec {
        assessment: Some(a.clone()),
        cwd: f.repo.clone(),
        role: a.role,
        unit: a.unit.clone(),
        prompt: "fixture".into(),
    };
    let worker = broker.clone();
    let input = spec.clone();
    let mut task = tokio::spawn(async move { worker.execute(&input).await });
    let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(d) = broker.dispatch().unwrap() {
                break d;
            }
            if task.is_finished() {
                panic!("dispatch ended early: {:?}", (&mut task).await);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(request.lane, expected);
    assert_eq!(request.model, model);
    let mut registration = NativeRegistration {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: agent.into(),
        decision_digest: Some("stale".into()),
    };
    assert!(broker.register(&registration).is_err());
    registration.decision_digest = Some(request.decision.digest.clone());
    broker.register(&registration).unwrap();
    let mut completion = NativeCompletion {
        dispatch_id: request.dispatch_id.clone(),
        agent_id: agent.into(),
        decision_digest: Some("stale".into()),
        agent_stopped: true,
        reported_usage: None,
        final_message: serde_json::json!({"dispatch_id":request.dispatch_id,"result":{}})
            .to_string(),
    };
    assert!(broker.complete(completion.clone()).is_err());
    completion.decision_digest = Some(request.decision.digest.clone());
    broker.complete(completion).unwrap();
    let outcome = task.await.unwrap().unwrap();
    outcome
        .receipt
        .verify_for(&spec, &hwahap::catalog::snapshot(store, &a.run_id).unwrap())
        .unwrap();
    broker.finish().unwrap();
    request.decision.reason_codes.join(",")
}
async fn refusal(f: &common::Fixture, store: &Store, a: &TaskAssessment, record: bool) -> String {
    if record {
        hwahap::delegation::store::record(store, a).unwrap();
    }
    let broker = NativeSessions::new(store.clone(), 20, 10).with_host_session_id("parent".into());
    let spec = SessionSpec {
        assessment: None,
        cwd: f.repo.clone(),
        role: a.role,
        unit: a.unit.clone(),
        prompt: "fixture".into(),
    };
    let error = broker.execute(&spec).await.unwrap_err();
    assert!(broker.dispatch().unwrap().is_none());
    error.to_string()
}
#[tokio::test]
async fn actual_dispatch_reuses_worker_and_falls_back_to_capable_parent() {
    let (f, store, mut a) = native_fixture(false);
    assert_eq!(
        dispatch_job(&f, &store, &a, NativeLane::Worker, "gpt-5.6-luna", "worker").await,
        "capability_match"
    );
    dispatch_job(&f, &store, &a, NativeLane::Worker, "gpt-5.6-luna", "worker").await;
    a.unit = Some("U2".into());
    a.topology.writer_owner = a.unit.clone();
    a.topology.write_paths = vec!["output-2".into()];
    a.requirements
        .capabilities
        .insert("cross_module_reasoning".into(), 3);
    assert_eq!(
        dispatch_job(
            &f,
            &store,
            &a,
            NativeLane::Coordinator,
            "gpt-6-astra",
            "coordinator"
        )
        .await,
        "bound_capability_insufficient"
    );
}
#[tokio::test]
async fn actual_overlap_routes_parent_and_missing_assessment_waits() {
    let (f, store, a) = native_fixture(true);
    assert!(refusal(&f, &store, &a, false)
        .await
        .contains("assessment_missing"));
    assert_eq!(
        dispatch_job(
            &f,
            &store,
            &a,
            NativeLane::Coordinator,
            "gpt-6-astra",
            "coordinator"
        )
        .await,
        "shared_state"
    );
}
#[tokio::test]
async fn actual_pending_dependency_and_high_risk_prevent_author_dispatch() {
    let (f, store, mut a) = native_fixture(false);
    a.topology.predecessors.push("U2".into());
    assert!(refusal(&f, &store, &a, true)
        .await
        .contains("predecessor_pending"));
    let (f, store, mut a) = native_fixture(false);
    a.risk.reversibility = Some(2);
    assert!(refusal(&f, &store, &a, true)
        .await
        .contains("recovery_validation_required"));
}
#[tokio::test]
async fn actual_capacity_and_reviewer_availability_are_enforced() {
    for case in 0..4 {
        let (f, store, mut a) = native_fixture(case == 3);
        let mut host = hwahap::catalog::host::latest(&store).unwrap().unwrap();
        let expected = match case {
            0 => {
                host.available_slots = 0;
                "slot_unavailable"
            }
            1 => {
                a.role = Role::UnitReviewer;
                host.models.remove("gpt-6-astra");
                "model_unavailable"
            }
            2 => {
                a.role = Role::UnitReviewer;
                host.models.get_mut("gpt-6-astra").unwrap().efforts = vec!["unsupported".into()];
                "model_unavailable"
            }
            _ => {
                a.requirements
                    .capabilities
                    .insert("adversarial_review".into(), 3);
                host.parent_model = "gpt-5.6-luna".into();
                host.parent_effort = "medium".into();
                "bound_capability_insufficient"
            }
        };
        hwahap::catalog::host::observe(&store, &hwahap::clock::SystemClock, "parent", &host)
            .unwrap();
        assert!(refusal(&f, &store, &a, true).await.contains(expected));
    }
}

#[tokio::test]
async fn high_risk_actual_build_reviews_and_tests_recovery_before_parent_writes() {
    high_risk_case(false, false, 0).await;
}
#[tokio::test]
async fn failed_preflight_review_prevents_commands_and_author() {
    high_risk_case(true, false, 0).await;
}
#[tokio::test]
async fn failed_isolated_recovery_prevents_author() {
    high_risk_case(false, true, 0).await;
}
struct FailUntrackedCleanup {
    trigger: std::path::PathBuf,
    directory: std::path::PathBuf,
    armed: std::sync::atomic::AtomicBool,
}
impl hwahap::clock::Clock for FailUntrackedCleanup {
    fn now(&self) -> String {
        if self.trigger.exists() && !self.armed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o500))
                .unwrap();
        }
        common::NOW.into()
    }
}
#[tokio::test]
async fn final_failed_author_recovers_after_tracked_reset_before_untracked_cleanup() {
    // The injected filesystem failure requires an unprivileged process.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    high_risk_case(false, false, 5).await;
}
async fn high_risk_case(review_failure: bool, recovery_failure: bool, interruption: u8) {
    use hwahap::engine::{BuildRequest, BuildUnit};
    let f = common::Fixture::new();
    common::git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let trigger = f.dir.path().join("interrupt-cleanup");
    let engine = if interruption == 5 {
        f.engine().with_parts(
            Box::new(FailUntrackedCleanup {
                trigger: trigger.clone(),
                directory: f.worktree().join("debris"),
                armed: std::sync::atomic::AtomicBool::new(false),
            }),
            hwahap::forge::Forge::with_program(f.gh.to_str().unwrap()),
        )
    } else {
        f.engine()
    };
    let paths = if interruption == 5 {
        vec![
            "feature.txt".into(),
            "src/existing.txt".into(),
            "debris/".into(),
        ]
    } else {
        vec!["feature.txt".into()]
    };
    engine
        .start_build(&BuildRequest {
            verification_inputs: vec![],
            user_instruction: "Implement the isolated fixture".into(),
            objective: "fixture".into(),
            base_branch: "main".into(),
            branch: "codex/high-risk".into(),
            full_suite: "test -f feature.txt".into(),
            units: vec![BuildUnit {
                title: "fixture".into(),
                acceptance: "feature.txt contains ready".into(),
                paths: paths.clone(),
                test_command: "test -f feature.txt".into(),
            }],
        })
        .unwrap();
    let store = Store::open(&f.repo).unwrap();
    let isolated = f.dir.path().join("isolated");
    common::git(
        f.dir.path(),
        &[
            "clone",
            "--quiet",
            f.repo.to_str().unwrap(),
            isolated.to_str().unwrap(),
        ],
    );
    let mut a = assessment();
    a.run_id = store.read_run().unwrap().unwrap().run_id;
    a.contract_digest = store
        .read_plan()
        .unwrap()
        .unwrap()
        .digest()
        .unwrap()
        .to_string();
    a.topology.write_paths = paths;
    a.risk.failure_cost = Some(2);
    a.recovery = Some(RecoveryRequirements {
        environment: isolated
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        user_authorization:
            "Test author authorizes process failure and recovery in this disposable clone".into(),
        failure_command: "test \"$(sh -c 'exit 7'; echo $?)\" = 7".into(),
        recovery_command: if recovery_failure {
            "sh -c 'exit 1'"
        } else {
            "sh -c 'exit 0'"
        }
        .into(),
    });
    hwahap::delegation::store::record(&store, &a).unwrap();
    let mut observed = common::fixture_observation(&store, "parent");
    observed.available_slots = 2;
    hwahap::catalog::host::observe(&store, &hwahap::clock::SystemClock, "parent", &observed)
        .unwrap();
    let broker =
        Arc::new(NativeSessions::new(store.clone(), 20, 10).with_host_session_id("parent".into()));
    let runner = broker.clone();
    let mut task = tokio::spawn(async move { engine.step_with(&*runner, None, None).await });
    let mut ids = std::collections::BTreeSet::new();
    let roles = [
        (Role::UnitReviewer, "critic"),
        (Role::FinalReview, "auditor"),
        (Role::Implementer, "coordinator"),
        (
            if interruption == 5 {
                Role::Rework
            } else {
                Role::UnitReviewer
            },
            if interruption == 5 {
                "coordinator"
            } else {
                "critic"
            },
        ),
    ];
    let count = if review_failure {
        1
    } else if recovery_failure {
        2
    } else {
        4
    };
    for (role, agent) in roles.into_iter().take(count) {
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(d) = broker.dispatch().unwrap() {
                    if !ids.contains(&d.dispatch_id) {
                        break d;
                    }
                }
                if task.is_finished() {
                    panic!("engine ended early: {:?}", (&mut task).await);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        ids.insert(request.dispatch_id.clone());
        assert_eq!(request.role, role.as_str());
        if role == Role::Implementer {
            assert!(hwahap::delegation::preflight::verified(&store, &a).unwrap());
            assert_eq!(request.lane, NativeLane::Coordinator);
            assert_eq!(
                hwahap::verification::recover_verifications(&store)
                    .unwrap()
                    .len(),
                2
            );
            assert!(!f.worktree().join("feature.txt").exists());
        }
        broker
            .register(&NativeRegistration {
                decision_digest: Some(request.decision.digest.clone()),
                dispatch_id: request.dispatch_id.clone(),
                agent_id: agent.into(),
            })
            .unwrap();
        if role == Role::Implementer && (1..=4).contains(&interruption) {
            std::fs::write(f.worktree().join("feature.txt"), "partial interrupted work").unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            resume_interrupted_high_risk(&f, &store, &request, interruption).await;
            return;
        }
        let result = if interruption == 5 && matches!(role, Role::Implementer | Role::Rework) {
            std::fs::write(
                f.worktree().join("src/existing.txt"),
                "changed tracked candidate",
            )
            .unwrap();
            std::fs::create_dir_all(f.worktree().join("debris")).unwrap();
            std::fs::write(f.worktree().join("debris/new.txt"), "untracked candidate").unwrap();
            if role == Role::Rework {
                std::fs::write(&trigger, "interrupt final cleanup").unwrap();
            }
            serde_json::json!({"status":"failed","summary":"rejected fixture","conflict":null})
        } else if role == Role::Implementer {
            std::fs::write(f.worktree().join("feature.txt"), "ready").unwrap();
            serde_json::json!({"status":"completed","summary":"fixture written","conflict":null})
        } else {
            if review_failure {
                serde_json::json!({"verdict":"fail","findings":["fixture authority was refused"]})
            } else {
                serde_json::json!({"verdict":"pass","findings":[]})
            }
        };
        broker
            .complete(NativeCompletion {
                decision_digest: Some(request.decision.digest.clone()),
                dispatch_id: request.dispatch_id.clone(),
                agent_id: agent.into(),
                final_message:
                    serde_json::json!({"dispatch_id":request.dispatch_id,"result":result})
                        .to_string(),
                agent_stopped: true,
                reported_usage: None,
            })
            .unwrap();
    }
    let result = task.await.unwrap();
    broker.finish().unwrap();
    if interruption == 5 {
        use std::os::unix::fs::PermissionsExt;
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(f.worktree().join("src/existing.txt")).unwrap(),
            "start\n"
        );
        assert!(f.worktree().join("debris/new.txt").exists());
        std::fs::set_permissions(
            f.worktree().join("debris"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let before = store.read_events().unwrap();
        assert_eq!(
            before
                .iter()
                .filter(|e| e.kind == "unit_attempt_finished" && e.data["passed"] == false)
                .count(),
            2
        );
        let empty = common::Script::new(vec![]);
        let resumed = f.engine().step_with(&empty, None, None).await.unwrap();
        assert_eq!(resumed.state, "blocked");
        assert!(resumed.message.contains("budget exhausted"));
        assert!(!f.worktree().join("debris").exists());
        assert!(empty.calls().is_empty());
        assert_eq!(
            store
                .read_events()
                .unwrap()
                .iter()
                .filter(|e| e.kind == "unit_attempt_started")
                .count(),
            before
                .iter()
                .filter(|e| e.kind == "unit_attempt_started")
                .count()
        );
        let backup = std::fs::read_dir(store.artifacts_path())
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("interrupted-author-")
                    .then(|| std::fs::read_to_string(p).unwrap())
            })
            .collect::<Vec<_>>();
        assert!(backup
            .iter()
            .any(|s| s.contains("changed tracked candidate") && s.contains("untracked candidate")));
        return;
    }
    if review_failure || recovery_failure {
        assert!(result.is_err());
        assert!(!f.worktree().join("feature.txt").exists());
        assert_eq!(
            hwahap::verification::recover_verifications(&store)
                .unwrap()
                .len(),
            if review_failure { 0 } else { 2 }
        );
        return;
    }
    result.unwrap();
    assert_eq!(
        store.read_run().unwrap().unwrap().accepted_units,
        vec!["U1"]
    );
}

#[tokio::test]
async fn shared_mutable_resource_routes_parent_without_overlapping_paths() {
    let (f, store, mut a) = native_fixture(false);
    a.topology.shared_resources = vec!["database:migrations".into()];
    let mut other = a.clone();
    other.unit = Some("U2".into());
    other.topology.writer_owner = other.unit.clone();
    other.topology.write_paths = vec!["output-2".into()];
    hwahap::delegation::store::record(&store, &other).unwrap();
    assert_eq!(
        dispatch_job(
            &f,
            &store,
            &a,
            NativeLane::Coordinator,
            "gpt-6-astra",
            "coordinator"
        )
        .await,
        "shared_state"
    );
}
#[tokio::test]
async fn stale_host_records_wait_without_starting_a_dispatch() {
    let (f, store, a) = native_fixture(false);
    store
        .append_event(
            &hwahap::clock::SystemClock,
            "host_observation_required",
            serde_json::json!({"reason":"fixture refresh required"}),
        )
        .unwrap();
    assert!(refusal(&f, &store, &a, true).await.contains("host_stale"));
    let decisions: Vec<_> = std::fs::read_dir(store.artifacts_path())
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("delegation-")
                .then(|| {
                    serde_json::from_slice::<DelegationDecision>(&std::fs::read(p).unwrap())
                        .unwrap()
                })
        })
        .collect();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].reason_codes, vec!["host_stale"]);
    assert_eq!(decisions[0].route, Route::Wait);
}
#[test]
fn planned_profile_changes_unit_fingerprint_and_invalid_owner_cannot_pin_evidence() {
    let (_f, store, mut a) = native_fixture(false);
    let mut plan = store.read_plan().unwrap().unwrap();
    let previous = plan.unit_fingerprint("U1").unwrap();
    plan.task_profiles.insert("U1".into(), a.profile());
    assert_ne!(previous, plan.unit_fingerprint("U1").unwrap());
    a.topology.writer_owner = Some("U2".into());
    assert!(hwahap::delegation::store::record(&store, &a).is_err());
    a.topology.writer_owner = Some("U1".into());
    hwahap::delegation::store::record(&store, &a).unwrap();
}

#[tokio::test]
async fn interrupted_high_risk_author_resumes_after_stop_with_same_attempt() {
    high_risk_case(false, false, 1).await;
}
async fn resume_interrupted_high_risk(
    f: &common::Fixture,
    store: &Store,
    previous: &hwahap::native::NativeDispatch,
    interruption: u8,
) {
    use hwahap::native::{NativeHost, NativeInput, NativeStopped};
    let host = NativeHost::default();
    let input = || NativeInput {
        host_session_id: Some("parent".into()),
        ..Default::default()
    };
    let waiting = host.advance(&f.repo, input()).await.unwrap();
    assert_eq!(waiting.outcome.next, "native_stop");
    assert_eq!(
        std::fs::read_to_string(f.worktree().join("feature.txt")).unwrap(),
        "partial interrupted work"
    );
    let blocked = f
        .engine()
        .step_with(&common::Script::new(vec![]), None, None)
        .await
        .unwrap_err();
    assert!(blocked.to_string().contains("native stop acknowledgment"));
    assert_eq!(
        std::fs::read_to_string(f.worktree().join("feature.txt")).unwrap(),
        "partial interrupted work"
    );
    if interruption >= 2 {
        // Recreate the crash boundary: the failed event is durable, cleanup has not run.
        let event = store
            .read_events()
            .unwrap()
            .into_iter()
            .rev()
            .find(|e| e.kind == "unit_attempt_started")
            .unwrap();
        let mut attempt: hwahap::revalidation::UnitAttempt =
            serde_json::from_value(event.data).unwrap();
        let code = hwahap::git::Git::open(&f.repo)
            .unwrap()
            .fingerprint(&f.worktree())
            .unwrap();
        for number in 1..if interruption == 4 { 2 } else { interruption } {
            if number > 1 {
                attempt = hwahap::revalidation::reserve_attempt(
                    store,
                    &hwahap::clock::SystemClock,
                    &attempt.run_id,
                    &attempt.unit_id,
                    attempt.kind,
                    &attempt.round,
                    128,
                )
                .unwrap();
            }
            hwahap::revalidation::finish_attempt(
                store,
                &hwahap::clock::SystemClock,
                &attempt,
                false,
                serde_json::json!({"findings":["candidate rejected"],"candidate_code":code}),
            )
            .unwrap();
        }
    }
    if interruption == 4 {
        let event = store
            .read_events()
            .unwrap()
            .into_iter()
            .rev()
            .find(|e| e.kind == "unit_attempt_started")
            .unwrap();
        let attempt: hwahap::revalidation::UnitAttempt =
            serde_json::from_value(event.data).unwrap();
        let git = hwahap::git::Git::open(&f.repo).unwrap();
        let code = git.fingerprint(&f.worktree()).unwrap();
        let name = format!("interrupted-author-{}-{code}.json", attempt.id);
        hwahap::pr_review::save_evidence(store, &name, &serde_json::json!({"attempt":attempt,"head":common::git(&f.worktree(), &["rev-parse","HEAD"]),"code":code,"patch":git.candidate_patch(&f.worktree()).unwrap(),"native_work_stopped":true})).unwrap();
        std::fs::write(
            f.worktree().join("feature.txt"),
            "partly restored candidate",
        )
        .unwrap();
    }
    let before = store
        .read_events()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "unit_attempt_started")
        .map(|e| e.data)
        .collect::<Vec<_>>();
    let stopped = NativeInput {
        stopped: Some(NativeStopped {
            dispatch_id: previous.dispatch_id.clone(),
            agent_id: Some("coordinator".into()),
            all_work_stopped: true,
        }),
        ..input()
    };
    host.advance(&f.repo, stopped).await.unwrap();
    if interruption == 3 {
        let result = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let result = host.advance(&f.repo, input()).await.unwrap();
                assert!(result.dispatch.is_none());
                if result.outcome.state == "blocked" {
                    break result;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(result.outcome.message.contains("budget exhausted"));
        assert!(!f.worktree().join("feature.txt").exists());
        assert_eq!(
            store
                .read_events()
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == "unit_attempt_started")
                .map(|e| e.data)
                .collect::<Vec<_>>(),
            before
        );
        assert!(store
            .read_events()
            .unwrap()
            .iter()
            .any(|e| e.kind == "interrupted_author_reconciled"));
        host.shutdown().await;
        return;
    }
    let author_role = if interruption >= 2 {
        Role::Rework
    } else {
        Role::Implementer
    };
    let mut last = previous.dispatch_id.clone();
    for (role, agent) in [(author_role, "coordinator"), (Role::UnitReviewer, "critic")] {
        let request = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let progress = host.advance(&f.repo, input()).await.unwrap();
                if let Some(d) = progress.dispatch {
                    if d.dispatch_id != last {
                        break d;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(request.role, role.as_str());
        last = request.dispatch_id.clone();
        if role == author_role {
            assert!(!f.worktree().join("feature.txt").exists());
            if interruption == 1 {
                assert_eq!(
                    store
                        .read_events()
                        .unwrap()
                        .into_iter()
                        .filter(|e| e.kind == "unit_attempt_started")
                        .map(|e| e.data)
                        .collect::<Vec<_>>(),
                    before
                );
            } else {
                let events = store.read_events().unwrap();
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e.kind == "unit_attempt_started")
                        .count(),
                    before.len() + 1
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e.kind == "unit_attempt_finished" && e.data["passed"] == false)
                        .count(),
                    1
                );
            }
            let saved = std::fs::read_dir(store.artifacts_path())
                .unwrap()
                .filter_map(|e| {
                    let p = e.unwrap().path();
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("interrupted-author-")
                        .then(|| std::fs::read_to_string(p).unwrap())
                })
                .collect::<Vec<_>>();
            assert!(saved.iter().any(|s| s.contains("partial interrupted work")));
        }
        host.advance(
            &f.repo,
            NativeInput {
                registration: Some(NativeRegistration {
                    decision_digest: Some(request.decision.digest.clone()),
                    dispatch_id: request.dispatch_id.clone(),
                    agent_id: agent.into(),
                }),
                ..input()
            },
        )
        .await
        .unwrap();
        let result = if role == author_role {
            std::fs::write(f.worktree().join("feature.txt"), "ready").unwrap();
            serde_json::json!({"status":"completed","summary":"recovered","conflict":null})
        } else {
            serde_json::json!({"verdict":"pass","findings":[]})
        };
        host.advance(
            &f.repo,
            NativeInput {
                completion: Some(NativeCompletion {
                    decision_digest: Some(request.decision.digest.clone()),
                    dispatch_id: request.dispatch_id.clone(),
                    agent_id: agent.into(),
                    agent_stopped: true,
                    reported_usage: None,
                    final_message:
                        serde_json::json!({"dispatch_id":request.dispatch_id,"result":result})
                            .to_string(),
                }),
                ..input()
            },
        )
        .await
        .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            if store
                .read_run()
                .unwrap()
                .unwrap()
                .accepted_units
                .contains(&"U1".into())
            {
                break;
            }
            host.advance(&f.repo, input()).await.unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    host.shutdown().await;
}
#[tokio::test]
async fn author_available_without_independent_reviewers_waits_before_dispatch() {
    let (f, store, a) = native_fixture(false);
    let mut host = hwahap::catalog::host::latest(&store).unwrap().unwrap();
    host.models.retain(|id, _| id == "gpt-5.6-luna");
    hwahap::catalog::host::observe(&store, &hwahap::clock::SystemClock, "parent", &host).unwrap();
    assert!(refusal(&f, &store, &a, true)
        .await
        .contains("model_unavailable"));
}

#[tokio::test]
async fn author_waits_when_slots_cannot_cover_worker_and_both_reviewers() {
    let (f, store, a) = native_fixture(false);
    let mut observed = hwahap::catalog::host::latest(&store).unwrap().unwrap();
    observed.available_slots = 2;
    hwahap::catalog::host::observe(&store, &hwahap::clock::SystemClock, "parent", &observed)
        .unwrap();
    assert!(refusal(&f, &store, &a, true)
        .await
        .contains("slot_unavailable"));
}

#[tokio::test]
async fn interrupted_rejected_high_risk_candidate_keeps_consumed_attempt_and_retries() {
    high_risk_case(false, false, 2).await;
}
#[tokio::test]
async fn interrupted_rejected_high_risk_candidate_reaches_exhausted_budget() {
    high_risk_case(false, false, 3).await;
}

#[tokio::test]
async fn interrupted_rejected_candidate_resumes_partly_completed_cleanup_from_backup() {
    high_risk_case(false, false, 4).await;
}
