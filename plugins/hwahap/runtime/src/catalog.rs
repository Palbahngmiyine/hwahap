//! Replaceable capability declarations. Availability is observed separately at dispatch time.
use crate::{canonical::Digest, profile::Role, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    Routine,
    Focused,
    Deep,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Requirements {
    pub capabilities: BTreeMap<String, u8>,
    pub depth: Depth,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEffort {
    pub name: String,
    pub depths: Vec<Depth>,
    pub preference: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub capabilities: BTreeMap<String, u8>,
    pub efforts: Vec<CatalogEffort>,
    pub preference: u32,
    pub sources: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: String,
    pub revision: String,
    pub models: Vec<Model>,
    pub role_requirements: BTreeMap<Role, Requirements>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    pub run_id: String,
    pub digest: Digest,
    pub catalog: Catalog,
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}
impl Catalog {
    pub fn validate(&self) -> Result<()> {
        let valid_caps = |caps: &BTreeMap<String, u8>| {
            !caps.is_empty() && caps.iter().all(|(k, v)| identifier(k) && *v <= 3)
        };
        if self.schema != "hwahap/catalog/v1"
            || !identifier(&self.revision)
            || self.models.is_empty()
            || self.role_requirements.len() != Role::ALL.len()
            || Role::ALL.iter().any(|r| {
                self.role_requirements
                    .get(r)
                    .is_none_or(|q| !valid_caps(&q.capabilities))
            })
        {
            return Err(Error::Rejected(
                "catalog needs its schema, revision, models and every role requirement".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        for model in &self.models {
            let mut efforts = BTreeSet::new();
            if !identifier(&model.id)
                || !ids.insert(&model.id)
                || !valid_caps(&model.capabilities)
                || model.sources.is_empty()
                || model.sources.iter().any(|s| s.trim().is_empty())
                || model.efforts.is_empty()
                || model.efforts.iter().any(|e| {
                    !identifier(&e.name)
                        || !efforts.insert(&e.name)
                        || e.depths.is_empty()
                        || e.depths.iter().collect::<BTreeSet<_>>().len() != e.depths.len()
                })
            {
                return Err(Error::Rejected(format!(
                    "invalid catalog model {}",
                    model.id
                )));
            }
        }
        Ok(())
    }
    pub fn supports(&self, model: &str, effort: &str, requirements: &Requirements) -> bool {
        self.models.iter().find(|m| m.id == model).is_some_and(|m| {
            requirements
                .capabilities
                .iter()
                .all(|(k, v)| m.capabilities.get(k).is_some_and(|level| level >= v))
                && m.efforts
                    .iter()
                    .any(|e| e.name == effort && e.depths.contains(&requirements.depth))
        })
    }
    pub fn candidates(&self, requirements: &Requirements) -> Vec<(&str, &str)> {
        let mut candidates: Vec<_> = self
            .models
            .iter()
            .flat_map(|m| m.efforts.iter().map(move |e| (m, e)))
            .filter(|(m, e)| self.supports(&m.id, &e.name, requirements))
            .collect();
        candidates.sort_by_key(|(m, e)| (m.preference, &m.id, e.preference, &e.name));
        candidates
            .into_iter()
            .map(|(m, e)| (m.id.as_str(), e.name.as_str()))
            .collect()
    }
}
impl CatalogSnapshot {
    pub fn new(run_id: &str, catalog: Catalog) -> Result<Self> {
        catalog.validate()?;
        Ok(Self {
            run_id: run_id.into(),
            digest: Digest::of(&catalog)?,
            catalog,
        })
    }
    pub fn validate(&self, run_id: &str) -> Result<()> {
        self.catalog.validate()?;
        if self.run_id != run_id || self.digest != Digest::of(&self.catalog)? {
            return Err(Error::Corrupt(
                "catalog snapshot binding differs from its run".into(),
            ));
        }
        Ok(())
    }
}

pub fn bundled() -> Catalog {
    let caps = |values: &[(&str, u8)]| values.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let mut role_requirements = BTreeMap::new();
    for role in Role::ALL {
        let (values, depth): (&[(&str, u8)], Depth) = match role {
            Role::FactFinder => (&[("repository_analysis", 1)], Depth::Routine),
            Role::Implementer => (&[("implementation", 2)], Depth::Focused),
            Role::PlanCritic | Role::UnitReviewer | Role::FailureDiagnosis => (
                &[("repository_analysis", 2), ("adversarial_review", 2)],
                Depth::Deep,
            ),
            Role::ColdConsumer | Role::FinalReview => (
                &[("adversarial_review", 2), ("security_review", 2)],
                Depth::Deep,
            ),
            _ => (
                &[("cross_module_reasoning", 2), ("implementation", 2)],
                Depth::Deep,
            ),
        };
        role_requirements.insert(
            role,
            Requirements {
                capabilities: caps(values),
                depth,
            },
        );
    }
    let model = |id: &str, preference, levels: &[(&str, u8)], depths, name: &str| Model {
        id: id.into(),
        preference,
        capabilities: caps(levels),
        efforts: vec![CatalogEffort {
            name: name.into(),
            depths,
            preference: 0,
        }],
        sources: vec![
            "bundled design policy; capability levels are declarations, not measurements".into(),
        ],
    };
    Catalog {
        schema: "hwahap/catalog/v1".into(),
        revision: "bundled-v1".into(),
        role_requirements,
        models: vec![
            model(
                "gpt-5.6-luna",
                10,
                &[("repository_analysis", 1), ("implementation", 2)],
                vec![Depth::Routine, Depth::Focused],
                "medium",
            ),
            model(
                "gpt-5.6-terra",
                20,
                &[
                    ("repository_analysis", 2),
                    ("implementation", 2),
                    ("cross_module_reasoning", 2),
                ],
                vec![Depth::Routine, Depth::Focused, Depth::Deep],
                "high",
            ),
            model(
                "gpt-6-astra",
                30,
                &[
                    ("repository_analysis", 3),
                    ("implementation", 3),
                    ("cross_module_reasoning", 3),
                    ("adversarial_review", 3),
                    ("security_review", 3),
                ],
                vec![Depth::Routine, Depth::Focused, Depth::Deep],
                "high",
            ),
        ],
    }
}

mod snapshot;
pub use snapshot::{configured, pin, snapshot};

pub mod host;
pub use host::{HostObservation, ObservedModel};

mod selection;
pub use selection::{tools_for, Selection};
