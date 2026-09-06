//! Import an explicit Codex plan implementation request without fabricating CONFIRM PLAN.
//! The host relays user text; byte binding is not independent authentication of that user.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{canonical::Digest, engine::BuildRequest, plan::Plan, Error, Result};

/// Prevent an approved-plan message from being demoted into a fresh interview.
/// Recognition of the framing is routing only; validate() still requires the complete binding.
pub(crate) fn reject_unbound_implementation_request(text: Option<&str>) -> Result<()> {
    if text.is_some_and(|s| s.trim_start().starts_with("PLEASE IMPLEMENT THIS PLAN:")) {
        return Err(Error::Rejected("Use approved_plan to bind the full Codex implementation request to its executable contract. A missing handoff is not a reason to ask for the same approval again.".into()));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanApproval {
    /// Exact body of the plan in the user's implementation request, excluding framing whitespace.
    pub markdown: String,
    pub markdown_digest: String,
    /// Full user message, including PLEASE IMPLEMENT THIS PLAN and the approved body.
    pub implementation_request: String,
    pub source_head: String,
}

impl PlanApproval {
    pub fn validate(&self) -> Result<()> {
        if self.markdown.is_empty()
            || self.markdown.trim() != self.markdown
            || self
                .implementation_request
                .strip_prefix("PLEASE IMPLEMENT THIS PLAN:")
                .map(str::trim)
                != Some(self.markdown.as_str())
            || Digest::of_bytes(self.markdown.as_bytes()).as_str() != self.markdown_digest
            || self.source_head.len() != 40
            || !self.source_head.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Rejected(
                "approved plan needs the exact implementation request, matching plan bytes and source commit; do not infer approval from agreement".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovedPlanRequest {
    pub approval: PlanApproval,
    /// Translation into executable units, independently reviewed against the approved markdown.
    pub contract: BuildRequest,
    /// Required to supersede an existing unexecuted draft; never silently replace another plan.
    pub replaces_plan_digest: Option<String>,
}

impl ApprovedPlanRequest {
    pub fn candidate(&self, id: &str, base_commit: &str) -> Result<Plan> {
        self.approval.validate()?;
        if self.contract.user_instruction != self.approval.implementation_request {
            return Err(Error::Rejected(
                "execution instruction differs from approved plan request".into(),
            ));
        }
        let mut plan = self.contract.plan(id, base_commit)?;
        plan.source_head = Some(self.approval.source_head.clone());
        plan.approved_plan = Some(self.approval.clone());
        plan.execution_branch = Some(self.contract.branch.clone());
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval() -> PlanApproval {
        let markdown = "# Plan\nPreserve original files.".to_string();
        PlanApproval {
            markdown_digest: Digest::of_bytes(markdown.as_bytes()).to_string(),
            implementation_request: format!("PLEASE IMPLEMENT THIS PLAN:\n{markdown}"),
            markdown,
            source_head: "a".repeat(40),
        }
    }

    #[test]
    fn implementation_request_binds_the_entire_plan() {
        let original = approval();
        original.validate().unwrap();
        for text in [
            "yes",
            "CONFIRM PLAN 12345678",
            "PLEASE IMPLEMENT THIS PLAN:",
        ] {
            let mut changed = original.clone();
            changed.implementation_request = text.into();
            assert!(changed.validate().is_err());
        }
        let mut changed = original.clone();
        changed.markdown.push_str("\nDelete original files.");
        changed.markdown_digest = Digest::of_bytes(changed.markdown.as_bytes()).to_string();
        assert!(changed.validate().is_err());
        let mut changed = original;
        changed.markdown_digest = Digest::zero().to_string();
        assert!(changed.validate().is_err());
    }

    #[test]
    fn explicit_empty_approval_round_trips_without_granting_authority() {
        let plan = Plan::new("current", "main", "Preserve explicit approval");
        let value = serde_json::to_value(&plan).unwrap();
        assert!(value.get("approved_plan").unwrap().is_null());
        let mut restored: Plan = serde_json::from_value(value).unwrap();
        assert_eq!(restored.digest().unwrap(), plan.digest().unwrap());
        restored.approved_plan = Some(approval());
        assert_ne!(restored.digest().unwrap(), plan.digest().unwrap());
    }
}
