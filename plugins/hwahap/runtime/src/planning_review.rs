//! Typed planning findings and independent resolution certificates.
use crate::{
    agentresult::{parse_strict, Verdict},
    canonical::Digest,
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Choice,
    Fact,
    Structure,
    Blocker,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Open,
    Resolved,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanningFinding {
    pub id: String,
    #[serde(deserialize_with = "crate::required_option")]
    pub parent_id: Option<String>,
    pub kind: FindingKind,
    pub targets: Vec<String>,
    pub evidence: Vec<String>,
    pub expected: String,
    pub status: FindingStatus,
    #[serde(default)]
    pub depends_on: Vec<String>,
}
impl PlanningFinding {
    pub fn summary(&self) -> String {
        format!("{}: {}", self.id, self.expected)
    }
    pub fn same_issue(&self, other: &Self) -> bool {
        self.id == other.id
            && self.parent_id == other.parent_id
            && self.kind == other.kind
            && self.targets == other.targets
            && self.expected == other.expected
            && self.depends_on == other.depends_on
    }
}

/// Proposed resolutions become authoritative only after two fresh independent reviews.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanningResolution {
    pub finding_ids: Vec<String>,
    pub action: String,
    pub evidence: Vec<String>,
    pub decision_ids: Vec<String>,
    pub revision: u32,
    pub reviewed: Digest,
    pub attempt: Option<u64>,
    pub validation: Vec<String>,
    pub prior_structure: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanningReviewResult {
    pub verdict: Verdict,
    #[serde(default)]
    pub findings: Vec<PlanningFinding>,
}
impl PlanningReviewResult {
    pub const CONTRACT: &'static str = r#"{"verdict":"pass|fail","findings":[{"id":"CC1","parent_id":null,"kind":"choice|fact|structure|blocker","targets":["U1"],"evidence":["current source or resolution evidence"],"expected":"observable correction","status":"open|resolved","depends_on":[]}]}"#;
    pub fn parse(text: &str, known: &[PlanningFinding]) -> Result<Self> {
        let result: Self = parse_strict(text, Self::CONTRACT)?;
        validate_findings(&result.findings, known)?;
        let open = result
            .findings
            .iter()
            .any(|f| f.status == FindingStatus::Open);
        if (result.verdict == Verdict::Pass) == open {
            return Err(Error::Rejected(
                "planning verdict must match open findings".into(),
            ));
        }
        for f in &result.findings {
            if let Some(old) = known.iter().find(|old| old.id == f.id) {
                if !f.same_issue(old) {
                    return Err(Error::Rejected(format!(
                        "finding {} changed identity",
                        f.id
                    )));
                }
            } else if f.status == FindingStatus::Resolved {
                return Err(Error::Rejected(format!("unknown resolution {}", f.id)));
            }
        }
        if result.verdict == Verdict::Pass {
            require_resolutions(known, &result.findings)?;
        }
        Ok(result)
    }
}

pub fn require_resolutions(known: &[PlanningFinding], reviewed: &[PlanningFinding]) -> Result<()> {
    for old in known {
        if old.status != FindingStatus::Resolved
            || !reviewed.iter().any(|f| {
                f.same_issue(old)
                    && f.status == FindingStatus::Resolved
                    && !f.evidence.is_empty()
                    && f.evidence.iter().all(|e| !e.trim().is_empty())
            })
        {
            return Err(Error::Rejected(format!(
                "finding {} needs resolution evidence in both fresh reviews",
                old.id
            )));
        }
    }
    Ok(())
}

pub fn validate_findings(findings: &[PlanningFinding], known: &[PlanningFinding]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for f in findings {
        if f.id.trim().is_empty()
            || !ids.insert(&f.id)
            || f.targets.is_empty()
            || f.targets.iter().any(|t| t.trim().is_empty())
            || f.expected.trim().is_empty()
            || f.evidence.is_empty()
            || f.evidence.iter().any(|e| e.trim().is_empty())
        {
            return Err(Error::Rejected(
                "planning findings need unique IDs, targets, evidence and expected results".into(),
            ));
        }
    }
    let mut all = known.to_vec();
    for f in findings {
        if let Some(old) = all.iter_mut().find(|old| old.id == f.id) {
            *old = f.clone();
        } else {
            all.push(f.clone());
        }
    }
    for f in &all {
        for id in f.depends_on.iter().chain(f.parent_id.iter()) {
            if id == &f.id || !all.iter().any(|v| &v.id == id) {
                return Err(Error::Rejected(format!(
                    "invalid finding link {} -> {id}",
                    f.id
                )));
            }
        }
    }
    // A parent depends on its children; explicit prerequisites point in the same direction.
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for f in &all {
            if f.depends_on.iter().all(|id| done.contains(id))
                && all
                    .iter()
                    .filter(|child| child.parent_id.as_ref() == Some(&f.id))
                    .all(|child| done.contains(&child.id))
            {
                done.insert(f.id.clone());
            }
        }
        if done.len() == all.len() {
            return Ok(());
        }
        if done.len() == before {
            return Err(Error::Rejected(
                "finding dependencies contain a cycle".into(),
            ));
        }
    }
}

/// Reserve before dispatch. Replaying an interrupted dispatch reuses its reservation.
pub fn reserve_structure_attempt(
    store: &crate::state::Store,
    clock: &dyn crate::clock::Clock,
    run_id: &str,
    reviewed: &Digest,
    finding_ids: &[String],
) -> Result<u64> {
    let events = store.read_events()?;
    let starts: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "planning_structure_attempt" && e.data["run_id"] == run_id)
        .collect();
    if let Some(event) = starts.iter().rev().find(|e| {
        e.data["reviewed"] == serde_json::json!(reviewed)
            && e.data["finding_ids"] == serde_json::json!(finding_ids)
            && !events.iter().any(|end| {
                end.kind == "planning_structure_finished"
                    && end.data["run_id"] == run_id
                    && end.data["attempt"] == e.data["attempt"]
            })
    }) {
        return event.data["attempt"]
            .as_u64()
            .ok_or_else(|| Error::Corrupt("invalid planning attempt".into()));
    }
    if starts.len() >= 3 {
        return Err(Error::Rejected(
            "구조 수정 3회가 끝났습니다. 미해결 지적과 마지막 검증 결과를 확인해 주세요.".into(),
        ));
    }
    let attempt = starts.len() as u64 + 1;
    store.append_event(
        clock,
        "planning_structure_attempt",
        serde_json::json!({
            "run_id":run_id,"reviewed":reviewed,"finding_ids":finding_ids,"attempt":attempt
        }),
    )?;
    Ok(attempt)
}

pub fn merge_open(plan: &mut crate::plan::Plan, findings: &[PlanningFinding]) -> Result<()> {
    let reviewed = plan.review_digest()?;
    let mut unique: Vec<PlanningFinding> = Vec::new();
    for f in findings.iter().filter(|f| f.status == FindingStatus::Open) {
        if let Some(old) = unique.iter_mut().find(|old| old.id == f.id) {
            if !old.same_issue(f) {
                return Err(Error::Rejected(format!(
                    "finding {} changed identity",
                    f.id
                )));
            }
            for evidence in &f.evidence {
                if !old.evidence.contains(evidence) {
                    old.evidence.push(evidence.clone());
                }
            }
        } else {
            unique.push(f.clone());
        }
    }
    validate_findings(&unique, &plan.planning_findings)?;
    for f in unique {
        if let Some(old) = plan.planning_findings.iter_mut().find(|old| old.id == f.id) {
            if !old.same_issue(&f) {
                return Err(Error::Rejected(format!(
                    "finding {} changed identity",
                    f.id
                )));
            }
            if old.status == FindingStatus::Resolved {
                plan.decomposition_history.push(PlanningResolution {
                    finding_ids: vec![f.id.clone()],
                    action: "reopened".into(),
                    evidence: f.evidence.clone(),
                    decision_ids: Vec::new(),
                    revision: plan.revision,
                    reviewed: reviewed.clone(),
                    attempt: None,
                    validation: Vec::new(),
                    prior_structure: None,
                });
            }
            *old = f;
        } else {
            plan.planning_findings.push(f);
        }
    }
    Ok(())
}

pub fn structure_value(plan: &crate::plan::Plan) -> serde_json::Value {
    serde_json::json!({"requirements":plan.requirements,"acceptance":plan.acceptance,
        "units":plan.units,"tests":plan.tests,"full_suite":plan.full_suite,
        "verification_inputs":plan.verification_inputs})
}
