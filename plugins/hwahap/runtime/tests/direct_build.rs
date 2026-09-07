#![cfg(unix)]
mod common;

use common::{git, security_review, step, Fixture, Reply, Script};
use hwahap::engine::{BuildRequest, BuildUnit};
use hwahap::profile::Role;
use hwahap::state::Store;

fn request() -> BuildRequest {
    BuildRequest {
        task_profiles: Default::default(),
        verification_inputs: vec![],
        user_instruction: "기획 제외하고 구현해 줘".into(),
        objective: "Create a checked feature".into(),
        base_branch: "main".into(),
        branch: "codex/direct-build".into(),
        full_suite: "test -f feature.txt".into(),
        units: vec![BuildUnit {
            title: "Create feature".into(),
            acceptance: "feature.txt contains ready".into(),
            paths: vec!["feature.txt".into()],
            test_command: "test \"$(cat feature.txt)\" = ready".into(),
        }],
    }
}

#[test]
fn t04_direct_build_rejects_an_empty_test_before_recording_authority() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let mut input = request();
    input.units[0].test_command.clear();
    assert!(fixture.engine().start_build(&input).is_err());
    assert!(Store::open(&fixture.repo)
        .unwrap()
        .read_run()
        .unwrap()
        .is_none());
    assert!(!fixture.worktree().exists());
}

#[tokio::test]
async fn recheck_rejects_mixed_actions_before_any_work() {
    let fixture = Fixture::new();
    let host = hwahap::native::NativeHost::default();
    let error = host
        .advance(
            &fixture.repo,
            hwahap::native::NativeInput {
                recheck_pr: true,
                request: Some("new work".into()),
                ..Default::default()
            },
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("exactly one"));
    assert!(!fixture.worktree().exists());
}

#[test]
fn review_policy_requires_two_astra_lanes_and_keeps_authorship_separate() {
    use hwahap::native::NativeLane;
    use hwahap::profile::Profiles;
    Profiles::defaults().require_astra_reviewers().unwrap();
    assert_eq!(NativeLane::for_role(Role::UnitReviewer), NativeLane::Critic);
    assert_eq!(NativeLane::for_role(Role::FinalReview), NativeLane::Auditor);
    assert_eq!(NativeLane::for_role(Role::Rework), NativeLane::Coordinator);
    let custom = "[profiles.economy]\nmodel='gpt-5.6-luna'\neffort='medium'\n[profiles.critic]\nmodel='gpt-5.6-terra'\neffort='high'\n[profiles.deep]\nmodel='gpt-6-astra'\neffort='high'";
    assert!(Profiles::from_toml(custom)
        .unwrap()
        .require_astra_reviewers()
        .is_err());
}

#[tokio::test]
async fn direct_build_reaches_a_real_commit_and_draft_without_planning() {
    exercise_repair(false, false, 3).await;
}
#[tokio::test]
async fn exhausted_repair_keeps_the_draft_and_budget() {
    exercise_repair(true, false, 0).await;
}
#[tokio::test]
async fn same_native_reviewer_cannot_approve_both_pr_teams() {
    exercise_repair(false, true, 0).await;
}
#[tokio::test]
async fn persistent_pr_head_lag_is_bounded_and_keeps_the_draft() {
    exercise_repair(false, false, 4).await;
}
#[tokio::test]
async fn unexpected_pr_head_is_rejected_without_waiting_for_convergence() {
    exercise_repair(false, false, 5).await;
}
async fn exercise_repair(exhausted: bool, same_agent: bool, lag: u32) {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let input = request();
    let engine = fixture.engine();
    let outcome = engine.start_build(&input).unwrap();
    assert_eq!(
        (outcome.phase.as_str(), outcome.state.as_str()),
        ("build", "coding")
    );
    assert_eq!(engine.start_build(&input).unwrap().run_id, outcome.run_id);
    let store = Store::open(&fixture.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    assert_eq!(
        plan.frozen.as_ref().unwrap().answer_text,
        input.user_instruction
    );
    assert!(plan.decisions.is_empty() && plan.reviews.critic.is_none());
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("feature.txt", "ready")],
                r#"{"status":"completed","summary":"Created feature","conflict":null}"#,
            ),
        ),
        step(
            Role::UnitReviewer,
            Reply::say(r#"{"verdict":"pass","findings":[]}"#),
        ),
    ]);
    let next = engine.step_with(&script, None, None).await.unwrap();
    assert_eq!(next.state, "final_verifying", "{}", next.message);
    let done = engine.step_with(&script, None, None).await.unwrap();
    assert_eq!(done.state, "pr_review", "{}", done.message);
    assert_eq!(done.next, "continue");
    let progress = hwahap::pr_review::ReviewProgress::load(&store)
        .unwrap()
        .unwrap();
    assert_eq!(
        progress.binding.head,
        git(&fixture.worktree(), &["rev-parse", "HEAD"])
    );
    assert_eq!(progress.binding.contract_digest, plan.digest().unwrap());
    assert_eq!(progress.stage, hwahap::pr_review::ReviewStage::Attack);
    assert!(engine.ship("SHIP anything").is_err());
    let binding = progress.binding;
    let finding = serde_json::json!({"id":"A1","file":"feature.txt","line":1,"condition":"Read feature as a line","expected":"ready followed by newline","observed":"no terminal newline","evidence":["inspected exact file bytes"]});
    let rejection = Script::new(vec![
        step(Role::UnitReviewer, Reply::say(serde_json::json!({"binding":binding,"security":security_review(),"findings":[finding],"evidence":["checked file bytes"]}).to_string())),
        step(Role::FinalReview, Reply::say(serde_json::json!({"binding":binding,"security":security_review(),"assessments":[{"finding_id":"A1","judgment":"confirmed","evidence":["independently read missing newline"]}],"additional_findings":[],"evidence":["confirmed byte comparison"]}).to_string())),
    ]);
    assert_eq!(
        engine
            .step_with(&rejection, None, None)
            .await
            .unwrap()
            .state,
        "pr_review"
    );
    if exhausted {
        let mut p = hwahap::pr_review::ReviewProgress::load(&store)
            .unwrap()
            .unwrap();
        p.repairs = 64;
        p.save(&store).unwrap();
        let out = engine
            .step_with(&Script::new(vec![]), None, None)
            .await
            .unwrap();
        assert_eq!(out.state, "blocked");
        assert!(out.pr_url.is_some());
        assert_eq!(
            hwahap::pr_review::ReviewProgress::load(&store)
                .unwrap()
                .unwrap()
                .repairs,
            64
        );
        return;
    }
    if lag > 0 {
        std::fs::write(fixture.dir.path().join("pr-lag-head"), &binding.head).unwrap();
        std::fs::write(
            fixture.dir.path().join("pr-lag-left"),
            if lag == 5 {
                "3".into()
            } else {
                lag.to_string()
            },
        )
        .unwrap();
        if lag == 5 {
            std::fs::write(fixture.dir.path().join("pr-lag-value"), "unexpected-head").unwrap();
        }
    }
    let fix = Script::new(vec![step(
        Role::Rework,
        Reply::write(
            &[("feature.txt", "ready\n")],
            r#"{"status":"completed","summary":"Repaired line","conflict":null}"#,
        ),
    )]);
    let repaired = engine.step_with(&fix, None, None).await.unwrap();
    if lag > 0 {
        assert_eq!(
            std::fs::read_to_string(fixture.dir.path().join("pr-lag-left"))
                .unwrap()
                .trim(),
            if lag == 5 { "2" } else { "0" }
        );
    }
    if lag >= 4 {
        assert_eq!(repaired.state, "blocked");
        assert_eq!(repaired.pr_url.as_ref(), Some(&binding.pr_url));
        assert_eq!(fix.remaining(), 0);
        return;
    }
    assert_eq!(repaired.state, "pr_review", "{}", repaired.message);
    let next = hwahap::pr_review::ReviewProgress::load(&store)
        .unwrap()
        .unwrap();
    assert_eq!(next.repairs, 1);
    assert_eq!(next.round, 2);
    assert_ne!(next.binding.head, binding.head);
    assert_eq!(next.binding.pr_url, binding.pr_url);
    assert_eq!(
        std::fs::read_to_string(fixture.worktree().join("feature.txt")).unwrap(),
        "ready\n"
    );
    assert_eq!(fix.remaining(), 0);
    let stale = Script::new(vec![step(Role::UnitReviewer, Reply::say(
        serde_json::json!({"binding":binding,"security":security_review(),"findings":[],"evidence":["stale pre-repair head"]}).to_string()
    ))]);
    assert!(engine.step_with(&stale, None, None).await.is_err());
    assert!(engine
        .ship(&format!("SHIP {}", plan.digest().unwrap().challenge()))
        .is_err());
    let binding = next.binding;
    let reviews = Script::new(vec![
        step(Role::UnitReviewer, Reply::NativeReview { message: serde_json::json!({"binding":binding,"security":security_review(),"findings":[],"evidence":["checked published feature"]}).to_string(), agent_id: "attacker".into() }),
        step(Role::FinalReview, Reply::NativeReview { message: serde_json::json!({"binding":binding,"security":security_review(),"assessments":[],"additional_findings":[],"evidence":["independently checked published feature"]}).to_string(), agent_id: if same_agent { "attacker" } else { "defender" }.into() }),
    ]);
    let reviewed = engine.step_with(&reviews, None, None).await.unwrap();
    if same_agent {
        assert_eq!(reviewed.state, "blocked");
        assert!(reviewed.message.contains("distinct"));
        assert!(reviewed.pr_url.is_some());
        assert!(engine
            .ship(&format!("SHIP {}", plan.digest().unwrap().challenge()))
            .is_err());
        return;
    }
    assert_eq!(
        reviewed.state, "awaiting_adjust_or_ship",
        "{}",
        reviewed.message
    );
    assert_eq!(reviews.remaining(), 0);
    assert_eq!(
        hwahap::pr_review::ReviewProgress::load(&store)
            .unwrap()
            .unwrap()
            .stage,
        hwahap::pr_review::ReviewStage::Complete
    );
    assert_eq!(script.remaining(), 0);
    assert!(git(&fixture.worktree(), &["log", "--format=%s"]).contains("U1"));
    assert!(done.pr_url.is_some());
    let completed = hwahap::pr_review::ReviewProgress::load(&store)
        .unwrap()
        .unwrap();
    assert_eq!(engine.recheck_pr().unwrap().state, "final_verifying");
    assert!(engine
        .ship(&format!("SHIP {}", plan.digest().unwrap().challenge()))
        .is_err());
    assert_eq!(
        engine
            .step_with(&Script::new(vec![]), None, None)
            .await
            .unwrap()
            .state,
        "pr_review"
    );
    let recheck = hwahap::pr_review::ReviewProgress::load(&store)
        .unwrap()
        .unwrap();
    assert_eq!(recheck.round, completed.round + 1);
    assert_eq!(recheck.repairs, completed.repairs);
    assert_eq!(recheck.binding, completed.binding);
    let interrupted = Script::new(vec![
        step(Role::UnitReviewer, Reply::PrAttack),
        step(Role::FinalReview, Reply::Fail("connection lost".into())),
    ]);
    assert!(engine.step_with(&interrupted, None, None).await.is_err());
    let resumed = Script::new(vec![step(Role::FinalReview, Reply::pr_defense())]);
    assert_eq!(
        fixture
            .engine()
            .step_with(&resumed, None, None)
            .await
            .unwrap()
            .state,
        "awaiting_adjust_or_ship"
    );
    assert_eq!(resumed.roles(), vec![Role::FinalReview]);
    assert_eq!(
        git(&fixture.repo, &["ls-remote", "origin", &input.branch])
            .split_whitespace()
            .next(),
        Some(completed.binding.head.as_str())
    );
}

#[test]
fn invalid_build_cannot_create_a_worktree_or_execute_commands() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let mut input = request();
    input.units[0].paths = vec!["../escape".into()];
    assert!(fixture.engine().start_build(&input).is_err());
    assert!(!fixture.worktree().exists());
    assert!(Store::open(&fixture.repo)
        .unwrap()
        .read_run()
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn native_direct_build_routes_low_risk_authorship_by_task_requirements() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    fixture.engine().start_build(&request()).unwrap();
    let store = Store::open(&fixture.repo).unwrap();
    common::fixture_native_observation(&store, "direct-owner");
    let host = hwahap::native::NativeHost::default();
    let root = fixture.repo.canonicalize().unwrap();
    let dispatch = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let progress = host
                .advance(
                    &root,
                    hwahap::native::NativeInput {
                        host_session_id: Some("direct-owner".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            if let Some(dispatch) = progress.dispatch {
                break dispatch;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!dispatch.coordinator_allowed);
    assert_eq!(dispatch.lane, hwahap::native::NativeLane::Worker);
    assert_eq!(dispatch.model, "gpt-5.6-luna");
    assert_eq!(dispatch.effort, "medium");
    dispatch.verify_decision().unwrap();
    assert!(dispatch.reuse_agent_id.is_none());
    host.shutdown().await;
}

#[test]
fn interrupted_build_replays_the_sealed_intent_without_a_second_worktree() {
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let original = f.engine().start_build(&request()).unwrap();
    let store = Store::open(&f.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    // A crash after worktree creation but before publishing run state.
    std::fs::remove_file(store.root().join("run.json")).unwrap();
    std::fs::remove_file(store.root().join("events.jsonl")).unwrap();
    std::fs::remove_file(store.root().join("plan.json")).unwrap();
    let recovered = f.engine().start_build(&request()).unwrap();
    assert_eq!(recovered.run_id, original.run_id);
    assert_eq!(store.read_plan().unwrap().unwrap(), plan);
    assert_eq!(
        git(&f.repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        2
    );
    let mut changed = request();
    changed.user_instruction.push_str(" changed");
    assert!(f.engine().start_build(&changed).is_err());
}

#[test]
fn build_rejects_missing_traceability_edges() {
    let original = request().plan("goal", "base").unwrap();
    for kind in 0..3 {
        let mut plan = original.clone();
        match kind {
            0 => plan.acceptance[0].requirement_ids.clear(),
            1 => plan.units[0].acceptance_ids.clear(),
            _ => plan.tests[0].acceptance_ids.clear(),
        }
        assert!(hwahap::validate::build_blockers(&plan)
            .unwrap()
            .iter()
            .any(|e| e.code == "empty_build_trace"));
    }
}

#[tokio::test]
async fn tampered_direct_contract_stops_before_any_worker_or_command() {
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    f.engine().start_build(&request()).unwrap();
    let store = Store::open(&f.repo).unwrap();
    let mut plan = store.read_plan().unwrap().unwrap();
    plan.units[0].paths = vec!["outside.txt".into()];
    store.write_plan(&plan).unwrap();
    let script = Script::new(vec![]);
    let out = f.engine().step_with(&script, None, None).await.unwrap();
    assert_eq!(out.state, "blocked");
    assert!(script.calls().is_empty());
    assert!(!f.worktree().join("outside.txt").exists());
}

#[tokio::test]
async fn recheck_retains_original_pr_on_close_replacement_or_suite_failure() {
    for failure in [
        "missing_progress",
        "closed",
        "replacement",
        "suite",
        "closed_late",
        "replacement_late",
    ] {
        let f = Fixture::new();
        git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let fail_file = f.dir.path().join("fail-recheck");
        let mut input = request();
        input.full_suite = format!("test ! -e '{}'", fail_file.display());
        let engine = f.engine();
        engine.start_build(&input).unwrap();
        let script = Script::new(vec![
            step(
                Role::Implementer,
                Reply::write(
                    &[("feature.txt", "ready\n")],
                    r#"{"status":"completed","summary":"feature","conflict":null}"#,
                ),
            ),
            step(
                Role::UnitReviewer,
                Reply::say(r#"{"verdict":"pass","findings":[]}"#),
            ),
        ]);
        assert_eq!(
            engine.step_with(&script, None, None).await.unwrap().state,
            "final_verifying"
        );
        let draft = engine.step_with(&script, None, None).await.unwrap();
        let url = draft.pr_url.unwrap();
        let store = Store::open(&f.repo).unwrap();
        let plan = store.read_plan().unwrap().unwrap();
        let mut run = store.read_run().unwrap().unwrap();
        run.state = hwahap::state::RunState::AwaitingAdjustOrShip {
            pr_url: url.clone(),
            challenge: plan.digest().unwrap().challenge(),
        };
        store
            .write_run(&hwahap::clock::FixedClock::new(common::NOW), &run)
            .unwrap();
        if failure == "missing_progress" {
            let snapshot = std::fs::read(store.root().join("run.json")).unwrap();
            let path = store.artifacts_path().join("pr-review.json");
            std::fs::remove_file(&path).unwrap();
            assert!(engine
                .recheck_pr()
                .unwrap_err()
                .to_string()
                .contains("missing current PR review progress"));
            assert!(!path.exists());
            assert_eq!(
                std::fs::read(store.root().join("run.json")).unwrap(),
                snapshot
            );
            continue;
        }
        assert_eq!(engine.recheck_pr().unwrap().pr_url.as_ref(), Some(&url));
        let control = if failure.ends_with("_late") {
            "pr-list-after-read"
        } else {
            "pr-list-json"
        };
        match failure {
            "closed" | "closed_late" => std::fs::write(f.dir.path().join(control), "[]").unwrap(),
            "replacement" | "replacement_late" => std::fs::write(
                f.dir.path().join(control),
                serde_json::json!([{"url":"https://github.com/example/repo/pull/2","isDraft":true,
                "headRefName":input.branch,"baseRefName":"main"}])
                .to_string(),
            )
            .unwrap(),
            _ => std::fs::write(&fail_file, "fail").unwrap(),
        }
        let blocked = engine
            .step_with(&Script::new(vec![]), None, None)
            .await
            .unwrap();
        assert_eq!(blocked.state, "blocked", "{failure}: {}", blocked.message);
        assert_eq!(blocked.pr_url.as_ref(), Some(&url));
        assert!(!f.dir.path().join("pr-edited").exists());
        assert_eq!(
            std::fs::read_to_string(f.dir.path().join("pr-created"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(engine
            .ship(&format!("SHIP {}", plan.digest().unwrap().challenge()))
            .is_err());
        if failure == "suite" {
            std::fs::remove_file(fail_file).unwrap();
            assert_eq!(engine.recheck_pr().unwrap().pr_url.as_ref(), Some(&url));
            assert_eq!(
                engine
                    .step_with(&Script::new(vec![]), None, None)
                    .await
                    .unwrap()
                    .pr_url
                    .as_ref(),
                Some(&url)
            );
        }
    }
}

async fn prepared_for_adversarial_repair(f: &Fixture, full_suite: String) {
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let mut input = request();
    input.full_suite = full_suite;
    f.engine().start_build(&input).unwrap();
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("feature.txt", "ready")],
                r#"{"status":"completed","summary":"feature","conflict":null}"#,
            ),
        ),
        step(
            Role::UnitReviewer,
            Reply::say(r#"{"verdict":"pass","findings":[]}"#),
        ),
    ]);
    assert_eq!(
        f.engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "final_verifying"
    );
    assert_eq!(
        f.engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "pr_review"
    );
    let store = Store::open(&f.repo).unwrap();
    let binding = hwahap::pr_review::ReviewProgress::load(&store)
        .unwrap()
        .unwrap()
        .binding;
    let finding = serde_json::json!({"id":"A1","file":"feature.txt","line":1,"condition":"Read line","expected":"newline","observed":"no newline","evidence":["inspected bytes"]});
    let reviews = Script::new(vec![
        step(Role::UnitReviewer, Reply::say(serde_json::json!({"binding":binding,"security":security_review(),"findings":[finding],"evidence":["bytes checked"]}).to_string())),
        step(Role::FinalReview, Reply::say(serde_json::json!({"binding":binding,"security":security_review(),"assessments":[{"finding_id":"A1","judgment":"confirmed","evidence":["independently read bytes"]}],"additional_findings":[],"evidence":["bytes compared"]}).to_string())),
    ]);
    assert_eq!(
        f.engine()
            .step_with(&reviews, None, None)
            .await
            .unwrap()
            .state,
        "pr_review"
    );
    assert_eq!(
        hwahap::pr_review::ReviewProgress::load(&store)
            .unwrap()
            .unwrap()
            .stage,
        hwahap::pr_review::ReviewStage::Repair
    );
}

#[tokio::test]
async fn repair_rechecks_frozen_units_and_preserves_failures_for_retry() {
    let f = Fixture::new();
    prepared_for_adversarial_repair(&f, "test -f feature.txt".into()).await;
    let repair = |contents: &str| {
        Script::new(vec![step(
            Role::Rework,
            Reply::write(
                &[("feature.txt", contents)],
                r#"{"status":"completed","summary":"repair","conflict":null}"#,
            ),
        )])
    };
    let bad = repair("broken\n");
    let failed = f.engine().step_with(&bad, None, None).await.unwrap();
    assert_eq!(failed.state, "pr_review");
    assert!(failed.message.contains("check failed"));
    assert_eq!(
        std::fs::read_to_string(f.worktree().join("feature.txt")).unwrap(),
        "ready"
    );
    let store = Store::open(&f.repo).unwrap();
    let progress = hwahap::pr_review::ReviewProgress::load(&store)
        .unwrap()
        .unwrap();
    assert_eq!(progress.repairs, 1);
    let evidence = std::fs::read_to_string(store.artifacts_path().join(format!(
        "{}-failed-1.json",
        progress.artifact("repair").unwrap()
    )))
    .unwrap();
    assert!(evidence.contains("broken"));
    assert!(evidence.contains("cat feature.txt"));
    assert!(bad.calls()[0].prompt.contains("cat feature.txt"));
    assert!(f.engine().ship("SHIP invalid").is_err());
    let good = repair("ready\n");
    let result = f.engine().step_with(&good, None, None).await.unwrap();
    assert_eq!(result.state, "pr_review");
    let next = hwahap::pr_review::ReviewProgress::load(&store)
        .unwrap()
        .unwrap();
    assert_eq!(next.round, 2);
    assert_eq!(next.repairs, 2);
    assert!(good.calls()[0].prompt.contains("previous repair failed"));
    let reviews = Script::new(vec![
        step(Role::UnitReviewer, Reply::PrAttack),
        step(Role::FinalReview, Reply::pr_defense()),
    ]);
    assert_eq!(
        f.engine()
            .step_with(&reviews, None, None)
            .await
            .unwrap()
            .state,
        "awaiting_adjust_or_ship"
    );
}

#[tokio::test]
async fn transient_repair_suite_failure_can_retry_without_losing_the_draft() {
    let f = Fixture::new();
    let failure = f.dir.path().join("suite-fails");
    prepared_for_adversarial_repair(
        &f,
        format!("test -f feature.txt && test ! -e '{}'", failure.display()),
    )
    .await;
    std::fs::write(&failure, "transient failure").unwrap();
    let fix = || {
        Script::new(vec![step(
            Role::Rework,
            Reply::write(
                &[("feature.txt", "ready\n")],
                r#"{"status":"completed","summary":"Repaired line","conflict":null}"#,
            ),
        )])
    };
    let failed = f.engine().step_with(&fix(), None, None).await.unwrap();
    assert_eq!(failed.state, "pr_review");
    assert_eq!(failed.next, "continue");
    std::fs::remove_file(&failure).unwrap();
    let retried = f.engine().step_with(&fix(), None, None).await.unwrap();
    assert_eq!(retried.state, "pr_review");
    assert_eq!(retried.pr_url, failed.pr_url);
    let progress = hwahap::pr_review::ReviewProgress::load(&Store::open(&f.repo).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(progress.round, 2);
    assert_eq!(progress.repairs, 2);
    assert_eq!(
        std::fs::read_to_string(f.worktree().join("feature.txt")).unwrap(),
        "ready\n"
    );
}

#[tokio::test]
async fn pr_review_indexes_large_changes_without_relaying_the_entire_patch() {
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let mut input = request();
    input.units[0].acceptance = "feature payload exists".into();
    input.units[0].test_command = "test -s feature.txt".into();
    let engine = f.engine();
    engine.start_build(&input).unwrap();
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("feature.txt", &"large-payload-line\n".repeat(20_000))],
                r#"{"status":"completed","summary":"payload","conflict":null}"#,
            ),
        ),
        step(
            Role::UnitReviewer,
            Reply::say(r#"{"verdict":"pass","findings":[]}"#),
        ),
        step(Role::UnitReviewer, Reply::PrAttack),
        step(Role::FinalReview, Reply::pr_defense()),
    ]);
    engine.step_with(&script, None, None).await.unwrap();
    engine.step_with(&script, None, None).await.unwrap();
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "awaiting_adjust_or_ship"
    );
    for call in script.calls().into_iter().skip(2) {
        assert!(call.prompt.contains("feature.txt"));
        assert!(call.prompt.contains("Read the full diff"));
        assert!(!call.prompt.contains("large-payload-line"));
        assert!(call.prompt.len() < 20_000);
    }
}
