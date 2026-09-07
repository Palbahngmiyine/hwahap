#![cfg(unix)]

use hwahap::{canonical::Digest, clock::FixedClock, revalidation::*, state::Store};
#[test]
fn t15_attempts_survive_restarts_and_different_rounds_with_separate_budgets() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let round = Digest::of(&1).unwrap();
    let first = reserve_attempt(
        &store,
        &clock,
        "run",
        "U1",
        AttemptKind::Implementation,
        &round,
        2,
    )
    .unwrap();
    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(
        reserve_attempt(
            &reopened,
            &clock,
            "run",
            "U1",
            AttemptKind::Implementation,
            &round,
            2
        )
        .unwrap(),
        first
    );
    finish_attempt(&store, &clock, &first, false, serde_json::json!("failed")).unwrap();
    let second = reserve_attempt(
        &store,
        &clock,
        "run",
        "U1",
        AttemptKind::Implementation,
        &Digest::of(&2).unwrap(),
        2,
    )
    .unwrap();
    assert_eq!(second.total, 2);
    finish_attempt(&store, &clock, &second, true, serde_json::json!("passed")).unwrap();
    assert!(reserve_attempt(
        &store,
        &clock,
        "run",
        "U1",
        AttemptKind::Implementation,
        &Digest::of(&3).unwrap(),
        2
    )
    .is_err());
    assert_eq!(
        reserve_attempt(
            &store,
            &clock,
            "run",
            "U1",
            AttemptKind::Revalidation,
            &round,
            2
        )
        .unwrap()
        .total,
        1
    );
}

mod common;
use common::{git, step, Fixture, Reply, Script};
use hwahap::{
    engine::{AdjustBuildRequest, BuildRequest, BuildUnit},
    profile::Role,
};
async fn reviewed() -> Fixture {
    reviewed_with_test("test -f two", None).await
}
async fn reviewed_with_test(second_test: &str, timeout: Option<u64>) -> Fixture {
    let f = Fixture::new();
    if let Some(timeout) = timeout {
        std::fs::create_dir_all(f.repo.join(".hwahap")).unwrap();
        std::fs::write(
            f.repo.join(".hwahap/config.toml"),
            format!("[limits]\ntest_timeout_secs={timeout}\n"),
        )
        .unwrap();
    }
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let engine = f.engine();
    engine
        .start_build(&BuildRequest {
            task_profiles: Default::default(),
            verification_inputs: vec![],
            user_instruction: "Build without planning".into(),
            objective: "Write two files".into(),
            base_branch: "main".into(),
            branch: "codex/adjust-build".into(),
            full_suite: "test -f one && test -f two".into(),
            units: ["one", "two"]
                .map(|name| BuildUnit {
                    title: name.into(),
                    acceptance: format!("{name} exists"),
                    paths: vec![name.into()],
                    test_command: if name == "two" {
                        second_test.into()
                    } else {
                        format!("test -f {name}")
                    },
                })
                .into(),
        })
        .unwrap();
    let script = implementation("initial");
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "final_verifying"
    );
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "pr_review"
    );
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "awaiting_adjust_or_ship"
    );
    f
}

fn implementation(value: &str) -> Script {
    let mut steps = vec![];
    for name in ["one", "two"] {
        steps.push(step(
            Role::Implementer,
            Reply::write(
                &[(name, value)],
                r#"{"status":"completed","summary":"wrote file"}"#,
            ),
        ));
        steps.push(step(
            Role::UnitReviewer,
            Reply::say(r#"{"verdict":"pass"}"#),
        ));
    }
    steps.push(step(Role::UnitReviewer, Reply::PrAttack));
    steps.push(step(Role::FinalReview, Reply::pr_defense()));
    Script::new(steps)
}

#[tokio::test]
async fn t09_dependency_revalidates_without_author_call_or_extra_commit() {
    let fixture = reviewed().await;
    let engine = fixture.engine();
    let store = Store::open(&fixture.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    let before = git(&fixture.worktree(), &["rev-list", "--count", "HEAD"])
        .parse::<u64>()
        .unwrap();
    engine
        .adjust_build(&AdjustBuildRequest {
            task_profiles: Default::default(),
            user_instruction: "Correct only one".into(),
            contract_digest: plan.digest().unwrap().to_string(),
            unit_ids: vec!["U1".into()],
        })
        .unwrap();
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("one", "corrected")],
                r#"{"status":"completed","summary":"corrected one"}"#,
            ),
        ),
        step(Role::UnitReviewer, Reply::say(r#"{"verdict":"pass"}"#)),
        step(Role::UnitReviewer, Reply::say(r#"{"verdict":"pass"}"#)),
    ]);
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "final_verifying"
    );
    assert_eq!(
        script
            .roles()
            .iter()
            .filter(|r| **r == Role::Implementer || **r == Role::Rework)
            .count(),
        1
    );
    assert_eq!(script.remaining(), 0);
    let attempts = store.read_events().unwrap();
    assert_eq!(
        attempts
            .iter()
            .filter(|e| e.kind == "unit_attempt_started" && e.data["kind"] == "implementation")
            .count(),
        3
    );
    assert_eq!(
        attempts
            .iter()
            .filter(|e| e.kind == "unit_attempt_started" && e.data["kind"] == "revalidation")
            .count(),
        1
    );

    assert_eq!(
        git(&fixture.worktree(), &["rev-list", "--count", "HEAD"])
            .parse::<u64>()
            .unwrap(),
        before + 1
    );
    assert_eq!(
        std::fs::read_to_string(fixture.worktree().join("two")).unwrap(),
        "initial"
    );
    assert!(
        obligations(&store, &store.read_run().unwrap().unwrap().run_id, "U1")
            .unwrap()
            .is_empty()
    );
    assert!(hwahap::verification::recover_verifications(&store)
        .unwrap()
        .values()
        .any(|v| v.request.unit_id.as_deref() == Some("U2")
            && v.request.kind == hwahap::verification::Kind::Revalidation));
}

#[test]
fn t10_overlapping_path_owners_are_direct_and_prefix_neighbors_are_separate() {
    use hwahap::plan::{Plan, Unit};
    let mut plan = Plan::new("run", "main", "scope");
    plan.units = ["./src/", "src/lib.rs", "src2/"]
        .iter()
        .enumerate()
        .map(|(i, path)| Unit {
            id: format!("U{}", i + 1),
            title: "scope".into(),
            paths: vec![path.to_string()],
            acceptance_ids: vec![],
            depends_on: vec![],
            probe: false,
        })
        .collect();
    assert_eq!(owners(&plan, "src/lib.rs"), vec!["U1", "U2"]);
    assert_eq!(owners(&plan, "src2/lib.rs"), vec!["U3"]);
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    record_obligation(
        &store,
        &clock,
        "run",
        &plan,
        owners(&plan, "src/lib.rs"),
        "pr_finding",
        "observed finding",
    )
    .unwrap();
    assert_eq!(obligations(&store, "run", "U1").unwrap().len(), 1);
    assert_eq!(obligations(&store, "run", "U2").unwrap().len(), 1);
    assert!(obligations(&store, "run", "U3").unwrap().is_empty());
}

#[tokio::test]
async fn t10_pr_findings_map_to_units_and_close_only_after_changed_head_reviews() {
    use hwahap::pr_review::ReviewProgress;
    for unmapped in [false, true] {
        let fixture = reviewed().await;
        let engine = fixture.engine();
        let store = Store::open(&fixture.repo).unwrap();
        engine.recheck_pr().unwrap();
        engine
            .step_with(&Script::new(vec![]), None, None)
            .await
            .unwrap();
        let binding = ReviewProgress::load(&store).unwrap().unwrap().binding;
        let finding = serde_json::json!({"id":"A1","file":if unmapped {"src/existing.txt"} else {"one"},"line":1,
            "condition":"dependency content changed","expected":"correct dependent output","observed":"initial output","evidence":["checked source and output"]});
        let rejection = Script::new(vec![
            step(Role::UnitReviewer,Reply::say(serde_json::json!({"binding":binding,"security":common::security_review(),"findings":[finding],"evidence":["observed output"]}).to_string())),
            step(Role::FinalReview,Reply::say(serde_json::json!({"binding":binding,"security":common::security_review(),"assessments":[{"finding_id":"A1","judgment":"confirmed","evidence":["independently observed output"]}],"additional_findings":[],"evidence":["checked source"]}).to_string()))]);
        engine.step_with(&rejection, None, None).await.unwrap();
        let mut steps = Vec::new();
        if unmapped {
            let before = git(&fixture.worktree(), &["rev-parse", "HEAD"]);
            let unresolved = Script::new(vec![step(
                Role::UnitReviewer,
                Reply::say(r#"{"unit_ids":[],"evidence":["source owner cannot be established"]}"#),
            )]);
            assert!(engine.step_with(&unresolved, None, None).await.is_err());
            assert_eq!(unresolved.calls().len(), 1);
            assert_eq!(git(&fixture.worktree(), &["rev-parse", "HEAD"]), before);
            steps.push(step(
                Role::UnitReviewer,
                Reply::say(
                    r#"{"unit_ids":["U1"],"evidence":["src/existing.txt:1 is consumed by U1"]}"#,
                ),
            ));
        }
        steps.push(step(
            Role::Rework,
            Reply::write(
                &[("one", "corrected")],
                r#"{"status":"completed","summary":"corrected output"}"#,
            ),
        ));
        let repair = Script::new(steps);
        engine.step_with(&repair, None, None).await.unwrap();
        assert_eq!(repair.remaining(), 0);
        let run_id = store.read_run().unwrap().unwrap().run_id;
        assert_eq!(obligations(&store, &run_id, "U1").unwrap().len(), 1);
        assert!(obligations(&store, &run_id, "U2").unwrap().is_empty());
        let reviews = Script::new(vec![
            step(Role::UnitReviewer, Reply::PrAttack),
            step(Role::FinalReview, Reply::pr_defense()),
        ]);
        assert_eq!(
            engine.step_with(&reviews, None, None).await.unwrap().state,
            "awaiting_adjust_or_ship"
        );
        assert!(obligations(&store, &run_id, "U1").unwrap().is_empty());
    }
}

fn correction_script() -> Vec<common::Step> {
    vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("one", "corrected")],
                r#"{"status":"completed","summary":"corrected one"}"#,
            ),
        ),
        step(Role::UnitReviewer, Reply::say(r#"{"verdict":"pass"}"#)),
    ]
}
fn adjust_one(fixture: &Fixture) {
    let plan = Store::open(&fixture.repo)
        .unwrap()
        .read_plan()
        .unwrap()
        .unwrap();
    fixture
        .engine()
        .adjust_build(&AdjustBuildRequest {
            task_profiles: Default::default(),
            user_instruction: "Correct one under unchanged requirements".into(),
            contract_digest: plan.digest().unwrap().to_string(),
            unit_ids: vec!["U1".into()],
        })
        .unwrap();
}
#[tokio::test]
async fn t11_failed_mutating_and_timed_out_revalidation_preserves_failed_attempts() {
    for (command, timeout) in [
        ("test \"$(cat one)\" = initial && test -f two", None),
        (
            "if [ \"$(cat one)\" = corrected ]; then printf mutated > two; fi; test -f two",
            None,
        ),
        (
            "if [ \"$(cat one)\" = corrected ]; then sleep 3; fi; test -f two",
            Some(1),
        ),
    ] {
        let fixture = reviewed_with_test(command, timeout).await;
        adjust_one(&fixture);
        let script = Script::new(correction_script());
        assert_eq!(
            fixture
                .engine()
                .step_with(&script, None, None)
                .await
                .unwrap()
                .state,
            "blocked"
        );
        assert_eq!(script.remaining(), 0);
        let store = Store::open(&fixture.repo).unwrap();
        let run = store.read_run().unwrap().unwrap();
        assert_eq!(run.accepted_units, vec!["U1"]);
        assert!(hwahap::verification::recover_verifications(&store)
            .unwrap()
            .values()
            .any(
                |v| v.request.kind == hwahap::verification::Kind::Revalidation
                    && v.status == hwahap::verification::Status::Failed
            ));
        assert!(store
            .read_events()
            .unwrap()
            .iter()
            .any(|e| e.kind == "unit_attempt_finished"
                && e.data["unit_id"] == "U2"
                && e.data["passed"] == false));
        assert_eq!(
            implementations(&store, &run.run_id)
                .unwrap()
                .iter()
                .filter(|i| i.unit_id == "U2")
                .count(),
            1
        );
    }
}
#[tokio::test]
async fn t12_interrupted_impact_review_keeps_one_logical_attempt_and_no_author_replay() {
    let fixture = reviewed().await;
    adjust_one(&fixture);
    let mut steps = correction_script();
    steps.push(step(
        Role::UnitReviewer,
        Reply::Fail("interrupted impact review".into()),
    ));
    let script = Script::new(steps);
    assert!(fixture
        .engine()
        .step_with(&script, None, None)
        .await
        .is_err());
    let before = git(&fixture.worktree(), &["rev-parse", "HEAD"]);
    script.extend(vec![step(
        Role::UnitReviewer,
        Reply::say(r#"{"verdict":"pass"}"#),
    )]);
    assert_eq!(
        fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "final_verifying"
    );
    assert_eq!(git(&fixture.worktree(), &["rev-parse", "HEAD"]), before);
    let store = Store::open(&fixture.repo).unwrap();
    assert_eq!(
        store
            .read_events()
            .unwrap()
            .iter()
            .filter(|e| e.kind == "unit_attempt_started" && e.data["kind"] == "revalidation")
            .count(),
        1
    );
    assert_eq!(
        script
            .roles()
            .iter()
            .filter(|r| **r == Role::Implementer)
            .count(),
        1
    );
}

#[tokio::test]
async fn t12_corrupt_implementation_evidence_stops_before_dependent_author_or_acceptance() {
    let fixture = reviewed().await;
    let store = Store::open(&fixture.repo).unwrap();
    let run = store.read_run().unwrap().unwrap();
    let mut record = implementations(&store, &run.run_id)
        .unwrap()
        .into_iter()
        .find(|r| r.unit_id == "U2")
        .unwrap();
    record.tree = "0".repeat(40);
    store
        .append_event(
            &FixedClock::new(common::NOW),
            "implementation_completed",
            serde_json::json!(record),
        )
        .unwrap();
    adjust_one(&fixture);
    let script = Script::new(correction_script());
    let outcome = fixture.engine().step_with(&script, None, None).await;
    assert!(outcome
        .unwrap_err()
        .to_string()
        .contains("commit/tree evidence"));
    assert_eq!(script.remaining(), 0);
    assert_eq!(
        store.read_run().unwrap().unwrap().accepted_units,
        vec!["U1"]
    );
}

struct InterruptAfterCommit {
    worktree: std::path::PathBuf,
    original: String,
}
impl hwahap::clock::Clock for InterruptAfterCommit {
    fn now(&self) -> String {
        assert_eq!(
            git(&self.worktree, &["rev-parse", "HEAD"]),
            self.original,
            "injected interruption after commit, before completion event"
        );
        common::NOW.into()
    }
}
#[tokio::test]
async fn t12_commit_before_completion_reconciles_without_duplicate_author_or_commit() {
    let fixture = reviewed().await;
    adjust_one(&fixture);
    let original = git(&fixture.worktree(), &["rev-parse", "HEAD"]);
    let interrupted = fixture.engine().with_parts(
        Box::new(InterruptAfterCommit {
            worktree: fixture.worktree(),
            original: original.clone(),
        }),
        hwahap::forge::Forge::with_program(fixture.gh.to_str().unwrap()),
    );
    let task = tokio::spawn(async move {
        interrupted
            .step_with(&Script::new(correction_script()), None, None)
            .await
    });
    assert!(task.await.unwrap_err().is_panic());
    let committed = git(&fixture.worktree(), &["rev-parse", "HEAD"]);
    assert_ne!(committed, original);
    let store = Store::open(&fixture.repo).unwrap();
    let run = store.read_run().unwrap().unwrap();
    assert!(implementations(&store, &run.run_id)
        .unwrap()
        .iter()
        .all(|r| r.commit != committed));
    let resume = Script::new(vec![step(
        Role::UnitReviewer,
        Reply::say(r#"{"verdict":"pass"}"#),
    )]);
    assert_eq!(
        fixture
            .engine()
            .step_with(&resume, None, None)
            .await
            .unwrap()
            .state,
        "final_verifying"
    );
    assert_eq!(resume.remaining(), 0);
    assert_eq!(git(&fixture.worktree(), &["rev-parse", "HEAD"]), committed);
    assert_eq!(
        implementations(&store, &run.run_id)
            .unwrap()
            .iter()
            .filter(|r| r.commit == committed)
            .count(),
        1
    );
}

#[tokio::test]
async fn t15_post_commit_pr_failure_and_timeout_finish_attempts_and_bound_revalidation() {
    use hwahap::pr_review::ReviewProgress;
    for (timeout, transient) in [(false, false), (true, false), (false, true)] {
        let marker = tempfile::NamedTempFile::new().unwrap();
        let transient_command = format!("case $(git log -1 --format=%s) in 'hwahap(PR):'*) test ! -f '{}' || exit 1;; esac; test -f two", marker.path().display());
        let command = if transient {
            transient_command.as_str()
        } else if timeout {
            "case $(git log -1 --format=%s) in 'hwahap(PR):'*) sleep 3;; esac; test -f two"
        } else {
            "case $(git log -1 --format=%s) in 'hwahap(PR):'*) exit 1;; esac; test -f two"
        };
        let fixture = reviewed_with_test(command, timeout.then_some(1)).await;
        let engine = fixture.engine();
        let store = Store::open(&fixture.repo).unwrap();
        engine.recheck_pr().unwrap();
        engine
            .step_with(&Script::new(vec![]), None, None)
            .await
            .unwrap();
        let unmapped = false;
        let binding = ReviewProgress::load(&store).unwrap().unwrap().binding;
        let finding = serde_json::json!({"id":"A1","file":if unmapped {"src/existing.txt"} else {"one"},"line":1,
            "condition":"dependency content changed","expected":"correct dependent output","observed":"initial output","evidence":["checked source and output"]});
        let rejection = Script::new(vec![
            step(Role::UnitReviewer,Reply::say(serde_json::json!({"binding":binding,"security":common::security_review(),"findings":[finding],"evidence":["observed output"]}).to_string())),
            step(Role::FinalReview,Reply::say(serde_json::json!({"binding":binding,"security":common::security_review(),"assessments":[{"finding_id":"A1","judgment":"confirmed","evidence":["independently observed output"]}],"additional_findings":[],"evidence":["checked source"]}).to_string()))]);
        engine.step_with(&rejection, None, None).await.unwrap();

        let repair = Script::new(vec![step(
            Role::Rework,
            Reply::write(
                &[("one", "corrected")],
                r#"{"status":"completed","summary":"fixed"}"#,
            ),
        )]);
        assert!(engine.step_with(&repair, None, None).await.is_err());
        let head = git(&fixture.worktree(), &["rev-parse", "HEAD"]);
        let failed = store.read_events().unwrap();
        assert!(failed.iter().any(|e| e.kind == "unit_attempt_finished"
            && e.data["unit_id"] == "U1"
            && e.data["passed"] == false));
        let empty = Script::new(vec![]);
        if transient {
            marker.close().unwrap();
            assert_eq!(
                engine.step_with(&empty, None, None).await.unwrap().state,
                "pr_review"
            );
            let reviews = Script::new(vec![
                step(Role::UnitReviewer, Reply::PrAttack),
                step(Role::FinalReview, Reply::pr_defense()),
            ]);
            assert_eq!(
                engine.step_with(&reviews, None, None).await.unwrap().state,
                "awaiting_adjust_or_ship"
            );
            assert_eq!(git(&fixture.worktree(), &["rev-parse", "HEAD"]), head);
            let events = store.read_events().unwrap();
            assert!(events.iter().any(|e| e.kind == "unit_attempt_finished"
                && e.data["unit_id"] == "U1"
                && e.data["passed"] == false));
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.kind == "unit_attempt_started"
                        && e.data["unit_id"] == "U1"
                        && e.data["kind"] == "revalidation")
                    .count(),
                2
            );
            assert_eq!(repair.calls().len(), 1);
            assert!(empty.calls().is_empty());
            continue;
        }
        assert!(engine.step_with(&empty, None, None).await.is_err());
        let verification_count = hwahap::verification::recover_verifications(&store)
            .unwrap()
            .len();
        let budget = engine.step_with(&empty, None, None).await.unwrap();
        assert_eq!(budget.state, "blocked");
        assert!(
            budget.message.contains("budget exhausted"),
            "{}",
            budget.message
        );
        assert_eq!(
            hwahap::verification::recover_verifications(&store)
                .unwrap()
                .len(),
            verification_count
        );
        assert_eq!(git(&fixture.worktree(), &["rev-parse", "HEAD"]), head);
        assert_eq!(repair.calls().len(), 1);
        assert!(empty.calls().is_empty());
    }
}

struct InterruptedPrWriter;
impl hwahap::engine::Sessions for InterruptedPrWriter {
    fn run<'a>(
        &'a self,
        spec: &'a hwahap::session::SessionSpec,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = hwahap::error::Result<hwahap::session::SessionOutcome>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            assert_eq!(spec.role, Role::Rework);
            std::fs::write(spec.cwd.join("one"), "interrupted PR candidate").unwrap();
            Err(hwahap::error::Error::Internal(
                "injected author interruption".into(),
            ))
        })
    }
}
#[tokio::test]
async fn interrupted_pr_author_preserves_patch_and_resumes_same_repair_attempt() {
    use hwahap::pr_review::ReviewProgress;
    let fixture = reviewed().await;
    let engine = fixture.engine();
    let store = Store::open(&fixture.repo).unwrap();
    engine.recheck_pr().unwrap();
    engine
        .step_with(&Script::new(vec![]), None, None)
        .await
        .unwrap();
    let binding = ReviewProgress::load(&store).unwrap().unwrap().binding;
    let finding = serde_json::json!({"id":"A1","file":"one","line":1,"condition":"wrong output","expected":"corrected","observed":"initial","evidence":["checked actual file"]});
    let rejection = Script::new(vec![
        step(Role::UnitReviewer, Reply::say(serde_json::json!({"binding":binding,"security":common::security_review(),"findings":[finding],"evidence":["observed output"]}).to_string())),
        step(Role::FinalReview, Reply::say(serde_json::json!({"binding":binding,"security":common::security_review(),"assessments":[{"finding_id":"A1","judgment":"confirmed","evidence":["independent observation"]}],"additional_findings":[],"evidence":["checked source"]}).to_string()))]);
    engine.step_with(&rejection, None, None).await.unwrap();
    assert!(engine
        .step_with(&InterruptedPrWriter, None, None)
        .await
        .unwrap_err()
        .to_string()
        .contains("injected author interruption"));
    assert_eq!(
        std::fs::read_to_string(fixture.worktree().join("one")).unwrap(),
        "interrupted PR candidate"
    );
    let repairs = ReviewProgress::load(&store).unwrap().unwrap().repairs;
    let attempts = || {
        store
            .read_events()
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "unit_attempt_started" && e.data["kind"] == "implementation")
            .map(|e| e.data)
            .collect::<Vec<_>>()
    };
    let before = attempts();
    let repair = Script::new(vec![step(
        Role::Rework,
        Reply::write(
            &[("one", "corrected")],
            r#"{"status":"completed","summary":"fixed"}"#,
        ),
    )]);
    let result = fixture
        .engine()
        .step_with(&repair, None, None)
        .await
        .unwrap();
    assert_eq!(result.state, "pr_review");
    assert_eq!(attempts(), before);
    assert_eq!(
        ReviewProgress::load(&store).unwrap().unwrap().repairs,
        repairs
    );
    let saved = store
        .read_events()
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "interrupted_author_reconciled")
        .unwrap();
    let artifact = std::fs::read_to_string(
        store
            .artifacts_path()
            .join(saved.data["evidence"].as_str().unwrap()),
    )
    .unwrap();
    assert!(artifact.contains("interrupted PR candidate"));
    assert_eq!(
        std::fs::read_to_string(fixture.worktree().join("one")).unwrap(),
        "corrected"
    );
}
