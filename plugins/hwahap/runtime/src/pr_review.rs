//! Evidence for two independent reviews of one published commit.
use crate::{canonical::Digest, Error, Result};
use serde::{Deserialize, Serialize};
mod defense;
pub use defense::*;
mod security;
pub use security::*;
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewBinding {
    pub pr_url: String,
    pub head: String,
    pub contract_digest: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: String,
    pub file: String,
    pub line: u32,
    pub condition: String,
    pub expected: String,
    pub observed: String,
    pub evidence: Vec<String>,
}

impl Finding {
    pub fn validate(&self) -> Result<()> {
        let text = [
            &self.id,
            &self.file,
            &self.condition,
            &self.expected,
            &self.observed,
        ];
        if text.iter().any(|s| s.trim().is_empty()) || self.line == 0 {
            return Err(Error::Rejected(
                "finding requires identity, location and reproduction details".into(),
            ));
        }
        if Path::new(&self.file)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(Error::Rejected(
                "finding file must be a repository-relative path".into(),
            ));
        }
        validate_evidence(&self.evidence)
    }
}

pub fn validate_evidence(evidence: &[String]) -> Result<()> {
    if evidence.is_empty() || evidence.iter().any(|s| s.trim().is_empty()) {
        return Err(Error::Rejected("review evidence must be nonempty".into()));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttackReport {
    pub binding: ReviewBinding,
    pub findings: Vec<Finding>,
    pub security: SecurityReview,
    /// Evidence of checked behavior is required even when no defects were found.
    pub evidence: Vec<String>,
}

impl AttackReport {
    pub fn validate(&self, expected: &ReviewBinding) -> Result<()> {
        if &self.binding != expected
            || expected.pr_url.trim().is_empty()
            || expected.head.trim().is_empty()
        {
            return Err(Error::Rejected(
                "review does not match the published PR head and contract".into(),
            ));
        }
        validate_evidence(&self.evidence)?;
        let mut ids = BTreeSet::new();
        for finding in &self.findings {
            finding.validate()?;
            if !ids.insert(finding.id.trim()) {
                return Err(Error::Rejected("duplicate finding identity".into()));
            }
        }
        self.security
            .validate(&self.findings.iter().collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn report() -> AttackReport {
        AttackReport {
            binding: ReviewBinding {
                pr_url: "https://github.com/a/b/pull/1".into(),
                head: "a".repeat(40),
                contract_digest: Digest::of_bytes(b"contract"),
            },
            findings: vec![Finding {
                id: "A1".into(),
                file: "src/main.rs".into(),
                line: 4,
                condition: "retry after a crash".into(),
                expected: "same operation".into(),
                observed: "duplicate operation".into(),
                evidence: vec!["reproduction output".into()],
            }],
            security: security::checked(),
            evidence: vec!["inspected retry path".into()],
        }
    }
    #[test]
    fn legacy_reports_cannot_supply_security_clearance() {
        let r = report();
        let mut json = serde_json::to_value(&r).unwrap();
        json.as_object_mut().unwrap().remove("security");
        assert!(serde_json::from_value::<AttackReport>(json).is_err());
        let defense = serde_json::json!({
            "binding": r.binding, "assessments": [],
            "additional_findings": [], "evidence": ["old clean review"]
        });
        assert!(serde_json::from_value::<DefenseReport>(defense).is_err());
    }
    #[test]
    fn accepts_bound_reproduction_and_evidenced_clean_review() {
        let mut r = report();
        assert!(r.validate(&r.binding).is_ok());
        r.findings.clear();
        assert!(r.validate(&r.binding).is_ok());
    }
    #[test]
    fn rejects_stale_head_and_contract() {
        let r = report();
        let mut expected = r.binding.clone();
        expected.head = "b".repeat(40);
        assert!(r.validate(&expected).is_err());
        expected = r.binding.clone();
        expected.contract_digest = Digest::of_bytes(b"changed");
        assert!(r.validate(&expected).is_err());
    }
    #[test]
    fn rejects_duplicate_missing_identity_and_empty_evidence() {
        let mut r = report();
        r.findings.push(r.findings[0].clone());
        assert!(r.validate(&r.binding).is_err());
        r.findings.pop();
        r.findings[0].id.clear();
        assert!(r.validate(&r.binding).is_err());
        r = report();
        r.findings[0].evidence.clear();
        assert!(r.validate(&r.binding).is_err());
        r = report();
        r.evidence = vec![" ".into()];
        assert!(r.validate(&r.binding).is_err());
    }
    #[test]
    fn rejects_escaping_locations_and_incomplete_reproduction() {
        for file in ["../outside", "/absolute", "src/../other"] {
            let mut r = report();
            r.findings[0].file = file.into();
            assert!(r.validate(&r.binding).is_err());
        }
        let mut r = report();
        r.findings[0].condition.clear();
        assert!(r.validate(&r.binding).is_err());
        r = report();
        r.findings[0].line = 0;
        assert!(r.validate(&r.binding).is_err());
    }
}
