use super::*;
use crate::approval::ApprovedPlanRequest;

impl Engine {
    /// Register the already-approved document, then review its translation before any execution.
    pub fn register_approved_plan(&self, input: &ApprovedPlanRequest) -> Result<StepOutcome> {
        self.register_approved_plan_for_parent(input, None)
    }

    pub fn register_approved_plan_for_parent(
        &self,
        input: &ApprovedPlanRequest,
        pool_scope: Option<&str>,
    ) -> Result<StepOutcome> {
        input.approval.validate()?;
        let existing = self.store.recover()?;
        let old = self.store.read_plan()?;
        if let (Some(run), Some(plan)) = (&existing, &old) {
            if let Some(event) = self
                .store
                .read_events()?
                .iter()
                .rev()
                .find(|e| e.kind == "approved_plan_snapshot")
            {
                if event.data["pool_scope"]
                    .as_str()
                    .is_some_and(|saved| saved != pool_scope.unwrap_or(&run.run_id))
                {
                    return Err(Error::Rejected(
                        "approved plan belongs to another parent task".into(),
                    ));
                }
                if event.data["request"] == serde_json::json!(input) {
                    let saved: Plan = serde_json::from_value(event.data["plan"].clone())
                        .map_err(|e| Error::Corrupt(e.to_string()))?;
                    if saved.review_digest()? != plan.review_digest()?
                        || saved.revision != run.revision
                    {
                        return Err(Error::Rejected(
                            "approved plan retry no longer matches the current contract".into(),
                        ));
                    }
                    return Ok(self.report(
                        run,
                        "Approved plan already registered; continuing without another approval."
                            .into(),
                    ));
                }
            }
            if !run.branch.is_empty()
                || run.plan_digest.is_some()
                || plan.frozen.is_some()
                || !run.accepted_units.is_empty()
                || self.store.worktree_path().symlink_metadata().is_ok()
                || !matches!(
                    run.state,
                    RunState::Inspecting
                        | RunState::Deciding
                        | RunState::Refining
                        | RunState::Proving
                        | RunState::AwaitingConfirmation { .. }
                        | RunState::PlanConflict { .. }
                )
            {
                return Err(Error::Rejected(
                    "approved plan import cannot replace an executing or frozen contract".into(),
                ));
            }
            if input.replaces_plan_digest.as_deref() != Some(plan.digest()?.as_str()) {
                return Err(Error::Rejected(
                    "approved plan import must name the exact draft it supersedes".into(),
                ));
            }
        } else if existing.is_some() || old.is_some() || input.replaces_plan_digest.is_some() {
            return Err(Error::Rejected(
                "approved plan import has inconsistent prior state".into(),
            ));
        }
        if !self.git.is_clean(&self.repo_root)?
            || self.git.head_sha()? != input.approval.source_head
        {
            return Err(Error::Rejected(
                "approved plan source changed; inspect its differences before registration".into(),
            ));
        }
        self.git
            .run(&["check-ref-format", "--branch", &input.contract.branch])?;
        self.git
            .run(&["check-ref-format", "--branch", &input.contract.base_branch])?;
        if self.git.branch_exists(&input.contract.branch)?
            || self.store.worktree_path().symlink_metadata().is_ok()
        {
            return Err(Error::Rejected(
                "approved plan BUILD branch/worktree already exists".into(),
            ));
        }
        let base = self.git.run(&[
            "rev-parse",
            "--verify",
            &format!(
                "refs/remotes/origin/{}^{{commit}}",
                input.contract.base_branch
            ),
        ])?;
        self.git.run(&[
            "merge-base",
            "--is-ancestor",
            &base,
            &input.approval.source_head,
        ])?;
        let id = existing
            .as_ref()
            .map(|r| r.goal_id.clone())
            .unwrap_or_else(|| super::goal_id(&self.clock.now(), &input.contract.objective));
        let mut candidate = input.candidate(&id, &base)?;
        candidate.revision = existing.as_ref().map_or(1, |r| r.revision + 1);
        let run = Run {
            schema: crate::plan::SCHEMA.into(),
            run_id: id.clone(),
            goal_id: id,
            revision: candidate.revision,
            state: RunState::Proving,
            accepted_units: vec![],
            accepted_fingerprints: Default::default(),
            plan_digest: None,
            branch: String::new(),
            reviewed_head: None,
            seq: 0,
        };
        self.store.write_approved_plan(
            &*self.clock,
            &run,
            &candidate,
            input,
            pool_scope.unwrap_or(&run.run_id),
        )?;
        Ok(self.report(&run, "Your plan approval is recorded. Hwahap is checking that the executable contract preserves it; no new approval is requested.".into()))
    }
}
