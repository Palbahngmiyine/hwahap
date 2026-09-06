use super::*;
use crate::revalidation::{self, AttemptKind, ImplementationRecord};
use crate::verification::{self, Kind, Status};

impl Engine {
    pub(super) fn prior_implementation(
        &self,
        plan: &Plan,
        unit: &Unit,
    ) -> Result<Option<ImplementationRecord>> {
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Corrupt("missing run".into()))?;
        let fingerprint = plan.unit_fingerprint(&unit.id)?;
        let records = revalidation::implementations(&self.store, &run.run_id)?;
        let Some(record) = records
            .into_iter()
            .rev()
            .find(|r| r.unit_id == unit.id && r.fingerprint == fingerprint)
        else {
            return Ok(None);
        };
        let cwd = self.store.worktree_path();
        if !matches!(record.commit.len(), 40 | 64)
            || !record.commit.bytes().all(|c| c.is_ascii_hexdigit())
            || self
                .git
                .run_in(&cwd, &["rev-parse", &format!("{}^{{tree}}", record.commit)])?
                != record.tree
        {
            return Err(Error::Rejected(
                "implementation commit/tree evidence is invalid".into(),
            ));
        }
        if self
            .git
            .run_in(
                &cwd,
                &["merge-base", "--is-ancestor", &record.commit, "HEAD"],
            )
            .is_err()
        {
            return Ok(None);
        }
        let verifications = verification::recover_verifications(&self.store)?;
        for test in plan.tests_for(&unit.id) {
            if !record.verification_ids.iter().any(|id| {
                verifications.get(id).is_some_and(|v| {
                    v.status == Status::Passed
                        && v.request.run_id == run.run_id
                        && v.request.unit_id.as_deref() == Some(&unit.id)
                        && v.request.test_id.as_deref() == Some(&test.id)
                        && v.request.kind == Kind::Unit
                        && v.request.contract == fingerprint
                        && v.request.command == test.command
                        && v.request.tree == record.tree
                })
            }) {
                return Err(Error::Rejected(
                    "implementation lacks matching verified tests".into(),
                ));
            }
        }
        Ok(Some(record))
    }

    pub(super) fn resolve_implementation_obligations(
        &self,
        record: &ImplementationRecord,
    ) -> Result<()> {
        let events = self.store.read_events()?;
        let completed = events
            .iter()
            .rev()
            .find(|e| e.kind == "implementation_completed" && e.data == serde_json::json!(record))
            .ok_or_else(|| Error::Corrupt("implementation completion is missing".into()))?;
        for started in events.iter().filter(|e| {
            e.kind == "unit_attempt_started"
                && e.seq < completed.seq
                && e.data["run_id"] == record.run_id
                && e.data["unit_id"] == record.unit_id
                && e.data["kind"] == "implementation"
        }) {
            if !events
                .iter()
                .any(|e| e.kind == "unit_attempt_finished" && e.data["id"] == started.data["id"])
            {
                let attempt = serde_json::from_value(started.data.clone())
                    .map_err(|e| Error::Corrupt(e.to_string()))?;
                revalidation::finish_attempt(
                    &self.store,
                    &*self.clock,
                    &attempt,
                    true,
                    serde_json::json!({"commit":record.commit}),
                )?;
            }
        }
        for obligation in revalidation::obligations(&self.store, &record.run_id, &record.unit_id)? {
            let created = events
                .iter()
                .find(|e| {
                    e.kind == "repair_obligation"
                        && e.data["id"] == serde_json::json!(obligation.id)
                })
                .ok_or_else(|| Error::Corrupt("repair obligation event missing".into()))?;
            if created.seq < completed.seq {
                self.store.append_event(&*self.clock,"repair_obligation_resolved",serde_json::json!({
                    "run_id":record.run_id,"unit_id":record.unit_id,"id":obligation.id,"implementation":record
                }))?;
            }
        }
        Ok(())
    }

    pub(super) async fn revalidate_unit(
        &self,
        plan: &Plan,
        unit: &Unit,
        prior: &ImplementationRecord,
        sessions: &dyn Sessions,
    ) -> Result<UnitOutcome> {
        let worktree = self.store.worktree_path();
        if !self.git.is_clean(&worktree)? {
            return Err(Error::BoundaryViolation(
                "revalidation requires a clean candidate".into(),
            ));
        }
        let head = self.git.run_in(&worktree, &["rev-parse", "HEAD"])?;
        let round = Digest::of(&(&head, plan.unit_fingerprint(&unit.id)?, "revalidation"))?;
        let attempt = match revalidation::reserve_attempt(
            &self.store,
            &*self.clock,
            &prior.run_id,
            &unit.id,
            AttemptKind::Revalidation,
            &round,
            self.config.native_max_calls,
        ) {
            Ok(attempt) => attempt,
            Err(Error::ExecutionLimit(reason)) => return Ok(UnitOutcome::Blocked(reason)),
            Err(error) => return Err(error),
        };
        let mut evidence = Vec::new();
        for test in plan.tests_for(&unit.id) {
            let output = self
                .run_verified_command(
                    plan,
                    Some(&unit.id),
                    Some(&test.id),
                    Kind::Revalidation,
                    &test.command,
                    &worktree,
                )
                .await;
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    revalidation::finish_attempt(
                        &self.store,
                        &*self.clock,
                        &attempt,
                        false,
                        serde_json::json!({"error": error.to_string()}),
                    )?;
                    return Err(error);
                }
            };
            if !output.success {
                let detail = format!(
                    "{} / {}: {}",
                    unit.id,
                    test.id,
                    tail(&output.combined, 4000)
                );
                revalidation::finish_attempt(
                    &self.store,
                    &*self.clock,
                    &attempt,
                    false,
                    serde_json::json!(detail),
                )?;
                return Ok(UnitOutcome::Blocked(detail));
            }
            let request = self.verification_request(
                plan,
                Some(&unit.id),
                Some(&test.id),
                Kind::Revalidation,
                &test.command,
                &worktree,
            )?;
            let record = verification::require_current_verification(&self.store, &request)?;
            evidence.push(record.id);
        }
        let diff = self
            .git
            .run_in(&worktree, &["diff", &prior.commit, "HEAD"])?;
        let prompt = prompts::revalidation_review(
            plan,
            unit,
            &head,
            &serde_json::json!({"implementation":prior,"verification_ids":evidence}).to_string(),
            &tail(&diff, 200_000),
        );
        let reviewed = match self
            .ask(sessions, Role::UnitReviewer, Some(unit.id.clone()), prompt)
            .await
        {
            Err(Error::BoundaryViolation(detail)) => {
                revalidation::finish_attempt(
                    &self.store,
                    &*self.clock,
                    &attempt,
                    false,
                    serde_json::json!(detail),
                )?;
                return Ok(UnitOutcome::Blocked(detail));
            }
            result => result?,
        };
        let result = match ReviewResult::parse(&reviewed.final_message) {
            Ok(result) => result,
            Err(error) => {
                revalidation::finish_attempt(
                    &self.store,
                    &*self.clock,
                    &attempt,
                    false,
                    serde_json::json!({"error":error.to_string(),"review":reviewed.receipt}),
                )?;
                return Ok(UnitOutcome::Blocked(error.to_string()));
            }
        };
        if result.verdict != Verdict::Pass {
            revalidation::finish_attempt(
                &self.store,
                &*self.clock,
                &attempt,
                false,
                serde_json::json!(result.findings),
            )?;
            return Ok(UnitOutcome::Blocked(result.findings.join("\n")));
        }
        for test in plan.tests_for(&unit.id) {
            let request = self.verification_request(
                plan,
                Some(&unit.id),
                Some(&test.id),
                Kind::Revalidation,
                &test.command,
                &worktree,
            )?;
            verification::require_current_verification(&self.store, &request)?;
        }
        revalidation::finish_attempt(
            &self.store,
            &*self.clock,
            &attempt,
            true,
            serde_json::json!({"head":head,"verification_ids":evidence,"review":reviewed.receipt}),
        )?;
        Ok(UnitOutcome::Accepted)
    }
}
