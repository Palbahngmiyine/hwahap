use super::*;
use crate::{session::SessionSpec, state::Store};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskProfile {
    pub requirements: Requirements,
    pub risk: Risk,
    pub topology: Topology,
    pub evidence: Vec<String>,
    pub recovery: Option<RecoveryRequirements>,
}
impl TaskAssessment {
    pub fn profile(&self) -> TaskProfile {
        TaskProfile {
            requirements: self.requirements.clone(),
            risk: self.risk.clone(),
            topology: self.topology.clone(),
            evidence: self.evidence.clone(),
            recovery: self.recovery.clone(),
        }
    }
}
fn key(run: &str, contract: &str, unit: Option<&str>, role: Role) -> String {
    Digest::of_bytes(
        format!(
            "{run}\0{contract}\0{}\0{}",
            unit.unwrap_or(""),
            role.as_str()
        )
        .as_bytes(),
    )
    .to_string()
}
fn name(run: &str, contract: &str, unit: Option<&str>, role: Role) -> String {
    format!("task-assessment-{}.json", key(run, contract, unit, role))
}
pub fn record(store: &Store, assessment: &TaskAssessment) -> Result<()> {
    assessment.validate()?;
    let run = store
        .read_run()?
        .ok_or_else(|| Error::Rejected("assessment_missing: start a run first".into()))?;
    let plan = store
        .read_plan()?
        .ok_or_else(|| Error::Rejected("assessment_missing: current plan is unavailable".into()))?;
    if run.run_id != assessment.run_id || plan.digest()?.to_string() != assessment.contract_digest {
        return Err(Error::Rejected(
            "assessment_binding_changed: use the current run and contract".into(),
        ));
    }
    if let Some(unit) = &assessment.unit {
        let unit = plan
            .units
            .iter()
            .find(|u| &u.id == unit)
            .ok_or_else(|| Error::Rejected("assessment names an unknown unit".into()))?;
        let mut paths = assessment.topology.write_paths.clone();
        paths.sort();
        paths.dedup();
        let mut expected = unit.paths.clone();
        expected.sort();
        expected.dedup();
        if paths != expected || assessment.topology.writer_owner.as_deref() != Some(&unit.id) {
            return Err(Error::Rejected(
                "writer_conflict: assessment must match unit scope and owner".into(),
            ));
        }
        if assessment.topology.predecessors.contains(&unit.id)
            || assessment
                .topology
                .predecessors
                .iter()
                .any(|p| !plan.units.iter().any(|u| &u.id == p))
            || unit
                .depends_on
                .iter()
                .any(|id| !assessment.topology.predecessors.contains(id))
        {
            return Err(Error::Rejected(
                "assessment topology differs from contract dependencies".into(),
            ));
        }
    }
    if assessment.unit.is_none() && matches!(assessment.role, Role::Implementer | Role::Rework) {
        let mut paths = assessment.topology.write_paths.clone();
        paths.sort();
        paths.dedup();
        let mut expected: Vec<_> = plan.units.iter().flat_map(|u| u.paths.clone()).collect();
        expected.sort();
        expected.dedup();
        if paths != expected || assessment.topology.writer_owner.as_deref() != Some(&run.run_id) {
            return Err(Error::Rejected(
                "writer_conflict: run writer must match aggregate scope and owner".into(),
            ));
        }
    }
    let profile = assessment.profile();
    let planned_key = assessment.unit.clone().unwrap_or_else(|| "run".into());
    if plan
        .task_profiles
        .get(&planned_key)
        .is_some_and(|p| p != &profile)
    {
        return Err(Error::Rejected(
            "assessment_binding_changed: revise the planned task profile".into(),
        ));
    }
    // All roles for one unit consume the same recorded task evidence.
    let task = Digest::of_bytes(
        format!(
            "{}\0{}\0{}",
            run.run_id, assessment.contract_digest, planned_key
        )
        .as_bytes(),
    );
    crate::pr_review::save_evidence(store, &format!("task-profile-{task}.json"), &profile)?;
    crate::pr_review::save_evidence(
        store,
        &name(
            &run.run_id,
            &assessment.contract_digest,
            assessment.unit.as_deref(),
            assessment.role,
        ),
        assessment,
    )
}
pub fn load(store: &Store, role: Role, unit: Option<&str>) -> Result<Option<TaskAssessment>> {
    let Some(run) = store.read_run()? else {
        return Ok(None);
    };
    let Some(plan) = store.read_plan()? else {
        return Ok(None);
    };
    let contract = plan.digest()?.to_string();
    let mut result = crate::pr_review::read_evidence::<TaskAssessment>(
        store,
        &name(&run.run_id, &contract, unit, role),
    )?;
    if result.is_none() {
        let planned_key = unit.map(str::to_owned).unwrap_or_else(|| "run".into());
        let task =
            Digest::of_bytes(format!("{}\0{}\0{}", run.run_id, contract, planned_key).as_bytes());
        let profile = match plan.task_profiles.get(&planned_key) {
            Some(profile) => Some(profile.clone()),
            None => crate::pr_review::read_evidence::<TaskProfile>(
                store,
                &format!("task-profile-{task}.json"),
            )?,
        };
        if let Some(profile) = profile {
            let assessment = TaskAssessment {
                run_id: run.run_id.clone(),
                contract_digest: contract.clone(),
                unit: unit.map(str::to_owned),
                role,
                requirements: profile.requirements,
                risk: profile.risk,
                topology: profile.topology,
                evidence: profile.evidence,
                recovery: profile.recovery,
            };
            record(store, &assessment)?;
            result = Some(assessment);
        }
    }
    if let Some(assessment) = &result {
        assessment.validate()?;
        if assessment.run_id != run.run_id
            || assessment.contract_digest != contract
            || assessment.role != role
            || assessment.unit.as_deref() != unit
        {
            return Err(Error::Corrupt("task assessment binding differs".into()));
        }
    }
    Ok(result)
}
pub fn for_spec(store: &Store, spec: &SessionSpec) -> Result<Option<TaskAssessment>> {
    let stored = load(store, spec.role, spec.unit.as_deref())?;
    if let Some(requested) = &spec.assessment {
        if stored.as_ref() != Some(requested) {
            return Err(Error::Rejected(
                "assessment differs from recorded task evidence".into(),
            ));
        }
    }
    Ok(stored)
}

pub fn profile_for(
    store: &Store,
    plan: &crate::plan::Plan,
    run_id: &str,
    contract: &str,
    unit: &str,
) -> Result<Option<TaskProfile>> {
    if let Some(profile) = plan.task_profiles.get(unit) {
        return Ok(Some(profile.clone()));
    }
    let key = Digest::of_bytes(format!("{run_id}\0{contract}\0{unit}").as_bytes());
    crate::pr_review::read_evidence(store, &format!("task-profile-{key}.json"))
}
