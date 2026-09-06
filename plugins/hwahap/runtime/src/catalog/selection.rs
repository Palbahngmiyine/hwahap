use super::*;
use schemars::JsonSchema;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub run_id: String,
    pub host_session_id: String,
    pub role: String,
    pub unit: Option<String>,
    pub model: String,
    pub effort: String,
    pub requirements: Requirements,
    pub tools: Vec<String>,
    pub catalog_digest: String,
    pub host_digest: String,
    pub digest: String,
}
pub fn tools_for(role: Role) -> Vec<String> {
    let mut tools = vec!["exec_command".into()];
    if matches!(role, Role::Implementer | Role::Rework) {
        tools.push("apply_patch".into());
    }
    tools
}
impl Selection {
    pub fn new(
        snapshot: &CatalogSnapshot,
        observation: &HostObservation,
        role: Role,
        unit: Option<String>,
        model: &str,
        effort: &str,
    ) -> Result<Self> {
        let mut selection = Self {
            run_id: snapshot.run_id.clone(),
            host_session_id: observation.host_session_id.clone(),
            role: role.as_str().into(),
            unit,
            model: model.into(),
            effort: effort.into(),
            requirements: snapshot.catalog.role_requirements[&role].clone(),
            tools: tools_for(role),
            catalog_digest: snapshot.digest.to_string(),
            host_digest: observation.digest()?.to_string(),
            digest: String::new(),
        };
        selection.digest = selection.expected_digest()?;
        selection.verify_catalog(snapshot)?;
        selection.verify_observation(observation)?;
        Ok(selection)
    }
    fn expected_digest(&self) -> Result<String> {
        let mut copy = self.clone();
        copy.digest.clear();
        Ok(Digest::of(&copy)?.to_string())
    }
    pub fn verify(&self) -> Result<()> {
        if self.digest != self.expected_digest()?
            || !identifier(&self.model)
            || !identifier(&self.effort)
            || self.run_id.is_empty()
            || self.host_session_id.is_empty()
            || !Role::ALL.iter().any(|r| r.as_str() == self.role)
        {
            return Err(Error::Rejected("model selection binding is invalid".into()));
        }
        Ok(())
    }
    pub fn verify_catalog(&self, snapshot: &CatalogSnapshot) -> Result<()> {
        self.verify()?;
        snapshot.validate(&self.run_id)?;
        let role = Role::ALL
            .into_iter()
            .find(|r| r.as_str() == self.role)
            .expect("verified role");
        if self.catalog_digest != snapshot.digest.to_string()
            || self.requirements != snapshot.catalog.role_requirements[&role]
            || self.tools != tools_for(role)
            || !snapshot
                .catalog
                .supports(&self.model, &self.effort, &self.requirements)
        {
            return Err(Error::Rejected(
                "bound_capability_insufficient: model selection differs from the pinned catalog"
                    .into(),
            ));
        }
        Ok(())
    }
    pub fn verify_observation(&self, observation: &HostObservation) -> Result<()> {
        if self.host_digest != observation.digest()?.to_string()
            || self.host_session_id != observation.host_session_id
            || !observation.available(&self.model, &self.effort, &self.tools)
        {
            return Err(Error::Rejected(
                "model_unavailable: the observed model, effort or tools do not match selection"
                    .into(),
            ));
        }
        Ok(())
    }
}
