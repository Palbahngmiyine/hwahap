#![cfg(unix)]
mod common;
use common::{git, step, Fixture, Reply, Script};
use hwahap::engine::{AdjustBuildRequest, BuildRequest, BuildUnit};
use hwahap::pr_review::ReviewProgress;
use hwahap::profile::Role;
use hwahap::state::Store;

async fn reviewed() -> Fixture {
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let engine = f.engine();
    engine
        .start_build(&BuildRequest {
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
                    test_command: format!("test -f {name}"),
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
async fn correction_preserves_contract_rebuilds_dependents_and_reviews_same_pr() {
    let f = reviewed().await;
    let engine = f.engine();
    let store = Store::open(&f.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    let previous = ReviewProgress::load(&store).unwrap().unwrap();
    let request = AdjustBuildRequest {
        user_instruction: "Correct the contents under the agreed behavior".into(),
        contract_digest: plan.digest().unwrap().to_string(),
        unit_ids: vec!["U1".into()],
    };
    assert_eq!(engine.adjust_build(&request).unwrap().state, "coding");
    assert_eq!(store.read_plan().unwrap().unwrap(), plan);
    let run = store.read_run().unwrap().unwrap();
    assert!(run.accepted_units.is_empty() && run.accepted_fingerprints.is_empty());
    assert!(run.reviewed_head.is_none());
    assert!(engine.adjust_build(&request).is_err());
    assert!(engine
        .ship(&format!("SHIP {}", plan.challenge().unwrap()))
        .is_err());
    let script = implementation("corrected");
    engine.step_with(&script, None, None).await.unwrap();
    assert!(script
        .prompts_for(Role::Implementer)
        .iter()
        .all(|p| p.contains(&request.user_instruction)));
    engine.step_with(&script, None, None).await.unwrap();
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "awaiting_adjust_or_ship"
    );
    let next = ReviewProgress::load(&store).unwrap().unwrap();
    assert_eq!(next.binding.pr_url, previous.binding.pr_url);
    assert_eq!(
        next.binding.contract_digest,
        previous.binding.contract_digest
    );
    assert_ne!(next.binding.head, previous.binding.head);
    assert!(next.round > previous.round);
    assert_eq!(next.repairs, previous.repairs);
    assert_eq!(store.read_plan().unwrap().unwrap(), plan);
}

#[tokio::test]
async fn invalid_requests_or_changed_reviewed_state_cannot_start_adjustment() {
    let f = reviewed().await;
    let engine = f.engine();
    let store = Store::open(&f.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    let original = store.read_run().unwrap().unwrap();
    let request = AdjustBuildRequest {
        user_instruction: "Correct the implementation".into(),
        contract_digest: plan.digest().unwrap().to_string(),
        unit_ids: vec!["U2".into()],
    };
    for mut invalid in [
        request.clone(),
        request.clone(),
        request.clone(),
        request.clone(),
    ]
    .into_iter()
    .enumerate()
    {
        match invalid.0 {
            0 => invalid.1.contract_digest = "stale".into(),
            1 => invalid.1.unit_ids = vec!["U99".into()],
            2 => invalid.1.unit_ids = vec!["U2".into(), "U2".into()],
            _ => invalid.1.user_instruction.clear(),
        }
        assert!(engine.adjust_build(&invalid.1).is_err());
        assert_eq!(store.read_run().unwrap().unwrap(), original);
    }
    let mut altered = serde_json::to_value(&request).unwrap();
    altered["acceptance"] = "replace the agreed contract".into();
    assert!(serde_json::from_value::<AdjustBuildRequest>(altered).is_err());
    std::fs::write(f.worktree().join("one"), "unrelated edit").unwrap();
    assert!(engine.adjust_build(&request).is_err());
    std::fs::write(f.worktree().join("one"), "initial").unwrap();
    f.move_pr_head("unreviewed-head");
    assert!(engine.adjust_build(&request).is_err());
    assert_eq!(store.read_run().unwrap().unwrap(), original);
    assert_eq!(store.read_plan().unwrap().unwrap(), plan);
    assert!(!store
        .artifacts_path()
        .join("build-adjustment.json")
        .exists());
}

#[tokio::test]
async fn adjustment_instruction_survives_implementation_retry_and_leaves_other_units_accepted() {
    let f = reviewed().await;
    let engine = f.engine();
    let store = Store::open(&f.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    let request = AdjustBuildRequest {
        user_instruction: "Correct only the second file".into(),
        contract_digest: plan.digest().unwrap().to_string(),
        unit_ids: vec!["U2".into()],
    };
    engine.adjust_build(&request).unwrap();
    assert_eq!(store.read_run().unwrap().unwrap().accepted_units, ["U1"]);
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::say(r#"{"status":"failed","summary":"try repair"}"#),
        ),
        step(
            Role::Rework,
            Reply::write(
                &[("two", "corrected")],
                r#"{"status":"completed","summary":"corrected second file"}"#,
            ),
        ),
        step(Role::UnitReviewer, Reply::say(r#"{"verdict":"pass"}"#)),
    ]);
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "final_verifying"
    );
    assert!(script.prompts_for(Role::Rework)[0].contains(&request.user_instruction));
    assert_eq!(
        std::fs::read_to_string(f.worktree().join("one")).unwrap(),
        "initial"
    );
    assert_eq!(store.read_plan().unwrap().unwrap(), plan);
}
