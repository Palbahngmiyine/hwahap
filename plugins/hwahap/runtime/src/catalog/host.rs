use super::*;
use crate::{clock::Clock, state::Store};
use schemars::JsonSchema;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservedModel {
    pub efforts: Vec<String>,
    pub tools: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostObservation {
    pub host_session_id: String,
    pub observed_at: String,
    pub source: String,
    pub models: BTreeMap<String, ObservedModel>,
    pub parent_model: String,
    pub parent_effort: String,
    pub available_slots: u32,
}
impl HostObservation {
    pub fn validate(&self, parent: &str, now: &str) -> Result<()> {
        let observed = chrono::DateTime::parse_from_rfc3339(&self.observed_at)
            .map_err(|_| Error::Rejected("host_stale: invalid observed_at".into()))?;
        let now = chrono::DateTime::parse_from_rfc3339(now)
            .map_err(|_| Error::Rejected("invalid current time".into()))?;
        let age = now.signed_duration_since(observed);
        if observed > now || age > chrono::Duration::seconds(300) {
            return Err(Error::Rejected(
                "host_stale: provide an observation from the last 300 seconds".into(),
            ));
        }
        if self.host_session_id != parent || parent.is_empty() {
            return Err(Error::Rejected("host_parent_mismatch".into()));
        }
        if self.source.trim().is_empty()
            || !identifier(&self.parent_model)
            || !identifier(&self.parent_effort)
            || self.models.iter().any(|(id, model)| {
                !identifier(id)
                    || model.efforts.iter().any(|e| !identifier(e))
                    || model.efforts.iter().collect::<BTreeSet<_>>().len() != model.efforts.len()
                    || model.tools.iter().any(|t| !identifier(t))
            })
        {
            return Err(Error::Rejected(
                "host observation needs source, exact models, efforts and tools".into(),
            ));
        }
        Ok(())
    }
    pub fn available(&self, model: &str, effort: &str, tools: &[String]) -> bool {
        self.models.get(model).is_some_and(|m| {
            m.efforts.iter().any(|e| e == effort) && tools.iter().all(|t| m.tools.contains(t))
        })
    }
    pub fn digest(&self) -> Result<Digest> {
        Digest::of(self)
    }
}

pub fn observe(
    store: &Store,
    clock: &dyn Clock,
    parent: &str,
    observation: &HostObservation,
) -> Result<()> {
    observation.validate(parent, &clock.now())?;
    if let Some(previous) = latest(store)? {
        let old = chrono::DateTime::parse_from_rfc3339(&previous.observed_at)
            .map_err(|e| Error::Corrupt(e.to_string()))?;
        let new = chrono::DateTime::parse_from_rfc3339(&observation.observed_at)
            .map_err(|e| Error::Corrupt(e.to_string()))?;
        if previous.host_session_id != parent || new < old {
            return Err(Error::Rejected(
                "host observation regressed or belongs to another parent".into(),
            ));
        }
        if previous == *observation {
            let events = store.read_events()?;
            if events
                .iter()
                .rev()
                .find(|e| {
                    matches!(
                        e.kind.as_str(),
                        "host_observed" | "host_observation_required"
                    )
                })
                .is_some_and(|e| e.kind == "host_observed")
            {
                return Ok(());
            }
        }
    }
    store.append_event(clock, "host_observed", serde_json::json!(observation))?;
    Ok(())
}
pub fn latest(store: &Store) -> Result<Option<HostObservation>> {
    store.verify_chain()?;
    store
        .read_events()?
        .into_iter()
        .rev()
        .find(|e| e.kind == "host_observed")
        .map(|e| serde_json::from_value(e.data).map_err(|e| Error::Corrupt(e.to_string())))
        .transpose()
}
pub fn current(store: &Store, parent: &str, now: &str) -> Result<HostObservation> {
    let observation = latest(store)?.ok_or_else(|| {
        Error::Rejected("host_stale: provide host_observation before dispatch".into())
    })?;
    observation.validate(parent, now)?;
    let events = store.read_events()?;
    let observed = events
        .iter()
        .rev()
        .find(|e| e.kind == "host_observed")
        .map(|e| e.seq)
        .unwrap_or(0);
    if events
        .iter()
        .any(|e| e.kind == "host_observation_required" && e.seq > observed)
    {
        return Err(Error::Rejected(
            "host_stale: refresh host_observation after the rejected update".into(),
        ));
    }
    Ok(observation)
}
