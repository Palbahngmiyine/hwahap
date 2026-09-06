use super::NativeSessions;
use crate::{
    catalog::{self, Selection},
    clock::{Clock, SystemClock},
    native::NativeLane,
    session::SessionSpec,
    Error, Result,
};

impl NativeSessions {
    pub(super) fn select(&self, spec: &SessionSpec) -> Result<(Selection, NativeLane)> {
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Rejected("native dispatch has no run".into()))?;
        let parent = self.host_session_id.as_deref().unwrap_or(&run.run_id);
        let observation = catalog::host::current(&self.store, parent, &SystemClock.now())?;
        let snapshot = catalog::snapshot(&self.store, &run.run_id)?;
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
        let direct = self
            .store
            .read_plan()?
            .is_some_and(|p| p.execution_authorization.is_some());
        let lane = if direct
            && matches!(
                spec.role,
                crate::profile::Role::Implementer | crate::profile::Role::FactFinder
            ) {
            NativeLane::Coordinator
        } else {
            NativeLane::for_role(spec.role)
        };
        let requirements = &snapshot.catalog.role_requirements[&spec.role];
        let tools = catalog::tools_for(spec.role);
        let (model, effort) = if lane == NativeLane::Coordinator {
            (
                observation.parent_model.clone(),
                observation.parent_effort.clone(),
            )
        } else if let Some(pair) =
            crate::native::pool::bound(&self.store, &run.run_id, parent, lane)?
        {
            pair
        } else {
            if crate::native::pool::free_slots(&self.store, &observation)? == 0 {
                return Err(Error::Rejected("slot_unavailable: refresh host capacity or start a new run in a new parent task".into()));
            }
            snapshot
                .catalog
                .candidates(requirements)
                .into_iter()
                .find(|(m, e)| observation.available(m, e, &tools))
                .map(|(m, e)| (m.to_string(), e.to_string()))
                .ok_or_else(|| {
                    Error::Rejected(
                        "model_unavailable: no observed model and effort meets this role".into(),
                    )
                })?
        };
        Ok((
            Selection::new(
                &snapshot,
                &observation,
                spec.role,
                spec.unit.clone(),
                &model,
                &effort,
            )?,
            lane,
        ))
    }
}
