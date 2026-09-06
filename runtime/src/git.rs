//! Git, the only witness Hwahap trusts.
//!
//! A coding agent reports what it believes it did; the repository records what it actually did.
//! Every judgement the engine makes — did the unit change anything, did it stay inside the paths
//! the plan allows, can the checkpoint be restored — is read from git here, so that the evidence
//! behind a decision is evidence the user can reproduce by hand with the same commands.
//!
//! Two properties make that reproducible. Git is invoked as an argv array with a scrubbed
//! environment, no system config and no hooks, so a differently configured machine answers the
//! same questions the same way. And every list of paths is read from a NUL-separated form, so a
//! filename holding a space, a quote or a newline stays a filename instead of becoming a parsing
//! accident that hides a change from the scope check.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::error::{Error, Result};

/// Environment variables forwarded from Hwahap's own process.
///
/// Everything else is dropped: `GIT_DIR`, `GIT_WORK_TREE` and `GIT_INDEX_FILE` inherited from a
/// caller would silently redirect every command at a repository the user never named.
const INHERITED_ENV: [&str; 3] = ["PATH", "HOME", "XDG_CONFIG_HOME"];

/// Environment variables Hwahap sets on every invocation.
const FIXED_ENV: [(&str, &str); 3] = [
    // Reading state must never take the index lock: an observation should not be able to fail
    // because a checkpoint commit is in flight.
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    // A push that stops to ask for a password would hang a run that has no terminal.
    ("GIT_TERMINAL_PROMPT", "0"),
];

/// Arguments prepended to every invocation.
///
/// `--no-pager` because a pager on a pipe would block forever, and `core.hooksPath` is aimed at a
/// path that can never hold a hook, because a repository hook is a third party able to edit the
/// tree Hwahap is about to measure.
// Git resolves author/committer from user configuration in the actual worktree. Never invent
// an OS-derived identity when configuration is missing. Credentials remain push authentication.
const GLOBAL_ARGS: [&str; 5] = [
    "--no-pager",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "user.useConfigOnly=true",
];

/// A git repository Hwahap operates on.
#[derive(Debug, Clone)]
pub struct Git {
    root: PathBuf,
}

impl Git {
    /// Opens the repository containing `root`.
    ///
    /// Fails when `root` is not inside a git work tree; a bare repository is not one, and Hwahap
    /// has nothing to observe without files on disk.
    pub fn open(root: &Path) -> Result<Git> {
        let args = ["rev-parse", "--show-toplevel"];
        let out = raw(root, &args)?;
        if !out.status.success() {
            return Err(Error::Rejected(format!(
                "{} is not inside a git work tree: {}",
                root.display(),
                describe_failure(&out)
            )));
        }
        let top = String::from_utf8(out.stdout).map_err(|e| {
            Error::command(describe(&args), format!("output is not valid UTF-8: {e}"))
        })?;
        let top = top.trim();
        if top.is_empty() {
            return Err(Error::Rejected(format!(
                "{} is not inside a git work tree: git named no top level",
                root.display()
            )));
        }
        Ok(Git {
            root: PathBuf::from(top),
        })
    }

    /// The top level of the work tree, as git resolved it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs a git subcommand in [`Git::root`] and returns its trimmed stdout.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        self.run_in(&self.root, args)
    }

    /// Runs a git subcommand in `cwd` and returns its trimmed stdout.
    ///
    /// A non-zero exit becomes [`Error::Command`] carrying git's own stderr, because the engine
    /// relays that text to the user and a summary of it would lose the actionable part.
    pub fn run_in(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let stdout = self.stdout_of(cwd, args)?;
        let text = String::from_utf8(stdout).map_err(|e| {
            Error::command(describe(args), format!("output is not valid UTF-8: {e}"))
        })?;
        Ok(text.trim().to_string())
    }

    /// The checked-out branch, or `HEAD` when the work tree is detached.
    pub fn current_branch(&self) -> Result<String> {
        self.run(&["rev-parse", "--abbrev-ref", "HEAD"])
    }

    /// The commit `HEAD` points at.
    pub fn head_sha(&self) -> Result<String> {
        self.run(&["rev-parse", "HEAD"])
    }

    /// Whether a local branch of that name exists.
    pub fn branch_exists(&self, name: &str) -> Result<bool> {
        require_plain_value("branch name", name)?;
        // The full ref path, so a branch called `HEAD` or `main` cannot resolve to a tag or a sha
        // that merely shares its name.
        let full_ref = format!("refs/heads/{name}");
        let args = ["show-ref", "--verify", "--quiet", &full_ref];
        let out = raw(&self.root, &args)?;
        match out.status.code() {
            Some(0) => Ok(true),
            // `show-ref` reserves exit 1 for "no such ref"; anything else is a broken repository
            // rather than an answer, and must not be reported as "the branch is absent".
            Some(1) => Ok(false),
            _ => Err(Error::command(describe(&args), describe_failure(&out))),
        }
    }

    /// Whether the work tree at `cwd` matches `HEAD`, untracked files included.
    pub fn is_clean(&self, cwd: &Path) -> Result<bool> {
        Ok(self.changed_paths(cwd)?.is_empty())
    }

    /// Creates a work tree at `path` on a new branch `branch`, starting from `base`.
    ///
    /// Refuses a path that already exists: Hwahap removes the work trees it makes, and adopting a
    /// directory someone else created would put that person's files inside a run's blast radius.
    pub fn add_worktree(&self, path: &Path, branch: &str, base: &str) -> Result<()> {
        let path_text = require_plain_path("worktree path", path)?;
        require_plain_value("branch name", branch)?;
        require_plain_value("base revision", base)?;
        if path.symlink_metadata().is_ok() {
            return Err(Error::Rejected(format!(
                "{} already exists; Hwahap will not adopt a directory it did not create",
                path.display()
            )));
        }
        self.run(&["worktree", "add", "-b", branch, path_text, base])?;
        Ok(())
    }

    /// Removes the work tree at `path`, discarding whatever is in it.
    ///
    /// Succeeds when the work tree is already gone, so cleanup after a crashed run is the same call
    /// as cleanup after a finished one.
    pub fn remove_worktree(&self, path: &Path) -> Result<()> {
        let path_text = require_plain_path("worktree path", path)?;
        let args = ["worktree", "remove", "--force", path_text];
        let out = raw(&self.root, &args)?;
        if out.status.success() {
            return Ok(());
        }
        if path.symlink_metadata().is_err() {
            // The directory is already gone, so only its registration can be left; pruning is what
            // clears that, and without it the next run cannot reuse the path or the branch.
            self.run(&["worktree", "prune"])?;
            return Ok(());
        }
        Err(Error::command(describe(&args), describe_failure(&out)))
    }

    /// Repository-relative paths that differ from `HEAD` in the work tree at `cwd`.
    ///
    /// Untracked files count: a unit that writes a new file outside its declared paths has still
    /// gone out of scope. Renames contribute both names, since both changed.
    pub fn changed_paths(&self, cwd: &Path) -> Result<Vec<String>> {
        // `--porcelain=v1 -z` is the only status form that does not quote or escape a path, so it
        // is the only one in which a filename with a space, a quote or a newline survives intact.
        let args = ["status", "--porcelain=v1", "-z", "--untracked-files=all"];
        parse_status_z(&self.stdout_of(cwd, &args)?)
    }

    /// Observe HEAD, the index, tracked diff and untracked content without staging user files.
    /// Exclude the root `.hwahap` runtime directory, which the host writes during a session.
    /// This checks repository changes, not the integrity of Hwahap's own state.
    /// This is a repository postcondition, not a filesystem sandbox or an external-effect audit.
    pub fn fingerprint(&self, cwd: &Path) -> Result<crate::canonical::Digest> {
        const WITHOUT_RUNTIME: &str = ":(top,exclude,literal).hwahap";
        let head = self.run_in(cwd, &["rev-parse", "HEAD"])?;
        let branch = self.run_in(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let index = self.stdout_of(cwd, &["ls-files", "--stage", "-z", "--", WITHOUT_RUNTIME])?;
        let diff = self.stdout_of(
            cwd,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "HEAD",
                "--",
                WITHOUT_RUNTIME,
            ],
        )?;
        let paths = parse_nul_paths(&self.stdout_of(
            cwd,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                WITHOUT_RUNTIME,
            ],
        )?)?;
        let mut untracked = Vec::new();
        for path in paths {
            let absolute = cwd.join(&path);
            let metadata =
                std::fs::symlink_metadata(&absolute).map_err(|e| Error::io(&absolute, e))?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode()
            };
            #[cfg(not(unix))]
            let mode = u32::from(metadata.permissions().readonly());
            let bytes = if metadata.file_type().is_symlink() {
                std::fs::read_link(&absolute)
                    .map_err(|e| Error::io(&absolute, e))?
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec()
            } else {
                std::fs::read(&absolute).map_err(|e| Error::io(&absolute, e))?
            };
            untracked.push((
                path,
                metadata.file_type().is_symlink(),
                mode,
                crate::canonical::Digest::of_bytes(&bytes),
            ));
        }
        crate::canonical::Digest::of(&(
            head,
            branch,
            hex::encode(index),
            hex::encode(diff),
            untracked,
        ))
    }

    /// Repository-relative paths that differ between two commits.
    pub fn changed_paths_between(&self, cwd: &Path, from: &str, to: &str) -> Result<Vec<String>> {
        require_plain_value("from revision", from)?;
        require_plain_value("to revision", to)?;
        let stdout = self.stdout_of(cwd, &["diff", "--name-only", "-z", from, to])?;
        parse_nul_paths(&stdout)
    }

    /// Returns the work tree at `cwd` to exactly `sha`.
    ///
    /// The `clean` is not optional: an abandoned attempt that leaves a file behind would be counted
    /// as part of the next attempt's changes and could push it out of scope.
    pub fn reset_hard(&self, cwd: &Path, sha: &str) -> Result<()> {
        require_plain_value("revision", sha)?;
        self.run_in(cwd, &["reset", "--hard", sha])?;
        self.run_in(cwd, &["clean", "-f", "-d", "-x"])?;
        Ok(())
    }

    /// Stages everything in the work tree at `cwd` and commits it, returning the new sha.
    ///
    /// An empty commit is an error rather than a no-op: a checkpoint that changed nothing means the
    /// unit did not do its work, and recording it would make an unfinished unit look accepted.
    pub fn commit_all(&self, cwd: &Path, message: &str) -> Result<String> {
        if message.trim().is_empty() {
            return Err(Error::Rejected(
                "a commit message must not be blank".to_string(),
            ));
        }
        self.run_in(cwd, &["add", "-A"])?;
        if self.is_clean(cwd)? {
            return Err(Error::Rejected(format!(
                "nothing to commit in {}: the work tree already matches HEAD",
                cwd.display()
            )));
        }
        self.run_in(cwd, &["commit", "-m", message])?;
        self.run_in(cwd, &["rev-parse", "HEAD"])
    }

    /// Publishes `branch` to `remote` and records it as the branch's upstream.
    pub fn push(&self, cwd: &Path, remote: &str, branch: &str) -> Result<()> {
        require_plain_value("remote", remote)?;
        require_plain_value("branch name", branch)?;
        self.run_in(cwd, &["push", "--set-upstream", remote, branch])?;
        Ok(())
    }

    /// Raw stdout of a successful invocation, kept as bytes for the NUL-separated forms.
    pub(crate) fn stdout_of(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let out = raw(cwd, args)?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        Err(Error::command(describe(args), describe_failure(&out)))
    }
}

/// The changed paths that no allowed prefix covers, sorted and deduplicated.
///
/// A unit declares the paths it may touch; anything else is out of scope and the attempt is reset.
/// Matching is by path segment, so the prefix `src/a` covers `src/a` and `src/a/b.rs` but NOT
/// `src/ab.rs`. Prefixes are normalized: a leading `./` and a trailing `/` are ignored.
///
/// A prefix that normalizes to nothing covers nothing, rather than covering the whole repository:
/// an allowlist entry that says nothing must not be the one that permits everything.
pub fn paths_outside(allowed: &[String], changed: &[String]) -> Vec<String> {
    let prefixes: Vec<&str> = allowed
        .iter()
        .map(|prefix| normalize_prefix(prefix.as_str()))
        .filter(|prefix| !prefix.is_empty())
        .collect();
    changed
        .iter()
        .filter(|path| !prefixes.iter().any(|prefix| covers(prefix, path.as_str())))
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

fn normalize_prefix(prefix: &str) -> &str {
    prefix
        .strip_prefix("./")
        .unwrap_or(prefix)
        .trim_end_matches('/')
}

/// Whether `prefix` covers `path` on a segment boundary.
fn covers(prefix: &str, path: &str) -> bool {
    // `starts_with` alone would let `src/a` swallow `src/ab.rs`, which is a different file.
    path == prefix || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
}

/// The child's complete environment, built from the parent's by allowlist.
///
/// `lookup` is the parent environment, injected so that the allowlist is a property this module can
/// state and test rather than a side effect of a process's ambient state.
fn child_env(lookup: impl Fn(&str) -> Option<OsString>) -> Vec<(&'static str, OsString)> {
    let mut env = Vec::with_capacity(INHERITED_ENV.len() + FIXED_ENV.len());
    for key in INHERITED_ENV {
        if let Some(value) = lookup(key) {
            env.push((key, value));
        }
    }
    for (key, value) in FIXED_ENV {
        env.push((key, OsString::from(value)));
    }
    env
}

/// Runs git with a scrubbed environment and returns the raw outcome, exit status included.
fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd).env_clear();
    command.envs(child_env(|key| std::env::var_os(key)));
    command.args(GLOBAL_ARGS).args(args);
    command
}

fn raw(cwd: &Path, args: &[&str]) -> Result<Output> {
    git_command(cwd, args).output().map_err(|e| {
        Error::command(
            describe(args),
            format!("could not run git in {}: {e}", cwd.display()),
        )
    })
}

/// The invocation as the user would retype it, without Hwahap's own hardening flags.
fn describe(args: &[&str]) -> String {
    let mut text = String::from("git");
    for arg in args {
        text.push(' ');
        text.push_str(arg);
    }
    text
}

fn describe_failure(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        out.status.to_string()
    } else {
        format!("{}: {stderr}", out.status)
    }
}

/// Rejects an argument git could read as an option, or that carries no value at all.
///
/// Validation happens before the invocation because git has no way to say "this is a name, not a
/// flag" for most positional arguments: a branch called `--upload-pack=evil` would be obeyed.
fn require_plain_value(kind: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::Rejected(format!("{kind} must not be blank")));
    }
    if value.starts_with('-') {
        return Err(Error::Rejected(format!(
            "{kind} {value:?} starts with '-'; git would read it as an option"
        )));
    }
    Ok(())
}

fn require_plain_path<'a>(kind: &str, path: &'a Path) -> Result<&'a str> {
    let text = path
        .to_str()
        .ok_or_else(|| Error::Rejected(format!("{kind} {} is not valid UTF-8", path.display())))?;
    require_plain_value(kind, text)?;
    Ok(text)
}

/// Splits NUL-terminated git output into its fields.
///
/// Requires the trailing NUL: output truncated mid-path would otherwise be read as a complete
/// shorter path, and a scope check that silently shortens a filename is worse than one that fails.
fn split_nul(stdout: &[u8]) -> Result<Vec<&[u8]>> {
    if stdout.is_empty() {
        return Ok(Vec::new());
    }
    let Some(body) = stdout.strip_suffix(b"\0") else {
        return Err(Error::command(
            "git",
            "NUL-separated output did not end with a NUL".to_string(),
        ));
    };
    Ok(body.split(|byte| *byte == 0).collect())
}

fn decode(field: &[u8]) -> Result<String> {
    std::str::from_utf8(field).map(str::to_string).map_err(|e| {
        Error::command(
            "git",
            format!("a path in git's output is not valid UTF-8: {e}"),
        )
    })
}

/// Parses `git status --porcelain=v1 -z`.
///
/// Each record is two status letters, a space, then the unescaped path. A rename or copy is
/// followed by a second field naming the source, which is a path this work tree changed too.
fn parse_status_z(stdout: &[u8]) -> Result<Vec<String>> {
    let fields = split_nul(stdout)?;
    let mut fields = fields.into_iter();
    let mut paths = BTreeSet::new();
    let moved = |code: u8| code == b'R' || code == b'C';
    while let Some(record) = fields.next() {
        if record.len() < 4 || record[2] != b' ' {
            return Err(Error::command(
                "git status --porcelain=v1 -z",
                format!(
                    "unparsable status record {:?}",
                    String::from_utf8_lossy(record)
                ),
            ));
        }
        paths.insert(decode(&record[3..])?);
        if moved(record[0]) || moved(record[1]) {
            let source = fields.next().ok_or_else(|| {
                Error::command(
                    "git status --porcelain=v1 -z",
                    format!(
                        "status record {:?} promises a rename source that is missing",
                        String::from_utf8_lossy(record)
                    ),
                )
            })?;
            paths.insert(decode(source)?);
        }
    }
    Ok(paths.into_iter().collect())
}

/// Parses a plain NUL-separated list of paths, as `git diff --name-only -z` emits.
fn parse_nul_paths(stdout: &[u8]) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    for field in split_nul(stdout)? {
        if field.is_empty() {
            return Err(Error::command(
                "git diff --name-only -z",
                "git named an empty path".to_string(),
            ));
        }
        paths.insert(decode(field)?);
    }
    Ok(paths.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Runs a setup command that is expected to succeed, outside the type under test.
    fn setup(cwd: &Path, args: &[&str]) -> String {
        let out = raw(cwd, args).expect("git must be runnable");
        assert!(
            out.status.success(),
            "setup `{}` failed: {}",
            describe(args),
            describe_failure(&out)
        );
        String::from_utf8(out.stdout)
            .expect("setup output is utf-8")
            .trim()
            .to_string()
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory");
        }
        fs::write(path, contents).expect("write");
    }

    /// A repository with one commit on `main`, and its `Git` handle.
    fn repo() -> (TempDir, Git) {
        let dir = TempDir::new().expect("temp dir");
        setup(dir.path(), &["init", "-b", "main"]);
        setup(dir.path(), &["config", "user.name", "Repository User"]);
        setup(
            dir.path(),
            &["config", "user.email", "repo@example.invalid"],
        );
        write(&dir.path().join("README.md"), "hello\n");
        setup(dir.path(), &["add", "-A"]);
        setup(dir.path(), &["commit", "-m", "initial"]);
        let git = Git::open(dir.path()).expect("the temp dir is a work tree");
        (dir, git)
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn rejection(err: Error) -> String {
        match err {
            Error::Rejected(message) => message,
            other => panic!("expected Error::Rejected, got {other:?}"),
        }
    }

    fn command_failure(err: Error) -> (String, String) {
        match err {
            Error::Command { command, detail } => (command, detail),
            other => panic!("expected Error::Command, got {other:?}"),
        }
    }

    // ---- the child environment ------------------------------------------------------------------

    #[test]
    fn the_child_environment_is_an_allowlist_that_drops_inherited_git_variables() {
        let env = child_env(|key| match key {
            "PATH" => Some(OsString::from("/usr/bin")),
            "HOME" => Some(OsString::from("/home/somebody")),
            // A GIT_DIR arriving from a caller would aim every command at another repository.
            "GIT_DIR" => Some(OsString::from("/elsewhere/.git")),
            "GIT_COMMITTER_NAME" => Some(OsString::from("Somebody Else")),
            "LD_PRELOAD" => Some(OsString::from("/tmp/evil.so")),
            _ => None,
        });
        let keys: Vec<&str> = env.iter().map(|(key, _)| *key).collect();
        assert_eq!(
            keys,
            vec![
                "PATH",
                "HOME",
                "GIT_OPTIONAL_LOCKS",
                "GIT_CONFIG_NOSYSTEM",
                "GIT_TERMINAL_PROMPT",
            ]
        );
        let value = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string_lossy().to_string())
        };
        assert_eq!(value("PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(value("HOME").as_deref(), Some("/home/somebody"));
        assert!(value("GIT_COMMITTER_NAME").is_none());
        assert_eq!(value("GIT_OPTIONAL_LOCKS").as_deref(), Some("0"));
        assert_eq!(value("GIT_CONFIG_NOSYSTEM").as_deref(), Some("1"));
        assert_eq!(value("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
    }

    #[test]
    fn an_absent_inherited_variable_is_not_invented() {
        let env = child_env(|_| None);
        let keys: Vec<&str> = env.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys.len(), FIXED_ENV.len());
        assert!(!keys.contains(&"PATH"), "{keys:?}");
        assert!(!keys.contains(&"HOME"), "{keys:?}");
    }

    #[test]
    fn the_xdg_config_location_is_preserved_without_inheriting_identity_overrides() {
        let env = child_env(|key| Some(OsString::from(key)));
        assert!(env
            .iter()
            .any(|(key, value)| *key == "XDG_CONFIG_HOME" && value == "XDG_CONFIG_HOME"));
        for key in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "EMAIL",
        ] {
            assert!(!env.iter().any(|(name, _)| *name == key));
        }
    }

    // ---- paths_outside, as a pure function ------------------------------------------------------

    #[test]
    fn a_prefix_covers_the_directory_and_everything_under_it() {
        let outside = paths_outside(
            &strings(&["src/"]),
            &strings(&["src/a.rs", "src/deep/b.rs", "srcx/a.rs", "other.rs"]),
        );
        assert_eq!(outside, strings(&["other.rs", "srcx/a.rs"]));
    }

    #[test]
    fn a_prefix_matches_whole_segments_and_not_string_prefixes() {
        let outside = paths_outside(
            &strings(&["src/a"]),
            &strings(&["src/a", "src/a/b.rs", "src/ab.rs", "src/a.rs"]),
        );
        assert_eq!(outside, strings(&["src/a.rs", "src/ab.rs"]));
    }

    #[test]
    fn a_leading_dot_slash_and_a_trailing_slash_do_not_change_a_prefix() {
        let changed = strings(&["src/a.rs", "src", "srcx/a.rs"]);
        let expected = strings(&["srcx/a.rs"]);
        for spelling in ["src", "./src", "src/", "./src/", "src///"] {
            assert_eq!(
                paths_outside(&strings(&[spelling]), &changed),
                expected,
                "prefix {spelling:?} should behave like `src`"
            );
        }
    }

    #[test]
    fn an_empty_allowlist_puts_every_changed_path_outside() {
        let changed = strings(&["src/a.rs", "README.md"]);
        assert_eq!(
            paths_outside(&[], &changed),
            strings(&["README.md", "src/a.rs"])
        );
    }

    #[test]
    fn an_empty_changed_list_is_outside_nothing() {
        for allowed in [strings(&[]), strings(&["src"]), strings(&[""])] {
            assert!(
                paths_outside(&allowed, &[]).is_empty(),
                "allowed {allowed:?}"
            );
        }
    }

    #[test]
    fn duplicates_collapse_and_the_result_is_sorted() {
        let outside = paths_outside(
            &strings(&["src", "src", "./src/"]),
            &strings(&["b.rs", "a.rs", "b.rs", "src/keep.rs", "a.rs"]),
        );
        assert_eq!(outside, strings(&["a.rs", "b.rs"]));
    }

    #[test]
    fn a_prefix_that_normalizes_to_nothing_covers_nothing() {
        let changed = strings(&["src/a.rs"]);
        for empty in ["", "/", "./", "   ", "///"] {
            assert_eq!(
                paths_outside(&strings(&[empty]), &changed),
                changed,
                "prefix {empty:?} must not act as a wildcard"
            );
        }
    }

    #[test]
    fn a_bare_dot_prefix_is_not_a_wildcard_either() {
        assert_eq!(
            paths_outside(&strings(&["."]), &strings(&["src/a.rs"])),
            strings(&["src/a.rs"])
        );
    }

    #[test]
    fn a_file_prefix_covers_that_file_alone() {
        let outside = paths_outside(
            &strings(&["src/a.rs"]),
            &strings(&["src/a.rs", "src/a.rs.bak", "src/a.rsx"]),
        );
        assert_eq!(outside, strings(&["src/a.rs.bak", "src/a.rsx"]));
    }

    #[test]
    fn unicode_and_spaces_in_paths_are_compared_by_segment() {
        let outside = paths_outside(
            &strings(&["문서/한글", "my dir"]),
            &strings(&[
                "문서/한글/a.rs",
                "문서/한글.rs",
                "my dir/x.rs",
                "my dirx.rs",
                "café ☕/b.rs",
            ]),
        );
        assert_eq!(
            outside,
            strings(&["café ☕/b.rs", "my dirx.rs", "문서/한글.rs"])
        );
    }

    #[test]
    fn an_unnormalized_changed_path_is_reported_outside_rather_than_guessed_at() {
        assert_eq!(
            paths_outside(&strings(&["src"]), &strings(&["./src/a.rs"])),
            strings(&["./src/a.rs"])
        );
    }

    #[test]
    fn a_changed_path_with_an_embedded_newline_is_matched_literally() {
        let outside = paths_outside(
            &strings(&["src"]),
            &strings(&["src/a\nb.rs", "top\nlevel.rs"]),
        );
        assert_eq!(outside, strings(&["top\nlevel.rs"]));
    }

    // ---- porcelain parsing ----------------------------------------------------------------------

    #[test]
    fn empty_status_output_means_nothing_changed() {
        assert_eq!(parse_status_z(b"").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_rename_reports_both_the_destination_and_the_source() {
        let paths = parse_status_z(b"R  new.txt\0old.txt\0").unwrap();
        assert_eq!(paths, strings(&["new.txt", "old.txt"]));
    }

    #[test]
    fn a_copy_reports_both_paths_too() {
        let paths = parse_status_z(b"C  copy.txt\0origin.txt\0").unwrap();
        assert_eq!(paths, strings(&["copy.txt", "origin.txt"]));
        let staged_then_renamed = parse_status_z(b" R dest.txt\0src.txt\0").unwrap();
        assert_eq!(staged_then_renamed, strings(&["dest.txt", "src.txt"]));
    }

    #[test]
    fn status_paths_holding_spaces_quotes_and_newlines_survive_the_nul_form() {
        let paths = parse_status_z(b"?? a b.txt\0?? q\"uote.txt\0 M line\nbreak.txt\0").unwrap();
        assert_eq!(
            paths,
            strings(&["a b.txt", "line\nbreak.txt", "q\"uote.txt"])
        );
    }

    #[test]
    fn status_paths_are_sorted_and_deduplicated() {
        let paths = parse_status_z(b"?? b.txt\0?? a.txt\0 M b.txt\0").unwrap();
        assert_eq!(paths, strings(&["a.txt", "b.txt"]));
    }

    #[test]
    fn a_status_record_shorter_than_a_status_and_a_path_is_rejected() {
        for truncated in [&b"?? \0"[..], &b"??\0"[..], &b"?\0"[..], &b"\0"[..]] {
            let err = parse_status_z(truncated).unwrap_err();
            let (command, detail) = command_failure(err);
            assert_eq!(command, "git status --porcelain=v1 -z");
            assert!(detail.contains("unparsable status record"), "{detail}");
        }
    }

    #[test]
    fn a_status_record_without_the_separating_space_is_rejected() {
        let err = parse_status_z(b"??x.txt\0").unwrap_err();
        let (_, detail) = command_failure(err);
        assert!(
            detail.contains("unparsable status record \"??x.txt\""),
            "{detail}"
        );
    }

    #[test]
    fn a_rename_record_missing_its_source_is_rejected() {
        let err = parse_status_z(b"R  new.txt\0").unwrap_err();
        let (_, detail) = command_failure(err);
        assert!(
            detail.contains("promises a rename source that is missing"),
            "{detail}"
        );
    }

    #[test]
    fn status_output_that_is_not_nul_terminated_is_rejected() {
        let err = parse_status_z(b"?? truncated.tx").unwrap_err();
        let (command, detail) = command_failure(err);
        assert_eq!(command, "git");
        assert_eq!(detail, "NUL-separated output did not end with a NUL");
    }

    #[test]
    fn a_status_path_that_is_not_utf8_is_rejected_rather_than_lossily_decoded() {
        let err = parse_status_z(b"?? bad\xffname\0").unwrap_err();
        let (_, detail) = command_failure(err);
        assert!(detail.contains("not valid UTF-8"), "{detail}");
    }

    #[test]
    fn a_diff_list_is_sorted_deduplicated_and_nul_checked() {
        assert_eq!(parse_nul_paths(b"").unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_nul_paths(b"b.txt\0a.txt\0b.txt\0").unwrap(),
            strings(&["a.txt", "b.txt"])
        );
        let (_, detail) = command_failure(parse_nul_paths(b"a.txt").unwrap_err());
        assert_eq!(detail, "NUL-separated output did not end with a NUL");
    }

    #[test]
    fn any_set_of_paths_survives_a_round_trip_through_the_nul_form() {
        let names = [
            "z.txt",
            "a b.txt",
            "q\"uote",
            "line\nbreak",
            "문서 ☕",
            "deep/dir/f.rs",
        ];
        let mut encoded = Vec::new();
        for name in names {
            encoded.extend_from_slice(b"?? ");
            encoded.extend_from_slice(name.as_bytes());
            encoded.push(0);
        }
        let mut expected: Vec<String> = names.iter().map(|name| name.to_string()).collect();
        expected.sort();
        assert_eq!(parse_status_z(&encoded).unwrap(), expected);
    }

    #[test]
    fn a_diff_list_naming_an_empty_path_is_rejected() {
        let err = parse_nul_paths(b"a.txt\0\0").unwrap_err();
        let (command, detail) = command_failure(err);
        assert_eq!(command, "git diff --name-only -z");
        assert_eq!(detail, "git named an empty path");
    }

    // ---- opening a repository -------------------------------------------------------------------

    #[test]
    fn open_fails_outside_a_work_tree() {
        let dir = TempDir::new().unwrap();
        let err = Git::open(dir.path()).unwrap_err();
        let message = rejection(err);
        assert!(
            message.contains("is not inside a git work tree"),
            "{message}"
        );
    }

    #[test]
    fn open_rejects_a_bare_repository_because_there_is_nothing_to_observe() {
        let dir = TempDir::new().unwrap();
        setup(dir.path(), &["init", "--bare", "-b", "main"]);
        let message = rejection(Git::open(dir.path()).unwrap_err());
        assert!(
            message.contains("is not inside a git work tree"),
            "{message}"
        );
    }

    /// Compares two paths by identity rather than by spelling.
    ///
    /// Git reports `C:/Users/...` while Rust canonicalizes to the verbatim `\\?\C:\Users\...`, so
    /// a string comparison of the two fails on Windows for paths that are the same directory.
    fn same_path(left: impl AsRef<Path>, right: impl AsRef<Path>) {
        let left = left.as_ref();
        let right = right.as_ref();
        assert_eq!(
            left.canonicalize().expect("left path exists"),
            right.canonicalize().expect("right path exists"),
            "{} and {} are not the same directory",
            left.display(),
            right.display()
        );
    }

    #[test]
    fn open_from_a_subdirectory_finds_the_top_level() {
        let (dir, _git) = repo();
        let nested = dir.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        let git = Git::open(&nested).unwrap();
        same_path(git.root(), dir.path());
    }

    #[test]
    fn open_fails_when_the_directory_does_not_exist() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("no-such-directory");
        let (command, detail) = command_failure(Git::open(&missing).unwrap_err());
        assert_eq!(command, "git rev-parse --show-toplevel");
        assert!(detail.contains("could not run git in"), "{detail}");
    }

    // ---- reading the current state --------------------------------------------------------------

    #[test]
    fn current_branch_and_head_sha_report_the_checked_out_commit() {
        let (dir, git) = repo();
        assert_eq!(git.current_branch().unwrap(), "main");
        let sha = git.head_sha().unwrap();
        assert_eq!(sha.len(), 40, "{sha}");
        assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()), "{sha}");
        assert_eq!(sha, setup(dir.path(), &["rev-parse", "main"]));
    }

    #[test]
    fn a_detached_head_reports_its_branch_as_head() {
        let (dir, git) = repo();
        setup(dir.path(), &["checkout", "--detach"]);
        assert_eq!(git.current_branch().unwrap(), "HEAD");
    }

    #[test]
    fn branch_exists_separates_a_real_branch_from_names_that_only_look_like_one() {
        let (dir, git) = repo();
        setup(dir.path(), &["branch", "feature/x"]);
        assert!(git.branch_exists("main").unwrap());
        assert!(git.branch_exists("feature/x").unwrap());
        assert!(!git.branch_exists("nope").unwrap());
        assert!(
            !git.branch_exists("mai").unwrap(),
            "a prefix of a branch is not that branch"
        );
        assert!(
            !git.branch_exists("refs/heads/main").unwrap(),
            "the argument is a branch name, never a fully qualified ref"
        );
        assert!(!git.branch_exists("HEAD").unwrap(), "HEAD is not a branch");
        assert!(!git.branch_exists("bad..name").unwrap());
        assert!(!git.branch_exists("with space").unwrap());
        assert!(!git.branch_exists("한글").unwrap());
    }

    #[test]
    fn branch_exists_reports_a_broken_repository_instead_of_answering_no() {
        let (dir, git) = repo();
        fs::remove_dir_all(dir.path().join(".git")).unwrap();
        let (command, detail) = command_failure(git.branch_exists("main").unwrap_err());
        assert_eq!(command, "git show-ref --verify --quiet refs/heads/main");
        assert!(detail.contains("not a git repository"), "{detail}");
    }

    #[test]
    fn a_branch_name_that_looks_like_an_option_is_rejected_before_git_runs() {
        let (_dir, git) = repo();
        let message = rejection(git.branch_exists("--upload-pack=evil").unwrap_err());
        assert_eq!(
            message,
            "branch name \"--upload-pack=evil\" starts with '-'; git would read it as an option"
        );
    }

    #[test]
    fn a_blank_branch_name_is_rejected_before_git_runs() {
        let (_dir, git) = repo();
        for blank in ["", "   ", "\t\n"] {
            let message = rejection(git.branch_exists(blank).unwrap_err());
            assert_eq!(message, "branch name must not be blank");
        }
    }

    #[test]
    fn an_argument_holding_a_nul_byte_cannot_reach_git() {
        let (_dir, git) = repo();
        let (command, detail) = command_failure(git.branch_exists("a\0b").unwrap_err());
        assert_eq!(command, "git show-ref --verify --quiet refs/heads/a\0b");
        assert!(detail.contains("could not run git in"), "{detail}");
    }

    // ---- changed_paths --------------------------------------------------------------------------

    #[test]
    fn a_clean_tree_reports_no_changed_paths_and_is_clean() {
        let (_dir, git) = repo();
        assert_eq!(git.changed_paths(git.root()).unwrap(), Vec::<String>::new());
        assert!(git.is_clean(git.root()).unwrap());
    }

    #[test]
    fn changed_paths_reports_modifications_creations_deletions_and_new_directories() {
        let (dir, git) = repo();
        write(&dir.path().join("kept.txt"), "one\n");
        write(&dir.path().join("gone.txt"), "two\n");
        setup(dir.path(), &["add", "-A"]);
        setup(dir.path(), &["commit", "-m", "more files"]);

        write(&dir.path().join("kept.txt"), "one changed\n");
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        write(&dir.path().join("fresh.txt"), "new\n");
        write(&dir.path().join("new/dir/deep.txt"), "deep\n");

        assert_eq!(
            git.changed_paths(git.root()).unwrap(),
            strings(&["fresh.txt", "gone.txt", "kept.txt", "new/dir/deep.txt"])
        );
        assert!(!git.is_clean(git.root()).unwrap());
    }

    #[test]
    fn changed_paths_survives_spaces_and_unicode_in_a_filename() {
        // The portable half of the name torture test, so no platform loses this coverage.
        let (dir, git) = repo();
        for name in ["a file with spaces.txt", "문서 ☕.txt"] {
            write(&dir.path().join(name), "x\n");
        }
        assert_eq!(
            git.changed_paths(git.root()).unwrap(),
            strings(&["a file with spaces.txt", "문서 ☕.txt"])
        );
    }

    #[test]
    // Unix only: Windows rejects `"` and a newline in a filename outright, so there is no way to
    // create the very names this test exists to prove are parsed correctly.
    #[cfg(unix)]
    fn changed_paths_survives_spaces_quotes_newlines_and_unicode_in_a_filename() {
        let (dir, git) = repo();
        for name in [
            "a file with spaces.txt",
            "quote\".txt",
            "line\nbreak.txt",
            "문서 ☕.txt",
        ] {
            write(&dir.path().join(name), "x\n");
        }
        assert_eq!(
            git.changed_paths(git.root()).unwrap(),
            strings(&[
                "a file with spaces.txt",
                "line\nbreak.txt",
                "quote\".txt",
                "문서 ☕.txt",
            ])
        );
    }

    #[test]
    fn changed_paths_reports_a_renamed_file_under_both_of_its_names() {
        let (dir, git) = repo();
        write(
            &dir.path().join("before.txt"),
            "content that stays the same\n",
        );
        setup(dir.path(), &["add", "-A"]);
        setup(dir.path(), &["commit", "-m", "add before"]);
        setup(dir.path(), &["mv", "before.txt", "after.txt"]);
        assert_eq!(
            git.changed_paths(git.root()).unwrap(),
            strings(&["after.txt", "before.txt"])
        );
    }

    #[test]
    fn changed_paths_are_relative_to_the_repository_root_not_the_working_directory() {
        let (dir, git) = repo();
        write(&dir.path().join("sub/inner.txt"), "x\n");
        let sub = dir.path().join("sub");
        assert_eq!(
            git.changed_paths(&sub).unwrap(),
            strings(&["sub/inner.txt"])
        );
    }

    #[test]
    fn changed_paths_fails_outside_a_repository_rather_than_reporting_nothing() {
        let (_dir, git) = repo();
        let elsewhere = TempDir::new().unwrap();
        let (command, detail) = command_failure(git.changed_paths(elsewhere.path()).unwrap_err());
        assert_eq!(
            command,
            "git status --porcelain=v1 -z --untracked-files=all"
        );
        assert!(detail.contains("not a git repository"), "{detail}");
    }

    // ---- worktrees ------------------------------------------------------------------------------

    #[test]
    fn add_worktree_creates_a_new_branch_checked_out_at_the_base() {
        let (_dir, git) = repo();
        let base_sha = git.head_sha().unwrap();
        let holder = TempDir::new().unwrap();
        let path = holder.path().join("run");

        git.add_worktree(&path, "hwahap/goal", &base_sha).unwrap();

        assert!(path.join("README.md").is_file());
        assert!(git.branch_exists("hwahap/goal").unwrap());
        assert_eq!(git.run_in(&path, &["rev-parse", "HEAD"]).unwrap(), base_sha);
        assert_eq!(
            git.run_in(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap(),
            "hwahap/goal"
        );
        assert!(
            git.is_clean(&path).unwrap(),
            "a fresh worktree starts clean"
        );
    }

    #[test]
    fn add_worktree_refuses_a_path_that_already_exists() {
        let (_dir, git) = repo();
        let holder = TempDir::new().unwrap();
        let occupied = holder.path().join("occupied");
        fs::create_dir(&occupied).unwrap();
        let message = rejection(git.add_worktree(&occupied, "b", "main").unwrap_err());
        assert!(message.contains("already exists"), "{message}");
        assert!(!git.branch_exists("b").unwrap(), "no branch may be created");
    }

    #[cfg(unix)]
    #[test]
    fn add_worktree_refuses_a_path_that_is_a_dangling_symlink() {
        let (_dir, git) = repo();
        let holder = TempDir::new().unwrap();
        let link = holder.path().join("link");
        std::os::unix::fs::symlink(holder.path().join("nowhere"), &link).unwrap();
        let message = rejection(git.add_worktree(&link, "b", "main").unwrap_err());
        assert!(message.contains("already exists"), "{message}");
    }

    #[test]
    fn add_worktree_rejects_option_shaped_arguments_before_git_runs() {
        let (_dir, git) = repo();
        let holder = TempDir::new().unwrap();
        let path = holder.path().join("run");

        let branch = rejection(
            git.add_worktree(&path, "--upload-pack=evil", "main")
                .unwrap_err(),
        );
        assert_eq!(
            branch,
            "branch name \"--upload-pack=evil\" starts with '-'; git would read it as an option"
        );
        let base = rejection(git.add_worktree(&path, "b", "--force").unwrap_err());
        assert!(
            base.starts_with("base revision \"--force\" starts with '-'"),
            "{base}"
        );
        let bad_path = rejection(
            git.add_worktree(Path::new("-detached"), "b", "main")
                .unwrap_err(),
        );
        assert!(
            bad_path.starts_with("worktree path \"-detached\" starts with '-'"),
            "{bad_path}"
        );

        assert!(!path.exists(), "nothing may be created by a rejected call");
    }

    #[cfg(unix)]
    #[test]
    fn a_worktree_path_that_is_not_utf8_is_rejected() {
        use std::os::unix::ffi::OsStrExt;
        let (_dir, git) = repo();
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/hwahap-\xff"));
        let message = rejection(git.add_worktree(&path, "b", "main").unwrap_err());
        assert!(message.contains("is not valid UTF-8"), "{message}");
    }

    #[test]
    fn remove_worktree_removes_the_tree_and_is_idempotent() {
        let (_dir, git) = repo();
        let holder = TempDir::new().unwrap();
        let path = holder.path().join("run");
        git.add_worktree(&path, "hwahap/goal", "main").unwrap();

        git.remove_worktree(&path).unwrap();
        assert!(!path.exists());
        git.remove_worktree(&path).unwrap();
        git.remove_worktree(&holder.path().join("never-existed"))
            .unwrap();

        // The registration must be gone too, or the path could never be reused.
        let listed = git.run(&["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");
    }

    #[test]
    fn remove_worktree_refuses_a_directory_that_is_not_a_worktree() {
        let (_dir, git) = repo();
        let holder = TempDir::new().unwrap();
        let plain = holder.path().join("plain");
        fs::create_dir(&plain).unwrap();
        let (command, detail) = command_failure(git.remove_worktree(&plain).unwrap_err());
        assert!(
            command.starts_with("git worktree remove --force "),
            "{command}"
        );
        assert!(detail.contains("is not a working tree"), "{detail}");
        assert!(plain.is_dir(), "the directory must be left alone");
    }

    #[test]
    fn remove_worktree_rejects_an_option_shaped_path() {
        let (_dir, git) = repo();
        let message = rejection(git.remove_worktree(Path::new("--force")).unwrap_err());
        assert!(
            message.starts_with("worktree path \"--force\" starts with '-'"),
            "{message}"
        );
    }

    // ---- reset, commit, push --------------------------------------------------------------------

    #[test]
    fn reset_hard_discards_both_modifications_and_untracked_files() {
        let (dir, git) = repo();
        let checkpoint = git.head_sha().unwrap();
        write(&dir.path().join("README.md"), "tampered\n");
        write(&dir.path().join("scratch/note.txt"), "left behind\n");
        write(&dir.path().join("ignored.log"), "log\n");
        write(&dir.path().join(".gitignore"), "*.log\n");
        assert!(!git.is_clean(git.root()).unwrap());

        git.reset_hard(git.root(), &checkpoint).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "hello\n"
        );
        assert!(
            !dir.path().join("scratch").exists(),
            "a new directory must be cleaned"
        );
        assert!(
            !dir.path().join("ignored.log").exists(),
            "an ignored file must be cleaned too"
        );
        assert!(git.is_clean(git.root()).unwrap());
        assert_eq!(git.head_sha().unwrap(), checkpoint);
    }

    #[test]
    fn resetting_to_the_same_checkpoint_twice_leaves_the_same_tree() {
        let (dir, git) = repo();
        let checkpoint = git.head_sha().unwrap();
        write(&dir.path().join("stray.txt"), "x\n");
        git.reset_hard(git.root(), &checkpoint).unwrap();
        let once = git.changed_paths(git.root()).unwrap();
        git.reset_hard(git.root(), &checkpoint).unwrap();
        assert_eq!(git.changed_paths(git.root()).unwrap(), once);
        assert_eq!(git.head_sha().unwrap(), checkpoint);
    }

    #[test]
    fn reset_hard_rejects_an_option_shaped_revision() {
        let (_dir, git) = repo();
        let message = rejection(git.reset_hard(git.root(), "--hard").unwrap_err());
        assert!(
            message.starts_with("revision \"--hard\" starts with '-'"),
            "{message}"
        );
    }

    #[test]
    fn reset_hard_reports_an_unknown_revision() {
        let (_dir, git) = repo();
        let (command, detail) =
            command_failure(git.reset_hard(git.root(), "cafe1234").unwrap_err());
        assert_eq!(command, "git reset --hard cafe1234");
        assert!(!detail.is_empty(), "git's own explanation must be carried");
    }

    #[test]
    fn commit_all_returns_the_sha_that_head_sha_then_reports() {
        let (dir, git) = repo();
        let before = git.head_sha().unwrap();
        write(&dir.path().join("added.txt"), "new\n");
        let sha = git.commit_all(git.root(), "U1 checkpoint").unwrap();
        assert_ne!(sha, before);
        assert_eq!(git.head_sha().unwrap(), sha);
        assert!(git.is_clean(git.root()).unwrap());
        assert_eq!(
            git.run(&["log", "-1", "--format=%s"]).unwrap(),
            "U1 checkpoint"
        );
    }

    #[test]
    fn commit_all_stages_deletions_and_untracked_files_alike() {
        let (dir, git) = repo();
        write(&dir.path().join("doomed.txt"), "x\n");
        git.commit_all(git.root(), "add doomed").unwrap();
        let before = git.head_sha().unwrap();
        fs::remove_file(dir.path().join("doomed.txt")).unwrap();
        write(&dir.path().join("nested/new.txt"), "y\n");
        let after = git.commit_all(git.root(), "remove and add").unwrap();
        assert_eq!(
            git.changed_paths_between(git.root(), &before, &after)
                .unwrap(),
            strings(&["doomed.txt", "nested/new.txt"])
        );
    }

    #[test]
    fn commit_all_errs_when_there_is_nothing_to_commit() {
        let (_dir, git) = repo();
        let before = git.head_sha().unwrap();
        let message = rejection(git.commit_all(git.root(), "empty checkpoint").unwrap_err());
        assert!(message.starts_with("nothing to commit in "), "{message}");
        assert!(message.contains("already matches HEAD"), "{message}");
        assert_eq!(git.head_sha().unwrap(), before, "no commit may be made");
    }

    #[test]
    fn commit_all_refuses_a_blank_message() {
        let (dir, git) = repo();
        write(&dir.path().join("added.txt"), "new\n");
        for blank in ["", "   ", "\n\t"] {
            let message = rejection(git.commit_all(git.root(), blank).unwrap_err());
            assert_eq!(message, "a commit message must not be blank");
        }
        assert!(
            !git.is_clean(git.root()).unwrap(),
            "the change must survive the rejection"
        );
        assert_eq!(git.run(&["rev-list", "--count", "HEAD"]).unwrap(), "1");
    }

    #[test]
    fn a_commit_message_that_looks_like_an_option_is_still_a_message() {
        let (dir, git) = repo();
        write(&dir.path().join("added.txt"), "new\n");
        git.commit_all(git.root(), "--amend").unwrap();
        assert_eq!(git.run(&["log", "-1", "--format=%s"]).unwrap(), "--amend");
        assert_eq!(git.run(&["rev-list", "--count", "HEAD"]).unwrap(), "2");
    }

    #[test]
    fn a_commit_message_with_unicode_and_newlines_is_stored_verbatim() {
        let (dir, git) = repo();
        write(&dir.path().join("added.txt"), "new\n");
        git.commit_all(git.root(), "U1 문서 ☕\n\nwhy: because")
            .unwrap();
        assert_eq!(
            git.run(&["log", "-1", "--format=%s"]).unwrap(),
            "U1 문서 ☕"
        );
        assert_eq!(
            git.run(&["log", "-1", "--format=%b"]).unwrap(),
            "why: because"
        );
    }

    #[test]
    fn checkpoint_and_repair_commits_use_the_repository_identity() {
        let (dir, git) = repo();
        setup(dir.path(), &["config", "user.name", "Somebody Else"]);
        setup(
            dir.path(),
            &["config", "user.email", "else@example.invalid"],
        );
        write(&dir.path().join("added.txt"), "new\n");
        git.commit_all(git.root(), "checkpoint").unwrap();
        assert_eq!(
            git.run(&["log", "-1", "--format=%an <%ae>|%cn <%ce>"])
                .unwrap(),
            "Somebody Else <else@example.invalid>|Somebody Else <else@example.invalid>"
        );
        let tree = git.run(&["write-tree"]).unwrap();
        let head = git.head_sha().unwrap();
        let repair = git
            .run(&["commit-tree", &tree, "-p", &head, "-m", "repair"])
            .unwrap();
        assert_eq!(
            git.run(&["show", "-s", "--format=%an <%ae>|%cn <%ce>", &repair])
                .unwrap(),
            "Somebody Else <else@example.invalid>|Somebody Else <else@example.invalid>"
        );
    }

    #[test]
    fn global_identity_is_used_but_absent_identity_is_never_guessed() {
        let (dir, git) = repo();
        for key in ["user.name", "user.email"] {
            setup(dir.path(), &["config", "--unset", key]);
        }
        let isolated = TempDir::new().unwrap();
        let command = |args: &[&str]| {
            let mut cmd = git_command(dir.path(), args);
            cmd.env("HOME", isolated.path())
                .env("XDG_CONFIG_HOME", isolated.path());
            cmd
        };
        write(&dir.path().join("added.txt"), "new\n");
        git.run(&["add", "-A"]).unwrap();
        let tree = git.run(&["write-tree"]).unwrap();
        let head = git.head_sha().unwrap();
        for args in [
            vec!["commit", "-m", "checkpoint"],
            vec!["commit-tree", &tree, "-p", &head, "-m", "repair"],
        ] {
            let output = command(&args).output().unwrap();
            assert!(
                !output.status.success(),
                "missing identity must prevent a commit"
            );
            assert!(!output.stderr.is_empty());
            assert_eq!(git.head_sha().unwrap(), head);
        }
        write(
            &isolated.path().join(".gitconfig"),
            "[user]\nname = Global User\nemail = global@example.invalid\n",
        );
        let output = command(&["commit", "-m", "configured"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            git.run(&["log", "-1", "--format=%an <%ae>|%cn <%ce>"])
                .unwrap(),
            "Global User <global@example.invalid>|Global User <global@example.invalid>"
        );
    }

    #[test]
    fn linked_worktree_commits_resolve_the_worktrees_identity() {
        let (dir, git) = repo();
        let container = TempDir::new().unwrap();
        let worktree = container.path().join("worker");
        setup(dir.path(), &["config", "extensions.worktreeConfig", "true"]);
        setup(
            dir.path(),
            &[
                "worktree",
                "add",
                "-b",
                "codex/identity",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        setup(
            &worktree,
            &["config", "--worktree", "user.name", "Worktree User"],
        );
        setup(
            &worktree,
            &[
                "config",
                "--worktree",
                "user.email",
                "worktree@example.invalid",
            ],
        );
        write(&worktree.join("change.txt"), "owned\n");
        git.commit_all(&worktree, "worktree checkpoint").unwrap();
        assert_eq!(
            git.run_in(&worktree, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"])
                .unwrap(),
            "Worktree User <worktree@example.invalid>|Worktree User <worktree@example.invalid>"
        );
    }

    #[test]
    fn a_repository_hook_cannot_interfere_with_a_checkpoint() {
        let (dir, git) = repo();
        let hook = dir.path().join(".git/hooks/pre-commit");
        write(&hook, "#!/bin/sh\nexit 1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        write(&dir.path().join("added.txt"), "new\n");
        git.commit_all(git.root(), "checkpoint").unwrap();
        assert!(git.is_clean(git.root()).unwrap());
    }

    #[test]
    fn changed_paths_between_reports_what_two_commits_differ_in() {
        let (dir, git) = repo();
        let first = git.head_sha().unwrap();
        write(&dir.path().join("b.txt"), "b\n");
        write(&dir.path().join("a.txt"), "a\n");
        let second = git.commit_all(git.root(), "two files").unwrap();

        assert_eq!(
            git.changed_paths_between(git.root(), &first, &second)
                .unwrap(),
            strings(&["a.txt", "b.txt"])
        );
        assert_eq!(
            git.changed_paths_between(git.root(), &second, &second)
                .unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn what_changed_before_a_checkpoint_is_exactly_what_the_checkpoint_recorded() {
        let (dir, git) = repo();
        let before = git.head_sha().unwrap();
        write(&dir.path().join("new/dir/a.txt"), "a\n");
        write(&dir.path().join("README.md"), "changed\n");
        let pending = git.changed_paths(git.root()).unwrap();
        assert_eq!(pending, strings(&["README.md", "new/dir/a.txt"]));

        let after = git.commit_all(git.root(), "checkpoint").unwrap();

        assert_eq!(
            git.changed_paths_between(git.root(), &before, &after)
                .unwrap(),
            pending
        );
        assert!(git.changed_paths(git.root()).unwrap().is_empty());
    }

    #[test]
    fn changed_paths_between_rejects_option_shaped_revisions() {
        let (_dir, git) = repo();
        let from = rejection(
            git.changed_paths_between(git.root(), "-1", "HEAD")
                .unwrap_err(),
        );
        assert!(
            from.starts_with("from revision \"-1\" starts with '-'"),
            "{from}"
        );
        let to = rejection(
            git.changed_paths_between(git.root(), "HEAD", "--cached")
                .unwrap_err(),
        );
        assert!(
            to.starts_with("to revision \"--cached\" starts with '-'"),
            "{to}"
        );
        let blank = rejection(
            git.changed_paths_between(git.root(), " ", "HEAD")
                .unwrap_err(),
        );
        assert_eq!(blank, "from revision must not be blank");
    }

    #[test]
    fn push_publishes_the_branch_and_records_its_upstream() {
        let (_dir, git) = repo();
        let remote_dir = TempDir::new().unwrap();
        setup(remote_dir.path(), &["init", "--bare", "-b", "main"]);
        let remote = remote_dir.path().to_str().unwrap();

        git.push(git.root(), remote, "main").unwrap();

        assert_eq!(
            setup(remote_dir.path(), &["rev-parse", "main"]),
            git.head_sha().unwrap()
        );
        assert_eq!(
            git.run(&["config", "--get", "branch.main.merge"]).unwrap(),
            "refs/heads/main"
        );
        assert_eq!(
            git.run(&["config", "--get", "branch.main.remote"]).unwrap(),
            remote
        );
    }

    #[test]
    fn push_rejects_option_shaped_remotes_and_branches() {
        let (_dir, git) = repo();
        let remote = rejection(git.push(git.root(), "--exec=evil", "main").unwrap_err());
        assert!(
            remote.starts_with("remote \"--exec=evil\" starts with '-'"),
            "{remote}"
        );
        let branch = rejection(git.push(git.root(), "origin", "--delete").unwrap_err());
        assert!(
            branch.starts_with("branch name \"--delete\" starts with '-'"),
            "{branch}"
        );
        let blank = rejection(git.push(git.root(), "", "main").unwrap_err());
        assert_eq!(blank, "remote must not be blank");
    }

    #[test]
    fn push_to_an_unknown_remote_fails_loudly() {
        let (_dir, git) = repo();
        let (command, detail) =
            command_failure(git.push(git.root(), "origin", "main").unwrap_err());
        assert_eq!(command, "git push --set-upstream origin main");
        assert!(detail.contains("origin"), "{detail}");
    }

    #[test]
    fn a_unit_that_writes_outside_its_declared_paths_is_visible_in_git_state() {
        let (dir, git) = repo();
        write(&dir.path().join("src/allowed.rs"), "in scope\n");
        write(&dir.path().join("docs/sneaky.md"), "out of scope\n");
        let changed = git.changed_paths(git.root()).unwrap();
        assert_eq!(
            paths_outside(&strings(&["src/"]), &changed),
            strings(&["docs/sneaky.md"])
        );
    }

    // ---- the invocation surface itself ----------------------------------------------------------

    #[test]
    fn run_returns_stdout_with_no_surrounding_whitespace() {
        let (_dir, git) = repo();
        let branch = git.run(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn a_failing_command_carries_its_name_and_gits_own_stderr() {
        let (_dir, git) = repo();
        let (command, detail) =
            command_failure(git.run(&["rev-parse", "no-such-ref"]).unwrap_err());
        assert_eq!(command, "git rev-parse no-such-ref");
        // The exit code, not the wording around it: std spells `ExitStatus` as "exit status: 128"
        // on unix and "exit code: 128" on Windows, and neither is the thing worth pinning.
        assert!(detail.contains("128"), "{detail}");
        assert!(detail.contains("unknown revision"), "{detail}");
    }

    #[test]
    fn run_in_uses_the_directory_it_is_given_and_not_the_repository_root() {
        let (_dir, git) = repo();
        let holder = TempDir::new().unwrap();
        let path = holder.path().join("run");
        git.add_worktree(&path, "hwahap/goal", "main").unwrap();
        same_path(
            git.run_in(&path, &["rev-parse", "--show-toplevel"])
                .unwrap(),
            &path,
        );
        same_path(
            git.run(&["rev-parse", "--show-toplevel"]).unwrap(),
            git.root(),
        );
    }

    #[test]
    fn a_command_in_a_missing_directory_fails_instead_of_falling_back_to_the_root() {
        let (_dir, git) = repo();
        let missing = git.root().join("no-such-directory");
        let (command, detail) = command_failure(git.run_in(&missing, &["status"]).unwrap_err());
        assert_eq!(command, "git status");
        assert!(detail.contains(&missing.display().to_string()), "{detail}");
    }

    #[test]
    fn output_that_is_not_utf8_is_an_error_rather_than_a_lossy_string() {
        let (dir, git) = repo();
        fs::write(dir.path().join("binary.dat"), [0xff_u8, 0xfe, 0x00, 0x01]).unwrap();
        git.commit_all(git.root(), "add binary").unwrap();
        let err = git
            .run(&["cat-file", "blob", "HEAD:binary.dat"])
            .unwrap_err();
        let (command, detail) = command_failure(err);
        assert_eq!(command, "git cat-file blob HEAD:binary.dat");
        assert!(detail.contains("output is not valid UTF-8"), "{detail}");
    }
}
