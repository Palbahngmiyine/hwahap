use super::*;
use crate::pr_review::{
    read_evidence, save_evidence, AttackReport, DefenseReport, ReviewProgress, ReviewStage,
};
use crate::session::SessionReceipt;
mod obligations;
mod repair;

#[derive(serde::Serialize, serde::Deserialize)]
struct ReviewRecord<T> {
    report: T,
    receipt: SessionReceipt,
}

impl Engine {
    pub(super) fn refresh_review_report(
        &self,
        run: &Run,
        plan: &Plan,
        progress: &ReviewProgress,
    ) -> Result<()> {
        let cost = crate::cost::persist(&self.store)?;
        let report = format!("{}\n## PR review\n\nHead: `{}`; round: {}; repairs: {}; stage: {:?}.\n\nDetailed review and reproduction evidence remains local in `.hwahap/artifacts`.\n\n## Cost evidence\n\n```json\n{}\n```\n",
            self.report_markdown(plan, run), progress.binding.head, progress.round,
            progress.repairs, progress.stage, serde_json::to_string_pretty(&cost).map_err(|e| Error::Internal(e.to_string()))?);
        let pr = self.forge.update_draft(
            &self.store.worktree_path(),
            &plan.base_branch,
            &run.branch,
            &progress.binding.pr_url,
            &plan.goal.statement,
            &report,
        )?;
        if pr.head_sha != progress.binding.head {
            return Err(Error::BoundaryViolation(
                "PR head changed while refreshing report".into(),
            ));
        }
        self.store.write_report(&report)
    }

    /// Recheck this run's recorded published draft with current-format evidence.
    pub fn recheck_pr(&self) -> Result<StepOutcome> {
        let mut run = self
            .store
            .recover()?
            .ok_or_else(|| Error::Rejected("no run to recheck".into()))?;
        if !matches!(
            run.state,
            RunState::AwaitingAdjustOrShip { .. }
                | RunState::PrReview { .. }
                | RunState::Blocked { .. }
        ) {
            return Err(Error::Rejected("this run has no reviewable draft".into()));
        }
        let plan = self.require_frozen_plan(&run)?;
        let mut previous = ReviewProgress::load(&self.store)?
            .ok_or_else(|| Error::Corrupt("missing current PR review progress".into()))?;
        let digest = plan.digest()?;
        let url = run
            .state
            .pr_url()
            .map(str::to_string)
            .or_else(|| Some(previous.binding.pr_url.clone()))
            .ok_or_else(|| Error::Rejected("no recorded draft to recheck".into()))?;
        let worktree = self.store.worktree_path();
        let head = self.git.run_in(&worktree, &["rev-parse", "HEAD"])?;
        if !self.git.is_clean(&worktree)?
            || self
                .git
                .run_in(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?
                != run.branch
            || self
                .forge
                .existing_draft(&worktree, &plan.base_branch, &run.branch)?
                .as_deref()
                != Some(url.as_str())
            || self.forge.head_sha(&worktree, &url)? != head
            || previous.binding.contract_digest != digest
        {
            return Err(Error::BoundaryViolation(
                "recheck PR ownership, head or contract mismatch".into(),
            ));
        }
        // Current interrupted/blocked reviews may be retried. Missing required evidence fields
        // are corruption; do not manufacture a new round to upgrade an obsolete report.
        let _: Option<ReviewRecord<AttackReport>> =
            read_evidence(&self.store, &previous.artifact("attack")?)?;
        let _: Option<ReviewRecord<DefenseReport>> =
            read_evidence(&self.store, &previous.artifact("defense")?)?;
        if previous.stage != ReviewStage::Repair && matches!(run.state, RunState::Blocked { .. }) {
            previous.round = previous
                .round
                .checked_add(1)
                .ok_or_else(|| Error::ExecutionLimit("review round overflow".into()))?;
            previous.stage = ReviewStage::Attack;
            previous.save(&self.store)?;
        }
        run.reviewed_head = None;
        run.state = RunState::FinalVerifying;
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(
            &run,
            "Rechecking the existing draft: full suite, then independent attack and defense."
                .into(),
        ))
    }

    pub(super) fn require_completed_reviews(&self, run: &Run, plan: &Plan) -> Result<()> {
        let p = self.require_review_progress(run, plan)?;
        if p.stage != ReviewStage::Complete || run.reviewed_head.as_ref() != Some(&p.binding.head) {
            return Err(Error::Rejected(
                "both current-head PR reviews must complete before SHIP".into(),
            ));
        }
        let a: ReviewRecord<AttackReport> = read_evidence(&self.store, &p.artifact("attack")?)?
            .ok_or_else(|| Error::Corrupt("missing attack report".into()))?;
        if read_evidence::<ReviewBindingProof>(&self.store, &p.artifact("single-review")?)?
            .is_some_and(|proof| proof.binding == p.binding)
        {
            if !single_review_eligible(plan)
                || p.repairs > 0
                || !a.report.findings.is_empty()
                || a.report.security.blocked()
            {
                return Err(Error::Rejected(
                    "single review no longer satisfies the frozen risk policy".into(),
                ));
            }
            a.report.validate(&p.binding)?;
            self.verify_session_receipt(
                &a.receipt,
                &SessionSpec {
                    assessment: crate::delegation::store::load(
                        &self.store,
                        Role::UnitReviewer,
                        None,
                    )?,
                    cwd: self.store.worktree_path(),
                    role: Role::UnitReviewer,
                    unit: None,
                    prompt: String::new(),
                },
            )?;
            let SessionReceipt::Native(receipt) = &a.receipt;
            if receipt.agent_id == "coordinator" {
                return Err(Error::Rejected(
                    "single review requires an independent reviewer".into(),
                ));
            }
            self.require_current_verifications(plan)?;
            return Ok(());
        }
        let d: ReviewRecord<DefenseReport> =
            read_evidence(&self.store, &p.artifact("defense")?)?
                .ok_or_else(|| Error::Corrupt("missing defense report".into()))?;
        d.report.validate(&a.report, &p.binding)?;
        Self::separate_reviewers(&a.receipt, &d.receipt)?;
        for (receipt, role) in [
            (&a.receipt, Role::UnitReviewer),
            (&d.receipt, Role::FinalReview),
        ] {
            self.verify_session_receipt(
                receipt,
                &SessionSpec {
                    assessment: crate::delegation::store::load(&self.store, role, None)?,
                    cwd: self.store.worktree_path(),
                    role,
                    unit: None,
                    prompt: String::new(),
                },
            )?;
        }
        if a.report.security.blocked()
            || d.report.security.blocked()
            || d.report.unresolved()
            || !d.report.repair_findings(&a.report).is_empty()
        {
            return Err(Error::Rejected(
                "incomplete security coverage or unresolved/confirmed PR findings prevent SHIP"
                    .into(),
            ));
        }
        self.require_current_verifications(plan)?;
        Ok(())
    }
    pub(super) async fn review_pr(
        &self,
        mut run: Run,
        sessions: &dyn Sessions,
    ) -> Result<StepOutcome> {
        let plan = self.require_frozen_plan(&run)?;
        if ReviewProgress::load(&self.store)?.is_some_and(|p| p.stage == ReviewStage::Repair) {
            return self.repair_pr(run, sessions).await;
        }
        let mut progress = self.require_review_progress(&run, &plan)?;
        let checks = self
            .forge
            .check_results(&self.store.worktree_path(), &progress.binding.pr_url)?;
        if checks.iter().any(|c| !crate::forge::check_succeeded(c)) {
            let pending = checks.iter().any(|c| {
                c["conclusion"].as_str().is_none_or(str::is_empty)
                    && !matches!(c["state"].as_str(), Some("SUCCESS" | "FAILURE" | "ERROR"))
            });
            if pending {
                let mut outcome = self.report(
                    &run,
                    "CI 실행 중입니다. 호스트의 검사 완료 알림 후 진행하세요.".into(),
                );
                outcome.next = "await_checks".into();
                return Ok(outcome);
            }
            self.require_review_progress(&run, &plan)?;
            save_evidence(
                &self.store,
                &progress.artifact("ci-failure")?,
                &CiFailure {
                    binding: progress.binding.clone(),
                    checks,
                },
            )?;
            progress.stage = ReviewStage::Repair;
            progress.save(&self.store)?;
            return Ok(self.report(
                &run,
                "CI 실패를 기록했습니다. 모델 리뷰에 앞서 해당 검사부터 수정합니다.".into(),
            ));
        }
        let revision = format!(
            "{}...{}",
            plan.base_commit.as_deref().unwrap_or(&plan.base_branch),
            progress.binding.head
        );
        let changed = self.git.run_in(
            &self.store.worktree_path(),
            &["diff", "--name-status", "--no-renames", &revision, "--"],
        )?;
        let manifest = format!("Review revision: {revision}\nChanged paths (possibly truncated; retrieve the full list with git diff --name-status):\n{}\nRead the full diff and relevant source locally using git diff for this exact revision. This manifest is an index, not review evidence.", tail(&changed, 12_000));
        let contract = render::plan_markdown(&plan)?;
        let attack: ReviewRecord<AttackReport> = self
            .review_record(
                sessions,
                Role::UnitReviewer,
                &progress,
                prompts::pr_attack(&progress.binding, &contract, &manifest),
            )
            .await?;
        attack.report.validate(&progress.binding)?;
        self.require_review_progress(&run, &plan)?;
        save_evidence(&self.store, &progress.artifact("attack")?, &attack)?;
        if single_review_eligible(&plan)
            && progress.repairs == 0
            && attack.report.findings.is_empty()
            && !attack.report.security.blocked()
        {
            let SessionReceipt::Native(receipt) = &attack.receipt;
            if receipt.agent_id == "coordinator" {
                return Err(Error::Rejected(
                    "single review requires an independent reviewer".into(),
                ));
            }
            self.require_current_verifications(&plan)?;
            save_evidence(
                &self.store,
                &progress.artifact("single-review")?,
                &ReviewBindingProof {
                    binding: progress.binding.clone(),
                },
            )?;
            progress.stage = ReviewStage::Complete;
            run.reviewed_head = Some(progress.binding.head.clone());
            run.state = RunState::AwaitingAdjustOrShip {
                pr_url: progress.binding.pr_url.clone(),
                challenge: plan.digest()?.challenge(),
            };
            progress.save(&self.store)?;
            self.refresh_review_report(&run, &plan, &progress)?;
            self.store.write_run(&*self.clock, &run)?;
            return Ok(self.report(&run, self.describe(&run, Some(&plan))?));
        }
        progress.stage = ReviewStage::Defense;
        progress.save(&self.store)?;
        let defense: ReviewRecord<DefenseReport> = self
            .review_record(
                sessions,
                Role::FinalReview,
                &progress,
                prompts::pr_defense(&attack.report, &contract, &manifest),
            )
            .await?;
        defense.report.validate(&attack.report, &progress.binding)?;
        Self::separate_reviewers(&attack.receipt, &defense.receipt)?;
        self.require_review_progress(&run, &plan)?;
        save_evidence(&self.store, &progress.artifact("defense")?, &defense)?;
        if attack.report.security.blocked()
            || defense.report.security.blocked()
            || defense.report.unresolved()
        {
            run.state = RunState::Blocked {
                reason: "PR review left incomplete security coverage or unresolved findings; evidence and draft are retained"
                    .into(),
            };
        } else if !defense.report.repair_findings(&attack.report).is_empty() {
            progress.stage = ReviewStage::Repair;
        } else {
            self.resolve_pr_obligations(&run, &plan, &progress)?;
            progress.stage = ReviewStage::Complete;
            run.reviewed_head = Some(progress.binding.head.clone());
            run.state = RunState::AwaitingAdjustOrShip {
                pr_url: progress.binding.pr_url.clone(),
                challenge: plan.digest()?.challenge(),
            };
        }
        progress.save(&self.store)?;
        self.refresh_review_report(&run, &plan, &progress)?;
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(&run, self.describe(&run, Some(&plan))?))
    }

    async fn review_record<T: serde::de::DeserializeOwned>(
        &self,
        sessions: &dyn Sessions,
        role: Role,
        progress: &ReviewProgress,
        prompt: String,
    ) -> Result<ReviewRecord<T>> {
        let team = if role == Role::UnitReviewer {
            "attack"
        } else {
            "defense"
        };
        let record: ReviewRecord<T> = match read_evidence(&self.store, &progress.artifact(team)?)? {
            Some(record) => record,
            None => {
                let outcome = self.ask(sessions, role, None, prompt).await?;
                ReviewRecord {
                    report: serde_json::from_str(&outcome.final_message).map_err(|e| {
                        Error::BoundaryViolation(format!("invalid {team} report: {e}"))
                    })?,
                    receipt: outcome.receipt,
                }
            }
        };
        let spec = SessionSpec {
            assessment: crate::delegation::store::load(&self.store, role, None)?,
            cwd: self.store.worktree_path(),
            role,
            unit: None,
            prompt: String::new(),
        };
        self.verify_session_receipt(&record.receipt, &spec)?;
        Ok(record)
    }

    fn separate_reviewers(attack: &SessionReceipt, defense: &SessionReceipt) -> Result<()> {
        match (attack, defense) {
            (SessionReceipt::Native(a), SessionReceipt::Native(d))
                if a.agent_id != d.agent_id
                    && a.agent_id != "coordinator"
                    && d.agent_id != "coordinator"
                    && a.role == Role::UnitReviewer
                    && d.role == Role::FinalReview =>
            {
                Ok(())
            }
            _ => Err(Error::BoundaryViolation(
                "PR review needs distinct read-only UnitReviewer and FinalReview children".into(),
            )),
        }
    }

    pub(super) fn require_review_progress(&self, run: &Run, plan: &Plan) -> Result<ReviewProgress> {
        let progress = ReviewProgress::load(&self.store)?
            .ok_or_else(|| Error::Corrupt("missing published PR review state".into()))?;
        let worktree = self.store.worktree_path();
        if progress.binding.contract_digest != plan.digest()?
            || run.state.pr_url() != Some(progress.binding.pr_url.as_str())
            || self.git.run_in(&worktree, &["rev-parse", "HEAD"])? != progress.binding.head
            || self
                .git
                .run_in(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?
                != run.branch
            || !self.git.is_clean(&worktree)?
            || self
                .forge
                .existing_draft(&worktree, &plan.base_branch, &run.branch)?
                .as_deref()
                != Some(progress.binding.pr_url.as_str())
            || self.forge.head_sha(&worktree, &progress.binding.pr_url)? != progress.binding.head
        {
            return Err(Error::BoundaryViolation(
                "PR, commit or contract changed during review".into(),
            ));
        }
        Ok(progress)
    }
}

#[cfg(test)]
mod independent_catalog_review {
    use super::*;
    #[test]
    fn arbitrary_catalog_model_keeps_role_and_identity_independence() {
        let mut catalog = crate::catalog::bundled();
        let mut model = catalog
            .models
            .iter()
            .find(|m| m.id == "gpt-6-astra")
            .unwrap()
            .clone();
        model.id = "successor-review-model".into();
        catalog.models = vec![model];
        let snapshot = crate::catalog::CatalogSnapshot::new("test-run", catalog).unwrap();
        let host = crate::catalog::HostObservation {
            host_session_id: "parent".into(),
            observed_at: "2026-09-07T00:00:00Z".into(),
            source: "test inventory".into(),
            parent_model: "successor-review-model".into(),
            parent_effort: "high".into(),
            available_slots: 2,
            models: [(
                "successor-review-model".into(),
                crate::catalog::ObservedModel {
                    efforts: vec!["high".into()],
                    tools: vec!["exec_command".into()],
                },
            )]
            .into(),
        };
        let receipt = |role: Role, id: &str| {
            let receipt = SessionReceipt::Native(crate::session::NativeReceipt {
                decision: None,
                assessment: None,
                selection: crate::catalog::Selection::new(
                    &snapshot,
                    &host,
                    role,
                    None,
                    "successor-review-model",
                    "high",
                )
                .unwrap(),
                dispatch_id: format!("dispatch-{id}"),
                agent_id: id.into(),
                profile: role.profile(),
                role,
                unit: None,
                model_requested: "successor-review-model".into(),
                effort_requested: crate::profile::Effort::High,
                elapsed_ms: 1,
                reported_usage: None,
            });
            receipt
                .verify_for(
                    &SessionSpec {
                        assessment: None,
                        cwd: "/tmp".into(),
                        role,
                        unit: None,
                        prompt: String::new(),
                    },
                    &snapshot,
                )
                .unwrap();
            receipt
        };
        let attack = receipt(Role::UnitReviewer, "critic");
        let defense = receipt(Role::FinalReview, "auditor");
        Engine::separate_reviewers(&attack, &defense).unwrap();
        assert!(
            Engine::separate_reviewers(&attack, &receipt(Role::FinalReview, "critic")).is_err()
        );
        assert!(
            Engine::separate_reviewers(&attack, &receipt(Role::UnitReviewer, "other")).is_err()
        );
        assert!(
            Engine::separate_reviewers(&attack, &receipt(Role::FinalReview, "coordinator"))
                .is_err()
        );
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewBindingProof {
    binding: crate::pr_review::ReviewBinding,
}

/// Only a frozen, complete low-risk profile opts into one clean independent PR review.
fn single_review_eligible(plan: &Plan) -> bool {
    std::iter::once("run")
        .chain(
            plan.units
                .iter()
                .filter(|u| !u.probe)
                .map(|u| u.id.as_str()),
        )
        .all(|id| {
            plan.task_profiles.get(id).is_some_and(|p| {
                [
                    p.risk.failure_cost,
                    p.risk.reversibility,
                    p.risk.blast_radius,
                ]
                .into_iter()
                .all(|v| v == Some(0))
                    && p.topology.separable
                    && p.topology.coupling == crate::delegation::Coupling::Independent
                    && p.topology.shared_resources.is_empty()
                    && p.requirements.depth <= crate::catalog::Depth::Focused
                    && p.requirements.capabilities.values().all(|v| *v <= 2)
                    && !p.evidence.is_empty()
            })
        })
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CiFailure {
    binding: crate::pr_review::ReviewBinding,
    checks: Vec<serde_json::Value>,
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    #[test]
    fn single_review_requires_every_frozen_profile_to_be_low_risk() {
        let mut plan = Plan::new("run", "main", "fixture");
        let profile = serde_json::from_value(serde_json::json!({"requirements":{"capabilities":{},"depth":"focused"},"risk":{"failure_cost":0,"reversibility":0,"blast_radius":0},"topology":{"predecessors":[],"coupling":"independent","shared_resources":[],"writer_owner":null,"separable":true,"write_paths":[]},"evidence":["isolated fixture"],"recovery":null})).unwrap();
        assert!(!single_review_eligible(&plan));
        plan.task_profiles.insert("run".into(), profile);
        plan.units.push(Unit {
            id: "U1".into(),
            title: "fixture".into(),
            paths: vec!["feature".into()],
            acceptance_ids: vec![],
            depends_on: vec![],
            probe: false,
        });
        assert!(!single_review_eligible(&plan));
        plan.task_profiles
            .insert("U1".into(), plan.task_profiles["run"].clone());
        assert!(single_review_eligible(&plan));
        plan.task_profiles.get_mut("U1").unwrap().risk.failure_cost = Some(2);
        assert!(!single_review_eligible(&plan));
    }
}
