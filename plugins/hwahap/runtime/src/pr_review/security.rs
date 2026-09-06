//! Required security coverage, including hypotheses without a known CVE.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityArea {
    Authorization,
    UntrustedInput,
    Secrets,
    SupplyChain,
    StateIntegrity,
    ResourceExhaustion,
}

pub const SECURITY_AREAS: [SecurityArea; 6] = [
    SecurityArea::Authorization,
    SecurityArea::UntrustedInput,
    SecurityArea::Secrets,
    SecurityArea::SupplyChain,
    SecurityArea::StateIntegrity,
    SecurityArea::ResourceExhaustion,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityStatus {
    Checked,
    NotApplicable,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityCheck {
    pub area: SecurityArea,
    pub status: SecurityStatus,
    pub evidence: Vec<String>,
    pub finding_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityReview {
    /// Assets, attacker-controlled entry points, trust boundaries and assumptions.
    pub threat_model: Vec<String>,
    pub checks: Vec<SecurityCheck>,
}

impl SecurityReview {
    pub fn validate(&self, findings: &[&Finding]) -> Result<()> {
        validate_evidence(&self.threat_model)?;
        let ids: BTreeSet<_> = findings.iter().map(|f| f.id.trim()).collect();
        let mut areas = BTreeSet::new();
        for check in &self.checks {
            validate_evidence(&check.evidence)?;
            if !areas.insert(check.area) {
                return Err(Error::Rejected("duplicate security area".into()));
            }
            let mut linked = BTreeSet::new();
            for id in &check.finding_ids {
                if !ids.contains(id.trim()) || !linked.insert(id.trim()) {
                    return Err(Error::Rejected(
                        "unknown or duplicate security finding".into(),
                    ));
                }
            }
            if check.status == SecurityStatus::NotApplicable && !linked.is_empty() {
                return Err(Error::Rejected(
                    "inapplicable security area has findings".into(),
                ));
            }
        }
        if areas != SECURITY_AREAS.into_iter().collect() {
            return Err(Error::Rejected(
                "security review must cover all six areas".into(),
            ));
        }
        Ok(())
    }

    pub fn blocked(&self) -> bool {
        self.checks
            .iter()
            .any(|c| c.status == SecurityStatus::Blocked)
    }
}

/// Prompt template: placeholders are deliberately not a completed security review.
pub fn security_example() -> serde_json::Value {
    serde_json::json!({
        "threat_model": ["replace with assets, attacker control, boundaries and assumptions"],
        "checks": SECURITY_AREAS.map(|area| serde_json::json!({
            "area": area, "status": "blocked", "finding_ids": [],
            "evidence": ["replace with inspected path/test and result, inapplicability reason or blocker"]
        }))
    })
}

#[cfg(test)]
pub(super) fn checked() -> SecurityReview {
    SecurityReview {
        threat_model: vec!["controlled fixture: local caller, repository and output".into()],
        checks: SECURITY_AREAS
            .map(|area| SecurityCheck {
                area,
                status: SecurityStatus::Checked,
                evidence: vec!["controlled source inspection".into()],
                finding_ids: vec![],
            })
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_cannot_omit_duplicate_or_invent_areas() {
        let mut s = checked();
        assert!(s.validate(&[]).is_ok());
        s.checks.pop();
        assert!(s.validate(&[]).is_err());
        s = checked();
        s.checks[0] = s.checks[1].clone();
        assert!(s.validate(&[]).is_err());
        let mut json = serde_json::to_value(checked()).unwrap();
        json["checks"][0]["area"] = "other".into();
        assert!(serde_json::from_value::<SecurityReview>(json).is_err());
    }

    #[test]
    fn evidence_references_and_unknowns_are_not_clean_results() {
        let mut s = checked();
        s.threat_model.clear();
        assert!(s.validate(&[]).is_err());
        s = checked();
        s.checks[0].status = SecurityStatus::NotApplicable;
        s.checks[0].evidence = vec![" ".into()];
        assert!(s.validate(&[]).is_err());
        s = checked();
        s.checks[0].finding_ids.push("missing".into());
        assert!(s.validate(&[]).is_err());
        s = checked();
        s.checks[0].status = SecurityStatus::Blocked;
        assert!(s.validate(&[]).is_ok());
        assert!(s.blocked());
        assert!(!checked().blocked());
    }
}
