//! Transport-independent sessions. Native dispatch evidence is not an adapter echo.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::profile::{Effort, Profile, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    WorkspaceWrite,
}

pub fn access_for(role: Role) -> Access {
    match role {
        Role::Implementer | Role::Rework => Access::WorkspaceWrite,
        _ => Access::ReadOnly,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSpec {
    pub cwd: PathBuf,
    pub role: Role,
    pub unit: Option<String>,
    pub prompt: String,
}

/// Only copy counters actually exposed by the native tool. Missing counters stay absent.
/// Cached input is a subset of input; reasoning output is already part of output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
}

impl TokenUsage {
    pub fn verify(&self) -> Result<()> {
        if self.cached_input_tokens > self.input_tokens {
            return Err(Error::Rejected("cached input exceeds total input".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeReceipt {
    pub selection: crate::catalog::Selection,
    pub dispatch_id: String,
    pub agent_id: String,
    pub profile: Profile,
    pub role: Role,
    pub unit: Option<String>,
    pub model_requested: String,
    pub effort_requested: Effort,
    pub elapsed_ms: u64,
    pub reported_usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "evidence_source",
    content = "details",
    rename_all = "snake_case"
)]
pub enum SessionReceipt {
    /// The host relayed an explicit native spawn request. Applied model is not independently known.
    Native(NativeReceipt),
}

impl SessionReceipt {
    pub fn verify(&self) -> Result<()> {
        match self {
            Self::Native(receipt) => {
                receipt.selection.verify()?;
                if receipt.selection.role != receipt.role.as_str()
                    || receipt.selection.unit != receipt.unit
                    || receipt.selection.model != receipt.model_requested
                    || receipt.selection.effort != receipt.effort_requested.as_str()
                    || receipt.profile != receipt.role.profile()
                    || receipt.dispatch_id.trim().is_empty()
                    || receipt.agent_id.trim().is_empty()
                    || receipt.model_requested.trim().is_empty()
                {
                    return Err(Error::UnsupportedProfile(format!(
                        "native dispatch evidence differs from selection for {} / {}",
                        receipt.model_requested,
                        receipt.effort_requested.as_str()
                    )));
                }
                if let Some(usage) = &receipt.reported_usage {
                    usage.verify()?;
                }
                Ok(())
            }
        }
    }

    pub fn verify_for(
        &self,
        spec: &SessionSpec,
        snapshot: &crate::catalog::CatalogSnapshot,
    ) -> Result<()> {
        self.verify()?;
        let Self::Native(receipt) = self;
        receipt.selection.verify_catalog(snapshot)?;
        if receipt.role != spec.role || receipt.unit != spec.unit {
            return Err(Error::UnsupportedProfile(
                "native result role or unit differs from its dispatch".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SessionOutcome {
    pub final_message: String,
    pub transcript: String,
    pub receipt: SessionReceipt,
    pub stop_reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_usage_and_applied_model_are_not_invented() {
        let snapshot =
            crate::catalog::CatalogSnapshot::new("run-1", crate::catalog::bundled()).unwrap();
        let observation = crate::catalog::HostObservation {
            host_session_id: "parent".into(),
            observed_at: "2026-09-07T00:00:00Z".into(),
            source: "unit fixture".into(),
            parent_model: "gpt-6-astra".into(),
            parent_effort: "high".into(),
            available_slots: 1,
            models: [(
                "gpt-6-astra".into(),
                crate::catalog::ObservedModel {
                    efforts: vec!["high".into()],
                    tools: vec!["exec_command".into()],
                },
            )]
            .into(),
        };
        let receipt = SessionReceipt::Native(NativeReceipt {
            selection: crate::catalog::Selection::new(
                &snapshot,
                &observation,
                Role::FinalReview,
                None,
                "gpt-6-astra",
                "high",
            )
            .unwrap(),
            dispatch_id: "run-1-job-1".into(),
            agent_id: "native-1".into(),
            profile: Profile::Deep,
            role: Role::FinalReview,
            unit: None,
            model_requested: "gpt-6-astra".into(),
            effort_requested: Effort::High,
            elapsed_ms: 100,
            reported_usage: None,
        });
        receipt.verify().unwrap();
        let value = serde_json::to_value(receipt).unwrap();
        assert!(value["details"]["reported_usage"].is_null());
        assert!(value["details"].get("model_applied").is_none());
        assert_eq!(value["evidence_source"], "native");
    }

    #[test]
    fn impossible_cache_counters_are_rejected() {
        assert!(TokenUsage {
            input_tokens: 3,
            output_tokens: 1,
            cached_input_tokens: 4
        }
        .verify()
        .is_err());
    }
}
