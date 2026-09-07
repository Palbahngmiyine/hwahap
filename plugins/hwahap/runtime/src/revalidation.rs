//! Durable implementation evidence, direct repair obligations and attempt budgets.
use crate::{canonical::Digest, clock::Clock, plan::Plan, state::Store, Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImplementationRecord {
    pub run_id: String,
    pub unit_id: String,
    pub fingerprint: Digest,
    pub commit: String,
    pub tree: String,
    pub verification_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairObligation {
    pub id: Digest,
    pub run_id: String,
    pub unit_ids: Vec<String>,
    pub evidence_kind: String,
    pub evidence_ref: String,
    pub contract_digest: Digest,
}

pub fn implementations(store: &Store, run_id: &str) -> Result<Vec<ImplementationRecord>> {
    store.verify_chain()?;
    store
        .read_events()?
        .into_iter()
        .filter(|e| e.kind == "implementation_completed" && e.data["run_id"] == run_id)
        .map(|e| serde_json::from_value(e.data).map_err(|e| Error::Corrupt(e.to_string())))
        .collect()
}

pub fn obligations(store: &Store, run_id: &str, unit_id: &str) -> Result<Vec<RepairObligation>> {
    store.verify_chain()?;
    let events = store.read_events()?;
    let mut found = Vec::new();
    for e in events
        .iter()
        .filter(|e| e.kind == "repair_obligation" && e.data["run_id"] == run_id)
    {
        let item: RepairObligation =
            serde_json::from_value(e.data.clone()).map_err(|e| Error::Corrupt(e.to_string()))?;
        if item.unit_ids.iter().any(|id| id == unit_id)
            && !events.iter().any(|e| {
                e.kind == "repair_obligation_resolved"
                    && e.data["run_id"] == run_id
                    && e.data["id"] == serde_json::json!(item.id)
                    && e.data["unit_id"] == unit_id
            })
        {
            found.push(item);
        }
    }
    Ok(found)
}

pub fn record_obligation(
    store: &Store,
    clock: &dyn Clock,
    run_id: &str,
    plan: &Plan,
    mut unit_ids: Vec<String>,
    kind: &str,
    evidence_ref: &str,
) -> Result<RepairObligation> {
    unit_ids.sort();
    unit_ids.dedup();
    if unit_ids.is_empty()
        || unit_ids
            .iter()
            .any(|id| plan.unit(id).is_none_or(|u| u.probe))
        || kind.trim().is_empty()
        || evidence_ref.trim().is_empty()
    {
        return Err(Error::Rejected(
            "repair obligation needs existing units and evidence".into(),
        ));
    }
    let contract_digest = plan.digest()?;
    let id = Digest::of(&(run_id, &unit_ids, kind, evidence_ref, &contract_digest))?;
    let record = RepairObligation {
        id,
        run_id: run_id.into(),
        unit_ids,
        evidence_kind: kind.into(),
        evidence_ref: evidence_ref.into(),
        contract_digest,
    };
    let data = serde_json::json!(record);
    if !store
        .read_events()?
        .iter()
        .any(|e| e.kind == "repair_obligation" && e.data == data)
    {
        store.append_event(clock, "repair_obligation", data)?;
    }
    Ok(record)
}

/// Compare directory boundaries, retaining every overlapping owner.
pub fn owners(plan: &Plan, file: &str) -> Vec<String> {
    plan.units
        .iter()
        .filter(|u| !u.probe && crate::git::paths_outside(&u.paths, &[file.into()]).is_empty())
        .map(|u| u.id.clone())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptKind {
    Implementation,
    Revalidation,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitAttempt {
    pub id: Digest,
    pub run_id: String,
    pub unit_id: String,
    pub kind: AttemptKind,
    pub round: Digest,
    pub ordinal: u32,
    pub total: u64,
}

/// Local retry rounds share a durable, per-unit run ceiling from native_max_calls.
pub fn reserve_attempt(
    store: &Store,
    clock: &dyn Clock,
    run_id: &str,
    unit: &str,
    kind: AttemptKind,
    round: &Digest,
    limit: u64,
) -> Result<UnitAttempt> {
    let events = store.read_events()?;
    let starts: Vec<UnitAttempt> = events
        .iter()
        .filter(|e| {
            e.kind == "unit_attempt_started"
                && e.data["run_id"] == run_id
                && e.data["unit_id"] == unit
                && e.data["kind"] == serde_json::json!(kind)
        })
        .map(|e| serde_json::from_value(e.data.clone()).map_err(|e| Error::Corrupt(e.to_string())))
        .collect::<Result<_>>()?;
    if let Some(pending) = starts.iter().find(|a| {
        !events
            .iter()
            .any(|e| e.kind == "unit_attempt_finished" && e.data["id"] == serde_json::json!(a.id))
    }) {
        if &pending.round == round {
            return Ok(pending.clone());
        }
        return Err(Error::Rejected(
            "unfinished unit attempt belongs to a different candidate; recover its evidence first"
                .into(),
        ));
    }
    let ordinal = starts.iter().filter(|a| &a.round == round).count() as u32 + 1;
    if starts.len() as u64 >= limit || ordinal > 2 {
        return Err(Error::ExecutionLimit(format!(
            "{unit}: unit attempt budget exhausted; previous attempts retained"
        )));
    }
    let total = starts.len() as u64 + 1;
    let attempt = UnitAttempt {
        id: Digest::of(&(run_id, unit, &kind, round, ordinal, total))?,
        run_id: run_id.into(),
        unit_id: unit.into(),
        kind,
        round: round.clone(),
        ordinal,
        total,
    };
    store.append_event(clock, "unit_attempt_started", serde_json::json!(attempt))?;
    Ok(attempt)
}

pub fn finish_attempt(
    store: &Store,
    clock: &dyn Clock,
    attempt: &UnitAttempt,
    passed: bool,
    evidence: serde_json::Value,
) -> Result<()> {
    let data = serde_json::json!({"id":attempt.id,"run_id":attempt.run_id,"unit_id":attempt.unit_id,"passed":passed,"evidence":evidence});
    if let Some(old) = store
        .read_events()?
        .iter()
        .find(|e| e.kind == "unit_attempt_finished" && e.data["id"] == data["id"])
    {
        if old.data != data {
            return Err(Error::Corrupt("conflicting unit attempt completion".into()));
        }
    } else {
        store.append_event(clock, "unit_attempt_finished", data)?;
    }
    Ok(())
}

pub fn previous_findings(store: &Store, attempt: &UnitAttempt) -> Result<Option<Vec<String>>> {
    let events = store.read_events()?;
    let previous = events.iter().rev().find(|e| {
        e.kind == "unit_attempt_started"
            && e.data["run_id"] == attempt.run_id
            && e.data["unit_id"] == attempt.unit_id
            && e.data["kind"] == serde_json::json!(attempt.kind)
            && e.data["round"] == serde_json::json!(attempt.round)
            && e.data["ordinal"]
                .as_u64()
                .is_some_and(|n| n < u64::from(attempt.ordinal))
    });
    let Some(previous) = previous else {
        return Ok(None);
    };
    let findings = events
        .iter()
        .find(|e| e.kind == "unit_attempt_finished" && e.data["id"] == previous.data["id"])
        .and_then(|e| e.data["evidence"].get("findings"));
    findings
        .map(|v| serde_json::from_value(v.clone()).map_err(|e| Error::Corrupt(e.to_string())))
        .transpose()
}
