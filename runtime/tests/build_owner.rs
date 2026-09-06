#![cfg(unix)]
mod common;

use common::{git, Fixture};
use hwahap::engine::{BuildRequest, BuildUnit};
use hwahap::state::Store;

fn request() -> BuildRequest {
    BuildRequest {
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

#[tokio::test]
async fn build_parent_is_sealed_before_first_poll_and_after_restart() {
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    std::os::unix::fs::symlink(&f.gh, f.dir.path().join("gh")).unwrap();
    let old_path = std::env::var_os("PATH").unwrap();
    let mut paths = vec![f.dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&old_path));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    let host = hwahap::native::NativeHost::default();
    let root = f.repo.canonicalize().unwrap();
    let started = host
        .advance(
            &root,
            hwahap::native::NativeInput {
                build: Some(request()),
                host_session_id: Some("actual-build-authorizer".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(started.outcome.state, "coding");
    let store = Store::open(&root).unwrap();
    assert!(store.artifacts_path().join("native-owner.json").exists());
    // Simulate failure after the BUILD transaction, before its derived owner file was saved.
    std::fs::remove_file(store.artifacts_path().join("native-owner.json")).unwrap();
    let result = host
        .advance(
            &root,
            hwahap::native::NativeInput {
                host_session_id: Some("unrelated-parent".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("another parent task"));
    assert!(!store.artifacts_path().join("native-owner.json").exists());
    host.advance(
        &root,
        hwahap::native::NativeInput {
            host_session_id: Some("actual-build-authorizer".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let owner = std::fs::read_to_string(store.artifacts_path().join("native-owner.json")).unwrap();
    assert!(owner.contains("actual-build-authorizer"));
    host.shutdown().await;
    std::env::set_var("PATH", old_path);
}
