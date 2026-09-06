#![cfg(unix)]
mod common;
use common::Fixture;
use hwahap::native::{NativeHost, NativeInput};
use hwahap::state::Store;
#[path = "lifecycle_recovery/plan_fixture.rs"]
mod plan_fixture;

#[tokio::test]
async fn first_plan_call_seals_its_parent_before_a_restart_or_another_dispatch() {
    let f = Fixture::new();
    let store = Store::open(&f.repo).unwrap();
    let host = NativeHost::default();
    host.advance(
        &f.repo,
        NativeInput {
            request: Some("Plan a repository change".into()),
            plan_only: true,
            host_session_id: Some("original-parent".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while store.read_run().unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    host.shutdown().await;
    let owner = store.artifacts_path().join("native-owner.json");
    let bytes = std::fs::read(&owner).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved["pool_scope"], "original-parent");
    let replacement = NativeHost::default();
    for parent in [None, Some("different-parent".into())] {
        assert!(replacement
            .advance(
                &f.repo,
                NativeInput {
                    host_session_id: parent,
                    ..Default::default()
                }
            )
            .await
            .is_err());
        assert_eq!(std::fs::read(&owner).unwrap(), bytes);
    }
}

#[tokio::test]
async fn interrupted_worktree_is_checked_before_adoption_and_cannot_discard_dirty_files() {
    let f = Fixture::new();
    let ready = plan_fixture::plan_ready(&f).await;
    let branch = format!("hwahap/{}", ready.run_id);
    common::git(
        &f.repo,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            f.worktree().to_str().unwrap(),
            "HEAD",
        ],
    );
    let path = f.worktree().join("user-owned.txt");
    std::fs::write(&path, "preserve").unwrap();
    let engine = f.engine();
    assert!(engine
        .build_confirmed(ready.plan_digest.as_ref().unwrap())
        .is_err());
    assert_eq!(engine.status().unwrap().state, "plan_ready");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "preserve");
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        engine
            .build_confirmed(ready.plan_digest.as_ref().unwrap())
            .unwrap()
            .state,
        "coding"
    );
}

#[tokio::test]
async fn changed_source_can_be_replanned_reconfirmed_and_built_without_old_answers() {
    let f = Fixture::new();
    let ready = plan_fixture::plan_ready(&f).await;
    common::git(
        &f.repo,
        &["commit", "--allow-empty", "-m", "source advanced"],
    );
    let engine = f.engine();
    assert!(engine
        .build_confirmed(ready.plan_digest.as_ref().unwrap())
        .is_err());
    engine
        .step(None, Some("Reconsider this plan against the new source"))
        .await
        .unwrap();
    let store = Store::open(&f.repo).unwrap();
    let changed = store.read_plan().unwrap().unwrap();
    assert_eq!(
        changed.source_head.as_deref(),
        Some(f.head_sha(&f.repo).as_str())
    );
    assert!(changed.facts.is_empty());
    assert!(changed.decisions.iter().all(|d| d.answer.is_none()));
    assert!(changed
        .surfaces
        .values()
        .all(|s| matches!(s, hwahap::plan::SurfaceStatus::Applicable)));
    assert!(changed.interactive && changed.frozen.is_none());
    let revised = plan_fixture::confirm_ready(&f).await;
    assert_ne!(revised.plan_digest, ready.plan_digest);
    assert_eq!(
        engine
            .build_confirmed(revised.plan_digest.as_ref().unwrap())
            .unwrap()
            .state,
        "coding"
    );
    assert_eq!(f.head_sha(&f.worktree()), f.head_sha(&f.repo));
}

#[tokio::test]
async fn direct_build_conflict_waits_for_user_then_reopens_interactive_plan_on_owned_source() {
    use common::{step, Reply, Script};
    use hwahap::engine::{BuildRequest, BuildUnit};
    use hwahap::profile::Role;
    let f = Fixture::new();
    common::git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let engine = f.engine();
    engine
        .start_build(&BuildRequest {
            user_instruction: "Build without planning".into(),
            objective: "Create output".into(),
            base_branch: "main".into(),
            branch: "codex/conflict".into(),
            full_suite: "test -f output".into(),
            units: vec![BuildUnit {
                title: "Output".into(),
                acceptance: "Output exists".into(),
                paths: vec!["output".into()],
                test_command: "test -f output".into(),
            }],
        })
        .unwrap();
    let script = Script::new(vec![step(
        Role::Implementer,
        Reply::say(
            r#"{"status":"plan_conflict","summary":"need a decision","conflict":"Overwrite policy is unspecified"}"#,
        ),
    )]);
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "plan_conflict"
    );
    let store = Store::open(&f.repo).unwrap();
    let before = store.read_plan().unwrap().unwrap();
    engine.step(None, None).await.unwrap();
    assert_eq!(store.read_plan().unwrap().unwrap(), before);
    assert_eq!(
        engine
            .step(None, Some("Open planning to decide overwrite policy"))
            .await
            .unwrap()
            .state,
        "inspecting"
    );
    let plan = store.read_plan().unwrap().unwrap();
    assert!(plan.interactive && plan.execution_authorization.is_none() && plan.frozen.is_none());
    assert_eq!(
        plan.source_head.as_deref(),
        Some(f.head_sha(&f.worktree()).as_str())
    );
}

#[tokio::test]
async fn claimed_branch_is_rejected_unless_its_commit_still_matches_the_planned_source() {
    let f = Fixture::new();
    let ready = plan_fixture::plan_ready(&f).await;
    let branch = format!("hwahap/{}", ready.run_id);
    let source = f.head_sha(&f.repo);
    let tree = common::git(&f.repo, &["rev-parse", "HEAD^{tree}"]);
    let other = common::git(
        &f.repo,
        &["commit-tree", &tree, "-p", &source, "-m", "unrelated"],
    );
    common::git(&f.repo, &["branch", &branch, &other]);
    assert!(f
        .engine()
        .build_confirmed(ready.plan_digest.as_ref().unwrap())
        .is_err());
    assert!(!f.worktree().exists());
    assert_eq!(common::git(&f.repo, &["rev-parse", &branch]), other);
    common::git(
        &f.repo,
        &[
            "update-ref",
            &format!("refs/heads/{branch}"),
            &source,
            &other,
        ],
    );
    assert_eq!(
        f.engine()
            .build_confirmed(ready.plan_digest.as_ref().unwrap())
            .unwrap()
            .state,
        "coding"
    );
    assert_eq!(f.head_sha(&f.worktree()), source);
}
