//! Journal-backed verification obligations; snapshots are derived caches.
pub mod inputs;
use crate::{canonical::Digest, clock::Clock, plan::SCHEMA, state::Store, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Unit,
    Revalidation,
    FinalUnit,
    FullSuite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRequest {
    pub run_id: String,
    pub unit_id: Option<String>,
    pub test_id: Option<String>,
    pub kind: Kind,
    pub contract: Digest,
    pub command: String,
    pub cwd: String,
    pub head: String,
    pub tree: String,
    pub code: Digest,
    pub inputs: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Started,
    Passed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRecord {
    pub schema: String,
    pub id: String,
    pub attempt: u64,
    pub request: VerificationRequest,
    pub status: Status,
    pub exit_code: Option<i32>,
    pub output_digest: Option<Digest>,
}

fn rejected(message: &str) -> Error {
    Error::Rejected(message.into())
}
fn output_name(id: &str) -> Result<String> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(rejected("invalid verification id"));
    }
    Ok(format!("verification-{id}.output"))
}

/// Replay complete events. An unfinished command remains Started until explicit stop recovery.
pub fn recover_verifications(store: &Store) -> Result<BTreeMap<String, VerificationRecord>> {
    store.read_run()?;
    store.verify_chain()?;
    let mut records: BTreeMap<String, VerificationRecord> = BTreeMap::new();
    let mut history: BTreeMap<String, Vec<VerificationRecord>> = BTreeMap::new();
    for event in store.read_events()? {
        if event.kind != "verification_started" && event.kind != "verification_finished" {
            continue;
        }
        let record: VerificationRecord = serde_json::from_value(event.data)
            .map_err(|e| Error::Corrupt(format!("verification event: {e}")))?;
        output_name(&record.id)?;
        if record.schema != SCHEMA {
            return Err(rejected("unsupported verification schema"));
        }
        if event.kind == "verification_started" {
            if record.status != Status::Started || records.contains_key(&record.id) {
                return Err(rejected("conflicting verification start"));
            }
        } else {
            let prior = records
                .get(&record.id)
                .ok_or_else(|| rejected("verification has no start"))?;
            if prior.request != record.request
                || prior.attempt != record.attempt
                || prior.status != Status::Started
                || record.status == Status::Started
            {
                return Err(rejected("conflicting verification completion"));
            }
            if record.status == Status::Passed && record.exit_code != Some(0) {
                return Err(rejected("passed verification requires exit code zero"));
            }
            let path = store.artifacts_path().join(output_name(&record.id)?);
            let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
            if record.output_digest.as_ref() != Some(&Digest::of_bytes(&bytes)) {
                return Err(rejected("verification output is missing or changed"));
            }
        }
        history
            .entry(record.id.clone())
            .or_default()
            .push(record.clone());
        records.insert(record.id.clone(), record);
    }
    let path = store.root().join("verification.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let cached: BTreeMap<String, VerificationRecord> = serde_json::from_slice(&bytes)
                .map_err(|e| Error::Corrupt(format!("verification snapshot: {e}")))?;
            for (id, record) in &cached {
                if !history
                    .get(id)
                    .is_some_and(|versions| versions.contains(record))
                {
                    return Err(rejected(
                        "verification snapshot is ahead of or conflicts with journal",
                    ));
                }
            }
            if cached != records {
                snapshot(store, &records)?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !records.is_empty() {
                snapshot(store, &records)?;
            }
        }
        Err(e) => return Err(Error::io(&path, e)),
    }
    Ok(records)
}

fn snapshot(store: &Store, records: &BTreeMap<String, VerificationRecord>) -> Result<()> {
    let json = serde_json::to_string(records).map_err(|e| Error::Internal(e.to_string()))?;
    store.write_atomic(&store.root().join("verification.json"), &json)
}

pub fn start(
    store: &Store,
    clock: &dyn Clock,
    request: VerificationRequest,
) -> Result<VerificationRecord> {
    if let Some(run) = store.read_run()? {
        if run.run_id != request.run_id {
            return Err(rejected("verification belongs to another run"));
        }
    }
    let mut records = recover_verifications(store)?;
    if records.values().any(|r| r.status == Status::Started) {
        return Err(rejected(
            "verification recovery requires confirmation that previous commands stopped",
        ));
    }
    let attempt = records
        .values()
        .filter(|r| {
            r.request.run_id == request.run_id
                && r.request.unit_id == request.unit_id
                && r.request.test_id == request.test_id
                && r.request.kind == request.kind
        })
        .count() as u64
        + 1;
    let digest = Digest::of(&(store.read_events()?.len(), &request, attempt))?;
    let id = digest.as_str().trim_start_matches("sha256:").to_string();
    let record = VerificationRecord {
        schema: SCHEMA.into(),
        id: id.clone(),
        attempt,
        request,
        status: Status::Started,
        exit_code: None,
        output_digest: None,
    };
    store.append_event(
        clock,
        "verification_started",
        serde_json::to_value(&record).unwrap(),
    )?;
    records.insert(id, record.clone());
    snapshot(store, &records)?;
    Ok(record)
}

pub fn record_verification(
    store: &Store,
    clock: &dyn Clock,
    mut record: VerificationRecord,
    status: Status,
    exit_code: Option<i32>,
    output: &str,
) -> Result<VerificationRecord> {
    let mut records = recover_verifications(store)?;
    if status == Status::Started || (status == Status::Passed && exit_code != Some(0)) {
        return Err(rejected("invalid verification outcome"));
    }
    record.status = status;
    record.exit_code = exit_code;
    record.output_digest = Some(Digest::of_bytes(output.as_bytes()));
    let prior = records
        .get(&record.id)
        .ok_or_else(|| rejected("unknown verification"))?;
    if prior == &record {
        return Ok(record);
    }
    if prior.schema != record.schema
        || prior.status != Status::Started
        || prior.request != record.request
        || prior.attempt != record.attempt
    {
        return Err(rejected("verification completion differs from its request"));
    }
    store.write_artifact(&output_name(&record.id)?, output)?;
    store.append_event(
        clock,
        "verification_finished",
        serde_json::to_value(&record).unwrap(),
    )?;
    records.insert(record.id.clone(), record.clone());
    snapshot(store, &records)?;
    Ok(record)
}

pub fn require_current_verification(
    store: &Store,
    request: &VerificationRequest,
) -> Result<VerificationRecord> {
    recover_verifications(store)?
        .into_values()
        .filter(|r| r.request == *request)
        .max_by_key(|r| r.attempt)
        .filter(|r| r.status == Status::Passed)
        .ok_or_else(|| rejected("current verification evidence is missing or unsuccessful"))
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recovery {
    pub run_id: String,
    pub verification_id: String,
    pub all_work_stopped: bool,
    pub evidence: String,
}

pub fn acknowledge_interrupted(
    store: &Store,
    clock: &dyn Clock,
    recovery: &Recovery,
) -> Result<()> {
    let run = store
        .read_run()?
        .ok_or_else(|| rejected("no run to recover"))?;
    if run.run_id != recovery.run_id
        || !recovery.all_work_stopped
        || recovery.evidence.trim().is_empty()
    {
        return Err(rejected(
            "verification recovery needs matching run and observed stop evidence",
        ));
    }
    let record = recover_verifications(store)?
        .remove(&recovery.verification_id)
        .ok_or_else(|| rejected("unknown interrupted verification"))?;
    if record.request.run_id != run.run_id {
        return Err(rejected("verification belongs to another run"));
    }
    if record.status == Status::Interrupted {
        return Ok(());
    }
    record_verification(
        store,
        clock,
        record,
        Status::Interrupted,
        None,
        &recovery.evidence,
    )?;
    Ok(())
}

pub fn require_stopped(store: &Store) -> Result<()> {
    if let Some(record) = recover_verifications(store)?
        .values()
        .find(|r| r.status == Status::Started)
    {
        return Err(rejected(&format!(
            "verification {} needs process-stop confirmation through verification_recovery",
            record.id
        )));
    }
    Ok(())
}
