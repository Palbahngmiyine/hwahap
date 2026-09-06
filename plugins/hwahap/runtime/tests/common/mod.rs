//! The scripted cycle harness: a real repository, a stub `gh`, and a fake agent.
//!
//! Hwahap's whole claim is that it judges from repository state rather than from what an agent
//! says. Testing that claim needs a real repository and a lying agent, but it does not need a
//! model. [`Script`] stands in for native sessions at the [`Sessions`] seam, so every branch of the
//! cycle — rework, out-of-scope resets, plan conflicts, crash recovery, the ship gate — runs
//! deterministically and in milliseconds.

#![allow(dead_code)] // Each integration test binary uses a different part of this harness.

use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;

use hwahap::clock::FixedClock;
use hwahap::engine::{Engine, Sessions};
use hwahap::error::{Error, Result};
use hwahap::forge::Forge;
use hwahap::profile::Role;
use hwahap::session::{NativeReceipt, SessionReceipt};
use hwahap::session::{SessionOutcome, SessionSpec};

pub const NOW: &str = "2026-09-04T00:00:00Z";

/// What a scripted session does before it answers.
#[derive(Debug, Clone)]
pub enum Reply {
    NativeReview {
        message: String,
        agent_id: String,
    },
    PrAttack,
    PrDefense {
        writes: Vec<(String, String)>,
    },
    /// Answer without touching the working tree.
    Say(String),
    /// Create or overwrite files under the session's cwd, then answer.
    WriteThenSay {
        files: Vec<(String, String)>,
        message: String,
    },
    /// Delete files under the session's cwd, then answer.
    DeleteThenSay {
        paths: Vec<String>,
        message: String,
    },
    /// Fail the session outright, as a dropped native session would.
    Fail(String),
    /// Answer with a receipt whose recorded request differs from the dispatch.
    SayWithSkewedReceipt(String),
}

impl Reply {
    pub fn pr_defense() -> Self {
        Self::PrDefense { writes: vec![] }
    }
    pub fn say(message: impl Into<String>) -> Reply {
        Reply::Say(message.into())
    }

    pub fn write(files: &[(&str, &str)], message: impl Into<String>) -> Reply {
        Reply::WriteThenSay {
            files: files
                .iter()
                .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
                .collect(),
            message: message.into(),
        }
    }
}

/// One session the script expects to be asked for.
#[derive(Debug, Clone)]
pub struct Step {
    pub role: Role,
    pub reply: Reply,
}

pub fn step(role: Role, reply: Reply) -> Step {
    Step { role, reply }
}

/// What the engine actually asked for.
#[derive(Debug, Clone)]
pub struct Call {
    pub role: Role,
    pub cwd: PathBuf,
    pub unit: Option<String>,
    pub prompt: String,
}

/// A fake agent driven by a queue of expected sessions.
///
/// An unexpected role is an error rather than a silent substitution: a test that drifts from the
/// cycle it means to exercise should say so loudly.
pub struct Script {
    queue: Mutex<VecDeque<Step>>,
    log: Mutex<Vec<Call>>,
}

impl Script {
    pub fn new(steps: Vec<Step>) -> Script {
        Script {
            queue: Mutex::new(steps.into()),
            log: Mutex::new(Vec::new()),
        }
    }

    /// Every session the engine asked for, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.log.lock().expect("poisoned").clone()
    }

    /// The prompts given to one role, in order.
    pub fn prompts_for(&self, role: Role) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.role == role)
            .map(|c| c.prompt)
            .collect()
    }

    pub fn roles(&self) -> Vec<Role> {
        self.calls().into_iter().map(|c| c.role).collect()
    }

    /// Sessions still queued but never asked for.
    pub fn remaining(&self) -> usize {
        self.queue.lock().expect("poisoned").len()
    }

    /// Adds more sessions to the end of the queue.
    pub fn extend(&self, steps: Vec<Step>) {
        self.queue.lock().expect("poisoned").extend(steps);
    }

    fn answer(&self, spec: &SessionSpec) -> Result<SessionOutcome> {
        self.log.lock().expect("poisoned").push(Call {
            role: spec.role,
            cwd: spec.cwd.clone(),
            unit: spec.unit.clone(),
            prompt: spec.prompt.clone(),
        });

        let step = self
            .queue
            .lock()
            .expect("poisoned")
            .pop_front()
            .ok_or_else(|| {
                Error::Internal(format!(
                    "the engine asked for an unscripted {:?} session",
                    spec.role
                ))
            })?;
        if step.role != spec.role {
            return Err(Error::Internal(format!(
                "the engine asked for {:?} but the script expected {:?}",
                spec.role, step.role
            )));
        }

        let common = git(
            &spec.cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        );
        let store = hwahap::state::Store::open(Path::new(&common).parent().unwrap())?;
        let profiles = hwahap::config::Config::for_run(&store)?.profiles;
        let wanted = profiles.for_role(spec.role);
        let run = store.read_run()?.expect("scripted run");
        let snapshot = hwahap::catalog::snapshot(&store, &run.run_id)?;
        let parent = hwahap::catalog::host::latest(&store)?
            .map(|o| o.host_session_id)
            .unwrap_or_else(|| run.run_id.clone());
        let host = fixture_observation(&store, &parent);
        let mut receipt = NativeReceipt {
            assessment: None,
            decision: None,
            selection: hwahap::catalog::Selection::new(
                &snapshot,
                &host,
                spec.role,
                spec.unit.clone(),
                &wanted.model,
                wanted.effort.as_str(),
            )?,
            dispatch_id: format!("script-{:?}-{}", spec.role, self.calls().len()),
            agent_id: format!("script-{:?}", spec.role),
            profile: spec.role.profile(),
            role: spec.role,
            unit: spec.unit.clone(),
            model_requested: wanted.model.clone(),
            effort_requested: wanted.effort.clone(),
            elapsed_ms: 1,
            reported_usage: None,
        };

        let mut native_id = None;
        let message = match step.reply {
            Reply::NativeReview { message, agent_id } => {
                native_id = Some(agent_id);
                message
            }
            Reply::PrAttack => pr_report(spec, false)?,
            Reply::PrDefense { writes } => {
                for (path, contents) in writes {
                    std::fs::write(spec.cwd.join(path), contents)
                        .map_err(|e| Error::io(&spec.cwd, e))?;
                }
                pr_report(spec, true)?
            }
            Reply::Say(message) => message,
            Reply::Fail(detail) => return Err(Error::command("scripted-session", detail)),
            Reply::SayWithSkewedReceipt(message) => {
                receipt.model_requested = "gpt-5.4-mini".to_string();
                message
            }
            Reply::WriteThenSay { files, message } => {
                for (path, contents) in files {
                    let target = spec.cwd.join(&path);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                    }
                    std::fs::write(&target, contents).map_err(|e| Error::io(&target, e))?;
                }
                message
            }
            Reply::DeleteThenSay { paths, message } => {
                for path in paths {
                    let target = spec.cwd.join(&path);
                    std::fs::remove_file(&target).map_err(|e| Error::io(&target, e))?;
                }
                message
            }
        };

        Ok(SessionOutcome {
            final_message: message.clone(),
            transcript: message,
            receipt: {
                if let Some(agent_id) = native_id {
                    receipt.agent_id = agent_id;
                }
                SessionReceipt::Native(receipt)
            },
            stop_reason: "end_turn".to_string(),
        })
    }
}

pub fn security_review() -> serde_json::Value {
    let mut review = hwahap::pr_review::security_example();
    review["threat_model"] =
        serde_json::json!(["controlled fixture: local repository and feature file"]);
    for check in review["checks"].as_array_mut().unwrap() {
        check["status"] = "checked".into();
        check["evidence"] = serde_json::json!(["controlled fixture source inspection"]);
    }
    review
}

fn pr_report(spec: &SessionSpec, defense: bool) -> Result<String> {
    assert_eq!(
        spec.role,
        if defense {
            Role::FinalReview
        } else {
            Role::UnitReviewer
        }
    );
    assert!(spec.unit.is_none());
    let store = hwahap::state::Store::open(spec.cwd.parent().unwrap().parent().unwrap())?;
    let binding = hwahap::pr_review::ReviewProgress::load(&store)?
        .unwrap()
        .binding;
    Ok(if defense {
        serde_json::json!({"binding":binding,"assessments":[],"additional_findings":[],"security":security_review(),"evidence":["controlled independent defense"]})
    } else {
        serde_json::json!({"binding":binding,"findings":[],"security":security_review(),"evidence":["controlled attack checks"]})
    }.to_string())
}

impl Sessions for Script {
    fn run<'a>(
        &'a self,
        spec: &'a SessionSpec,
    ) -> Pin<Box<dyn Future<Output = Result<SessionOutcome>> + Send + 'a>> {
        let answer = self.answer(spec);
        Box::pin(async move { answer })
    }
}

/// A temporary repository with a bare `origin` and a stub `gh`.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub repo: PathBuf,
    pub gh: PathBuf,
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("repo");
        let remote = dir.path().join("remote.git");
        std::fs::create_dir_all(&repo).expect("repo dir");

        git(&repo, &["init", "-b", "main", "--quiet"]);
        git(&repo, &["config", "user.email", "hwahap@example.invalid"]);
        git(&repo, &["config", "user.name", "Hwahap Test"]);
        std::fs::write(repo.join(".gitignore"), ".hwahap/\n").expect("gitignore");
        std::fs::create_dir_all(repo.join("src")).expect("src");
        std::fs::write(repo.join("src/existing.txt"), "start\n").expect("seed file");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "initial", "--quiet"]);

        git(
            dir.path(),
            &["init", "--bare", "--quiet", remote.to_str().expect("utf-8")],
        );
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().expect("utf-8")],
        );

        let gh = dir.path().join("gh-stub");
        std::fs::write(&gh, GH_STUB).expect("gh stub");
        make_executable(&gh);

        Fixture { dir, repo, gh }
    }

    /// An engine wired to the fixture's stub `gh` and a clock that never moves.
    pub fn engine(&self) -> Engine {
        Engine::open(&self.repo).expect("engine").with_parts(
            Box::new(FixedClock::new(NOW)),
            Forge::with_program(self.gh.to_str().expect("utf-8")),
        )
    }

    /// The run worktree, spelled the way the engine spells it.
    ///
    /// The engine roots itself at git's own view of the work tree, which on macOS resolves the
    /// `/var` symlink to `/private/var`. Comparing against the unresolved temp-dir path would fail
    /// on a difference that is not a difference.
    pub fn worktree(&self) -> PathBuf {
        self.repo
            .canonicalize()
            .unwrap_or_else(|_| self.repo.clone())
            .join(".hwahap/worktree")
    }

    /// Makes the stub `gh` report a failing required check.
    pub fn fail_checks(&self) {
        std::fs::write(self.dir.path().join("checks-fail"), "1").expect("control file");
    }

    /// Makes the stub `gh` report a pull request head that is not the worktree's.
    pub fn move_pr_head(&self, sha: &str) {
        std::fs::write(self.dir.path().join("pr-head"), sha).expect("control file");
    }

    /// Whether `gh pr ready` was called.
    pub fn was_marked_ready(&self) -> bool {
        self.dir.path().join("pr-ready").exists()
    }

    pub fn head_sha(&self, cwd: &Path) -> String {
        git(cwd, &["rev-parse", "HEAD"])
    }

    pub fn log_subjects(&self, cwd: &Path) -> Vec<String> {
        git(cwd, &["log", "--format=%s", "--reverse"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn commit_body(&self, cwd: &Path, subject_contains: &str) -> String {
        let full = git(cwd, &["log", "--format=%B%x00"]);
        full.split('\0')
            .find(|message| message.contains(subject_contains))
            .unwrap_or_default()
            .to_string()
    }
}

/// Runs git and returns trimmed stdout, panicking with stderr on failure.
pub fn git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Hwahap Test")
        .env("GIT_AUTHOR_EMAIL", "hwahap@example.invalid")
        .env("GIT_COMMITTER_NAME", "Hwahap Test")
        .env("GIT_COMMITTER_EMAIL", "hwahap@example.invalid")
        .output()
        .unwrap_or_else(|e| panic!("could not run git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// A stub `gh`.
///
/// It answers from the repository itself rather than from canned strings, so the pull request head
/// it reports is the head the engine actually pushed — which is what makes the stale-head ship
/// refusal a real test rather than a tautology. Control files in the fixture directory switch it
/// into the failure modes each test needs.
const GH_STUB: &str = r#"#!/bin/sh
set -eu
control=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

case "$1 ${2:-}" in
  "auth status")
    exit 0
    ;;
  "pr create")
    shift 2
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--head" ]; then
        printf '%s\n' "$2" > "$control/pr-branch"
        printf '%s\n' "$2" >> "$control/pr-created"
        rm -f "$control/pr-ready"
        break
      fi
      shift
    done
    echo "https://github.com/example/repo/pull/1"
    exit 0
    ;;
  "pr list")
    head=''
    base=''
    shift 2
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --head) head=$2 ;;
        --base) base=$2 ;;
      esac
      shift 2
    done
    if [ -f "$control/pr-list-json" ]; then
      cat "$control/pr-list-json"
    elif [ -f "$control/pr-branch" ] && [ "$(cat "$control/pr-branch")" = "$head" ]; then
      draft=true
      if [ -f "$control/pr-ready" ]; then draft=false; fi
      printf '[{"url":"https://github.com/example/repo/pull/1","isDraft":%s,"headRefName":"%s","baseRefName":"%s"}]\n' "$draft" "$head" "$base"
    else
      echo '[]'
    fi
    if [ -f "$control/pr-list-after-read" ]; then
      mv "$control/pr-list-after-read" "$control/pr-list-json"
    fi
    exit 0
    ;;
  "pr edit")
    echo "$3" >> "$control/pr-edited"
    exit 0
    ;;
  "pr view")
    case "$*" in
      *headRefOid*)
        if [ -f "$control/pr-head" ]; then
          printf '{"headRefOid":"%s"}\n' "$(cat "$control/pr-head")"
        else
          # The run branch, not HEAD: the real `gh` resolves a pull request by URL, so the answer
          # must not depend on which directory Hwahap happened to call from.
          pr_head=$(git --git-dir="$control/remote.git" rev-parse --verify "refs/heads/$(cat "$control/pr-branch")") || exit 1
          if [ -f "$control/pr-lag-head" ] && [ "$pr_head" != "$(cat "$control/pr-lag-head")" ]; then
            remaining=$(cat "$control/pr-lag-left")
            if [ "$remaining" -gt 0 ]; then
              echo "$((remaining - 1))" > "$control/pr-lag-left"
              pr_head=$(cat "$control/pr-lag-head")
              if [ -f "$control/pr-lag-value" ]; then pr_head=$(cat "$control/pr-lag-value"); fi
            fi
          fi
          printf '{"headRefOid":"%s"}\n' "$pr_head"
        fi
        ;;
      *statusCheckRollup*)
        if [ -f "$control/checks-fail" ]; then
          echo '{"statusCheckRollup":[{"conclusion":"FAILURE"}]}'
        else
          echo '{"statusCheckRollup":[{"conclusion":"SUCCESS"}]}'
        fi
        ;;
      *)
        echo "gh-stub: unexpected pr view: $*" >&2
        exit 1
        ;;
    esac
    exit 0
    ;;
  "pr ready")
    : > "$control/pr-ready"
    exit 0
    ;;
esac

echo "gh-stub: unexpected invocation: $*" >&2
exit 1
"#;

pub fn fixture_observation(
    store: &hwahap::state::Store,
    parent: &str,
) -> hwahap::catalog::HostObservation {
    use hwahap::clock::{Clock, SystemClock};
    let catalog = hwahap::catalog::bundled();
    let parent = parent.to_owned();
    let observed = hwahap::catalog::HostObservation {
        host_session_id: parent.clone(),
        observed_at: SystemClock.now(),
        source: "scripted test host inventory".into(),
        parent_model: "gpt-6-astra".into(),
        parent_effort: "high".into(),
        available_slots: 3,
        models: catalog
            .models
            .into_iter()
            .map(|m| {
                (
                    m.id,
                    hwahap::catalog::ObservedModel {
                        efforts: m.efforts.into_iter().map(|e| e.name).collect(),
                        tools: vec!["exec_command".into(), "apply_patch".into()],
                    },
                )
            })
            .collect(),
    };
    hwahap::catalog::host::observe(store, &SystemClock, &parent, &observed).unwrap();
    observed
}

pub fn fixture_assessments(store: &hwahap::state::Store) {
    use hwahap::delegation::*;
    let run = store.read_run().unwrap().unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    for unit in std::iter::once(None).chain(plan.units.iter().map(Some)) {
        for role in Role::ALL {
            let assessment = TaskAssessment {
                run_id: run.run_id.clone(),
                contract_digest: plan.digest().unwrap().to_string(),
                unit: unit.map(|u| u.id.clone()),
                role,
                requirements: hwahap::catalog::Requirements {
                    capabilities: Default::default(),
                    depth: hwahap::catalog::Depth::Routine,
                },
                risk: Risk {
                    failure_cost: Some(0),
                    reversibility: Some(0),
                    blast_radius: Some(0),
                },
                topology: Topology {
                    predecessors: unit.map(|u| u.depends_on.clone()).unwrap_or_default(),
                    coupling: Coupling::Independent,
                    shared_resources: vec![],
                    writer_owner: Some(
                        unit.map(|u| u.id.clone())
                            .unwrap_or_else(|| run.run_id.clone()),
                    ),
                    separable: true,
                    write_paths: unit.map(|u| u.paths.clone()).unwrap_or_else(|| {
                        plan.units.iter().flat_map(|u| u.paths.clone()).collect()
                    }),
                },
                evidence: vec!["test-owned disposable repository; no external resources".into()],
                recovery: None,
            };
            store::record(store, &assessment).unwrap();
        }
    }
}
pub fn fixture_native_observation(
    store: &hwahap::state::Store,
    parent: &str,
) -> hwahap::catalog::HostObservation {
    fixture_assessments(store);
    fixture_observation(store, parent)
}
