use super::*;
use crate::pr_review::{read_evidence, save_evidence};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An explicit request to correct implementation under the unchanged frozen contract.
/// This records the host's routing decision, not independent proof of its semantics.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdjustBuildRequest {
    pub user_instruction: String,
    pub contract_digest: String,
    pub unit_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BuildAdjustment {
    request: AdjustBuildRequest,
    head: String,
    affected_units: Vec<String>,
}

const CURRENT: &str = "build-adjustment.json";

impl Engine {
    pub fn adjust_build(&self, input: &AdjustBuildRequest) -> Result<StepOutcome> {
        let mut run = self
            .store
            .recover()?
            .ok_or_else(|| Error::Rejected("there is no reviewed draft to adjust".into()))?;
        if !matches!(run.state, RunState::AwaitingAdjustOrShip { .. }) {
            return Err(Error::Rejected(
                "BUILD adjustment requires a reviewed draft".into(),
            ));
        }
        let plan = self.require_frozen_plan(&run)?;
        if input.user_instruction.trim().is_empty()
            || input.contract_digest != plan.digest()?.to_string()
            || input.unit_ids.is_empty()
        {
            return Err(Error::Rejected(
                "BUILD adjustment needs an instruction, current contract digest and unit IDs"
                    .into(),
            ));
        }
        let mut selected = std::collections::BTreeSet::new();
        for id in &input.unit_ids {
            if plan.unit(id).is_none_or(|u| u.probe) || !selected.insert(id.clone()) {
                return Err(Error::Rejected(format!(
                    "BUILD adjustment needs unique existing implementation units: {id}"
                )));
            }
        }
        // Includes exact PR/head/branch, clean worktree, current reviews and unresolved findings.
        self.require_completed_reviews(&run, &plan)?;
        let head = run
            .reviewed_head
            .clone()
            .ok_or_else(|| Error::Rejected("BUILD adjustment has no reviewed head".into()))?;
        run.accepted_units.retain(|id| !selected.contains(id));
        let affected_units = Self::invalidated_units(&plan, &run)?;
        let first = affected_units
            .first()
            .cloned()
            .ok_or_else(|| Error::Rejected("BUILD adjustment selected no work".into()))?;
        run.accepted_units.retain(|id| !affected_units.contains(id));
        run.accepted_fingerprints
            .retain(|id, _| !affected_units.contains(id));
        let adjustment = BuildAdjustment {
            request: input.clone(),
            head,
            affected_units,
        };
        let evidence = format!("build-adjustment-{}.json", Digest::of(&adjustment)?);
        save_evidence(&self.store, &evidence, &adjustment)?;
        self.store.write_artifact(
            CURRENT,
            &serde_json::to_string_pretty(&adjustment)
                .map_err(|e| Error::Internal(e.to_string()))?,
        )?;
        crate::revalidation::record_obligation(
            &self.store,
            &*self.clock,
            &run.run_id,
            &plan,
            input.unit_ids.clone(),
            "adjust",
            &evidence,
        )?;
        run.reviewed_head = None;
        run.state = RunState::Coding {
            unit: first,
            attempt: 1,
        };
        self.store.write_run(&*self.clock, &run)?;
        Ok(self.report(&run, "BUILD adjustment recorded under the unchanged contract. Affected units and their dependents will be verified, then both teams will review the updated draft.".into()))
    }

    pub(super) fn build_adjustment_findings(
        &self,
        plan: &Plan,
        unit: &Unit,
    ) -> Result<Vec<String>> {
        let Some(adjustment) = read_evidence::<BuildAdjustment>(&self.store, CURRENT)? else {
            return Ok(vec![]);
        };
        if adjustment.request.contract_digest != plan.digest()?.to_string()
            || !adjustment.request.unit_ids.contains(&unit.id)
        {
            return Ok(vec![]);
        }
        Ok(vec![format!(
            "User requested an implementation correction under this unchanged frozen contract: {}. Apply it only within the unit's existing acceptance, tests and paths. Report plan_conflict if a contract decision must change.",
            adjustment.request.user_instruction
        )])
    }
}
