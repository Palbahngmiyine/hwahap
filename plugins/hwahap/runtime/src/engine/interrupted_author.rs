use super::*;
impl Engine {
    pub(super) fn recover_interrupted_author(
        &self,
        plan: &Plan,
        unit: &Unit,
        worktree: &Path,
    ) -> Result<()> {
        crate::native::require_stopped(&self.store)?;
        if self.git.is_clean(worktree)? {
            return Ok(());
        }
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Corrupt("missing run".into()))?;
        let events = self.store.read_events()?;
        let head = self.git.run_in(worktree, &["rev-parse", "HEAD"])?;
        let round = Digest::of(&(
            &head,
            plan.unit_fingerprint(&unit.id)?,
            self.build_adjustment_findings(plan, unit)?,
        ))?;
        let code = self.git.fingerprint(worktree)?;
        let pending = events
            .iter()
            .rev()
            .filter(|e| e.kind == "unit_attempt_started")
            .map(|e| {
                serde_json::from_value::<crate::revalidation::UnitAttempt>(e.data.clone())
                    .map_err(|e| Error::Corrupt(e.to_string()))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .find(|a| {
                a.run_id == run.run_id
                    && a.unit_id == unit.id
                    && a.kind == crate::revalidation::AttemptKind::Implementation
                    && a.round == round
            });
        let Some(attempt) = pending else {
            return Ok(());
        };
        if let Some(finished) = events.iter().find(|e| {
            e.kind == "unit_attempt_finished" && e.data["id"] == serde_json::json!(attempt.id)
        }) {
            if !self.rejected_candidate_owned(&attempt, &finished.data, &head, &code)? {
                return Ok(());
            }
        }
        self.preserve_interrupted_author(plan, &unit.paths, worktree, &head, &attempt)
    }

    pub(super) fn recover_interrupted_pr_author(
        &self,
        plan: &Plan,
        progress: &crate::pr_review::ReviewProgress,
        paths: &[String],
    ) -> Result<bool> {
        crate::native::require_stopped(&self.store)?;
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Corrupt("missing run".into()))?;
        let events = self.store.read_events()?;
        let rounds = plan
            .units
            .iter()
            .filter(|u| !u.probe)
            .map(|u| {
                Ok((
                    u.id.clone(),
                    Digest::of(&(&progress.binding, "pr_repair", &u.id))?,
                ))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
        for event in events
            .iter()
            .rev()
            .filter(|e| e.kind == "unit_attempt_started")
        {
            let attempt: crate::revalidation::UnitAttempt =
                serde_json::from_value(event.data.clone())
                    .map_err(|e| Error::Corrupt(e.to_string()))?;
            if attempt.run_id == run.run_id
                && attempt.kind == crate::revalidation::AttemptKind::Implementation
                && rounds.get(&attempt.unit_id) == Some(&attempt.round)
            {
                let worktree = self.store.worktree_path();
                if self.git.run_in(&worktree, &["rev-parse", "HEAD"])? != progress.binding.head {
                    return Err(Error::BoundaryViolation(
                        "interrupted PR author HEAD changed".into(),
                    ));
                }
                let code = self.git.fingerprint(&worktree)?;
                let finished = events.iter().find(|e| {
                    e.kind == "unit_attempt_finished"
                        && e.data["id"] == serde_json::json!(attempt.id)
                });
                if let Some(event) = finished {
                    if !self.rejected_candidate_owned(
                        &attempt,
                        &event.data,
                        &progress.binding.head,
                        &code,
                    )? {
                        return Ok(false);
                    }
                }
                if !self.git.is_clean(&worktree)? {
                    self.preserve_interrupted_author(
                        plan,
                        paths,
                        &worktree,
                        &progress.binding.head,
                        &attempt,
                    )?;
                }
                return Ok(finished.is_none());
            }
        }
        Ok(false)
    }

    fn rejected_candidate_owned(
        &self,
        attempt: &crate::revalidation::UnitAttempt,
        finished: &serde_json::Value,
        head: &str,
        code: &Digest,
    ) -> Result<bool> {
        if finished["passed"] != false {
            return Ok(false);
        }
        let Some(recorded) = finished["evidence"]["candidate_code"].as_str() else {
            return Ok(false);
        };
        if recorded == code.as_str() {
            return Ok(true);
        }
        // An interrupted reset can leave only part of the rejected candidate.
        let name = format!("interrupted-author-{}-{recorded}.json", attempt.id);
        let saved: Option<serde_json::Value> = crate::pr_review::read_evidence(&self.store, &name)?;
        Ok(saved.is_some_and(|v| {
            v["attempt"] == serde_json::json!(attempt)
                && v["head"] == head
                && v["code"] == recorded
                && v["native_work_stopped"] == true
                && v["patch"].is_string()
        }))
    }

    pub(super) fn backup_author_candidate(
        &self,
        worktree: &Path,
        head: &str,
        attempt: &crate::revalidation::UnitAttempt,
    ) -> Result<(Digest, String)> {
        crate::native::require_stopped(&self.store)?;
        let code = self.git.fingerprint(worktree)?;
        let patch = self.git.candidate_patch(worktree)?;
        let name = format!("interrupted-author-{}-{code}.json", attempt.id);
        crate::pr_review::save_evidence(
            &self.store,
            &name,
            &serde_json::json!({"attempt":attempt,"head":head,"code":code,"patch":patch,"native_work_stopped":true}),
        )?;
        Ok((code, name))
    }

    fn preserve_interrupted_author(
        &self,
        plan: &Plan,
        paths: &[String],
        worktree: &Path,
        head: &str,
        attempt: &crate::revalidation::UnitAttempt,
    ) -> Result<()> {
        let events = self.store.read_events()?;
        let outside = paths_outside(paths, &self.git.changed_paths(worktree)?);
        if !outside.is_empty() {
            return Err(Error::Rejected(format!(
                "interrupted candidate includes paths outside {}: {outside:?}",
                attempt.unit_id
            )));
        }
        let (_, name) = self.backup_author_candidate(worktree, head, attempt)?;
        self.git
            .reset_preserving_inputs(worktree, head, &plan.verification_inputs)?;
        if !events
            .iter()
            .any(|e| e.kind == "interrupted_author_reconciled" && e.data["evidence"] == name)
        {
            self.store.append_event(
                &*self.clock,
                "interrupted_author_reconciled",
                serde_json::json!({"attempt_id":attempt.id,"evidence":name}),
            )?;
        }
        Ok(())
    }
}
