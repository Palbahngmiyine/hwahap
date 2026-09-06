//! Three reusable child identities per parent task, separated by authorship and model.
use super::{json, NativeDispatch};
use crate::{
    canonical::Digest,
    error::{Error, Result},
    profile::Role,
    state::Store,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeLane {
    #[default]
    Worker,
    Critic,
    Auditor,
    Coordinator,
}

impl NativeLane {
    pub fn for_role(role: Role) -> Self {
        match role {
            Role::FactFinder | Role::Implementer => Self::Worker,
            Role::PlanCritic | Role::UnitReviewer | Role::FailureDiagnosis => Self::Critic,
            Role::ColdConsumer | Role::FinalReview => Self::Auditor,
            Role::Recommender | Role::PlanSynthesis | Role::ConflictReplan | Role::Rework => {
                Self::Coordinator
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Agent {
    id: String,
    model: String,
    effort: String,
    dispatch_id: String,
    stopped: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pool {
    agents: BTreeMap<NativeLane, Agent>,
}

fn path(store: &Store, scope: &str) -> std::path::PathBuf {
    store.root().join(format!(
        "native-pool-{}.json",
        Digest::of_bytes(scope.as_bytes())
    ))
}

fn load(store: &Store, scope: &str) -> Result<Pool> {
    match std::fs::read(path(store, scope)) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(format!("native pool: {e}")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Pool::default()),
        Err(e) => Err(Error::io(path(store, scope), e)),
    }
}

/// Read-only scheduling: a busy, missing, or changed identity is never silently replaced.
pub fn reusable(store: &Store, dispatch: &NativeDispatch) -> Result<Option<String>> {
    let pool = load(store, &dispatch.pool_scope)?;
    let Some(agent) = pool.agents.get(&dispatch.lane) else {
        return Ok(None);
    };
    if agent.model != dispatch.model || agent.effort != dispatch.effort || !agent.stopped {
        return Err(Error::Rejected(
            "native lane is busy or its model/effort changed; do not spawn a replacement".into(),
        ));
    }
    Ok(Some(agent.id.clone()))
}

/// Check before changing pending, so a refused cross-lane identity cannot poison it.
pub fn check_registration(store: &Store, dispatch: &NativeDispatch, id: &str) -> Result<()> {
    checked_registration(store, dispatch, id).map(|_| ())
}

fn checked_registration(store: &Store, dispatch: &NativeDispatch, id: &str) -> Result<Pool> {
    if dispatch.pool_scope.is_empty() {
        return Err(Error::Rejected(
            "native dispatch parent scope is missing".into(),
        ));
    }
    if dispatch.lane == NativeLane::Coordinator {
        return if id == "coordinator" {
            Ok(Pool::default())
        } else {
            Err(Error::Rejected(
                "pooled planning and repair require the Astra coordinator".into(),
            ))
        };
    }
    if dispatch
        .reuse_agent_id
        .as_ref()
        .is_some_and(|expected| expected != id)
    {
        return Err(Error::Rejected(
            "reuse must retain the exact native agent identity".into(),
        ));
    }
    let pool = load(store, &dispatch.pool_scope)?;
    if pool
        .agents
        .iter()
        .any(|(lane, agent)| *lane != dispatch.lane && agent.id == id)
    {
        return Err(Error::Rejected(
            "an agent cannot cross native authorship/review lanes".into(),
        ));
    }
    if let Some(agent) = pool.agents.get(&dispatch.lane) {
        if agent.id != id
            || agent.model != dispatch.model
            || agent.effort != dispatch.effort
            || (!agent.stopped && agent.dispatch_id != dispatch.dispatch_id)
        {
            return Err(Error::Rejected(
                "native lane already owns another identity or active turn".into(),
            ));
        }
    } else if pool.agents.len() >= 3 || dispatch.reuse_agent_id.is_some() {
        return Err(Error::Rejected(
            "native pool identity is missing or its three-child limit was reached".into(),
        ));
    }
    Ok(pool)
}

/// Pending registration is saved first; failed pool persistence retries that same identity.
pub fn register(store: &Store, dispatch: &NativeDispatch, id: &str) -> Result<()> {
    write_agent(store, dispatch, id, false)
}

fn write_agent(store: &Store, dispatch: &NativeDispatch, id: &str, stopped: bool) -> Result<()> {
    let mut pool = checked_registration(store, dispatch, id)?;
    if dispatch.lane == NativeLane::Coordinator {
        return Ok(());
    }
    pool.agents.insert(
        dispatch.lane,
        Agent {
            id: id.into(),
            model: dispatch.model.clone(),
            effort: dispatch.effort.clone(),
            dispatch_id: dispatch.dispatch_id.clone(),
            stopped,
        },
    );
    store.write_atomic(&path(store, &dispatch.pool_scope), &json(&pool)?)
}

/// Only durable completion or exact stop acknowledgment makes a lane reusable.
pub fn stopped(store: &Store, dispatch: &NativeDispatch) -> Result<()> {
    if dispatch.lane == NativeLane::Coordinator {
        return Ok(());
    }
    let Some(id) = &dispatch.agent_id else {
        return Ok(());
    };
    write_agent(store, dispatch, id, true)
}
