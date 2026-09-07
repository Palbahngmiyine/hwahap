//! Task requirements and deterministic native delegation decisions.
use crate::{
    canonical::Digest,
    catalog::{Depth, Requirements, Selection},
    native::NativeLane,
    profile::Role,
    Error, Result,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Risk {
    pub failure_cost: Option<u8>,
    pub reversibility: Option<u8>,
    pub blast_radius: Option<u8>,
}
impl Risk {
    pub fn high(&self) -> Result<bool> {
        let values = [self.failure_cost, self.reversibility, self.blast_radius];
        if values
            .iter()
            .any(|v| v.is_none() || v.is_some_and(|v| v > 2))
        {
            return Err(Error::Rejected(
                "assessment_missing: rate all three risk axes from 0 to 2".into(),
            ));
        }
        Ok(values.contains(&Some(2)))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Coupling {
    Independent,
    Shared,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Topology {
    pub predecessors: Vec<String>,
    pub coupling: Coupling,
    pub shared_resources: Vec<String>,
    pub writer_owner: Option<String>,
    pub separable: bool,
    pub write_paths: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRequirements {
    pub environment: String,
    pub user_authorization: String,
    pub failure_command: String,
    pub recovery_command: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskAssessment {
    pub run_id: String,
    pub unit: Option<String>,
    pub role: Role,
    pub contract_digest: String,
    pub requirements: Requirements,
    pub risk: Risk,
    pub topology: Topology,
    pub evidence: Vec<String>,
    pub recovery: Option<RecoveryRequirements>,
}
impl TaskAssessment {
    pub fn digest(&self) -> Result<String> {
        Ok(Digest::of(self)?.to_string())
    }
    /// A shared task key permits author and independent reviewers to assess the same work.
    pub fn task_digest(&self) -> Result<String> {
        Ok(Digest::of(&serde_json::json!({"run_id":self.run_id,"unit":self.unit,"contract_digest":self.contract_digest,"risk":self.risk,"topology":self.topology,"requirements":self.requirements,"evidence":self.evidence,"recovery":self.recovery}))?.to_string())
    }
    pub fn validate(&self) -> Result<()> {
        self.risk.high()?;
        if self.run_id.is_empty()
            || self.contract_digest.is_empty()
            || self.evidence.is_empty()
            || self.evidence.iter().any(|s| s.trim().is_empty())
            || self
                .requirements
                .capabilities
                .iter()
                .any(|(k, v)| !crate::catalog::identifier(k) || *v > 3)
            || self
                .topology
                .predecessors
                .iter()
                .chain(&self.topology.shared_resources)
                .any(|s| s.trim().is_empty())
        {
            return Err(Error::Rejected(
                "assessment_missing: bind task requirements, topology and source evidence".into(),
            ));
        }
        Ok(())
    }
    pub fn merged(&self, baseline: &Requirements) -> Result<Requirements> {
        self.validate()?;
        let mut combined = baseline.clone();
        for (capability, level) in &self.requirements.capabilities {
            let value = combined.capabilities.entry(capability.clone()).or_default();
            *value = (*value).max(*level);
        }
        combined.depth = combined.depth.max(self.requirements.depth);
        if self.risk.high()? {
            if matches!(
                self.role,
                Role::UnitReviewer
                    | Role::FinalReview
                    | Role::PlanCritic
                    | Role::ColdConsumer
                    | Role::FailureDiagnosis
            ) {
                for capability in ["adversarial_review", "security_review"] {
                    combined.capabilities.insert(capability.into(), 3);
                }
            }
            combined.depth =
                combined
                    .depth
                    .max(if matches!(self.role, Role::Implementer | Role::Rework) {
                        Depth::Focused
                    } else {
                        Depth::Deep
                    });
        }
        Ok(combined)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Worker,
    Coordinator,
    Replan,
    Wait,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Obligations {
    pub independent_review: bool,
    pub review_depth: Depth,
    pub recovery_validation: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegationDecision {
    pub route: Route,
    pub lane: NativeLane,
    pub selection: Option<Selection>,
    pub assessment_digest: Option<String>,
    pub task_digest: Option<String>,
    pub catalog_digest: String,
    pub host_digest: Option<String>,
    pub obligations: Obligations,
    pub reason_codes: Vec<String>,
    pub digest: String,
}
impl DelegationDecision {
    pub fn seal(&mut self) -> Result<()> {
        self.digest.clear();
        self.digest = Digest::of(self)?.to_string();
        Ok(())
    }
    pub fn verify(&self) -> Result<()> {
        let mut copy = self.clone();
        copy.seal()?;
        if copy.digest != self.digest || self.reason_codes.is_empty() {
            return Err(Error::Rejected(
                "delegation decision binding differs".into(),
            ));
        }
        Ok(())
    }
}

mod policy;
pub use policy::{decide, Context};

pub mod store;
pub use store::TaskProfile;

pub mod preflight;

pub fn waiting(
    snapshot: &crate::catalog::CatalogSnapshot,
    role: Role,
    assessment: Option<&TaskAssessment>,
    host_digest: Option<String>,
    reason: &str,
) -> Result<DelegationDecision> {
    let mut decision = DelegationDecision {
        route: Route::Wait,
        lane: NativeLane::for_role(role),
        selection: None,
        assessment_digest: assessment.map(TaskAssessment::digest).transpose()?,
        task_digest: assessment.map(TaskAssessment::task_digest).transpose()?,
        catalog_digest: snapshot.digest.to_string(),
        host_digest,
        obligations: Obligations {
            independent_review: true,
            review_depth: Depth::Deep,
            recovery_validation: assessment.is_some_and(|a| a.risk.high().unwrap_or(true)),
        },
        reason_codes: vec![reason.into()],
        digest: String::new(),
    };
    decision.seal()?;
    Ok(decision)
}

#[cfg(test)]
pub(crate) fn test_payload() -> (serde_json::Value, serde_json::Value) {
    let assessment = serde_json::json!({"run_id":"run","unit":null,"role":"recommender","contract_digest":"contract","requirements":{"capabilities":{},"depth":"routine"},"risk":{"failure_cost":0,"reversibility":0,"blast_radius":0},"topology":{"predecessors":[],"coupling":"independent","shared_resources":[],"writer_owner":null,"separable":true,"write_paths":[]},"evidence":["measurement fixture"],"recovery":null});
    let decision = serde_json::json!({"route":"wait","lane":"coordinator","selection":null,"assessment_digest":null,"task_digest":null,"catalog_digest":"catalog","host_digest":null,"obligations":{"independent_review":true,"review_depth":"deep","recovery_validation":false},"reason_codes":["fixture"],"digest":"fixture"});
    (assessment, decision)
}
