use super::NativeSessions;
use crate::{
    catalog::{self, Selection},
    clock::{Clock, SystemClock},
    native::NativeLane,
    session::SessionSpec,
    Error, Result,
};

impl NativeSessions {
    pub(super) fn select(
        &self,
        spec: &SessionSpec,
    ) -> Result<(
        Selection,
        NativeLane,
        crate::delegation::DelegationDecision,
        crate::delegation::TaskAssessment,
    )> {
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Rejected("native dispatch has no run".into()))?;
        let parent = self.host_session_id.as_deref().unwrap_or(&run.run_id);
        let snapshot = catalog::snapshot(&self.store, &run.run_id)?;
        let observation = match catalog::host::current(&self.store, parent, &SystemClock.now()) {
            Ok(observation) => observation,
            Err(error) => {
                let assessment = crate::delegation::store::for_spec(&self.store, spec)?;
                let observed_digest = catalog::host::latest(&self.store)?
                    .map(|o| o.digest().map(|d| d.to_string()))
                    .transpose()?;
                let decision = crate::delegation::waiting(
                    &snapshot,
                    spec.role,
                    assessment.as_ref(),
                    observed_digest,
                    "host_stale",
                )?;
                crate::pr_review::save_evidence(
                    &self.store,
                    &format!("delegation-{}.json", decision.digest),
                    &decision,
                )?;
                return Err(error);
            }
        };
        let binding = serde_json::json!({"run_id":run.run_id,"host_session_id":parent,"model":observation.parent_model,"effort":observation.parent_effort});
        if let Some(old) =
            crate::pr_review::read_evidence::<serde_json::Value>(&self.store, "native-parent.json")?
        {
            if old != binding {
                return Err(Error::Rejected("parent_model_changed: preserve this run and start a new run for the new parent model or effort".into()));
            }
        } else {
            crate::pr_review::save_evidence(&self.store, "native-parent.json", &binding)?;
        }
        let plan = self
            .store
            .read_plan()?
            .ok_or_else(|| Error::Rejected("assessment_missing".into()))?;
        let assessment = crate::delegation::store::for_spec(&self.store, spec)?;
        let lane = NativeLane::for_role(spec.role);
        let paths = spec
            .unit
            .as_deref()
            .and_then(|id| plan.unit(id))
            .map(|u| u.paths.clone())
            .unwrap_or_else(|| plan.units.iter().flat_map(|u| u.paths.clone()).collect());
        let contract = plan.digest()?.to_string();
        let mut shared = assessment.as_ref().is_some_and(|a| !a.topology.separable);
        for other in plan
            .units
            .iter()
            .filter(|u| Some(&u.id) != spec.unit.as_ref())
        {
            shared |= other
                .paths
                .iter()
                .any(|p| paths.iter().any(|q| overlap(p, q)));
            if let (Some(a), Some(other)) = (
                &assessment,
                crate::delegation::store::profile_for(
                    &self.store,
                    &plan,
                    &run.run_id,
                    &contract,
                    &other.id,
                )?,
            ) {
                shared |= other
                    .topology
                    .shared_resources
                    .iter()
                    .any(|r| a.topology.shared_resources.contains(r));
            }
        }
        let mut reviewer_bindings = std::collections::BTreeMap::new();
        if matches!(
            spec.role,
            crate::profile::Role::Implementer | crate::profile::Role::Rework
        ) {
            for lane in [NativeLane::Critic, NativeLane::Auditor] {
                if let Some(pair) =
                    crate::native::pool::bound(&self.store, &run.run_id, parent, lane)?
                {
                    reviewer_bindings.insert(lane, pair);
                }
            }
        }
        let context = crate::delegation::Context {
            reviewer_bindings,
            run_id: run.run_id.clone(),
            contract_digest: contract,
            role: spec.role,
            unit: spec.unit.clone(),
            completed: run.accepted_units.iter().cloned().collect(),
            write_paths: paths,
            shared_state: shared,
            writer_available: self.dispatch()?.is_none(),
            bound: crate::native::pool::bound(&self.store, &run.run_id, parent, lane)?,
            free_slots: crate::native::pool::free_slots(&self.store, &observation)?,
            preflight_verified: match &assessment {
                Some(a)
                    if a.risk.high()?
                        && matches!(
                            spec.role,
                            crate::profile::Role::Implementer | crate::profile::Role::Rework
                        ) =>
                {
                    crate::delegation::preflight::verified(&self.store, a)?
                }
                _ => false,
            },
        };
        let decision =
            crate::delegation::decide(&snapshot, &observation, assessment.as_ref(), &context)?;
        crate::pr_review::save_evidence(
            &self.store,
            &format!("delegation-{}.json", decision.digest),
            &decision,
        )?;
        let selection = decision.selection.clone().ok_or_else(|| {
            Error::DelegationWait(format!(
                "{}: provide task assessment for {} / {}; decision {}",
                decision.reason_codes.join(","),
                spec.role.as_str(),
                spec.unit.as_deref().unwrap_or("run"),
                decision.digest
            ))
        })?;
        Ok((
            selection,
            decision.lane,
            decision,
            assessment.expect("selected assessment"),
        ))
    }
}
fn overlap(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('/');
    let b = b.trim_end_matches('/');
    a == b
        || a == "."
        || b == "."
        || a.starts_with(&format!("{b}/"))
        || b.starts_with(&format!("{a}/"))
}
