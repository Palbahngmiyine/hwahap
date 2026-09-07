#![cfg(unix)]
mod common;
use common::{git, security_review, step, Fixture, Reply, Script};
use hwahap::engine::{BuildRequest, BuildUnit};
use hwahap::pr_review::ReviewProgress;
use hwahap::profile::Role;
use hwahap::state::Store;

async fn draft() -> Fixture {
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let engine = f.engine();
    engine
        .start_build(&BuildRequest {
            task_profiles: Default::default(),
            verification_inputs: vec![],
            user_instruction: "Implement without planning".into(),
            objective: "Create a checked feature".into(),
            base_branch: "main".into(),
            branch: "codex/security-check".into(),
            full_suite: "test -f feature.txt".into(),
            units: vec![BuildUnit {
                title: "Feature".into(),
                acceptance: "feature exists".into(),
                paths: vec!["feature.txt".into()],
                test_command: "test -f feature.txt".into(),
            }],
        })
        .unwrap();
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("feature.txt", "ready\n")],
                r#"{"status":"completed","summary":"Created feature","conflict":null}"#,
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
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "pr_review"
    );
    f
}

#[tokio::test]
async fn either_teams_blocked_security_prevents_ship_and_explicit_recheck_gets_fresh_evidence() {
    for attack_blocked in [true, false] {
        let f = draft().await;
        let store = Store::open(&f.repo).unwrap();
        let p = ReviewProgress::load(&store).unwrap().unwrap();
        let mut blocked = security_review();
        blocked["checks"][0]["status"] = "blocked".into();
        blocked["checks"][0]["evidence"] =
            serde_json::json!(["fixture environment unavailable; repeat after recovery"]);
        let attack = serde_json::json!({"binding":p.binding,"findings":[],"evidence":["source inspected"],
            "security":if attack_blocked { blocked.clone() } else { security_review() }});
        let defense = serde_json::json!({"binding":p.binding,"assessments":[],"additional_findings":[],
            "evidence":["independently inspected"],"security":if attack_blocked { security_review() } else { blocked }});
        let script = Script::new(vec![
            step(Role::UnitReviewer, Reply::say(attack.to_string())),
            step(Role::FinalReview, Reply::say(defense.to_string())),
        ]);
        let engine = f.engine();
        let result = engine.step_with(&script, None, None).await.unwrap();
        assert_eq!(result.state, "blocked", "{}", result.message);
        assert!(result.message.contains("security"));
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
        assert!(engine.ship(&ship).is_err());
        let old = store.artifacts_path().join(p.artifact("attack").unwrap());
        let bytes = std::fs::read(&old).unwrap();
        engine.recheck_pr().unwrap();
        assert_eq!(
            engine
                .step_with(&Script::new(vec![]), None, None)
                .await
                .unwrap()
                .state,
            "pr_review"
        );
        let fresh = ReviewProgress::load(&store).unwrap().unwrap();
        assert_eq!(fresh.round, p.round + 1);
        assert_eq!(fresh.repairs, p.repairs);
        let clean = Script::new(vec![
            step(Role::UnitReviewer, Reply::PrAttack),
            step(Role::FinalReview, Reply::pr_defense()),
        ]);
        assert_eq!(
            engine.step_with(&clean, None, None).await.unwrap().state,
            "awaiting_adjust_or_ship"
        );
        assert_eq!(clean.remaining(), 0);
        assert_eq!(std::fs::read(old).unwrap(), bytes);
        // A completed run must not trust a legacy cached report without security fields.
        let path = store
            .artifacts_path()
            .join(fresh.artifact("attack").unwrap());
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record["report"].as_object_mut().unwrap().remove("security");
        std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(engine.ship(&ship).is_err());
    }
}

#[tokio::test]
async fn recheck_rejects_obsolete_report_without_rewriting_evidence() {
    let f = draft().await;
    let engine = f.engine();
    let store = Store::open(&f.repo).unwrap();
    let p = ReviewProgress::load(&store).unwrap().unwrap();
    let interrupted = Script::new(vec![
        step(Role::UnitReviewer, Reply::PrAttack),
        step(Role::FinalReview, Reply::Fail("connection lost".into())),
    ]);
    assert!(engine.step_with(&interrupted, None, None).await.is_err());
    let path = store.artifacts_path().join(p.artifact("attack").unwrap());
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    legacy["report"].as_object_mut().unwrap().remove("security");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let snapshot = std::fs::read(store.root().join("run.json")).unwrap();
    let progress = ReviewProgress::load(&store).unwrap().unwrap();
    assert!(engine.recheck_pr().is_err());
    assert_eq!(
        std::fs::read(store.root().join("run.json")).unwrap(),
        snapshot
    );
    assert_eq!(ReviewProgress::load(&store).unwrap().unwrap(), progress);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn failed_ci_enters_repair_without_spending_review_calls() {
    let f = draft().await;
    let store = Store::open(&f.repo).unwrap();
    f.fail_checks();
    let engine = f.engine();
    let empty = Script::new(vec![]);
    engine.step_with(&empty, None, None).await.unwrap();
    let progress = ReviewProgress::load(&store).unwrap().unwrap();
    assert_eq!(progress.stage, hwahap::pr_review::ReviewStage::Repair);
    assert!(!store
        .artifacts_path()
        .join(progress.artifact("attack").unwrap())
        .exists());
    let repair = Script::new(vec![step(
        Role::Rework,
        Reply::write(
            &[("feature.txt", "repaired\n")],
            r#"{"status":"completed","summary":"fixed CI","conflict":null}"#,
        ),
    )]);
    engine.step_with(&repair, None, None).await.unwrap();
    assert_eq!(repair.remaining(), 0);
    std::fs::remove_file(f.dir.path().join("checks-fail")).unwrap();
    let reviews = Script::new(vec![
        step(Role::UnitReviewer, Reply::PrAttack),
        step(Role::FinalReview, Reply::pr_defense()),
    ]);
    let done = engine.step_with(&reviews, None, None).await.unwrap();
    assert_eq!(done.state, "awaiting_adjust_or_ship");
    assert!(
        hwahap::revalidation::obligations(&store, &done.run_id, "U1")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn frozen_low_risk_contract_accepts_one_independent_clean_review() {
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let profile: hwahap::delegation::TaskProfile = serde_json::from_value(serde_json::json!({"requirements":{"capabilities":{"implementation":2},"depth":"focused"},"risk":{"failure_cost":0,"reversibility":0,"blast_radius":0},"topology":{"predecessors":[],"coupling":"independent","shared_resources":[],"writer_owner":"U1","separable":true,"write_paths":["feature.txt"]},"evidence":["one disposable file; fixed assertion"],"recovery":null})).unwrap();
    let mut run_profile = profile.clone();
    run_profile.topology.writer_owner = None;
    let mut request = BuildRequest {
        task_profiles: [("U1".into(), profile), ("run".into(), run_profile)].into(),
        verification_inputs: vec![],
        user_instruction: "Implement the approved narrow fixture".into(),
        objective: "fixture".into(),
        base_branch: "main".into(),
        branch: "codex/single-review".into(),
        full_suite: "test -f feature.txt".into(),
        units: vec![BuildUnit {
            title: "feature".into(),
            acceptance: "feature exists".into(),
            paths: vec!["feature.txt".into()],
            test_command: "test -f feature.txt".into(),
        }],
    };
    // Run writer ownership is keyed by the generated run; only unit profiles are needed by the scripted author.
    request
        .task_profiles
        .get_mut("run")
        .unwrap()
        .topology
        .write_paths = vec!["feature.txt".into()];
    let engine = f.engine();
    engine.start_build(&request).unwrap();
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("feature.txt", "ready")],
                r#"{"status":"completed","summary":"ready","conflict":null}"#,
            ),
        ),
        step(
            Role::UnitReviewer,
            Reply::say(r#"{"verdict":"pass","findings":[]}"#),
        ),
    ]);
    engine.step_with(&script, None, None).await.unwrap();
    engine.step_with(&script, None, None).await.unwrap();
    let review = Script::new(vec![step(Role::UnitReviewer, Reply::PrAttack)]);
    let done = engine.step_with(&review, None, None).await.unwrap();
    assert_eq!(done.state, "awaiting_adjust_or_ship");
    let store = Store::open(&f.repo).unwrap();
    let progress = ReviewProgress::load(&store).unwrap().unwrap();
    assert!(!store
        .artifacts_path()
        .join(progress.artifact("defense").unwrap())
        .exists());
    let challenge = store
        .read_plan()
        .unwrap()
        .unwrap()
        .digest()
        .unwrap()
        .challenge();
    engine.ship(&format!("SHIP {challenge}")).unwrap();
}
