//! Draft adjustment tests use real Git state and a controlled GitHub CLI.
#![cfg(unix)]
mod common;
use common::Fixture;
use hwahap::forge::Forge;

#[test]
fn adjusting_a_matching_draft_updates_it_instead_of_creating_another() {
    let fixture = Fixture::new();
    common::git(&fixture.repo, &["push", "origin", "main"]);
    let forge = Forge::with_program(fixture.gh.to_str().unwrap());
    let first = forge
        .create_draft(&fixture.repo, "main", "main", "first", "body")
        .unwrap();
    let revised = forge
        .create_draft(&fixture.repo, "main", "main", "revised", "new body")
        .unwrap();
    assert_eq!(first, revised);
    assert_eq!(revised.head_sha, fixture.head_sha(&fixture.repo));
    let control = fixture.dir.path();
    assert_eq!(
        std::fs::read_to_string(control.join("pr-created"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(control.join("pr-edited"))
            .unwrap()
            .trim(),
        first.url
    );
}

#[test]
fn ready_ambiguous_or_mismatched_prs_cannot_be_adopted_as_drafts() {
    let fixture = Fixture::new();
    common::git(&fixture.repo, &["push", "origin", "main"]);
    let forge = Forge::with_program(fixture.gh.to_str().unwrap());
    let first = forge
        .create_draft(&fixture.repo, "main", "main", "first", "body")
        .unwrap();
    forge.mark_ready(&fixture.repo, &first.url).unwrap();
    assert!(forge
        .create_draft(&fixture.repo, "main", "main", "revision", "body")
        .is_err());
    let valid = serde_json::json!({"url":first.url,"isDraft":true,"headRefName":"main","baseRefName":"main"});
    let mut wrong_head = valid.clone();
    wrong_head["headRefName"] = "another-run".into();
    let mut wrong_base = valid.clone();
    wrong_base["baseRefName"] = "release".into();
    for entries in [
        serde_json::json!([valid.clone(), valid]),
        serde_json::json!([wrong_head]),
        serde_json::json!([wrong_base]),
        serde_json::json!([{}]),
    ] {
        std::fs::write(fixture.dir.path().join("pr-list-json"), entries.to_string()).unwrap();
        assert!(forge
            .create_draft(&fixture.repo, "main", "main", "revision", "body")
            .is_err());
    }
    assert!(!fixture.dir.path().join("pr-edited").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.dir.path().join("pr-created"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
