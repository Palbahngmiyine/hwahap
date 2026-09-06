use super::*;
use crate::plan::Fact;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    let git = Git::open(dir.path()).unwrap();
    git.run(&["config", "user.name", "Grounding Test"]).unwrap();
    git.run(&["config", "user.email", "grounding@example.invalid"])
        .unwrap();
    std::fs::write(dir.path().join("cited file.txt"), "\nalpha\n\nomega\n\n").unwrap();
    git.run(&["add", "cited file.txt"]).unwrap();
    git.run(&["commit", "-qm", "citation fixture"]).unwrap();
    dir
}

fn plan(source: &str) -> Plan {
    let mut plan = Plan::new("grounding", "main", "verify citations");
    plan.facts.push(Fact {
        id: "F1".into(),
        question: "Where is the fixture text?".into(),
        answer: "The tracked text file contains five lines".into(),
        sources: vec![source.into()],
    });
    plan
}

#[test]
fn verifies_committed_line_ranges_without_trimming_blank_lines() {
    let dir = fixture();
    for source in ["cited file.txt:1", "cited file.txt:2-5", "cited file.txt:5"] {
        verify_sources(&plan(source), dir.path()).unwrap();
    }
    std::fs::write(dir.path().join("cited file.txt"), "1\n2\n3\n4\n5\n6\n").unwrap();
    let err = verify_sources(&plan("cited file.txt:6"), dir.path()).unwrap_err();
    assert!(
        err.to_string().contains("committed file's 5 lines"),
        "{err}"
    );
}

#[test]
fn rejects_fabricated_untracked_unsafe_and_out_of_bounds_citations() {
    let dir = fixture();
    std::fs::write(dir.path().join("untracked.txt"), "uncommitted\n").unwrap();
    for source in [
        "missing.txt:1",
        "untracked.txt:1",
        "../cited file.txt:1",
        "/etc/passwd:1",
        "./cited file.txt:1",
        "a/../cited file.txt:1",
        ".git/config:1",
        "cited file.txt:0",
        "cited file.txt:3-2",
        "cited file.txt:1-6",
        "cited file.txt:-1",
        "cited file.txt",
        "cited*.txt:1",
        "cited file.txt:18446744073709551616",
        "cited file.txt:+1",
    ] {
        assert!(
            verify_sources(&plan(source), dir.path()).is_err(),
            "accepted {source:?}"
        );
    }
}

#[test]
fn rejects_nontext_empty_and_oversized_blobs_without_following_symlinks() {
    let dir = fixture();
    let git = Git::open(dir.path()).unwrap();
    std::fs::write(dir.path().join("invalid.bin"), [0xff]).unwrap();
    std::fs::write(dir.path().join("binary-control.bin"), b"start\0end\n").unwrap();
    std::fs::write(dir.path().join("empty.txt"), "").unwrap();
    std::fs::write(
        dir.path().join("large.txt"),
        vec![b'x'; 4 * 1024 * 1024 + 1],
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("directory")).unwrap();
    std::fs::write(dir.path().join("directory/nested.txt"), "nested\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("cited file.txt", dir.path().join("link.txt")).unwrap();
    git.run(&["add", "--all"]).unwrap();
    git.run(&["commit", "-qm", "source boundary fixtures"])
        .unwrap();
    for (source, reason) in [
        ("invalid.bin:1", "not UTF-8"),
        ("binary-control.bin:1", "binary control"),
        ("empty.txt:1", "0 lines"),
        ("large.txt:1", "4 MiB"),
        ("directory:1", "regular tracked file"),
    ] {
        let err = verify_sources(&plan(source), dir.path()).unwrap_err();
        assert!(err.to_string().contains(reason), "{source}: {err}");
    }
    #[cfg(unix)]
    assert!(verify_sources(&plan("link.txt:1"), dir.path())
        .unwrap_err()
        .to_string()
        .contains("regular tracked file"));
}

#[test]
fn checks_every_citation_and_rejects_an_empty_source_array() {
    let dir = fixture();
    let mut plan = plan("cited file.txt:2");
    plan.facts[0].sources.push("fabricated.txt:1".into());
    assert!(verify_sources(&plan, dir.path()).is_err());
    plan.facts[0].sources.clear();
    assert!(verify_sources(&plan, dir.path())
        .unwrap_err()
        .to_string()
        .contains("F1 has no source locations"));
}
