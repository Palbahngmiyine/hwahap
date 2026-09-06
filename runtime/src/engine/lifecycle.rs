use super::*;

impl Engine {
    pub(super) fn refresh_plan_source(&self, plan: &mut Plan, run: &Run) -> Result<()> {
        let worktree = self.store.worktree_path();
        let cwd = if worktree.symlink_metadata().is_ok() {
            let branch = if run.branch.is_empty() {
                format!("hwahap/{}", plan.goal_id)
            } else {
                run.branch.clone()
            };
            self.check_plan_worktree(&branch, None)?;
            worktree
        } else if run.branch.is_empty() {
            self.repo_root.clone()
        } else {
            return Err(Error::Rejected(
                "the owned BUILD worktree is missing".into(),
            ));
        };
        if !self.git.is_clean(&cwd)? {
            return Err(Error::Rejected(
                "reopening PLAN requires a clean committed source".into(),
            ));
        }
        let head = self.git.run_in(&cwd, &["rev-parse", "HEAD"])?;
        if plan.source_head.as_ref() != Some(&head) {
            plan.facts.clear();
            for decision in &mut plan.decisions {
                decision.answer = None;
            }
            for status in plan.surfaces.values_mut() {
                *status = SurfaceStatus::Applicable;
            }
        }
        plan.source_head = Some(head);
        plan.interactive = true;
        plan.question_frontier.clear();
        plan.execution_authorization = None;
        plan.approved_plan = None;
        Ok(())
    }

    pub(super) fn prepare_plan_worktree(&self, run: &Run, plan: &Plan) -> Result<String> {
        let branch = if run.branch.is_empty() {
            plan.execution_branch
                .clone()
                .unwrap_or_else(|| format!("hwahap/{}", plan.goal_id))
        } else {
            run.branch.clone()
        };
        let worktree = self.store.worktree_path();
        let source = match &plan.source_head {
            Some(head) => head.clone(),
            None => self.git.run(&["rev-parse", &plan.base_branch])?,
        };
        if worktree.symlink_metadata().is_ok() {
            self.check_plan_worktree(&branch, Some(&source))?;
        } else if !run.branch.is_empty() {
            return Err(Error::Rejected(
                "the owned BUILD worktree is missing".into(),
            ));
        } else if self.git.branch_exists(&branch)? {
            if self.git.run(&["rev-parse", &branch])? != source {
                return Err(Error::Rejected(
                    "the interrupted PLAN branch no longer matches its source".into(),
                ));
            }
            self.git.run(&[
                "worktree",
                "add",
                worktree
                    .to_str()
                    .ok_or_else(|| Error::Rejected("non-UTF8 worktree".into()))?,
                &branch,
            ])?;
            self.check_plan_worktree(&branch, Some(&source))?;
        } else {
            self.git.add_worktree(&worktree, &branch, &source)?;
            self.check_plan_worktree(&branch, Some(&source))?;
        }
        Ok(branch)
    }

    pub(super) fn check_plan_worktree(&self, branch: &str, head: Option<&str>) -> Result<()> {
        let worktree = self.store.worktree_path();
        if worktree
            .symlink_metadata()
            .map_err(|e| Error::io(&worktree, e))?
            .file_type()
            .is_symlink()
            || self.git.run_in(
                &worktree,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )? != self
                .git
                .run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?
            || self
                .git
                .run_in(&worktree, &["rev-parse", "--show-toplevel"])?
                != worktree
                    .canonicalize()
                    .map_err(|e| Error::io(&worktree, e))?
                    .to_string_lossy()
            || self
                .git
                .run_in(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?
                != branch
            || !self.git.is_clean(&worktree)?
            || head.is_some_and(|expected| {
                self.git
                    .run_in(&worktree, &["rev-parse", "HEAD"])
                    .as_deref()
                    .ok()
                    != Some(expected)
            })
        {
            return Err(Error::Rejected(
                "PLAN worktree ownership, source or clean state changed".into(),
            ));
        }
        Ok(())
    }
}
