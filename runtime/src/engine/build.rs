use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::plan::{Acceptance, Plan, Requirement, Test, Unit};
use crate::{validate, Error, Result};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildRequest {
    /// User's exact execution instruction; approved imports additionally bind and review the full plan.
    pub user_instruction: String,
    pub objective: String,
    /// Remote target branch, for example main.
    pub base_branch: String,
    /// A new codex/ branch. Execution starts from the caller's current committed HEAD.
    pub branch: String,
    pub units: Vec<BuildUnit>,
    pub full_suite: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildUnit {
    pub title: String,
    /// Complete behavior, constraints and observable success for this unit.
    pub acceptance: String,
    pub paths: Vec<String>,
    pub test_command: String,
}

impl BuildRequest {
    /// Construct and validate an execution contract without planning decisions or reviews.
    pub fn plan(&self, id: &str, base_commit: &str) -> Result<Plan> {
        if !self.branch.starts_with("codex/") || self.branch == self.base_branch {
            return Err(Error::Rejected(
                "BUILD needs a new codex/ branch distinct from its base".into(),
            ));
        }
        let mut plan = Plan::new(id, &self.base_branch, &self.objective);
        plan.execution_authorization = Some(self.user_instruction.clone());
        plan.base_commit = Some(base_commit.into());
        plan.full_suite = self.full_suite.clone();
        for (index, unit) in self.units.iter().enumerate() {
            let n = index + 1;
            let (r, a, u) = (format!("R{n}"), format!("A{n}"), format!("U{n}"));
            plan.goal.success.push(unit.acceptance.clone());
            plan.requirements.push(Requirement {
                id: r.clone(),
                statement: unit.acceptance.clone(),
                decision_ids: vec![],
            });
            plan.acceptance.push(Acceptance {
                id: a.clone(),
                requirement_ids: vec![r],
                observable: unit.acceptance.clone(),
            });
            plan.units.push(Unit {
                id: u.clone(),
                title: unit.title.clone(),
                paths: unit.paths.clone(),
                acceptance_ids: vec![a.clone()],
                depends_on: if n == 1 {
                    vec![]
                } else {
                    vec![format!("U{}", n - 1)]
                },
                probe: false,
            });
            plan.tests.push(Test {
                id: format!("T{n}"),
                command: unit.test_command.clone(),
                acceptance_ids: vec![a],
                unit_id: u,
            });
        }
        let errors = validate::build_blockers(&plan)?;
        if !errors.is_empty() {
            return Err(Error::Rejected(format!(
                "invalid BUILD contract: {errors:?}"
            )));
        }
        Ok(plan)
    }
}

impl super::Engine {
    /// Start only from a caller-relayed, explicit execution instruction, without planning calls.
    pub fn start_build(&self, input: &BuildRequest) -> Result<super::StepOutcome> {
        self.start_build_for_parent(input, None)
    }

    pub(crate) fn start_build_for_parent(
        &self,
        input: &BuildRequest,
        parent: Option<&str>,
    ) -> Result<super::StepOutcome> {
        crate::approval::reject_unbound_implementation_request(Some(&input.user_instruction))?;
        let saved = crate::pr_review::read_evidence::<serde_json::Value>(
            &self.store,
            "build-request.json",
        )?;
        if let Some(scope) = saved.as_ref().and_then(|s| s["pool_scope"].as_str()) {
            let fallback = saved.as_ref().and_then(|s| s["plan"]["goal_id"].as_str());
            if parent.or(fallback) != Some(scope) {
                return Err(Error::Rejected(
                    "BUILD belongs to another parent task".into(),
                ));
            }
        }
        if let Some(run) = self.store.recover()? {
            let plan = self.require_frozen_plan(&run)?;
            if saved
                .as_ref()
                .is_none_or(|v| v["request"] != serde_json::json!(input))
                || input
                    .plan(&run.goal_id, plan.base_commit.as_deref().unwrap_or(""))?
                    .digest()?
                    != plan.digest()?
                || run.branch != input.branch
            {
                return Err(Error::Rejected(
                    "BUILD retry differs from the recorded execution contract".into(),
                ));
            }
            return Ok(self.report(
                &run,
                "Existing BUILD retained; no branch, worktree or authorization was replaced."
                    .into(),
            ));
        }
        if let Some(saved) = saved {
            return self.resume_build(input, &saved);
        }
        if !self.git.is_clean(&self.repo_root)? {
            return Err(Error::Rejected(
                "direct BUILD requires a clean committed starting point".into(),
            ));
        }
        self.git
            .run(&["check-ref-format", "--branch", &input.branch])?;
        if input.base_branch.is_empty() || input.base_branch.starts_with('-') {
            return Err(Error::Rejected("invalid BUILD base branch".into()));
        }
        self.git
            .run(&["check-ref-format", "--branch", &input.base_branch])?;
        if self.git.branch_exists(&input.branch)? || self.store.worktree_path().exists() {
            return Err(Error::Rejected(
                "BUILD branch/worktree already exists; do not duplicate or adopt interrupted work"
                    .into(),
            ));
        }
        let base = self.git.run(&[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{}^{{commit}}", input.base_branch),
        ])?;
        let source = self.git.head_sha()?;
        self.git
            .run(&["merge-base", "--is-ancestor", &base, &source])?;
        let id = super::goal_id(&self.clock.now(), &input.objective);
        let mut plan = input.plan(&id, &base)?;
        let digest = plan.digest()?;
        plan.frozen = Some(crate::plan::Frozen {
            digest: digest.clone(),
            confirmed_at: self.clock.now(),
            answer_text: input.user_instruction.clone(),
        });
        self.forge.require_auth(&self.repo_root)?;
        let saved = serde_json::json!({
            "request":input, "source_head":source, "base_commit":base, "contract_digest":digest,
            "authorization_source":"explicit_build_instruction", "planning_performed":false, "plan":plan,
            "pool_scope":parent
        });
        crate::pr_review::save_evidence(&self.store, "build-request.json", &saved)?;
        self.resume_build(input, &saved)
    }

    fn resume_build(
        &self,
        input: &BuildRequest,
        saved: &serde_json::Value,
    ) -> Result<super::StepOutcome> {
        let plan: Plan = serde_json::from_value(saved["plan"].clone()).map_err(|e| {
            Error::Corrupt(format!("interrupted BUILD lacks its sealed contract: {e}"))
        })?;
        let source = saved["source_head"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("missing BUILD source".into()))?;
        let base = saved["base_commit"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("missing BUILD base".into()))?;
        let digest = plan.digest()?;
        if saved["request"] != serde_json::json!(input)
            || saved["contract_digest"] != serde_json::json!(digest)
            || !plan.is_frozen()?
            || input.plan(&plan.goal_id, base)?.digest()? != digest
            || plan
                .frozen
                .as_ref()
                .is_none_or(|f| f.answer_text != input.user_instruction)
            || self.git.head_sha()? != source
            || !self.git.is_clean(&self.repo_root)?
        {
            return Err(Error::Rejected(
                "interrupted BUILD request, source or contract changed".into(),
            ));
        }
        self.forge.require_auth(&self.repo_root)?;
        let worktree = self.store.worktree_path();
        if !worktree.exists() {
            if self.git.branch_exists(&input.branch)? {
                if self.git.run(&["rev-parse", &input.branch])? != source {
                    return Err(Error::BoundaryViolation(
                        "BUILD branch changed before recovery".into(),
                    ));
                }
                self.git.run(&[
                    "worktree",
                    "add",
                    worktree
                        .to_str()
                        .ok_or_else(|| Error::Rejected("non-UTF8 worktree".into()))?,
                    &input.branch,
                ])?;
            } else {
                self.git.add_worktree(&worktree, &input.branch, source)?;
            }
        }
        if self.git.run_in(&worktree, &["rev-parse", "HEAD"])? != source
            || self.git.run_in(
                &worktree,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )? != self
                .git
                .run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?
            || self
                .git
                .run_in(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?
                != input.branch
            || !self.git.is_clean(&worktree)?
        {
            return Err(Error::BoundaryViolation(
                "interrupted BUILD worktree ownership changed".into(),
            ));
        }
        self.save_plan(&plan)?;
        let id = plan.goal_id.clone();
        let run = crate::state::Run {
            schema: crate::plan::SCHEMA.into(),
            run_id: id.clone(),
            goal_id: id,
            revision: 1,
            state: crate::state::RunState::Coding {
                unit: "U1".into(),
                attempt: 1,
            },
            accepted_units: vec![],
            accepted_fingerprints: Default::default(),
            plan_digest: Some(digest),
            branch: input.branch.clone(),
            reviewed_head: None,
            seq: 0,
        };
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(&run, "BUILD started from the recorded explicit instruction. Planning was omitted; scope, tests and independent review remain required.".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> BuildRequest {
        BuildRequest {
            user_instruction: "Skip planning and implement the requested check".into(),
            objective: "Validate settings".into(),
            base_branch: "main".into(),
            branch: "codex/check".into(),
            full_suite: "cargo test".into(),
            units: vec![BuildUnit {
                title: "Reject invalid settings".into(),
                acceptance: "Invalid settings return an error without changing data".into(),
                paths: vec!["src".into()],
                test_command: "cargo test settings".into(),
            }],
        }
    }

    #[test]
    fn direct_contract_has_no_fabricated_planning_evidence() {
        let input = request();
        let plan = input.plan("build", "abc").unwrap();
        assert_eq!(
            plan.execution_authorization.as_ref(),
            Some(&input.user_instruction)
        );
        assert!(plan.decisions.is_empty() && plan.frozen.is_none());
        assert!(plan.reviews.critic.is_none() && plan.reviews.cold_consumer.is_none());
        assert!(!validate::freeze_blockers(&plan).unwrap().is_empty());
        assert!(validate::build_blockers(&plan).unwrap().is_empty());
        assert!(crate::render::plan_markdown(&plan)
            .unwrap()
            .contains("Planning omitted"));
        assert!(crate::prompts::implementer(&plan, &plan.units[0], &[])
            .contains(&input.units[0].acceptance));
    }

    #[test]
    fn direct_contract_rejects_missing_consent_scope_and_tests() {
        let mut input = request();
        input.user_instruction.clear();
        assert!(input.plan("build", "abc").is_err());
        input = request();
        input.units[0].paths = vec!["../outside".into()];
        assert!(input.plan("build", "abc").is_err());
        input = request();
        input.units[0].test_command.clear();
        assert!(input.plan("build", "abc").is_err());
        input = request();
        input.units.clear();
        assert!(input.plan("build", "abc").is_err());
    }
}
