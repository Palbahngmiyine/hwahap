//! The GitHub operations Hwahap performs, and the ones it refuses to.
//!
//! Hwahap creates a draft PR and, on an explicit `SHIP <challenge>`, marks it ready. It never
//! merges, never enables auto-merge, and never force-pushes. The `gh` CLI is the whole surface:
//! there is no API client, no token handling, and no credential reading.

use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// A pull request Hwahap opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub url: String,
    /// The commit the PR currently points at.
    pub head_sha: String,
}

/// The `gh` CLI, scoped to one repository checkout.
#[derive(Debug, Clone)]
pub struct Forge {
    program: String,
}

impl Default for Forge {
    fn default() -> Self {
        Forge {
            program: "gh".to_string(),
        }
    }
}

impl Forge {
    /// Uses a specific `gh` binary. Tests point this at a stub.
    pub fn with_program(program: impl Into<String>) -> Forge {
        Forge {
            program: program.into(),
        }
    }

    /// Fails unless `gh` is installed and authenticated.
    ///
    /// Checked before the coding cycle starts rather than at delivery time: discovering a missing
    /// login after an hour of autonomous work wastes the whole run.
    pub fn require_auth(&self, cwd: &Path) -> Result<()> {
        self.run(cwd, &["auth", "status"]).map(|_| ())
    }

    /// Checks publication eligibility before pushing, and again before editing PR metadata.
    pub fn existing_draft(&self, cwd: &Path, base: &str, head: &str) -> Result<Option<String>> {
        reject_flag_like("base branch", base)?;
        reject_flag_like("head branch", head)?;
        let json = self.run(
            cwd,
            &[
                "pr",
                "list",
                "--state",
                "open",
                "--base",
                base,
                "--head",
                head,
                "--json",
                "url,isDraft,headRefName,baseRefName",
            ],
        )?;
        let existing: Vec<serde_json::Value> = serde_json::from_str(&json)
            .map_err(|e| Error::command("gh", format!("pr list returned invalid JSON: {e}")))?;
        if !existing.is_empty() {
            let pr = &existing[0];
            if existing.len() != 1
                || pr["isDraft"] != true
                || pr["headRefName"] != head
                || pr["baseRefName"] != base
            {
                return Err(Error::Rejected(
                    "expected one matching draft PR; refusing to update another or ready PR".into(),
                ));
            }
            let url = pr["url"]
                .as_str()
                .filter(|url| url.starts_with("https://"))
                .ok_or_else(|| Error::command("gh", "matching draft has no valid URL"))?;
            return Ok(Some(url.into()));
        }
        Ok(None)
    }

    /// Opens a draft, or updates this branch's existing draft after an adjustment.
    pub fn create_draft(
        &self,
        cwd: &Path,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest> {
        if let Some(url) = self.existing_draft(cwd, base, head)? {
            self.run(cwd, &["pr", "edit", &url, "--title", title, "--body", body])?;
            return Ok(PullRequest {
                head_sha: self.head_sha(cwd, &url)?,
                url,
            });
        }
        let url = self.run(
            cwd,
            &[
                "pr", "create", "--draft", "--base", base, "--head", head, "--title", title,
                "--body", body,
            ],
        )?;
        let url = last_url(&url).ok_or_else(|| {
            Error::command(
                "gh",
                format!("pr create printed no pull request URL: {url:?}"),
            )
        })?;
        let head_sha = self.head_sha(cwd, &url)?;
        Ok(PullRequest { url, head_sha })
    }

    /// Update only the recorded draft; never discover or create a replacement during recheck.
    pub fn update_draft(
        &self,
        cwd: &Path,
        base: &str,
        head: &str,
        url: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest> {
        if self.existing_draft(cwd, base, head)?.as_deref() != Some(url) {
            return Err(Error::BoundaryViolation(
                "recorded draft changed before update".into(),
            ));
        }
        self.run(cwd, &["pr", "edit", url, "--title", title, "--body", body])?;
        if self.existing_draft(cwd, base, head)?.as_deref() != Some(url) {
            return Err(Error::BoundaryViolation(
                "recorded draft changed during update".into(),
            ));
        }
        Ok(PullRequest {
            url: url.into(),
            head_sha: self.head_sha(cwd, url)?,
        })
    }

    /// The commit the pull request currently points at.
    pub fn head_sha(&self, cwd: &Path, pr: &str) -> Result<String> {
        let json = self.run(cwd, &["pr", "view", pr, "--json", "headRefOid"])?;
        let value: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| Error::command("gh", format!("pr view returned invalid JSON: {e}")))?;
        value
            .get("headRefOid")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::command("gh", format!("pr view returned no headRefOid: {json}")))
    }

    /// Whether every required check on the pull request has succeeded.
    ///
    /// A pending check is not a success: shipping on a green-so-far run would bind the plan to a
    /// verification that had not finished.
    pub fn checks_passed(&self, cwd: &Path, pr: &str) -> Result<bool> {
        let json = self.run(cwd, &["pr", "view", pr, "--json", "statusCheckRollup"])?;
        let value: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| Error::command("gh", format!("pr view returned invalid JSON: {e}")))?;
        let Some(checks) = value.get("statusCheckRollup").and_then(|v| v.as_array()) else {
            // No rollup at all means no checks are configured; there is nothing to be green.
            return Ok(true);
        };
        Ok(checks.iter().all(check_succeeded))
    }

    /// Marks the draft pull request ready for review.
    ///
    /// This is the entire consequence of `SHIP`. Merging stays with the user.
    pub fn mark_ready(&self, cwd: &Path, pr: &str) -> Result<()> {
        self.run(cwd, &["pr", "ready", pr]).map(|_| ())
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.program)
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|e| Error::command(&self.program, format!("could not run {args:?}: {e}")))?;
        if !output.status.success() {
            return Err(Error::command(
                &self.program,
                format!(
                    "{args:?} exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// True when one entry of `statusCheckRollup` counts as passing.
///
/// The rollup mixes two shapes: check runs carry `conclusion`, legacy commit statuses carry
/// `state`. Anything unrecognized is treated as not-passing, because an unknown check is not a
/// green one.
fn check_succeeded(check: &serde_json::Value) -> bool {
    if let Some(conclusion) = check.get("conclusion").and_then(|v| v.as_str()) {
        return matches!(conclusion, "SUCCESS" | "NEUTRAL" | "SKIPPED");
    }
    matches!(check.get("state").and_then(|v| v.as_str()), Some("SUCCESS"))
}

/// The last whitespace-separated token that looks like an https URL.
fn last_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .rfind(|t| t.starts_with("https://"))
        .map(str::to_string)
}

/// Rejects a value that `gh` would read as a flag.
fn reject_flag_like(what: &str, value: &str) -> Result<()> {
    if value.starts_with('-') || value.is_empty() {
        return Err(Error::Rejected(format!(
            "the {what} {value:?} is empty or would be read as a command-line flag"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_check_run_passes_on_success_neutral_and_skipped() {
        for conclusion in ["SUCCESS", "NEUTRAL", "SKIPPED"] {
            assert!(
                check_succeeded(&json!({"conclusion": conclusion})),
                "{conclusion}"
            );
        }
    }

    #[test]
    fn a_check_run_fails_on_anything_else_including_pending() {
        for conclusion in [
            "FAILURE",
            "CANCELLED",
            "TIMED_OUT",
            "ACTION_REQUIRED",
            "STARTUP_FAILURE",
        ] {
            assert!(
                !check_succeeded(&json!({"conclusion": conclusion})),
                "{conclusion}"
            );
        }
        // A queued check run has a null conclusion and no state: not green.
        assert!(!check_succeeded(
            &json!({"conclusion": serde_json::Value::Null})
        ));
        assert!(!check_succeeded(&json!({"status": "IN_PROGRESS"})));
        assert!(!check_succeeded(&json!({})));
    }

    #[test]
    fn a_legacy_commit_status_passes_only_on_success() {
        assert!(check_succeeded(&json!({"state": "SUCCESS"})));
        for state in ["PENDING", "FAILURE", "ERROR", "EXPECTED"] {
            assert!(!check_succeeded(&json!({"state": state})), "{state}");
        }
    }

    #[test]
    fn an_unknown_check_shape_is_not_treated_as_green() {
        assert!(!check_succeeded(&json!({"name": "build"})));
        assert!(!check_succeeded(&json!("SUCCESS")));
        assert!(!check_succeeded(&json!(null)));
    }

    #[test]
    fn the_pull_request_url_is_the_last_https_token() {
        assert_eq!(
            last_url("https://github.com/o/r/pull/12").as_deref(),
            Some("https://github.com/o/r/pull/12")
        );
        assert_eq!(
            last_url("Creating draft pull request for x into main\nhttps://github.com/o/r/pull/12")
                .as_deref(),
            Some("https://github.com/o/r/pull/12")
        );
        // gh has printed a docs link before the PR URL in the past; the PR is always last.
        assert_eq!(
            last_url("see https://cli.github.com/manual\nhttps://github.com/o/r/pull/9").as_deref(),
            Some("https://github.com/o/r/pull/9")
        );
    }

    #[test]
    fn output_with_no_url_yields_none() {
        for text in ["", "   ", "no url here", "http://github.com/o/r/pull/1"] {
            assert_eq!(last_url(text), None, "{text:?}");
        }
    }

    #[test]
    fn branch_names_that_look_like_flags_are_rejected_before_gh_runs() {
        for value in ["-x", "--upload-pack=evil", "--draft", ""] {
            let err = reject_flag_like("head branch", value)
                .unwrap_err()
                .to_string();
            assert!(err.contains("head branch"), "{err}");
        }
    }

    #[test]
    fn ordinary_branch_names_are_accepted() {
        for value in ["main", "hwahap/2026-09-04-dry-run", "feature_1", "a-b.c"] {
            reject_flag_like("base branch", value).unwrap();
        }
    }

    #[test]
    fn a_missing_gh_binary_is_reported_as_a_command_failure() {
        let forge = Forge::with_program("gh-that-does-not-exist-hwahap");
        let err = forge.require_auth(Path::new(".")).unwrap_err();
        assert!(matches!(err, Error::Command { .. }), "{err:?}");
        assert!(
            err.to_string().contains("gh-that-does-not-exist-hwahap"),
            "{err}"
        );
    }

    #[test]
    fn the_default_forge_uses_the_gh_cli() {
        assert_eq!(Forge::default().program, "gh");
    }

    #[test]
    fn a_rollup_of_all_successes_passes_and_one_failure_does_not() {
        let all_good =
            json!({"statusCheckRollup": [{"conclusion": "SUCCESS"}, {"state": "SUCCESS"}]});
        let one_bad =
            json!({"statusCheckRollup": [{"conclusion": "SUCCESS"}, {"conclusion": "FAILURE"}]});
        let checks = |v: &serde_json::Value| {
            v.get("statusCheckRollup")
                .and_then(|r| r.as_array())
                .map(|a| a.iter().all(check_succeeded))
        };
        assert_eq!(checks(&all_good), Some(true));
        assert_eq!(checks(&one_bad), Some(false));
    }
}
