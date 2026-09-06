use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedRepair {
    base: String,
    tree: String,
    commit: String,
    attempts: Vec<crate::revalidation::UnitAttempt>,
}

impl Engine {
    pub(super) async fn repair_pr(&self, run: Run, sessions: &dyn Sessions) -> Result<StepOutcome> {
        let plan = self.require_frozen_plan(&run)?;
        let mut p = ReviewProgress::load(&self.store)?
            .ok_or_else(|| Error::Corrupt("missing repair state".into()))?;
        if p.binding.contract_digest != plan.digest()?
            || run.state.pr_url() != Some(p.binding.pr_url.as_str())
        {
            return Err(Error::BoundaryViolation(
                "repair contract or PR changed".into(),
            ));
        }
        let attack: ReviewRecord<AttackReport> =
            read_evidence(&self.store, &p.artifact("attack")?)?
                .ok_or_else(|| Error::Corrupt("missing attack evidence".into()))?;
        let defense: ReviewRecord<DefenseReport> =
            read_evidence(&self.store, &p.artifact("defense")?)?
                .ok_or_else(|| Error::Corrupt("missing defense evidence".into()))?;
        defense.report.validate(&attack.report, &p.binding)?;
        Self::separate_reviewers(&attack.receipt, &defense.receipt)?;
        let findings = defense.report.repair_findings(&attack.report);
        if defense.report.unresolved() || findings.is_empty() {
            return Err(Error::BoundaryViolation(
                "repair requires confirmed findings without unresolved claims".into(),
            ));
        }
        self.record_pr_obligations(&run, &plan, &p, &findings, sessions)
            .await?;
        let worktree = self.store.worktree_path();
        let paths: Vec<String> = plan
            .units
            .iter()
            .flat_map(|u| u.paths.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let key = p.artifact("repair")?;
        let prepared = match read_evidence::<PreparedRepair>(&self.store, &key)? {
            Some(prepared) => prepared,
            None => {
                let resuming = self.recover_interrupted_pr_author(&plan, &p, &paths)?;
                self.require_review_progress(&run, &plan)?;
                if !resuming && u64::from(p.repairs) >= self.config.native_max_calls {
                    return Err(Error::ExecutionLimit(
                        "PR repair budget exhausted; draft and evidence retained".into(),
                    ));
                }
                let previous_failure = read_evidence::<serde_json::Value>(
                    &self.store,
                    &format!("{key}-failed-{}.json", p.repairs),
                )?;
                let unit = Unit {
                    id: "PR".into(),
                    title: "Repair confirmed PR findings".into(),
                    paths: paths.clone(),
                    acceptance_ids: plan.acceptance.iter().map(|a| a.id.clone()).collect(),
                    depends_on: vec![],
                    probe: false,
                };
                let mut details: Vec<String> = findings
                    .iter()
                    .map(|f| serde_json::to_string(f).expect("finding serialization"))
                    .collect();
                if let Some(failure) = previous_failure {
                    details.push(format!("The previous repair failed `{}`: {}. Its full patch and results are retained at {}",
                        failure["command"], tail(failure["output"].as_str().unwrap_or("unknown"), 4000),
                        self.store.artifacts_path().join(format!("{key}-failed-{}.json", p.repairs)).display()));
                }
                // Repairs may change every non-probe unit: retain each frozen test obligation.
                let commands: std::collections::BTreeSet<String> = plan
                    .units
                    .iter()
                    .filter(|u| !u.probe)
                    .flat_map(|u| plan.tests_for(&u.id))
                    .map(|t| t.command.clone())
                    .chain(std::iter::once(plan.full_suite.clone()))
                    .collect();
                let prompt = format!(
                    "{}\nRequired repair checks (the host runs each before publication):\n{}",
                    prompts::implementer(&plan, &unit, &details),
                    serde_json::to_string(&commands).expect("command serialization")
                );
                self.high_risk_preflight(&plan, None, Role::Rework, sessions)
                    .await?;
                sessions.preflight(&crate::session::SessionSpec {
                    assessment: crate::delegation::store::load(&self.store, Role::Rework, None)?,
                    cwd: worktree.clone(),
                    role: Role::Rework,
                    unit: None,
                    prompt: String::new(),
                })?;
                if !resuming {
                    p.repairs = p
                        .repairs
                        .checked_add(1)
                        .ok_or_else(|| Error::ExecutionLimit("PR repair count overflow".into()))?;
                    p.save(&self.store)?;
                }
                let mut attempts = Vec::new();
                for target in plan.units.iter().filter(|u| !u.probe) {
                    if crate::revalidation::obligations(&self.store, &run.run_id, &target.id)?
                        .iter()
                        .any(|o| o.evidence_kind == "pr_finding")
                    {
                        let round = Digest::of(&(&p.binding, "pr_repair", &target.id))?;
                        attempts.push(crate::revalidation::reserve_attempt(
                            &self.store,
                            &*self.clock,
                            &run.run_id,
                            &target.id,
                            crate::revalidation::AttemptKind::Implementation,
                            &round,
                            self.config.native_max_calls,
                        )?);
                    }
                }
                let outcome = self.ask(sessions, Role::Rework, None, prompt).await?;
                let result = WorkerResult::parse(&outcome.final_message)?;
                if result.status != WorkerStatus::Completed {
                    for attempt in &attempts {
                        let (candidate_code, _) =
                            self.backup_author_candidate(&worktree, &p.binding.head, attempt)?;
                        crate::revalidation::finish_attempt(
                            &self.store,
                            &*self.clock,
                            attempt,
                            false,
                            serde_json::json!({"worker":result,"candidate_code":candidate_code}),
                        )?;
                    }
                    return Err(Error::BoundaryViolation(format!(
                        "PR repair incomplete: {}",
                        result.summary
                    )));
                }
                let changed = self.git.changed_paths(&worktree)?;
                if changed.is_empty() || !paths_outside(&paths, &changed).is_empty() {
                    return Err(Error::BoundaryViolation(
                        "PR repair changed nothing or exceeded the frozen scope".into(),
                    ));
                }
                self.git.run_in(&worktree, &["add", "-A"])?;
                let before = self.git.fingerprint(&worktree)?;
                for command in commands {
                    let check = self
                        .run_verified_command(
                            &plan,
                            None,
                            None,
                            crate::verification::Kind::Unit,
                            &command,
                            &worktree,
                        )
                        .await?;
                    if self.git.fingerprint(&worktree)? != before {
                        return Err(Error::BoundaryViolation(
                            "PR repair check changed files".into(),
                        ));
                    }
                    if !check.success {
                        self.git.run_in(&worktree, &["add", "-A"])?;
                        let patch = self
                            .git
                            .run_in(&worktree, &["diff", "--cached", "--binary", "HEAD"])?;
                        save_evidence(
                            &self.store,
                            &format!("{key}-failed-{}.json", p.repairs),
                            &serde_json::json!({"binding":p.binding,"attempt":p.repairs,
                                "command":command,"output":check.combined,"patch":patch}),
                        )?;
                        for attempt in &attempts {
                            let (candidate_code, _) =
                                self.backup_author_candidate(&worktree, &p.binding.head, attempt)?;
                            crate::revalidation::finish_attempt(
                                &self.store,
                                &*self.clock,
                                attempt,
                                false,
                                serde_json::json!({"command":command,"output":check.combined,"candidate_code":candidate_code}),
                            )?;
                        }
                        // Only discard this owned attempt after durable reproduction evidence exists.
                        self.git.reset_preserving_inputs(
                            &worktree,
                            &p.binding.head,
                            &plan.verification_inputs,
                        )?;
                        return Ok(self.report(&run, format!("PR repair check failed: `{command}`. Evidence retained; retrying within the remaining repair budget.")));
                    }
                }
                self.git.run_in(&worktree, &["add", "-A"])?;
                let tree = self.git.run_in(&worktree, &["write-tree"])?;
                let message = format!(
                    "hwahap(PR): verified repair {}\n\nplan-digest: {}",
                    p.repairs,
                    plan.digest()?
                );
                let commit = self.git.run_in(
                    &worktree,
                    &["commit-tree", &tree, "-p", &p.binding.head, "-m", &message],
                )?;
                let prepared = PreparedRepair {
                    base: p.binding.head.clone(),
                    tree,
                    commit,
                    attempts,
                };
                save_evidence(&self.store, &key, &prepared)?;
                prepared
            }
        };
        // The prepared commit is persisted before branch movement. A restart accepts only old/new.
        let head = self.git.run_in(&worktree, &["rev-parse", "HEAD"])?;
        if prepared.base != p.binding.head
            || self
                .git
                .run_in(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?
                != run.branch
            || self
                .git
                .run_in(&worktree, &["rev-parse", &format!("{}^", prepared.commit)])?
                != prepared.base
            || self.git.run_in(
                &worktree,
                &["rev-parse", &format!("{}^{{tree}}", prepared.commit)],
            )? != prepared.tree
            || self.git.run_in(&worktree, &["write-tree"])? != prepared.tree
            || !paths_outside(
                &paths,
                &self
                    .git
                    .changed_paths_between(&worktree, &prepared.base, &prepared.commit)?,
            )
            .is_empty()
            || (head != prepared.base && head != prepared.commit)
        {
            return Err(Error::BoundaryViolation(
                "prepared PR repair no longer matches the branch".into(),
            ));
        }
        self.git.run_in(&worktree, &["diff", "--exit-code"])?;
        if head == prepared.base {
            self.git.run_in(
                &worktree,
                &[
                    "update-ref",
                    &format!("refs/heads/{}", run.branch),
                    &prepared.commit,
                    &prepared.base,
                ],
            )?;
        }
        if !self.git.is_clean(&worktree)?
            || self
                .forge
                .existing_draft(&worktree, &plan.base_branch, &run.branch)?
                .as_deref()
                != Some(p.binding.pr_url.as_str())
        {
            return Err(Error::BoundaryViolation(
                "repair publication lost its clean branch or matching draft".into(),
            ));
        }
        self.verify_prepared_pr_repair(&plan, &prepared).await?;
        let remote = self.forge.head_sha(&worktree, &p.binding.pr_url)?;
        if remote != prepared.base && remote != prepared.commit {
            return Err(Error::BoundaryViolation(
                "PR changed externally during repair".into(),
            ));
        }
        if remote != prepared.commit {
            self.git.push(&worktree, "origin", &run.branch)?;
        }
        for observation in 0..4 {
            let observed = self.forge.head_sha(&worktree, &p.binding.pr_url)?;
            if observed == prepared.commit {
                break;
            }
            // Only the known old head may lag. Never accept or wait through an unknown head.
            if observed != prepared.base || observation == 3 {
                return Err(Error::BoundaryViolation(format!(
                    "pushed PR head {observed} did not match verified repair {}",
                    prepared.commit
                )));
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        p.binding.head = prepared.commit;
        p.round = p
            .round
            .checked_add(1)
            .ok_or_else(|| Error::ExecutionLimit("PR round overflow".into()))?;
        p.stage = ReviewStage::Attack;
        p.save(&self.store)?;
        self.refresh_review_report(&run, &plan, &p)?;
        Ok(self.report(&run, "Verified repair published to the same draft; both Astra teams must review the new commit.".into()))
    }
}

impl Engine {
    fn finish_prepared_attempts(
        &self,
        attempts: &[crate::revalidation::UnitAttempt],
        passed: bool,
        evidence: &serde_json::Value,
    ) -> Result<()> {
        let events = self.store.read_events()?;
        for attempt in attempts {
            if !events.iter().any(|e| {
                e.kind == "unit_attempt_finished" && e.data["id"] == serde_json::json!(attempt.id)
            }) {
                crate::revalidation::finish_attempt(
                    &self.store,
                    &*self.clock,
                    attempt,
                    passed,
                    evidence.clone(),
                )?;
            }
        }
        Ok(())
    }

    async fn verify_prepared_pr_repair(
        &self,
        plan: &Plan,
        prepared: &PreparedRepair,
    ) -> Result<()> {
        use crate::revalidation::{AttemptKind, UnitAttempt};
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Corrupt("missing run".into()))?;
        let worktree = self.store.worktree_path();
        let round = Digest::of(&(
            &prepared.commit,
            plan.digest()?,
            "pr_postcommit",
            crate::verification::inputs::digest(&worktree, &plan.verification_inputs)?,
        ))?;
        let evidence = serde_json::json!({"commit":prepared.commit,"verification_round":round});
        // A lost return after final verification is reconciled from current command evidence.
        // The prepared writer attempt retains its first completed outcome across later retries.
        match self.require_current_verifications(plan) {
            Ok(()) => {
                let attempts: Vec<UnitAttempt> = self
                    .store
                    .read_events()?
                    .into_iter()
                    .filter(|e| {
                        e.kind == "unit_attempt_started"
                            && e.data["run_id"] == run.run_id
                            && e.data["kind"] == "revalidation"
                            && e.data["round"] == serde_json::json!(round)
                    })
                    .map(|e| {
                        serde_json::from_value(e.data).map_err(|e| Error::Corrupt(e.to_string()))
                    })
                    .collect::<Result<_>>()?;
                self.finish_prepared_attempts(&attempts, true, &evidence)?;
                return self.finish_prepared_attempts(&prepared.attempts, true, &evidence);
            }
            Err(Error::Rejected(_)) => {}
            Err(error) => return Err(error),
        }
        let mut attempts = Vec::new();
        for unit in plan.units.iter().filter(|u| !u.probe) {
            attempts.push(crate::revalidation::reserve_attempt(
                &self.store,
                &*self.clock,
                &run.run_id,
                &unit.id,
                AttemptKind::Revalidation,
                &round,
                self.config.native_max_calls,
            )?);
        }
        let checks = self.run_final_verification(plan, &worktree).await;
        let passed = checks.as_ref().is_ok_and(|output| output.success);
        let detail = match &checks {
            Ok(output) => {
                serde_json::json!({"commit":prepared.commit,"verification_round":round,"output":output.combined})
            }
            Err(error) => {
                serde_json::json!({"commit":prepared.commit,"verification_round":round,"error":error.to_string()})
            }
        };
        self.finish_prepared_attempts(&attempts, passed, &detail)?;
        self.finish_prepared_attempts(&prepared.attempts, passed, &detail)?;
        let checks = checks?;
        if !checks.success {
            return Err(Error::Rejected(format!(
                "repaired candidate verification failed: {}",
                checks.combined
            )));
        }
        Ok(())
    }
}
