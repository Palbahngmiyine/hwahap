use super::*;
use crate::verification::{self, Kind, Status, VerificationRequest};

impl Engine {
    pub(super) fn verification_request(
        &self,
        plan: &Plan,
        unit: Option<&str>,
        test: Option<&str>,
        kind: Kind,
        command: &str,
        cwd: &Path,
    ) -> Result<VerificationRequest> {
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Rejected("verification requires an active run".into()))?;
        Ok(VerificationRequest {
            run_id: run.run_id,
            unit_id: unit.map(str::to_owned),
            test_id: test.map(str::to_owned),
            kind,
            contract: match unit {
                Some(id) => plan.unit_fingerprint(id)?,
                None => plan.digest()?,
            },
            command: command.into(),
            cwd: cwd.to_string_lossy().into_owned(),
            head: self.git.run_in(cwd, &["rev-parse", "HEAD"])?,
            tree: self.git.run_in(cwd, &["write-tree"])?,
            code: self.git.fingerprint(cwd)?,
            inputs: verification::inputs::digest(cwd, &plan.verification_inputs)?,
        })
    }

    pub(super) async fn run_verified_command(
        &self,
        plan: &Plan,
        unit: Option<&str>,
        test: Option<&str>,
        kind: Kind,
        command: &str,
        cwd: &Path,
    ) -> Result<CommandOutput> {
        let request = self.verification_request(plan, unit, test, kind.clone(), command, cwd)?;
        let record = verification::start(&self.store, &*self.clock, request.clone())?;
        let result = self
            .run_command_owned(cwd, command, 1024 * 1024, Some(&record.id))
            .await;
        let mut output = match result {
            Ok(output) => output,
            Err(error) => {
                verification::record_verification(
                    &self.store,
                    &*self.clock,
                    record,
                    Status::Failed,
                    None,
                    &error.to_string(),
                )?;
                return Err(error);
            }
        };
        let after = self.verification_request(plan, unit, test, kind, command, cwd);
        if after.as_ref().ok() != Some(&request) {
            output.success = false;
            output
                .combined
                .push_str("\nverification changed source, index, HEAD or declared inputs");
        }
        verification::record_verification(
            &self.store,
            &*self.clock,
            record,
            if output.success {
                Status::Passed
            } else {
                Status::Failed
            },
            output.exit_code,
            &output.combined,
        )?;
        Ok(output)
    }

    pub(super) async fn run_final_verification(
        &self,
        plan: &Plan,
        cwd: &Path,
    ) -> Result<CommandOutput> {
        for unit in plan.units.iter().filter(|u| !u.probe) {
            for test in plan.tests_for(&unit.id) {
                let output = self
                    .run_verified_command(
                        plan,
                        Some(&unit.id),
                        Some(&test.id),
                        Kind::FinalUnit,
                        &test.command,
                        cwd,
                    )
                    .await?;
                if !output.success {
                    return Ok(CommandOutput {
                        combined: format!(
                            "{} / {}: `{}`\n{}",
                            unit.id, test.id, test.command, output.combined
                        ),
                        ..output
                    });
                }
            }
        }
        let output = self
            .run_verified_command(plan, None, None, Kind::FullSuite, &plan.full_suite, cwd)
            .await?;
        if output.success {
            self.require_current_verifications(plan)?;
        }
        Ok(output)
    }

    pub(super) fn require_current_verifications(&self, plan: &Plan) -> Result<()> {
        let cwd = self.store.worktree_path();
        for unit in plan.units.iter().filter(|u| !u.probe) {
            for test in plan.tests_for(&unit.id) {
                let request = self.verification_request(
                    plan,
                    Some(&unit.id),
                    Some(&test.id),
                    Kind::FinalUnit,
                    &test.command,
                    &cwd,
                )?;
                verification::require_current_verification(&self.store, &request)?;
            }
        }
        let request =
            self.verification_request(plan, None, None, Kind::FullSuite, &plan.full_suite, &cwd)?;
        verification::require_current_verification(&self.store, &request)?;
        Ok(())
    }
    pub(super) fn require_verified_unit(&self, plan: &Plan, unit: &Unit, cwd: &Path) -> Result<()> {
        for test in plan.tests_for(&unit.id) {
            let request = self.verification_request(
                plan,
                Some(&unit.id),
                Some(&test.id),
                Kind::Unit,
                &test.command,
                cwd,
            )?;
            verification::require_current_verification(&self.store, &request)?;
        }
        Ok(())
    }

    pub(super) fn verified_implementation_record(
        &self,
        plan: &Plan,
        unit: &Unit,
        cwd: &Path,
        commit: &str,
    ) -> Result<crate::revalidation::ImplementationRecord> {
        let records = verification::recover_verifications(&self.store)?;
        let tree = self
            .git
            .run_in(cwd, &["rev-parse", &format!("{commit}^{{tree}}")])?;
        let inputs = verification::inputs::digest(cwd, &plan.verification_inputs)?;
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Rejected("missing implementation run".into()))?;
        let mut evidence = Vec::new();
        for test in plan.tests_for(&unit.id) {
            let record = records
                .values()
                .filter(|r| {
                    r.request.run_id == run.run_id
                        && r.request.kind == Kind::Unit
                        && r.request.unit_id.as_deref() == Some(&unit.id)
                        && r.request.test_id.as_deref() == Some(&test.id)
                })
                .max_by_key(|r| r.attempt)
                .ok_or_else(|| Error::Rejected("implementation verification is missing".into()))?;
            if record.status != Status::Passed
                || record.request.tree != tree
                || record.request.inputs != inputs
                || record.request.contract != plan.unit_fingerprint(&unit.id)?
                || record.request.command != test.command
            {
                return Err(Error::Rejected(
                    "committed implementation differs from verified candidate".into(),
                ));
            }
            evidence.push(record.id.clone());
        }
        let implementation = crate::revalidation::ImplementationRecord {
            run_id: run.run_id,
            unit_id: unit.id.clone(),
            fingerprint: plan.unit_fingerprint(&unit.id)?,
            commit: commit.into(),
            tree,
            verification_ids: evidence,
        };
        Ok(implementation)
    }

    pub(super) fn bind_verified_implementation(
        &self,
        plan: &Plan,
        unit: &Unit,
        cwd: &Path,
        commit: &str,
    ) -> Result<()> {
        let implementation = self.verified_implementation_record(plan, unit, cwd, commit)?;
        if !crate::revalidation::implementations(&self.store, &implementation.run_id)?
            .contains(&implementation)
        {
            self.store.append_event(
                &*self.clock,
                "implementation_completed",
                serde_json::json!(implementation),
            )?;
        }
        self.resolve_implementation_obligations(&implementation)
    }
}
