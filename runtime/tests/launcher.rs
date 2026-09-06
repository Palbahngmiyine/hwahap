//! Launcher tests use fake executables; they do not validate the real MCP process.
#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

fn executable(path: &std::path::Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn only_the_packaged_current_release_can_start() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let launcher = root.join("bin/hwahap");
    executable(&launcher, include_str!("../../bin/hwahap"));
    let obsolete = "#!/bin/sh\necho obsolete-was-executed\n";
    executable(&root.join("runtime/target/debug/hwahap"), obsolete);
    executable(&root.join("path/hwahap"), obsolete);
    let launch = || {
        Command::new(&launcher)
            .env("HWAHAP_BIN", root.join("path/hwahap"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", root.join("path").display()),
            )
            .output()
            .unwrap()
    };
    assert!(!launch().status.success());
    let release = root.join("runtime/target/release/hwahap");
    executable(&release, "#!/bin/sh\necho 'hwahap 3.0.0'\n");
    let rejected = launch();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    executable(&release, "#!/bin/sh\nif [ \"${1:-}\" = --version ]; then echo 'hwahap 4.0.0'; else echo current; fi\n");
    let accepted = launch();
    assert!(accepted.status.success());
    assert_eq!(accepted.stdout, b"current\n");
}

#[test]
fn compiled_runtime_reports_the_launcher_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_hwahap"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        "hwahap 4.0.0"
    );
}
