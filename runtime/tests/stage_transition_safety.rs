#![cfg(unix)]
mod common;
use common::{git, step, Fixture, Reply, Script};
use hwahap::engine::{BuildRequest, BuildUnit};
use hwahap::profile::Role;
use hwahap::state::Store;

async fn reviewed() -> Fixture {
    let f = Fixture::new();
    std::fs::write(f.repo.join(".gitignore"), ".hwahap/\n*.cache\n").unwrap();
    git(&f.repo, &["add", ".gitignore"]);
    git(&f.repo, &["commit", "-qm", "ignore cache files"]);
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let engine = f.engine();
    engine
        .start_build(&BuildRequest {
            user_instruction: "Build without planning".into(),
            objective: "Create output".into(),
            branch: "codex/stage-safety".into(),
            base_branch: "main".into(),
            full_suite: "test -f output".into(),
            units: vec![BuildUnit {
                title: "Output".into(),
                paths: vec!["output".into()],
                acceptance: "Output exists".into(),
                test_command: "test -f output".into(),
            }],
        })
        .unwrap();
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("output", "done")],
                r#"{"status":"completed","summary":"wrote output"}"#,
            ),
        ),
        step(Role::UnitReviewer, Reply::say(r#"{"verdict":"pass"}"#)),
        step(Role::UnitReviewer, Reply::PrAttack),
        step(Role::FinalReview, Reply::pr_defense()),
    ]);
    for expected in ["final_verifying", "pr_review", "awaiting_adjust_or_ship"] {
        assert_eq!(
            engine.step_with(&script, None, None).await.unwrap().state,
            expected
        );
    }
    f
}

#[tokio::test]
async fn ship_rejects_extra_prose_other_directives_and_duplicate_approvals() {
    let f = reviewed().await;
    let store = Store::open(&f.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    let original = store.read_run().unwrap().unwrap();
    let ship = format!("SHIP {}", plan.challenge().unwrap());
    for extra in [
        "Do not ship until I approve later.".into(),
        "C1=ALT2".into(),
        format!("CONFIRM PLAN {}", plan.challenge().unwrap()),
        ship.clone(),
    ] {
        assert!(
            f.engine().ship(&format!("{ship}\n{extra}")).is_err(),
            "accepted {extra}"
        );
        assert!(!f.was_marked_ready());
        assert_eq!(store.read_run().unwrap().unwrap(), original);
        assert_eq!(store.read_plan().unwrap().unwrap(), plan);
    }
    assert_eq!(f.engine().ship(&ship).unwrap().state, "shipped");
    assert!(f.was_marked_ready());
}

#[tokio::test]
async fn next_request_preserves_tracked_untracked_and_ignored_files_before_archiving() {
    let f = reviewed().await;
    let engine = f.engine();
    let store = Store::open(&f.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    engine
        .ship(&format!("SHIP {}", plan.challenge().unwrap()))
        .unwrap();
    let original = store.read_run().unwrap().unwrap();
    let journal = std::fs::read(store.root().join("events.jsonl")).unwrap();
    for name in ["output", "user-owned.txt", "user.cache"] {
        let path = f.worktree().join(name);
        std::fs::write(&path, "preserve this independent work").unwrap();
        assert!(
            engine
                .start_planning("Next planning request", true)
                .is_err(),
            "removed {name}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "preserve this independent work"
        );
        assert_eq!(store.read_run().unwrap().unwrap(), original);
        assert_eq!(store.read_plan().unwrap().unwrap(), plan);
        assert_eq!(
            std::fs::read(store.root().join("events.jsonl")).unwrap(),
            journal
        );
        assert!(!store.root().join("archive").exists());
        if name == "output" {
            std::fs::write(path, "done").unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }
    assert_eq!(
        engine
            .start_planning("Next planning request", true)
            .unwrap()
            .state,
        "inspecting"
    );
    assert!(!f.worktree().exists());
    assert!(store.root().join("archive").exists());
}
