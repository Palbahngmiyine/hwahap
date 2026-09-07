//! Optional host references. The host owns Plan, Goal and execution lifecycle.
use crate::{canonical::Digest, state::Store, Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostContext {
    pub provider: String,
    pub task_id: String,
    #[serde(default)]
    pub goal_ref: Option<String>,
    #[serde(default)]
    pub plan_ref: Option<String>,
    /// Callable capabilities observed by the host, not a promise of future availability.
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
}
impl HostContext {
    pub fn validate(&self, task_id: &str) -> Result<()> {
        if self.task_id != task_id
            || self.provider.trim().is_empty()
            || self.provider.len() > 128
            || self.capabilities.len() > 32
            || self
                .capabilities
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 128)
            || [&self.goal_ref, &self.plan_ref]
                .into_iter()
                .flatten()
                .any(|s| s.trim().is_empty() || s.len() > 2048)
        {
            return Err(Error::Rejected(
                "host context requires matching task identity and bounded references".into(),
            ));
        }
        Ok(())
    }
}

pub fn record(store: &Store, context: &HostContext) -> Result<()> {
    let run = store
        .read_run()?
        .ok_or_else(|| Error::Rejected("host context requires a run".into()))?;
    let value = serde_json::json!({"run_id":run.run_id,"context":context});
    crate::pr_review::save_evidence(
        store,
        &format!("host-context-{}.json", Digest::of(&value)?),
        &value,
    )?;
    store.write_artifact(
        "host-context.json",
        &serde_json::to_string(&value).map_err(|e| Error::Internal(e.to_string()))?,
    )
}

pub fn report(store: &Store) -> Result<Option<serde_json::Value>> {
    let Some(run) = store.read_run()? else {
        return Ok(None);
    };
    let Some(value) =
        crate::pr_review::read_evidence::<serde_json::Value>(store, "host-context.json")?
    else {
        return Ok(None);
    };
    if value["run_id"] != run.run_id {
        return Ok(None);
    }
    Ok(Some(
        serde_json::json!({"references":value["context"], "run_state":run.state.name(),
        "accepted_units":run.accepted_units, "completion_owner":"host",
        "goal_completion":"Compare the host goal's full completion criteria with this run's evidence; one unit or run does not imply goal completion."}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_references_are_optional_and_identity_bound() {
        let context: HostContext = serde_json::from_value(
            serde_json::json!({"provider":"codex","task_id":"task","goal_ref":"goal:task"}),
        )
        .unwrap();
        context.validate("task").unwrap();
        assert!(context.validate("other").is_err());
        let mut changed = context;
        changed.plan_ref = Some("".into());
        assert!(changed.validate("task").is_err());
    }
}
