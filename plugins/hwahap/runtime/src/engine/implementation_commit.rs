use super::*;
use crate::revalidation::ImplementationRecord;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedImplementation {
    base: String,
    branch: String,
    implementation: ImplementationRecord,
}

impl Engine {
    pub(super) fn commit_verified_implementation(
        &self,
        plan: &Plan,
        unit: &Unit,
        cwd: &Path,
    ) -> Result<String> {
        self.require_verified_unit(plan, unit, cwd)?;
        let base = self.git.run_in(cwd, &["rev-parse", "HEAD"])?;
        let tree = self.git.run_in(cwd, &["write-tree"])?;
        let branch = self
            .git
            .run_in(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let message = format!(
            "hwahap({}): {}\n\nplan-digest: {}\nunit: {}",
            unit.id,
            unit.title,
            plan.digest()?,
            unit.id
        );
        let commit = self
            .git
            .run_in(cwd, &["commit-tree", &tree, "-p", &base, "-m", &message])?;
        let prepared = PreparedImplementation {
            base,
            branch,
            implementation: self.verified_implementation_record(plan, unit, cwd, &commit)?,
        };
        self.store.append_event(
            &*self.clock,
            "implementation_prepared",
            serde_json::json!(prepared),
        )?;
        self.apply_prepared_implementation(plan, unit, &prepared)?;
        Ok(commit)
    }

    pub(super) fn recover_prepared_implementation(&self, plan: &Plan, unit: &Unit) -> Result<bool> {
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Corrupt("missing run".into()))?;
        let completed = crate::revalidation::implementations(&self.store, &run.run_id)?;
        for event in self
            .store
            .read_events()?
            .iter()
            .rev()
            .filter(|e| e.kind == "implementation_prepared")
        {
            let prepared: PreparedImplementation = serde_json::from_value(event.data.clone())
                .map_err(|e| Error::Corrupt(e.to_string()))?;
            let record = &prepared.implementation;
            if record.run_id != run.run_id
                || record.unit_id != unit.id
                || completed.contains(record)
            {
                continue;
            }
            if record.fingerprint != plan.unit_fingerprint(&unit.id)? {
                return Err(Error::Rejected("prepared implementation belongs to a different contract; recover it before replanning".into()));
            }
            self.apply_prepared_implementation(plan, unit, &prepared)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn apply_prepared_implementation(
        &self,
        plan: &Plan,
        unit: &Unit,
        prepared: &PreparedImplementation,
    ) -> Result<()> {
        let cwd = self.store.worktree_path();
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Corrupt("missing run".into()))?;
        let record = &prepared.implementation;
        let head = self.git.run_in(&cwd, &["rev-parse", "HEAD"])?;
        if record.run_id != run.run_id
            || record.unit_id != unit.id
            || prepared.branch != run.branch
            || self
                .git
                .run_in(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?
                != prepared.branch
            || !matches!(record.commit.len(), 40 | 64)
            || !record.commit.bytes().all(|c| c.is_ascii_hexdigit())
            || self
                .git
                .run_in(&cwd, &["rev-parse", &format!("{}^", record.commit)])?
                != prepared.base
            || (head != prepared.base && head != record.commit)
            || self.git.run_in(&cwd, &["write-tree"])? != record.tree
            || self.verified_implementation_record(plan, unit, &cwd, &record.commit)? != *record
        {
            return Err(Error::BoundaryViolation(
                "prepared implementation differs from its verified candidate".into(),
            ));
        }
        if head == prepared.base {
            self.require_verified_unit(plan, unit, &cwd)?;
            self.git.run_in(
                &cwd,
                &[
                    "update-ref",
                    &format!("refs/heads/{}", prepared.branch),
                    &record.commit,
                    &prepared.base,
                ],
            )?;
        }
        if !self.git.is_clean(&cwd)? {
            return Err(Error::BoundaryViolation(
                "prepared implementation has changed files".into(),
            ));
        }
        self.bind_verified_implementation(plan, unit, &cwd, &record.commit)
    }
}
