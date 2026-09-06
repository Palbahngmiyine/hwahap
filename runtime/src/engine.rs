//! The state machine: PLAN, FREEZE, autonomous CODING, draft PR, ADJUST or SHIP.
//!
//! Everything the host can ask for arrives here through three entry points — [`Engine::step`],
//! [`Engine::status`], [`Engine::ship`] — and every decision about what happens next is made here
//! rather than by the calling model. That is the point of having only three tools.
//!
//! Agent sessions reach the engine through the [`Sessions`] trait. Production uses a durable
//! broker for native Codex children; tests use scripts with deterministic results. The
//! engine cannot tell the difference, which is what makes the whole cycle — including crash
//! recovery, out-of-scope resets, rework, and plan conflicts — testable without a model.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::agentresult::{ReviewResult, Verdict, WorkerResult, WorkerStatus};
use crate::answer::{parse_message, Directive};
use crate::canonical::Digest;
use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::forge::Forge;
use crate::git::{paths_outside, Git};
use crate::plan::{Answer, Frozen, Plan, PlanReview, Selection, Surface, SurfaceStatus, Unit};
use crate::profile::Role;
use crate::session::{SessionOutcome, SessionSpec};
use crate::state::{Next, Run, RunState, Store};
use crate::{frontier, prompts, proposal, render, validate};

/// How many implementation attempts one unit gets before the run is blocked.
///
/// One Economy attempt followed by one Deep repair with the failure evidence. No separate
/// diagnosis call, and no repeated same-model retry loop.
const MAX_ATTEMPTS: u32 = 2;

mod adjust_build;
mod approved_plan;
mod build;
mod grounding;
mod interview;
mod lifecycle;
mod pr_review;
pub use adjust_build::AdjustBuildRequest;
pub use build::{BuildRequest, BuildUnit};

/// A source of agent sessions.
///
/// Boxed futures rather than `async fn` in the trait, because the engine holds it behind `dyn` so
/// that a scripted implementation can stand in for native dispatch.
pub trait Sessions: Send + Sync {
    fn run<'a>(
        &'a self,
        spec: &'a SessionSpec,
    ) -> Pin<Box<dyn Future<Output = Result<SessionOutcome>> + Send + 'a>>;
}

/// What a tool call reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    pub run_id: String,
    pub phase: String,
    pub state: String,
    pub next: String,
    pub message: String,
    pub plan_digest: Option<String>,
    pub pr_url: Option<String>,
}

/// One repository's engine.
pub struct Engine {
    repo_root: PathBuf,
    store: Store,
    git: Git,
    forge: Forge,
    config: Config,
    clock: Box<dyn Clock>,
}

impl Engine {
    /// Opens the engine for a repository, creating `.hwahap/` if needed.
    ///
    /// The run is identified by the git work tree, not by the path the caller happened to pass.
    /// A host whose user is sitting in a package subdirectory addresses the same repository, and
    /// rooting the store at the raw argument would give that subdirectory a second `.hwahap/` and
    /// therefore a second active run in one repository — the one thing Hwahap says it never allows.
    pub fn open(repo_root: &Path) -> Result<Engine> {
        let git = Git::open(repo_root)?;
        let root = git.root().to_path_buf();
        let store = Store::open(&root)?;
        let config = Config::for_run(&store)?;
        Ok(Engine {
            repo_root: root,
            store,
            git,
            forge: Forge::default(),
            config,
            clock: Box::new(SystemClock),
        })
    }

    /// Replaces the clock and the forge, for tests.
    pub fn with_parts(mut self, clock: Box<dyn Clock>, forge: Forge) -> Engine {
        self.clock = clock;
        self.forge = forge;
        self
    }

    /// Reads the run without changing anything.
    pub fn status(&self) -> Result<StepOutcome> {
        match self.store.read_run()? {
            None => Ok(StepOutcome {
                run_id: String::new(),
                phase: "plan".into(),
                state: "no_run".into(),
                next: "await_user".into(),
                message: "No Hwahap run is active in this repository. Send an implementation \
                          request to start one."
                    .into(),
                plan_digest: None,
                pr_url: None,
            }),
            Some(run) => {
                let plan = self.store.read_plan()?;
                Ok(self.report(&run, self.describe(&run, plan.as_ref())?))
            }
        }
    }

    /// Advances states without agents; use the native MCP dispatcher for agent work.
    pub async fn step(
        &self,
        request: Option<&str>,
        user_input: Option<&str>,
    ) -> Result<StepOutcome> {
        crate::approval::reject_unbound_implementation_request(user_input)?;
        match self.resolve(request)? {
            Resolved::Started(outcome) => Ok(outcome),
            Resolved::Advance(run) if run.state.needs_sessions() => Err(Error::Rejected(
                "agent work must use the native MCP dispatcher".into(),
            )),
            Resolved::Advance(run) => {
                let resumable = run.clone();
                self.block_if_terminal(resumable, self.advance(run, user_input))
            }
        }
    }

    /// Turns an error the run cannot recover from into a persisted `blocked` state.
    ///
    /// Without this an unsupported profile leaves as a protocol error: the host sees an opaque
    /// internal failure with no `next`, the run's stored state is untouched, and the very next call
    /// tries the same impossible thing again. The promise is that the run *stops*, and stopping is
    /// something that has to be written down.
    fn block_if_terminal(&self, mut run: Run, outcome: Result<StepOutcome>) -> Result<StepOutcome> {
        match outcome {
            Err(e) if e.is_terminal_for_run() => {
                let reason = e.to_string();
                run.state = RunState::Blocked {
                    reason: reason.clone(),
                };
                self.store.write_run(&*self.clock, &run)?;
                Ok(self.report(&run, reason))
            }
            other => other,
        }
    }

    /// The same dispatch against a supplied session source.
    ///
    /// This is what makes the whole cycle testable: a scripted `Sessions` drives crash recovery,
    /// out-of-scope resets, rework, and plan conflicts without a model.
    pub async fn step_with(
        &self,
        sessions: &dyn Sessions,
        request: Option<&str>,
        user_input: Option<&str>,
    ) -> Result<StepOutcome> {
        crate::approval::reject_unbound_implementation_request(user_input)?;
        match self.resolve(request)? {
            Resolved::Started(outcome) => Ok(outcome),
            Resolved::Advance(run) if run.state.needs_sessions() => {
                let resumable = run.clone();
                self.block_if_terminal(resumable, self.drive(run, sessions).await)
            }
            Resolved::Advance(run) => {
                let resumable = run.clone();
                self.block_if_terminal(resumable, self.advance(run, user_input))
            }
        }
    }

    fn resolve(&self, request: Option<&str>) -> Result<Resolved> {
        self.resolve_planning(request, false)
    }

    pub fn start_planning(&self, request: &str, plan_only: bool) -> Result<StepOutcome> {
        match self.resolve_planning(Some(request), plan_only)? {
            Resolved::Started(outcome) => Ok(outcome),
            Resolved::Advance(_) => unreachable!("a request starts or rejects"),
        }
    }

    fn resolve_planning(&self, request: Option<&str>, plan_only: bool) -> Result<Resolved> {
        crate::approval::reject_unbound_implementation_request(request)?;
        let existing = self.store.recover()?;
        match (existing, request) {
            (None, Some(request)) => Ok(Resolved::Started(self.start(request, plan_only)?)),
            (None, None) => Err(Error::Rejected(
                "there is no active Hwahap run in this repository; call hwahap_step again with \
                 `request` set to the user's implementation request"
                    .into(),
            )),
            (Some(run), Some(request)) if run.state.is_terminal() => {
                let worktree = self.store.worktree_path();
                match worktree.symlink_metadata() {
                    Ok(_) => {
                        let branch = if run.branch.is_empty() {
                            format!("hwahap/{}", run.goal_id)
                        } else {
                            run.branch.clone()
                        };
                        self.check_plan_worktree(&branch, None)?;
                        // Even non-force removal deletes ignored files; observe them explicitly.
                        if !self.git.stdout_of(&worktree, &["ls-files", "--others", "--directory", "--no-empty-directory", "-z"])?.is_empty() {
                            return Err(Error::Rejected("the previous worktree contains untracked or ignored files; review and preserve or clean up those files, then retry or use a new checkout".into()));
                        }
                        self.git.run(&["worktree", "remove", worktree.to_str().ok_or_else(|| Error::Rejected("non-UTF8 worktree".into()))?])?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(Error::io(&worktree, error)),
                }
                // A finished run does not block the next one, but it is not silently overwritten:
                // only retire its state after the owned worktree was safely removed.
                self.store.archive(&*self.clock)?;
                Ok(Resolved::Started(self.start(request, plan_only)?))
            }
            (Some(run), Some(_)) => Err(Error::Rejected(format!(
                "a Hwahap run is already active in this repository ({}, state {}). Finish or abandon \
                 it before starting another; Hwahap allows one active run per repository.",
                run.run_id,
                run.state.name()
            ))),
            (Some(run), None) => Ok(Resolved::Advance(run)),
        }
    }

    /// Marks the draft pull request ready, after re-checking everything it depends on.
    pub fn ship(&self, confirmation: &str) -> Result<StepOutcome> {
        let mut run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Rejected("there is no Hwahap run to ship".into()))?;
        let plan = self.require_plan()?;

        let RunState::AwaitingAdjustOrShip { pr_url, challenge } = run.state.clone() else {
            return Err(Error::Rejected(format!(
                "this run is in state {} and has nothing to ship; a draft pull request must exist \
                 first",
                run.state.name()
            )));
        };

        let parsed = parse_message(confirmation);
        if let Some(rejection) = parsed.rejection_message() {
            return Err(Error::Rejected(rejection));
        }
        if parsed.directives.len() != 1 {
            return Err(Error::Rejected(
                "SHIP must be the only directive in the confirmation message".into(),
            ));
        }
        let typed = parsed.directives.iter().find_map(|d| match d {
            Directive::Ship { challenge } => Some(challenge.clone()),
            _ => None,
        });
        let Some(typed) = typed else {
            return Err(Error::Rejected(format!(
                "that is not a ship confirmation. The user must type exactly:\n\nSHIP {challenge}"
            )));
        };
        if typed != challenge {
            return Err(Error::Rejected(format!(
                "the confirmation names {typed}, but this run's challenge is {challenge}. \
                 Something changed since the summary was shown; re-read it before shipping."
            )));
        }

        // Everything below is re-checked now rather than trusted from when the PR was opened.
        if !plan.is_frozen()? || plan.frozen.as_ref().map(|f| &f.digest) != run.plan_digest.as_ref()
        {
            return Err(Error::Rejected(
                "the frozen plan has changed since this pull request was created; ship is refused"
                    .into(),
            ));
        }
        let head = self.forge.head_sha(&self.repo_root, &pr_url)?;
        if Some(&head) != run.reviewed_head.as_ref() {
            return Err(Error::Rejected(format!(
                "the pull request head is now {head}, but the final review looked at {}. Re-run the \
                 cycle before shipping.",
                run.reviewed_head.as_deref().unwrap_or("nothing")
            )));
        }
        self.require_completed_reviews(&run, &plan)?;
        if !self.forge.checks_passed(&self.repo_root, &pr_url)? {
            return Err(Error::Rejected(
                "the pull request's required checks have not all succeeded; ship is refused".into(),
            ));
        }

        let progress = self.require_review_progress(&run, &plan)?;
        self.refresh_review_report(&run, &plan, &progress)?;
        self.forge.mark_ready(&self.repo_root, &pr_url)?;
        run.state = RunState::Shipped {
            pr_url: pr_url.clone(),
        };
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            format!("{pr_url} is now ready for review. Hwahap does not merge; that is yours."),
        ))
    }

    // ---------------------------------------------------------------- planning

    fn start(&self, request: &str, plan_only: bool) -> Result<StepOutcome> {
        if request.trim().is_empty() {
            return Err(Error::Rejected(
                "the implementation request is empty".into(),
            ));
        }
        let base_branch = self.git.current_branch()?;
        let stem = goal_id(&self.clock.now(), request);
        let archived = self.store.archived_run_ids()?;
        let mut incarnation = 1;
        let mut goal_id = stem.clone();
        while archived.contains(&goal_id) || self.git.branch_exists(&format!("hwahap/{goal_id}"))? {
            incarnation += 1;
            goal_id = format!("{stem}-{incarnation}");
        }
        let mut plan = Plan::new(&goal_id, &base_branch, request.trim());
        plan.plan_only = plan_only;
        plan.interactive = true;
        if !self.git.is_clean(&self.repo_root)? {
            return Err(Error::Rejected(
                "PLAN requires a clean committed source so its evidence can be bound".into(),
            ));
        }
        plan.source_head = Some(self.git.head_sha()?);
        self.store.write_plan(&plan)?;
        self.store
            .write_plan_markdown(&render::plan_markdown(&plan)?)?;

        let run = Run {
            schema: crate::plan::SCHEMA.to_string(),
            run_id: goal_id.clone(),
            goal_id,
            revision: 1,
            state: RunState::Inspecting,
            accepted_units: Vec::new(),
            accepted_fingerprints: Default::default(),
            plan_digest: None,
            branch: String::new(),
            reviewed_head: None,
            seq: 0,
        };
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            "Hwahap is reading the repository before asking anything.".into(),
        ))
    }

    /// The states that need no agent session.
    fn advance(&self, run: Run, user_input: Option<&str>) -> Result<StepOutcome> {
        match run.state.clone() {
            RunState::Deciding => self.decide(run, user_input),
            RunState::AwaitingConfirmation { challenge } => {
                self.freeze(run, &challenge, user_input)
            }
            RunState::PlanReady => {
                if user_input.is_none() && self.require_plan()?.approved_plan.is_some() {
                    self.begin_frozen_build(run)
                } else {
                    self.adjust(run, user_input)
                }
            }
            RunState::AwaitingAdjustOrShip { .. } => self.adjust(run, user_input),
            RunState::PlanConflict { .. } => self.resolve_conflict(run, user_input),
            RunState::Shipped { .. } | RunState::Blocked { .. } => {
                let plan = self.store.read_plan()?;
                let message = self.describe(&run, plan.as_ref())?;
                Ok(self.report(&run, message))
            }
            other => Err(Error::Internal(format!(
                "state {} needs an agent session and cannot be advanced without one",
                other.name()
            ))),
        }
    }

    /// Runs the states that need native agents.
    /// The state dispatch that needs sessions. Split out so tests can drive it with a script.
    pub async fn drive(&self, run: Run, sessions: &dyn Sessions) -> Result<StepOutcome> {
        match run.state.clone() {
            RunState::Inspecting => self.inspect(run, sessions).await,
            RunState::Refining => self.refine(run, sessions).await,
            RunState::Proving => self.prove(run, sessions).await,
            RunState::Coding { .. } => self.code(run, sessions).await,
            RunState::FinalVerifying => self.finalize(run, sessions).await,
            RunState::PrReview { .. } => self.review_pr(run, sessions).await,
            other => Err(Error::Internal(format!(
                "state {} does not need an agent session",
                other.name()
            ))),
        }
    }

    async fn inspect(&self, mut run: Run, sessions: &dyn Sessions) -> Result<StepOutcome> {
        let mut plan = self.require_plan()?;

        // Facts are established once. An adjustment re-enters here to get its questions asked, and
        // re-reading the repository would only pile duplicate facts onto the plan.
        if plan.facts.is_empty() {
            let facts = self
                .ask(
                    sessions,
                    Role::FactFinder,
                    None,
                    prompts::fact_finder(&format!(
                        "Everything a planner needs to know about this repository before \
                         designing: {}",
                        plan.goal.statement
                    )),
                )
                .await?;
            let facts = proposal::FactsProposal::parse(&facts.final_message, &plan)?;
            plan.facts.extend(facts.facts);
            self.verify_planning_source(&plan)?;
            // A later native spawn refusal must not discard already verified repository facts.
            self.save_plan(&plan)?;
        }

        let decisions = self
            .ask(
                sessions,
                Role::Recommender,
                None,
                prompts::decisions(&plan, &[]),
            )
            .await?;
        self.apply_decisions(&mut plan, &decisions.final_message)?;
        Self::capture_frontier(&mut plan)?;

        self.save_plan(&plan)?;
        run.state = RunState::Deciding;
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(&run, self.describe(&run, Some(&plan))?))
    }

    fn decide(&self, mut run: Run, user_input: Option<&str>) -> Result<StepOutcome> {
        let mut plan = self.require_plan()?;

        if let Some(input) = user_input {
            let parsed = parse_message(input);
            if let Some(rejection) = parsed.rejection_message() {
                return Ok(self.report(&run, rejection));
            }
            self.record_answers(&mut plan, &parsed.directives)?;
            self.save_plan(&plan)?;
        }

        let frontier = frontier::derive(&plan)?;
        if plan.interactive
            && user_input.is_some()
            && crate::dialogue::QuestionBatch::derive(&plan)?.is_none()
        {
            run.state = RunState::Refining;
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(
                &run,
                "Answers recorded. Checking their implications before the next round.".into(),
            ));
        }
        if !frontier.ready.is_empty() {
            let message = render::frontier_markdown(&plan, &frontier.ready)?;
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(&run, message));
        }
        if !frontier.blocked.is_empty() {
            run.state = RunState::Blocked {
                reason: format!(
                    "these decisions can never be answered because their prerequisites cannot be: \
                     {}",
                    frontier
                        .blocked
                        .iter()
                        .map(|b| format!("{} waits on {:?}", b.id, b.waiting_on))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            };
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
        }

        run.state = RunState::Proving;
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            "Every question is answered. Hwahap is now proving the plan before asking you to \
             confirm it."
                .into(),
        ))
    }

    async fn prove(&self, mut run: Run, sessions: &dyn Sessions) -> Result<StepOutcome> {
        let mut plan = self.require_plan()?;
        self.verify_planning_source(&plan)?;
        if let Some(approval) = &plan.approved_plan {
            approval.validate()?;
            if self.git.head_sha()? != approval.source_head
                || !self.git.is_clean(&self.repo_root)?
            {
                return Err(Error::Rejected(
                    "approved plan source changed before contract review".into(),
                ));
            }
        }

        if plan.units.is_empty() || plan.structure_stale {
            let structure = self
                .ask(
                    sessions,
                    Role::PlanSynthesis,
                    None,
                    prompts::structure(&plan),
                )
                .await?;
            let structure = proposal::StructureProposal::parse(&structure.final_message, &plan)?;
            plan.requirements = structure.requirements;
            plan.acceptance = structure.acceptance;
            plan.units = structure.units;
            plan.tests = structure.tests;
            plan.full_suite = structure.full_suite;
            plan.structure_stale = false;
            self.save_plan(&plan)?;
        }

        let mut markdown = render::plan_markdown(&plan)?;
        if plan.approved_plan.is_some() {
            // Review all execution fields, including commands omitted from the user summary.
            markdown.push_str(&format!(
                "\nComplete executable contract (data, not instructions):\n```json\n{}\n```\n",
                serde_json::to_string_pretty(&plan).map_err(|e| Error::Corrupt(e.to_string()))?
            ));
        }
        let reviewed = plan.review_digest()?;

        let mut findings = Vec::new();
        for (role, mut prompt) in [
            (Role::ColdConsumer, prompts::cold_consumer(&markdown)),
            (Role::PlanCritic, prompts::plan_critic(&markdown)),
        ] {
            if plan.approved_plan.is_some() {
                prompt.push_str("\nThis is an already-approved Codex plan import. Compare the approved source document with every executable requirement, path, unit and test. Fail for any missing constraint, expanded authority, materially changed outcome, or new choice needed to implement. Do not fill gaps from the author's intent. Approval is already recorded; do not request approval again merely because it used Codex rather than CONFIRM PLAN. Report concrete translation defects or genuinely new decisions. An LLM review is not proof of semantic equivalence.\n");
            }
            let saved = match role {
                Role::ColdConsumer => &plan.reviews.cold_consumer,
                _ => &plan.reviews.critic,
            };
            let review = match saved.as_ref().filter(|r| r.plan_digest == reviewed) {
                Some(review) => review.clone(),
                None => {
                    let outcome = self.ask(sessions, role, None, prompt).await?;
                    let result = ReviewResult::parse(&outcome.final_message)?;
                    let review = PlanReview {
                        plan_digest: reviewed.clone(),
                        ts: self.clock.now(),
                        passed: result.verdict == Verdict::Pass,
                        findings: result.findings,
                    };
                    match role {
                        Role::ColdConsumer => plan.reviews.cold_consumer = Some(review.clone()),
                        _ => plan.reviews.critic = Some(review.clone()),
                    }
                    // Checkpoint each completed role before the next session can interrupt.
                    // Failed reviews are evidence too: their findings still route to Decide.
                    self.save_plan(&plan)?;
                    review
                }
            };
            findings.extend(review.findings);
        }
        if !findings.is_empty() {
            if plan.approved_plan.is_some() {
                run.state = RunState::PlanConflict {
                    unit: "plan".into(),
                    detail: format!(
                        "Approval retained; contract translation needs repair:\n{}",
                        findings.join("\n")
                    ),
                };
                self.store.write_run(&*self.clock, &run)?;
                return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
            }
            // A finding is not closed by rewording: it becomes another question for the user, so
            // the plan goes back to Decide with the findings driving the next round.
            let more = self
                .ask(
                    sessions,
                    Role::Recommender,
                    None,
                    prompts::decisions(&plan, &findings),
                )
                .await?;
            let before = plan.decisions.len();
            self.apply_decisions(&mut plan, &more.final_message)?;
            Self::capture_frontier(&mut plan)?;
            let listed = findings
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n");

            if plan.decisions.len() == before {
                // The findings reduced to nothing the user can answer. Going back to Deciding would
                // find an empty frontier, return here, and burn three sessions again on every pass;
                // the run has to stop and say what it could not turn into a question.
                self.save_plan(&plan)?;
                run.state = RunState::Blocked {
                    reason: format!(
                        "the plan review raised {} finding(s) that could not be turned into a \
                         decision for you to answer:\n{listed}",
                        findings.len()
                    ),
                };
                self.store.write_run(&*self.clock, &run)?;
                return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
            }

            self.save_plan(&plan)?;
            run.state = RunState::Deciding;
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(
                &run,
                format!(
                    "The plan review raised {} finding(s), so there are more decisions to make:\n\n{listed}",
                    findings.len()
                ),
            ));
        }

        let blockers = if plan.approved_plan.is_some() {
            validate::approved_plan_blockers(&plan)?
        } else {
            validate::freeze_blockers(&plan)?
        };
        if !blockers.is_empty() {
            Self::capture_frontier(&mut plan)?;
            self.save_plan(&plan)?;
            run.state = RunState::Deciding;
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(
                &run,
                format!(
                    "The plan is not yet complete:\n\n{}",
                    blockers
                        .iter()
                        .map(|b| format!("- {} — {}", b.code, b.detail))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            ));
        }

        if let Some(approval) = &plan.approved_plan {
            plan.frozen = Some(Frozen {
                digest: plan.digest()?,
                confirmed_at: self.clock.now(),
                answer_text: approval.implementation_request.clone(),
            });
            run.plan_digest = Some(plan.digest()?);
            run.state = RunState::PlanReady;
            self.save_plan(&plan)?;
            self.store.write_run(&*self.clock, &run)?;
            return self.begin_frozen_build(run);
        }
        let challenge = plan.challenge()?;
        self.store
            .write_plan_markdown(&render::plan_markdown(&plan)?)?;
        run.state = RunState::AwaitingConfirmation {
            challenge: challenge.clone(),
        };
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            format!(
                "Read `.hwahap/plan.md`, including decisions and expected behavior.\n\n{} To confirm this exact plan, type:\n\nCONFIRM PLAN {challenge}",
                if plan.plan_only { "PLAN-only saves the confirmed plan and stops before BUILD." }
                else { "Confirmation starts BUILD, tests, independent review and draft publication." }
            ),
        ))
    }

    fn freeze(
        &self,
        mut run: Run,
        challenge: &str,
        user_input: Option<&str>,
    ) -> Result<StepOutcome> {
        let mut plan = self.require_plan()?;
        let Some(input) = user_input else {
            return Ok(self.report(
                &run,
                format!("Waiting for the user to type exactly:\n\nCONFIRM PLAN {challenge}"),
            ));
        };

        let parsed = parse_message(input);

        if let Some(rejection) = parsed.rejection_message() {
            if plan.interactive && parsed.directives.is_empty() && parsed.conflicts.is_empty() {
                return self.adjust(run, Some(input));
            }
            return Ok(self.report(&run, rejection));
        }

        // Answers are read before the confirmation, and win over it. A message holding both is a
        // user who changed their mind and confirmed in one breath: the challenge they typed
        // describes the plan as it was a moment ago, so honouring it would freeze the very content
        // they just replaced. Nothing here may be silently dropped either way.
        if parsed
            .directives
            .iter()
            .any(|d| matches!(d, Directive::Decision { .. } | Directive::Surface { .. }))
        {
            let also_confirmed = parsed
                .directives
                .iter()
                .any(|d| matches!(d, Directive::ConfirmPlan { .. }));
            self.record_answers(&mut plan, &parsed.directives)?;
            self.save_plan(&plan)?;
            run.state = if plan.interactive {
                RunState::Refining
            } else {
                RunState::Deciding
            };
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(
                &run,
                if also_confirmed {
                    "That message both changed an answer and confirmed the plan. The answer was \
                     recorded and the confirmation was not: it named the plan as it stood before \
                     the change. Hwahap is re-deriving the remaining questions and will print a \
                     new challenge."
                        .to_string()
                } else {
                    "That changed an answer, so the plan is no longer the one you were about to \
                     confirm. Hwahap is re-deriving the remaining questions."
                        .to_string()
                },
            ));
        }

        let typed = parsed.directives.iter().find_map(|d| match d {
            Directive::ConfirmPlan { challenge } => Some(challenge.clone()),
            _ => None,
        });
        let Some(typed) = typed else {
            if plan.interactive && !input.trim().is_empty() {
                return self.adjust(run, Some(input));
            }
            return Ok(self.report(
                &run,
                format!("To freeze the plan type exactly:\n\nCONFIRM PLAN {challenge}"),
            ));
        };

        if typed != challenge {
            return Ok(self.report(
                &run,
                format!(
                    "The confirmation names {typed}, but this plan's challenge is {challenge}. The \
                     plan changed after that challenge was shown; re-read `.hwahap/plan.md` and \
                     confirm the current one."
                ),
            ));
        }

        self.verify_planning_source(&plan)?;
        let digest = plan.digest()?;
        if plan.structure_stale
            || digest.challenge() != challenge
            || !validate::freeze_blockers(&plan)?.is_empty()
        {
            plan.frozen = None;
            plan.reviews = crate::plan::PlanReviews::default();
            self.save_plan(&plan)?;
            run.state = RunState::Proving;
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(&run, "The plan changed after its preview. Hwahap must review the current plan and issue a fresh confirmation challenge.".into()));
        }
        plan.frozen = Some(Frozen {
            digest: digest.clone(),
            confirmed_at: self.clock.now(),
            answer_text: input.trim().to_string(),
        });
        self.store.write_plan(&plan)?;
        self.store
            .write_plan_markdown(&render::plan_markdown(&plan)?)?;

        run.plan_digest = Some(digest);
        run.state = RunState::PlanReady;
        self.store.write_run(&*self.clock, &run)?;
        if plan.plan_only {
            return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
        }
        self.begin_frozen_build(run)
    }

    /// Resume precisely the previously confirmed PLAN, never construct a direct BUILD contract.
    pub fn build_confirmed(&self, digest: &str) -> Result<StepOutcome> {
        let run = self
            .store
            .recover()?
            .ok_or_else(|| Error::Rejected("no confirmed plan".into()))?;
        if run.state != RunState::PlanReady
            || run.plan_digest.as_ref().map(ToString::to_string).as_deref() != Some(digest)
        {
            return Err(Error::Rejected(
                "BUILD requires the current plan_ready contract digest".into(),
            ));
        }
        self.begin_frozen_build(run)
    }

    fn begin_frozen_build(&self, mut run: Run) -> Result<StepOutcome> {
        let plan = self.require_frozen_plan(&run)?;
        if run.branch.is_empty()
            && plan
                .source_head
                .as_ref()
                .is_some_and(|head| self.git.head_sha().as_ref().ok() != Some(head))
        {
            return Err(Error::Rejected(
                "source changed after PLAN; reopen planning before BUILD".into(),
            ));
        }
        if run.branch.is_empty() && !self.git.is_clean(&self.repo_root)? {
            return Err(Error::Rejected(
                "source has uncommitted changes after PLAN".into(),
            ));
        }
        // Checked here, at the boundary of the autonomous phase, rather than at delivery time.
        // Discovering a missing GitHub login after an hour of unattended coding wastes the run.
        self.forge.require_auth(&self.repo_root)?;

        let branch = self.prepare_plan_worktree(&run, &plan)?;

        // Anything accepted against a plan detail this revision changed is no longer accepted.
        // Keeping it would let an adjustment ship the branch unchanged; dropping everything would
        // rebuild work the user never questioned. The fingerprint is what tells the two apart.
        let invalidated = Self::invalidated_units(&plan, &run)?;
        run.accepted_units.retain(|id| !invalidated.contains(id));
        for id in &invalidated {
            run.accepted_fingerprints.remove(id);
        }

        let order = validate::unit_order(&plan)?;
        let first = order
            .first()
            .cloned()
            .ok_or_else(|| Error::Rejected("the frozen plan has no units to build".into()))?;

        run.branch = branch;
        run.state = RunState::Coding {
            unit: first,
            attempt: 1,
        };
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            format!(
                "Plan frozen. Hwahap is building {} of {} unit(s) on `{}` and will report when the \
                 draft pull request is ready.",
                order.len() - run.accepted_units.len(),
                order.len(),
                run.branch
            ),
        ))
    }

    /// Accepted units whose plan detail has moved since they were accepted.
    fn invalidated_units(plan: &Plan, run: &Run) -> Result<Vec<String>> {
        let mut invalidated = Vec::new();
        for id in &run.accepted_units {
            let recorded = run.accepted_fingerprints.get(id).ok_or_else(|| {
                Error::Corrupt(format!("accepted unit {id} has no recorded fingerprint"))
            })?;
            let still_valid = plan.unit(id).is_some() && *recorded == plan.unit_fingerprint(id)?;
            if !still_valid {
                invalidated.push(id.clone());
            }
        }
        // A dependent's own contract can be unchanged while its input is no longer the input
        // it was tested against. Propagate invalidation in dependency order.
        for id in validate::unit_order(plan)? {
            let unit = plan.unit(&id).expect("validated unit order");
            if (!run.accepted_units.contains(&id)
                || unit.depends_on.iter().any(|dep| invalidated.contains(dep)))
                && !invalidated.contains(&id)
            {
                invalidated.push(id);
            }
        }
        Ok(invalidated)
    }

    // ------------------------------------------------------------------ coding

    async fn code(&self, mut run: Run, sessions: &dyn Sessions) -> Result<StepOutcome> {
        let plan = self.require_frozen_plan(&run)?;
        let worktree = self.store.worktree_path();
        let order = validate::unit_order(&plan)?;

        for (position, unit_id) in order.iter().enumerate() {
            if run.accepted_units.contains(unit_id) {
                continue;
            }
            let unit = plan
                .unit(unit_id)
                .ok_or_else(|| Error::Internal(format!("unit {unit_id} vanished from the plan")))?;
            match self.build_unit(&plan, unit, &worktree, sessions).await? {
                UnitOutcome::Accepted => {
                    run.accepted_fingerprints
                        .insert(unit_id.clone(), plan.unit_fingerprint(unit_id)?);
                    run.accepted_units.push(unit_id.clone());
                    // Name what is being built next, not what was just finished: `status` reports
                    // this state and a user reading it should not be told a finished unit is
                    // underway.
                    run.state = match order.get(position + 1) {
                        Some(next) => RunState::Coding {
                            unit: next.clone(),
                            attempt: 1,
                        },
                        None => RunState::FinalVerifying,
                    };
                    self.store.write_run(&*self.clock, &run)?;
                }
                UnitOutcome::Conflict(detail) => {
                    run.state = RunState::PlanConflict {
                        unit: unit_id.clone(),
                        detail: detail.clone(),
                    };
                    self.store.write_run(&*self.clock, &run)?;
                    return Ok(self.report(
                        &run,
                        format!(
                            "The frozen plan cannot hold:\n\n{detail}\n\nNo code was written for \
                             that unit. Answer the affected decisions again to continue."
                        ),
                    ));
                }
                UnitOutcome::Blocked(reason) => {
                    run.state = RunState::Blocked {
                        reason: reason.clone(),
                    };
                    self.store.write_run(&*self.clock, &run)?;
                    return Ok(self.report(&run, reason));
                }
            }
        }

        run.state = RunState::FinalVerifying;
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            "Every unit is accepted. Hwahap is running the full suite and the final review.".into(),
        ))
    }

    async fn build_unit(
        &self,
        plan: &Plan,
        unit: &Unit,
        worktree: &Path,
        sessions: &dyn Sessions,
    ) -> Result<UnitOutcome> {
        let checkpoint = self.git.run_in(worktree, &["rev-parse", "HEAD"])?;
        let adjustment_findings = self.build_adjustment_findings(plan, unit)?;
        let mut findings = adjustment_findings.clone();

        for attempt in 1..=MAX_ATTEMPTS {
            self.git.reset_hard(worktree, &checkpoint)?;
            let role = if attempt == 1 {
                Role::Implementer
            } else {
                Role::Rework
            };
            let outcome = self
                .ask(
                    sessions,
                    role,
                    Some(unit.id.clone()),
                    prompts::implementer(plan, unit, &findings),
                )
                .await?;
            self.store.write_artifact(
                &format!("{}-attempt-{attempt}.md", unit.id),
                &outcome.transcript,
            )?;

            // A malformed result is a rejection like any other, and it must reach the repair
            // below rather than jumping past it — an attempt that never parses is exactly the
            // repeated failure the repair attempt needs to explain.
            let parsed = WorkerResult::parse(&outcome.final_message);
            let mut rejected: Option<Vec<String>> = None;
            let result = match parsed {
                Ok(result) => Some(result),
                Err(e) => {
                    rejected = Some(vec![e.to_string()]);
                    None
                }
            };

            if let Some(result) = result {
                match result.status {
                    WorkerStatus::PlanConflict => {
                        let changed = self.git.changed_paths(worktree)?;
                        if !changed.is_empty() {
                            // A conflict that also wrote code is not a conflict report; it is an
                            // unreviewed change, and it goes back the way any other rejection does.
                            rejected = Some(vec![format!(
                            "you reported a plan conflict but changed {changed:?}; a plan conflict \
                             must leave the working tree untouched"
                        )]);
                        } else {
                            return Ok(UnitOutcome::Conflict(
                                result.conflict.unwrap_or(result.summary),
                            ));
                        }
                    }
                    WorkerStatus::Failed => {
                        rejected = Some(vec![format!(
                            "the previous attempt reported failure: {}",
                            result.summary
                        )]);
                    }
                    WorkerStatus::Completed => {
                        match self.verify_unit(plan, unit, worktree, sessions).await? {
                            Ok(()) => {
                                if unit.probe {
                                    self.git.reset_hard(worktree, &checkpoint)?;
                                    return Ok(UnitOutcome::Accepted);
                                }
                                let sha = self.git.commit_all(
                                    worktree,
                                    &format!(
                                        "hwahap({}): {}\n\nplan-digest: {}\nunit: {}",
                                        unit.id,
                                        unit.title,
                                        plan.digest()?,
                                        unit.id
                                    ),
                                )?;
                                let _ = sha;
                                return Ok(UnitOutcome::Accepted);
                            }
                            Err(reasons) => rejected = Some(reasons),
                        }
                    }
                }
            }

            findings = adjustment_findings
                .iter()
                .cloned()
                .chain(rejected.unwrap_or_default())
                .collect();
        }

        self.git.reset_hard(worktree, &checkpoint)?;
        Ok(UnitOutcome::Blocked(format!(
            "{} failed {MAX_ATTEMPTS} attempts:\n{}",
            unit.id,
            findings.join("\n")
        )))
    }

    /// Host-side verification. Nothing here believes the agent.
    #[allow(clippy::type_complexity)]
    async fn verify_unit(
        &self,
        plan: &Plan,
        unit: &Unit,
        worktree: &Path,
        sessions: &dyn Sessions,
    ) -> Result<std::result::Result<(), Vec<String>>> {
        let changed = self.git.changed_paths(worktree)?;
        if changed.is_empty() {
            return Ok(Err(vec![
                "the attempt reported completion but changed nothing".to_string(),
            ]));
        }
        let outside = paths_outside(&unit.paths, &changed);
        if !outside.is_empty() {
            return Ok(Err(vec![format!(
                "these paths are outside {}'s declared scope {:?} and were discarded: {outside:?}",
                unit.id, unit.paths
            )]));
        }

        for test in plan.tests_for(&unit.id) {
            let output = self.run_command(worktree, &test.command).await?;
            if !output.success {
                return Ok(Err(vec![format!(
                    "`{}` failed:\n{}",
                    test.command,
                    tail(&output.combined, 4_000)
                )]));
            }
        }
        let outside = paths_outside(&unit.paths, &self.git.changed_paths(worktree)?);
        if !outside.is_empty() {
            return Ok(Err(vec![format!(
                "test commands changed paths outside the unit: {outside:?}"
            )]));
        }

        // The reviewer is read-only. That is enforced by comparing the tree before and after,
        // because a permission callback that is never invoked proves nothing.
        // Stage first: a unit's new files are untracked, and `git diff HEAD` does not show
        // untracked content. A reviewer handed an empty diff would pass everything.
        //
        // The before/after guard is a tree object, not a list of path names. A reviewer that
        // rewrites a file the implementer already changed leaves the set of changed *paths*
        // identical, so a name comparison keeps its verdict and commits the reviewer's text as the
        // unit's work. A tree id covers content, mode and deletion in one value.
        self.git.run_in(worktree, &["add", "-A"])?;
        let diff = self.git.run_in(worktree, &["diff", "--cached", "HEAD"])?;
        let review = match self
            .ask(
                sessions,
                Role::UnitReviewer,
                Some(unit.id.clone()),
                prompts::unit_reviewer(plan, unit, &tail(&diff, 200_000)),
            )
            .await
        {
            Err(Error::BoundaryViolation(detail)) => return Ok(Err(vec![detail])),
            result => result?,
        };
        let review = ReviewResult::parse(&review.final_message)?;
        if review.verdict == Verdict::Fail {
            return Ok(Err(review.findings));
        }
        Ok(Ok(()))
    }

    async fn finalize(&self, mut run: Run, _sessions: &dyn Sessions) -> Result<StepOutcome> {
        let plan = self.require_frozen_plan(&run)?;
        let worktree = self.store.worktree_path();

        let suite_tree = self.git.fingerprint(&worktree)?;
        if !self.git.is_clean(&worktree)? {
            return Err(Error::BoundaryViolation(
                "accepted branch is not clean before the full suite".into(),
            ));
        }
        let suite = self.run_command(&worktree, &plan.full_suite).await?;
        if self.git.fingerprint(&worktree)? != suite_tree {
            return Err(Error::BoundaryViolation(
                "full suite changed the accepted branch or files".into(),
            ));
        }
        if !suite.success {
            run.state = RunState::Blocked {
                reason: format!(
                    "every unit was accepted but the full suite `{}` failed:\n{}",
                    plan.full_suite,
                    tail(&suite.combined, 4_000)
                ),
            };
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
        }

        let head = self.git.run_in(&worktree, &["rev-parse", "HEAD"])?;
        let previous = crate::pr_review::ReviewProgress::load(&self.store)?;
        let existing = self
            .forge
            .existing_draft(&worktree, &plan.base_branch, &run.branch)?;
        if previous
            .as_ref()
            .is_some_and(|p| existing.as_deref() != Some(p.binding.pr_url.as_str()))
        {
            return Err(Error::BoundaryViolation(
                "recorded draft was closed, replaced or changed externally".into(),
            ));
        }
        self.git.push(&worktree, "origin", &run.branch)?;
        let report = format!(
            "{}\n## Cost evidence\n\n```json\n{}\n```\n",
            self.report_markdown(&plan, &run),
            crate::cost::persist(&self.store)?
        );
        self.store.write_report(&report)?;
        let pr = if let Some(previous) = &previous {
            self.forge.update_draft(
                &worktree,
                &plan.base_branch,
                &run.branch,
                &previous.binding.pr_url,
                &plan.goal.statement,
                &report,
            )?
        } else {
            self.forge.create_draft(
                &worktree,
                &plan.base_branch,
                &run.branch,
                &plan.goal.statement,
                &report,
            )?
        };

        let mut progress = crate::pr_review::ReviewProgress {
            binding: crate::pr_review::ReviewBinding {
                pr_url: pr.url.clone(),
                head: head.clone(),
                contract_digest: plan.digest()?,
            },
            round: 1,
            stage: crate::pr_review::ReviewStage::Attack,
            repairs: 0,
        };
        if let Some(previous) = previous {
            progress.repairs = previous.repairs;
            if previous.binding == progress.binding
                && previous.stage != crate::pr_review::ReviewStage::Complete
            {
                progress = previous;
            } else {
                progress.round = previous
                    .round
                    .checked_add(1)
                    .ok_or_else(|| Error::ExecutionLimit("review round overflow".into()))?;
            }
        }
        progress.save(&self.store)?;
        run.reviewed_head = None;
        run.state = RunState::PrReview {
            pr_url: pr.url.clone(),
        };
        self.store.write_run(&*self.clock, &run)?;
        if pr.head_sha != head || self.git.run_in(&worktree, &["rev-parse", "HEAD"])? != head {
            return Err(Error::BoundaryViolation(
                "published PR head differs from tested commit".into(),
            ));
        }
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(&run, self.describe(&run, Some(&plan))?))
    }

    fn adjust(&self, mut run: Run, user_input: Option<&str>) -> Result<StepOutcome> {
        let plan = self.require_plan()?;
        let Some(input) = user_input else {
            return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
        };
        if input.trim().is_empty() {
            return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
        }

        // An adjustment re-opens PLAN at the next revision. The feedback is *written into the
        // plan*, not merely echoed: the next Recommender round reads it from there. Echoing it and
        // dropping it is how a cycle re-freezes, finds every unit already accepted, rebuilds
        // nothing, and offers the user the identical pull request to ship.
        let mut plan = plan;
        let parsed = parse_message(input);
        // Unrecognized lines are not errors here: at this gate prose *is* the message, and a user
        // who answers a decision and explains why in the next breath has said two useful things.
        // Only answering the same thing twice is genuinely ambiguous.
        if !parsed.conflicts.is_empty() {
            return Ok(self.report(
                &run,
                format!(
                    "These were answered more than once in the same message, so Hwahap recorded \
                     none of them: {}. Send one answer each.",
                    parsed.conflicts.join(", ")
                ),
            ));
        }
        self.refresh_plan_source(&mut plan, &run)?;
        self.record_answers(&mut plan, &parsed.directives)?;

        let prose = free_text(input, &parsed.directives);
        plan.revision += 1;
        // The original authorization remains in build-request.json. This revision is PLAN.
        plan.execution_authorization = None;
        plan.approved_plan = None;
        if !prose.is_empty() {
            plan.adjustments.push(crate::plan::Adjustment {
                revision: plan.revision,
                text: prose.clone(),
                ts: self.clock.now(),
            });
        }
        plan.frozen = None;
        plan.reviews = crate::plan::PlanReviews::default();
        plan.structure_stale = true;
        self.save_plan(&plan)?;

        run.revision = plan.revision;
        run.state = RunState::Inspecting;
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            format!(
                "Hwahap recorded this as revision {} and will plan against it:\n\n{}\n\nIt will \
                 ask about anything that changes a decision you already made, and rebuild only the \
                 units the change actually affects.",
                plan.revision,
                if prose.is_empty() {
                    "(the answers you gave)".to_string()
                } else {
                    prose
                }
            ),
        ))
    }

    /// Lets the user answer their way out of a plan conflict.
    ///
    /// Without this the state is a trap: the worker reported that the frozen plan cannot hold, the
    /// message says "answer the affected decisions again", and nothing in the engine reads an
    /// answer in this state or lets a new request replace the run. The only exit was deleting
    /// `.hwahap` by hand and losing the plan and the journal with it.
    fn resolve_conflict(&self, mut run: Run, user_input: Option<&str>) -> Result<StepOutcome> {
        let mut plan = self.require_plan()?;
        let Some(input) = user_input.map(str::trim).filter(|i| !i.is_empty()) else {
            return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
        };

        if plan.approved_plan.is_some() && plan.frozen.is_none() {
            return Err(Error::Rejected("Approval is retained. Repair the translation with approved_plan and the exact current draft digest; do not restart the interview or ask for the same approval.".into()));
        }
        let parsed = parse_message(input);
        if !parsed.conflicts.is_empty() {
            return Ok(self.report(
                &run,
                "Conflicting answers were not recorded. Send one answer per decision.".into(),
            ));
        }
        self.refresh_plan_source(&mut plan, &run)?;
        self.record_answers(&mut plan, &parsed.directives)?;

        let prose = free_text(input, &parsed.directives);
        plan.revision += 1;
        if !prose.is_empty() {
            plan.adjustments.push(crate::plan::Adjustment {
                revision: plan.revision,
                text: prose,
                ts: self.clock.now(),
            });
        }
        plan.frozen = None;
        plan.reviews = crate::plan::PlanReviews::default();
        plan.structure_stale = true;
        self.save_plan(&plan)?;

        run.revision = plan.revision;
        run.state = RunState::Inspecting;
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            format!(
                "Hwahap reopened the plan as revision {}. Accepted units are kept; the ones the \
                 conflict changes will be built again.",
                plan.revision
            ),
        ))
    }

    // ----------------------------------------------------------------- helpers

    async fn ask(
        &self,
        sessions: &dyn Sessions,
        role: Role,
        unit: Option<String>,
        prompt: String,
    ) -> Result<SessionOutcome> {
        let cwd = match crate::session::access_for(role) {
            // Writers see only the run worktree. Readers see the repository, because a reviewer
            // that cannot read the code it is reviewing is not a reviewer.
            crate::session::Access::WorkspaceWrite => self.store.worktree_path(),
            crate::session::Access::ReadOnly => {
                let worktree = self.store.worktree_path();
                if worktree.exists() {
                    worktree
                } else {
                    self.repo_root.clone()
                }
            }
        };
        let spec = SessionSpec {
            cwd,
            role,
            unit,
            prompt,
        };
        let head = self.git.run_in(&spec.cwd, &["rev-parse", "HEAD"])?;
        let branch = self
            .git
            .run_in(&spec.cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let before = if crate::session::access_for(role) == crate::session::Access::ReadOnly {
            Some(self.git.fingerprint(&spec.cwd)?)
        } else {
            None
        };
        let profiles = Config::for_run(&self.store)?.profiles;
        let sequence = self
            .store
            .append_event(
                &*self.clock,
                "session_requested",
                serde_json::json!({
                    "role": role.as_str(), "unit": spec.unit,
                    "model_requested": profiles.for_role(role).model,
                    "prompt_digest": Digest::of_bytes(spec.prompt.as_bytes()),
                }),
            )?
            .seq;
        let outcome = sessions.run(&spec).await;
        if self.git.run_in(&spec.cwd, &["rev-parse", "HEAD"])? != head
            || self
                .git
                .run_in(&spec.cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?
                != branch
        {
            return Err(Error::BoundaryViolation(format!(
                "{} changed HEAD or branch; the host owns checkpoints",
                role.as_str()
            )));
        }
        if let Some(before) = before {
            if self.git.fingerprint(&spec.cwd)? != before {
                return Err(Error::BoundaryViolation(format!("the read-only {} session changed the working tree or index; its result was discarded", role.as_str())));
            }
        }
        let outcome = outcome?;
        outcome.receipt.verify_for(&spec, &profiles)?;
        self.store.write_artifact(
            &format!("receipt-{sequence:04}-{}.json", role.as_str()),
            &serde_json::to_string_pretty(&outcome.receipt)
                .map_err(|e| Error::Internal(e.to_string()))?,
        )?;
        Ok(outcome)
    }

    fn apply_decisions(&self, plan: &mut Plan, final_message: &str) -> Result<()> {
        let (decisions, not_applicable) = proposal::DecisionsProposal::parse(final_message, plan)?;
        if !decisions.is_empty() || !not_applicable.is_empty() {
            plan.structure_stale = true;
        }
        for decision in decisions {
            if let Some(existing) = plan.decision_mut(&decision.id) {
                *existing = decision.clone();
                plan.open_items
                    .retain(|o| o.id != format!("CLARIFY-{}", decision.id));
            } else {
                plan.decisions.push(decision);
            }
        }
        for na in not_applicable {
            if matches!(
                plan.surfaces.get(&na.surface),
                Some(SurfaceStatus::NotApplicable { .. })
            ) {
                continue;
            }
            // Keep a pending proposal and its user clarification intact across refinement.
            if plan
                .open_items
                .iter()
                .any(|o| o.id == format!("NA-{}", na.surface))
            {
                continue;
            }
            // Recorded as a proposal only. The surface stays applicable until the user types
            // `S<n>=NA`, which is what [`Engine::record_answers`] acts on.
            plan.open_items.push(crate::plan::OpenItem {
                id: format!("NA-{}", na.surface),
                decision_id: String::new(),
                detail: format!(
                    "{} is proposed as not applicable ({}); confirm with {}=NA",
                    na.surface, na.reason, na.surface
                ),
            });
        }
        Ok(())
    }

    fn record_answers(&self, plan: &mut Plan, directives: &[Directive]) -> Result<()> {
        let now = self.clock.now();
        let before = plan.decisions.clone();
        for directive in directives {
            match directive {
                Directive::Decision { id, selection } => {
                    let Some(decision) = plan.decision_mut(id) else {
                        return Err(Error::Rejected(format!(
                            "{id} is not a decision in this plan"
                        )));
                    };
                    if let Selection::Alternative { id: alt } = selection {
                        if !decision.alternatives.iter().any(|a| &a.id == alt) {
                            return Err(Error::Rejected(format!("{id} has no alternative {alt}")));
                        }
                    }
                    // "Take the recommendation" is only an answer where there is one to take.
                    // Recorded blindly it counts as answered, resolves to nothing, and freezes a
                    // plan whose unit briefs never mention the decision at all.
                    if matches!(selection, Selection::Recommendation)
                        && decision.recommendation.recommended_alternative().is_none()
                    {
                        return Err(Error::Rejected(format!(
                            "{id} has no recommendation to take, so {id}=REC answers nothing. \
                             Choose one of its alternatives, or answer {id}=OTHER: <value>."
                        )));
                    }
                    let identity = decision.identity_digest()?;
                    let recommendation = matches!(selection, Selection::Recommendation)
                        .then(|| decision.recommendation_digest())
                        .transpose()?;
                    decision.answer = Some(Answer {
                        text: format!("{id}={}", describe_selection(selection)),
                        selection: selection.clone(),
                        ts: now.clone(),
                        identity,
                        recommendation,
                    });
                    plan.open_items.retain(|o| o.id != format!("CLARIFY-{id}"));
                }
                Directive::Surface { id } => {
                    let Some(surface) = Surface::parse(id) else {
                        return Err(Error::Rejected(format!("{id} is not one of S1..S12")));
                    };
                    let reason = plan
                        .open_items
                        .iter()
                        .find(|item| item.id == format!("NA-{id}"))
                        .map(|item| item.detail.clone())
                        .unwrap_or_else(|| "the user marked this surface not applicable".into());
                    plan.surfaces.insert(
                        surface.id().to_string(),
                        SurfaceStatus::NotApplicable {
                            reason,
                            answer: Answer {
                                text: format!("{id}=NA"),
                                selection: Selection::NotApplicable,
                                ts: now.clone(),
                                identity: Digest::zero(),
                                recommendation: None,
                            },
                        },
                    );
                    plan.open_items.retain(|item| item.id != format!("NA-{id}"));
                }
                Directive::ConfirmPlan { .. } | Directive::Ship { .. } => continue,
            }
            plan.structure_stale = true;
        }
        let changed: Vec<_> = plan
            .decisions
            .iter()
            .filter(|d| {
                before.iter().find(|old| old.id == d.id).is_some_and(|old| {
                    old.answer
                        .as_ref()
                        .map(|a| (&a.selection, &a.identity, &a.recommendation))
                        != d.answer
                            .as_ref()
                            .map(|a| (&a.selection, &a.identity, &a.recommendation))
                })
            })
            .map(|d| d.id.clone())
            .collect();
        if !changed.is_empty() {
            Self::invalidate_dependents(plan, &changed);
            plan.frozen = None;
            plan.reviews = Default::default();
        }
        Ok(())
    }

    fn save_plan(&self, plan: &Plan) -> Result<()> {
        self.store.write_plan(plan)?;
        self.store
            .write_plan_markdown(&render::plan_markdown(plan)?)
    }

    fn require_frozen_plan(&self, run: &Run) -> Result<Plan> {
        let plan = self.require_plan()?;
        if plan.approved_plan.is_some() && !validate::approved_plan_blockers(&plan)?.is_empty() {
            return Err(Error::BoundaryViolation(
                "approved plan evidence or translation review is invalid".into(),
            ));
        }
        if !plan.is_frozen()? || plan.frozen.as_ref().map(|f| &f.digest) != run.plan_digest.as_ref()
        {
            return Err(Error::BoundaryViolation(
                "the plan content no longer matches the user's confirmation; reopen planning before execution".into(),
            ));
        }
        Ok(plan)
    }

    fn require_plan(&self) -> Result<Plan> {
        let plan = self
            .store
            .read_plan()?
            .ok_or_else(|| Error::Corrupt("the run has no plan.json".into()))?;
        plan.require_supported_schema()?;
        Ok(plan)
    }

    async fn run_command(&self, cwd: &Path, command: &str) -> Result<CommandOutput> {
        self.run_command_with_limit(cwd, command, 1024 * 1024).await
    }

    async fn run_command_with_limit(
        &self,
        cwd: &Path,
        command: &str,
        limit: usize,
    ) -> Result<CommandOutput> {
        // Run through a shell because the plan's commands are written the way a person writes
        // them, with pipes and flags. The command comes from a frozen plan the user confirmed.
        let mut builder = tokio::process::Command::new("sh");
        builder
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Kill the shell if this future is ever dropped — a cancelled step must not leave a
            // test suite running against the worktree it is about to reset.
            .kill_on_drop(true);
        // Its own process group, so a timeout can end everything the command started rather than
        // only the shell that started it. `sh -c 'sleep 300; ...'` leaves the sleep behind
        // otherwise, and that orphan goes on writing into the worktree Hwahap has already moved on
        // from — which makes the diff and the changed-path check describe a tree nobody built.
        #[cfg(unix)]
        builder.process_group(0);

        let mut child = builder
            .spawn()
            .map_err(|e| Error::command(command, e.to_string()))?;
        let pid = child.id();
        let group = CommandGroup(pid);
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let timeout = std::time::Duration::from_secs(self.config.test_timeout_secs);
        let collect = async {
            tokio::try_join!(
                read_command_stream(stdout, limit, "stdout"),
                read_command_stream(stderr, limit, "stderr"),
                async { child.wait().await.map_err(CommandReadError::Io) },
            )
        };
        let result = tokio::time::timeout(timeout, collect).await;
        if !matches!(&result, Ok(Ok(_))) {
            // End descendants before reaping the shell on timeout, overflow or a read failure.
            drop(group);
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        match result {
            Ok(Ok((stdout, stderr, status))) => Ok(CommandOutput {
                success: status.success(),
                combined: format!(
                    "{}{}",
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                ),
            }),
            Ok(Err(CommandReadError::Io(e))) => Err(Error::command(command, e.to_string())),
            Ok(Err(e @ CommandReadError::Limit { .. })) => Ok(CommandOutput {
                success: false,
                combined: e.to_string(),
            }),
            Err(_) => Ok(CommandOutput {
                success: false,
                combined: format!(
                    "the command did not finish within {}s and was killed",
                    self.config.test_timeout_secs
                ),
            }),
        }
    }

    fn report(&self, run: &Run, message: String) -> StepOutcome {
        StepOutcome {
            run_id: run.run_id.clone(),
            phase: run.state.phase().name().to_string(),
            state: run.state.name().to_string(),
            next: match self.store.read_plan().ok().flatten() {
                Some(plan)
                    if plan.approved_plan.is_some() && matches!(run.state, RunState::PlanReady) =>
                {
                    "continue".into()
                }
                Some(plan)
                    if plan.approved_plan.is_some()
                        && plan.frozen.is_none()
                        && matches!(run.state, RunState::PlanConflict { .. }) =>
                {
                    "repair_translation".into()
                }
                _ => run.state.next().name().to_string(),
            },
            message,
            plan_digest: run.plan_digest.as_ref().map(|d| d.to_string()),
            pr_url: run.state.pr_url().map(str::to_string).or_else(|| {
                crate::pr_review::ReviewProgress::load(&self.store)
                    .ok()
                    .flatten()
                    .filter(|p| Some(&p.binding.contract_digest) == run.plan_digest.as_ref())
                    .map(|p| p.binding.pr_url)
            }),
        }
    }

    fn describe(&self, run: &Run, plan: Option<&Plan>) -> Result<String> {
        Ok(match &run.state {
            RunState::Inspecting => "Hwahap is reading the repository.".into(),
            RunState::Refining => "Hwahap is checking new implications and unresolved interpretations from this round.".into(),
            RunState::Deciding => match plan {
                Some(plan) => {
                    let frontier = frontier::derive(plan)?;
                    render::frontier_markdown(plan, &frontier.ready)?
                }
                None => "Hwahap is deciding.".into(),
            },
            RunState::Proving => "Hwahap is proving the plan.".into(),
            RunState::AwaitingConfirmation { challenge } => format!(
                "Read `.hwahap/plan.md`. To freeze it, type exactly:\n\nCONFIRM PLAN {challenge}"
            ),
            RunState::PlanReady if plan.is_some_and(|p| p.approved_plan.is_some()) => "Your approved plan is saved. Continue BUILD without another approval.".into(),
            RunState::PlanReady => "PLAN is confirmed and saved in `.hwahap/plan.md`. Implementation has not started. Request BUILD with this plan's digest when ready.".into(),
            RunState::Coding { unit, attempt } => format!(
                "Building {unit} (attempt {attempt}). {} unit(s) accepted so far.",
                run.accepted_units.len()
            ),
            RunState::FinalVerifying => "Running the full suite before draft publication.".into(),
            RunState::PrReview { pr_url } => {
                format!("Draft {pr_url} requires independent Astra attack and defense review.")
            }
            RunState::AwaitingAdjustOrShip { pr_url, challenge } => match plan {
                Some(plan) => self.summary(plan, run, pr_url, challenge),
                None => format!("{pr_url} is ready. Type `SHIP {challenge}` to mark it ready."),
            },
            RunState::Shipped { pr_url } => format!("{pr_url} is ready for review."),
            RunState::Blocked { reason } => format!("Hwahap stopped: {reason}"),
            RunState::PlanConflict { unit, detail } => {
                let repair = if plan.is_some_and(|p| p.approved_plan.is_some() && p.frozen.is_none()) { "\nPreserve the approved source. Repair its executable translation using approved_plan with replaces_plan_digest for this draft. Ask the user only for a genuinely new decision." } else { "" };
                format!("{unit} hit a plan conflict: {detail}{repair}")
            }
        })
    }

    fn summary(&self, plan: &Plan, run: &Run, pr_url: &str, challenge: &str) -> String {
        format!(
            "Hwahap cycle completed\n\n\
             Units                 {}/{} accepted\n\
             Full suite            passed\n\
             Final review          passed\n\
             Draft PR              {pr_url}\n\n\
             Tell Hwahap what to change, or type exactly:\n\nSHIP {challenge}",
            run.accepted_units.len(),
            plan.units.iter().filter(|u| !u.probe).count(),
        )
    }

    fn report_markdown(&self, plan: &Plan, run: &Run) -> String {
        format!(
            "## Conclusion\n\n{}\n\n## Evidence\n\n- Plan digest: `{}`\n- Units accepted: {}\n\
             - Full suite: `{}`\n\n## Verification\n\nEvery unit's tests and the full suite were run \
             by Hwahap and judged by exit status. Changed paths were checked against each unit's \
             declared scope.\n\n## Limitations\n\nHwahap opened this pull request as a draft and did \
             not merge it.\n",
            plan.goal.statement,
            run.plan_digest
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_default(),
            run.accepted_units.join(", "),
            plan.full_suite,
        )
    }
}

/// Also runs when a native continuation is cancelled while awaiting command output.
struct CommandGroup(Option<u32>);
impl Drop for CommandGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill_process_group(pid);
        }
    }
}

/// Kills the process group led by `pid`, best effort.
///
/// Best effort because there is nothing useful to do when it fails: the group may already be gone,
/// which is the outcome we wanted anyway. What matters is that the attempt is made before Hwahap
/// resets the worktree out from under whatever was still running in it.
#[cfg(unix)]
fn kill_process_group(pid: u32) -> bool {
    // SAFETY: `killpg` takes a process group id and a signal and touches no memory. The group was
    // created by `process_group(0)` on the child Hwahap spawned, so the id is the child's own pid
    // and cannot name an unrelated group that Hwahap did not create.
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) == 0 }
}

/// Without process groups there is nothing portable to kill here.
///
/// Hwahap runs a plan's commands through `sh -c` and is unix-only for that reason; see
/// PLATFORM.md §4. `kill_on_drop` still ends the shell itself when the future is dropped.
#[cfg(not(unix))]
fn kill_process_group(_pid: u32) -> bool {
    false
}

enum Resolved {
    Started(StepOutcome),
    Advance(Run),
}

enum UnitOutcome {
    Accepted,
    Conflict(String),
    Blocked(String),
}

struct CommandOutput {
    success: bool,
    combined: String,
}

#[derive(Debug, thiserror::Error)]
enum CommandReadError {
    #[error("{stream} output exceeded {limit} bytes and the command was killed")]
    Limit { stream: &'static str, limit: usize },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

async fn read_command_stream(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
    stream: &'static str,
) -> std::result::Result<Vec<u8>, CommandReadError> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    // One extra byte distinguishes exact-limit success from overflow without draining a flood.
    reader
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(CommandReadError::Limit { stream, limit });
    }
    Ok(bytes)
}

fn describe_selection(selection: &Selection) -> String {
    match selection {
        Selection::Recommendation => "REC".into(),
        Selection::Alternative { id } => id.clone(),
        Selection::Other { value } => format!("OTHER: {value}"),
        Selection::Unknown => "UNKNOWN".into(),
        Selection::NotApplicable => "NA".into(),
    }
}

/// The lines of a message that were not directives, joined back together.
///
/// A user answering `C1=ALT2` and explaining why in the next breath said two things, and both are
/// worth keeping: the answer is recorded against the decision, and the explanation is what the next
/// planning round reads.
fn free_text(input: &str, directives: &[Directive]) -> String {
    if directives.is_empty() {
        return input.trim().to_string();
    }
    input
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && parse_message(line).directives.is_empty()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// A stable, filesystem-safe run identifier derived from the date and the request.
///
/// The slug is only the first few words, so it is readable but not unique: "add a dry-run flag to
/// apply" and "add a dry-run flag to delete" share it. The identifier also names the branch, and
/// two runs sharing a branch means the second silently builds on the first's commits — so a digest
/// of the whole request is appended. It is derived, not random: the same request on the same day is
/// still the same run.
fn goal_id(now: &str, request: &str) -> String {
    let date: String = now.chars().take(10).collect();
    let slug: String = request
        .chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    let fingerprint: String = Digest::of_bytes(request.trim().as_bytes())
        .as_str()
        .trim_start_matches("sha256:")
        .chars()
        .take(8)
        .collect();
    if slug.is_empty() {
        format!("{date}-{fingerprint}")
    } else {
        format!("{date}-{slug}-{fingerprint}")
    }
}

/// The last `limit` bytes of `text`, cut at a character boundary.
fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut start = text.len() - limit;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…\n{}", &text[start..])
}

impl Next {
    fn name(&self) -> &'static str {
        match self {
            Next::Continue => "continue",
            Next::AwaitUser => "await_user",
            Next::Completed => "completed",
            Next::Blocked => "blocked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_work_without_fingerprints_is_rejected_even_if_unit_was_removed() {
        let run = Run {
            schema: crate::plan::SCHEMA.into(),
            run_id: "current".into(),
            goal_id: "current".into(),
            revision: 1,
            state: RunState::Deciding,
            accepted_units: vec!["U1".into()],
            accepted_fingerprints: Default::default(),
            plan_digest: None,
            branch: "test".into(),
            reviewed_head: None,
            seq: 0,
        };
        let plan = Plan::new("current", "main", "current contract");
        assert!(Engine::invalidated_units(&plan, &run)
            .unwrap_err()
            .to_string()
            .contains("U1 has no recorded fingerprint"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_output_budget_bounds_both_streams_and_preserves_availability() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        for output in [
            "printf '%01025d' 0",
            "printf '%01025d' 0 >&2",
            "printf '%01025d' 0 & printf '%01025d' 0 >&2; wait",
        ] {
            let result = engine
                .run_command_with_limit(dir.path(), output, 1024)
                .await
                .unwrap();
            assert!(!result.success, "oversized output was accepted");
            assert!(result.combined.contains("output exceeded"));
            assert!(result.combined.len() < 200);
        }
        let normal = engine
            .run_command_with_limit(dir.path(), "printf '%01024d' 0; printf ok >&2", 1024)
            .await
            .unwrap();
        assert!(normal.success);
        assert_eq!(normal.combined.len(), 1026);
        let late = "sh -c 'sleep 1; touch overflow-marker' & printf '%01025d' 0; wait";
        assert!(
            !engine
                .run_command_with_limit(dir.path(), late, 1024)
                .await
                .unwrap()
                .success
        );
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert!(!dir.path().join("overflow-marker").exists());
    }

    /// PROOF: a command that outruns its timeout keeps running after `run_command` returns.
    ///
    /// `tokio::process::Command` defaults to `kill_on_drop(false)`, so dropping the
    /// `wait_with_output()` future on timeout drops the `Child` without signalling it. The shell
    /// and everything it spawned survive — still holding, and still writing into, the run worktree
    /// that Hwahap is about to reset for the next attempt.
    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_or_cancelled_commands_kill_their_process_groups() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for args in [
            vec!["init", "-b", "main", "--quiet"],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "t"],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(repo)
                .output()
                .unwrap();
        }
        std::fs::write(repo.join("seed"), "x").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "i", "--quiet"])
            .current_dir(repo)
            .output()
            .unwrap();

        let mut engine = Engine::open(repo).unwrap();
        engine.config.test_timeout_secs = 1;

        let marker = repo.join("marker-written-after-the-timeout");
        let command = format!("sleep 3; touch {}", marker.display());

        let outcome = engine.run_command(repo, &command).await.unwrap();
        assert!(
            !outcome.success,
            "the timeout must be reported as a failure"
        );
        assert!(!marker.exists(), "the command cannot have finished yet");

        // Well past the command's own 3s runtime.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        assert!(
            !marker.exists(),
            "the timed-out command kept running and wrote {} after Hwahap had already \
             given up on it and moved on",
            marker.display()
        );
        // Cancelling the parent future must also kill a grandchild shell, not only its parent.
        let cancelled = repo.join("cancelled-grandchild-marker");
        let command = format!("sh -c 'sleep 1; touch {}' & wait", cancelled.display());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(100),
            engine.run_command(repo, &command)
        )
        .await
        .is_err());
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(
            !cancelled.exists(),
            "cancelled continuation left a writing grandchild"
        );
    }

    /// The eight-hex tail every goal id carries, split off so the readable part can be asserted.
    fn without_fingerprint(id: &str) -> &str {
        let (head, tail) = id
            .rsplit_once('-')
            .expect("every goal id has a fingerprint");
        assert_eq!(tail.len(), 8, "{id}");
        assert!(tail.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
        head
    }

    #[test]
    fn a_goal_id_is_dated_slugged_and_filesystem_safe() {
        assert_eq!(
            without_fingerprint(&goal_id(
                "2026-09-04T10:00:00Z",
                "Add a dry-run flag to apply"
            )),
            "2026-09-04-add-a-dry-run-flag"
        );
        assert_eq!(
            without_fingerprint(&goal_id("2026-09-04T10:00:00Z", "  Fix   the   BUG  ")),
            "2026-09-04-fix-the-bug"
        );
    }

    #[test]
    fn a_goal_id_survives_a_request_with_no_usable_characters() {
        assert_eq!(
            without_fingerprint(&goal_id("2026-09-04T10:00:00Z", "!!! ???")),
            "2026-09-04"
        );
        assert_eq!(
            without_fingerprint(&goal_id("2026-09-04T10:00:00Z", "")),
            "2026-09-04"
        );
    }

    #[test]
    fn a_goal_id_keeps_unicode_word_characters() {
        let id = goal_id("2026-09-04T10:00:00Z", "드라이런 기능 추가");
        assert_eq!(without_fingerprint(&id), "2026-09-04-드라이런-기능-추가");
        assert!(!id.contains(' '));
    }

    #[test]
    fn two_requests_sharing_their_opening_words_get_different_ids() {
        // The slug keeps only the first five words, and the id names the branch. Without the
        // fingerprint these two would share a branch and the second run would build on the first's
        // commits.
        let first = goal_id("2026-09-04T10:00:00Z", "add a dry-run flag to apply");
        let second = goal_id("2026-09-04T10:00:00Z", "add a dry-run flag to delete");
        assert_eq!(without_fingerprint(&first), without_fingerprint(&second));
        assert_ne!(first, second);
    }

    #[test]
    fn a_goal_id_never_contains_a_path_separator() {
        for request in ["a/b", "../escape", "a\\b", "C:\\x"] {
            let id = goal_id("2026-09-04T10:00:00Z", request);
            assert!(!id.contains('/'), "{id}");
            assert!(!id.contains('\\'), "{id}");
            assert!(!id.contains(".."), "{id}");
        }
    }

    #[test]
    fn the_same_request_on_the_same_day_yields_the_same_id() {
        assert_eq!(
            goal_id("2026-09-04T00:00:00Z", "add dry-run"),
            goal_id("2026-09-04T23:59:59Z", "add dry-run")
        );
    }

    #[test]
    fn tail_returns_short_text_unchanged() {
        assert_eq!(tail("short", 100), "short");
        assert_eq!(tail("", 10), "");
    }

    #[test]
    fn tail_truncates_from_the_front_and_marks_it() {
        let long = "x".repeat(100);
        let cut = tail(&long, 10);
        assert!(cut.starts_with('…'), "{cut}");
        assert_eq!(cut.chars().filter(|c| *c == 'x').count(), 10);
    }

    #[test]
    fn tail_never_splits_a_multi_byte_character() {
        let text = "가".repeat(100);
        let cut = tail(&text, 10);
        // Slicing mid-character would have panicked; reaching here at all is most of the assertion.
        assert!(
            cut.chars().all(|c| c == '…' || c == '\n' || c == '가'),
            "{cut}"
        );
    }

    #[test]
    fn selections_render_back_into_the_grammar_the_user_typed() {
        assert_eq!(describe_selection(&Selection::Recommendation), "REC");
        assert_eq!(
            describe_selection(&Selection::Alternative { id: "ALT2".into() }),
            "ALT2"
        );
        assert_eq!(
            describe_selection(&Selection::Other {
                value: "fail after 10s".into()
            }),
            "OTHER: fail after 10s"
        );
        assert_eq!(describe_selection(&Selection::Unknown), "UNKNOWN");
        assert_eq!(describe_selection(&Selection::NotApplicable), "NA");
    }

    #[test]
    fn a_unit_gets_one_economy_attempt_and_one_deep_repair() {
        // The constant is the whole retry policy; a change to it is a change to the contract with
        // the user about how long a stuck unit burns tokens.
        assert_eq!(MAX_ATTEMPTS, 2);
    }
}
