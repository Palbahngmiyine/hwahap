//! Role classifications and validated effort identifiers. Catalogs select execution models.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A validated host effort identifier. Catalog data defines depth and preference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Effort(std::borrow::Cow<'static, str>);
const EFFORTS: [Effort; 3] = [Effort::Medium, Effort::High, Effort::Xhigh];
const PROFILES: [Profile; 3] = [Profile::Economy, Profile::Critic, Profile::Deep];
#[allow(non_upper_case_globals)]
impl Effort {
    pub const Medium: Self = Self(std::borrow::Cow::Borrowed("medium"));
    pub const High: Self = Self(std::borrow::Cow::Borrowed("high"));
    pub const Xhigh: Self = Self(std::borrow::Cow::Borrowed("xhigh"));
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn parse(text: &str) -> Result<Self> {
        if !crate::catalog::identifier(text) {
            return Err(Error::UnsupportedProfile(format!(
                "invalid effort identifier {text:?}"
            )));
        }
        Ok(Self(std::borrow::Cow::Owned(text.into())))
    }
}
impl Serialize for Effort {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for Effort {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// The body of [`Effort::parse`], reporting the bare message.
///
/// Split out so that a caller holding more context — the `[profiles.<name>]` an effort came from —
/// can prefix that message instead of taking an [`Error`] apart to rebuild it.
fn parse_effort(text: &str) -> std::result::Result<Effort, String> {
    if let Some(effort) = EFFORTS.into_iter().find(|e| e.as_str() == text) {
        return Ok(effort);
    }
    // `none`, `low` and `max` are real efforts upstream, so a user who typed one made a policy
    // error rather than a typo. Saying which it is saves them looking for the misspelling.
    let detail = if matches!(text, "none" | "low" | "max") {
        "is forbidden by the effort policy"
    } else {
        "is not a known effort"
    };
    Err(format!(
        "effort {text:?} {detail}; Hwahap uses only {}",
        joined(EFFORTS.iter().map(|e| e.as_str()))
    ))
}

/// The three profiles an agent can run under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Economy,
    Critic,
    Deep,
}

impl Profile {
    /// The wire name, identical to the serialized form and to the `[profiles.<name>]` section.
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Economy => "economy",
            Profile::Critic => "critic",
            Profile::Deep => "deep",
        }
    }
}

/// A role is what an agent is being asked to do; it maps to exactly one profile.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    FactFinder,
    ColdConsumer,
    Implementer,
    Rework,
    PlanCritic,
    UnitReviewer,
    FailureDiagnosis,
    Recommender,
    PlanSynthesis,
    ConflictReplan,
    FinalReview,
}

impl Role {
    /// Every role, in the order the policy table lists them.
    pub const ALL: [Role; 11] = [
        Role::FactFinder,
        Role::ColdConsumer,
        Role::Implementer,
        Role::Rework,
        Role::PlanCritic,
        Role::UnitReviewer,
        Role::FailureDiagnosis,
        Role::Recommender,
        Role::PlanSynthesis,
        Role::ConflictReplan,
        Role::FinalReview,
    ];

    /// The profile this role always runs under.
    ///
    /// Total by construction: a new role cannot be added without deciding what it costs.
    pub fn profile(self) -> Profile {
        match self {
            Role::FactFinder | Role::Implementer => Profile::Economy,
            Role::PlanCritic | Role::UnitReviewer | Role::FailureDiagnosis => Profile::Critic,
            Role::ColdConsumer
            | Role::Rework
            | Role::Recommender
            | Role::PlanSynthesis
            | Role::ConflictReplan
            | Role::FinalReview => Profile::Deep,
        }
    }

    /// The wire name, identical to the serialized form.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::FactFinder => "fact_finder",
            Role::ColdConsumer => "cold_consumer",
            Role::Implementer => "implementer",
            Role::Rework => "rework",
            Role::PlanCritic => "plan_critic",
            Role::UnitReviewer => "unit_reviewer",
            Role::FailureDiagnosis => "failure_diagnosis",
            Role::Recommender => "recommender",
            Role::PlanSynthesis => "plan_synthesis",
            Role::ConflictReplan => "conflict_replan",
            Role::FinalReview => "final_review",
        }
    }
}

/// What one profile resolves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProfileSpec {
    /// The model id, passed to the adapter verbatim.
    pub model: String,
    /// The effort requested alongside it. The pair travels together everywhere.
    pub effort: Effort,
}

/// The three resolved profiles a run works from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profiles {
    economy: ProfileSpec,
    critic: ProfileSpec,
    deep: ProfileSpec,
}

impl Profiles {
    /// Direct BUILD uses the Astra parent for authorship and two separate Astra reviewers.
    pub fn direct_build(&self) -> Result<Self> {
        if self.deep.model != "gpt-6-astra" {
            return Err(Error::UnsupportedProfile(
                "direct BUILD requires the Astra profile".into(),
            ));
        }
        Ok(Self {
            economy: self.deep.clone(),
            critic: self.deep.clone(),
            deep: self.deep.clone(),
        })
    }
    /// The policy defaults.
    pub fn defaults() -> Profiles {
        Profiles {
            economy: ProfileSpec {
                model: "gpt-5.6-luna".to_string(),
                effort: Effort::Medium,
            },
            critic: ProfileSpec {
                model: "gpt-6-astra".to_string(),
                effort: Effort::High,
            },
            deep: ProfileSpec {
                model: "gpt-6-astra".to_string(),
                effort: Effort::High,
            },
        }
    }

    /// Parses the `[profiles.*]` TOML. All three profiles are required.
    ///
    /// Nothing is inherited from [`Profiles::defaults`]: a file that configures two profiles is a
    /// half-written policy, and filling the third in silently would run agents on a model the file
    /// never names.
    pub fn from_toml(text: &str) -> Result<Profiles> {
        let document: toml::Table = toml::from_str(text).map_err(|e| {
            Error::UnsupportedProfile(format!("profile configuration is not valid TOML: {e}"))
        })?;
        reject_split_settings(&document)?;

        let config: RawConfig = toml::from_str(text).map_err(|e| {
            Error::UnsupportedProfile(format!("profile configuration is invalid: {e}"))
        })?;

        // Unknown names first: told "turbo is not a profile", the user fixes the typo and the
        // missing-section complaint that would follow it disappears with the same edit.
        let unknown: Vec<String> = config
            .profiles
            .keys()
            .filter(|name| !PROFILES.iter().any(|p| p.as_str() == name.as_str()))
            .map(|name| format!("[profiles.{name}]"))
            .collect();
        if !unknown.is_empty() {
            return Err(Error::UnsupportedProfile(format!(
                "profile configuration has unknown section(s) {}; the only profiles are {}",
                joined(&unknown),
                joined(PROFILES.iter().map(|p| p.as_str()))
            )));
        }

        // One match over all three lookups, so that "they are all present" is the same step as
        // taking them out. A separate presence check would leave a fourth, unreachable arm here.
        let (economy, critic, deep) = match (
            config.profiles.get(Profile::Economy.as_str()),
            config.profiles.get(Profile::Critic.as_str()),
            config.profiles.get(Profile::Deep.as_str()),
        ) {
            (Some(economy), Some(critic), Some(deep)) => (economy, critic, deep),
            (economy, critic, deep) => {
                let missing: Vec<String> = [
                    (Profile::Economy, economy),
                    (Profile::Critic, critic),
                    (Profile::Deep, deep),
                ]
                .into_iter()
                .filter(|(_, found)| found.is_none())
                .map(|(profile, _)| format!("[profiles.{}]", profile.as_str()))
                .collect();
                return Err(Error::UnsupportedProfile(format!(
                    "profile configuration is missing the required section(s) {}; \
                     Hwahap does not fill a missing profile in from its defaults",
                    joined(&missing)
                )));
            }
        };
        Ok(Profiles {
            economy: economy.resolve(Profile::Economy)?,
            critic: critic.resolve(Profile::Critic)?,
            deep: deep.resolve(Profile::Deep)?,
        })
    }

    /// The model and effort of one profile.
    pub fn spec(&self, profile: Profile) -> &ProfileSpec {
        match profile {
            Profile::Economy => &self.economy,
            Profile::Critic => &self.critic,
            Profile::Deep => &self.deep,
        }
    }

    /// The model and effort a role runs under.
    pub fn for_role(&self, role: Role) -> &ProfileSpec {
        self.spec(role.profile())
    }

    /// Both independent PR review lanes request Astra; never substitute another model.
    pub fn require_astra_reviewers(&self) -> Result<()> {
        for role in [Role::UnitReviewer, Role::FinalReview] {
            if self.for_role(role).model != "gpt-6-astra" {
                return Err(Error::UnsupportedProfile(format!(
                    "{} requires gpt-6-astra for independent PR review",
                    role.as_str()
                )));
            }
        }
        Ok(())
    }
}

/// The whole config document. `deny_unknown_fields` keeps `[profiles]` the only table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    /// Defaulted so that a document with no `[profiles]` at all reports every missing section,
    /// rather than serde's "missing field `profiles`" which names none of them.
    #[serde(default)]
    profiles: BTreeMap<String, RawSpec>,
}

/// One `[profiles.<name>]` table, still unvalidated.
///
/// Both fields stay plain strings so that this module says what is wrong with them — an empty
/// model, an effort the policy forbids — where serde would only manage "unknown variant".
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpec {
    model: String,
    effort: String,
}

impl RawSpec {
    fn resolve(&self, profile: Profile) -> Result<ProfileSpec> {
        let name = profile.as_str();
        let reject =
            |detail: String| Error::UnsupportedProfile(format!("[profiles.{name}]: {detail}"));
        if self.model.trim().is_empty() {
            return Err(reject("model must not be empty".to_string()));
        }
        if self.model.trim() != self.model {
            return Err(reject(format!(
                "model {:?} must not have leading or trailing whitespace",
                self.model
            )));
        }
        if let Some(ch) = self.model.chars().find(|c| is_invisible(*c)) {
            return Err(reject(format!(
                "model {:?} must not contain the invisible character U+{:04X}",
                self.model, ch as u32
            )));
        }
        let effort = parse_effort(&self.effort).map_err(reject)?;
        Ok(ProfileSpec {
            model: self.model.clone(),
            effort,
        })
    }
}

/// Rejects a `model` or `effort` value set outside a `[profiles.<name>]` table.
///
/// Splitting the pair is the configuration skew the policy forbids, so it is named as such rather
/// than left to `deny_unknown_fields`, whose "unknown field `model`" reads like a typo.
fn reject_split_settings(document: &toml::Table) -> Result<()> {
    let mut stray = Vec::new();
    walk_table(document, &mut Vec::new(), &mut stray);
    if stray.is_empty() {
        return Ok(());
    }
    // Sorted, because the report must not depend on how the TOML map happens to iterate.
    stray.sort();
    Err(Error::UnsupportedProfile(format!(
        "profile configuration sets {} outside a [profiles.<name>] table; \
         model and effort must be set together in one of {}, because splitting them is exactly \
         the configuration skew the effort policy forbids",
        joined(&stray),
        joined(
            PROFILES
                .iter()
                .map(|p| format!("[profiles.{}]", p.as_str()))
        )
    )))
}

fn walk_table(table: &toml::Table, path: &mut Vec<String>, stray: &mut Vec<String>) {
    for (key, child) in table {
        if (key == "model" || key == "effort")
            && !is_profile_table(path)
            && !names_a_profile(path, child)
        {
            let mut named = path.clone();
            named.push(key.clone());
            stray.push(named.join("."));
        }
        path.push(key.clone());
        walk_value(child, path, stray);
        path.pop();
    }
}

fn walk_value(value: &toml::Value, path: &mut Vec<String>, stray: &mut Vec<String>) {
    match value {
        toml::Value::Table(table) => walk_table(table, path, stray),
        toml::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(format!("[{index}]"));
                walk_value(item, path, stray);
                path.pop();
            }
        }
        _ => {}
    }
}

fn is_profile_table(path: &[String]) -> bool {
    path.len() == 2 && path[0] == "profiles"
}

/// True when a `model` or `effort` key is the NAME of a profile section rather than a setting.
///
/// `[profiles.model]` declares a profile called `model`; the unknown-section check names that
/// mistake exactly, where "you set model outside a profile" would describe a different one. A
/// scalar `model` directly under `[profiles]` names nothing, so it stays skew.
fn names_a_profile(path: &[String], child: &toml::Value) -> bool {
    path.len() == 1
        && path[0] == "profiles"
        && matches!(child, toml::Value::Table(_) | toml::Value::Array(_))
}

/// True for a character that occupies no visible width: the C0/C1 controls, the bidirectional
/// overrides, and the zero-width spaces and joiners.
///
/// A model id reaches the adapter verbatim and is printed into every receipt. An invisible
/// character makes two different ids render identically there, so the configuration the user reads
/// back would not be the configuration that ran — and U+202E can reorder the rest of the line.
fn is_invisible(ch: char) -> bool {
    ch.is_control()
        || matches!(ch,
            '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{feff}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}')
}

/// Renders names as `a, b, c` for error messages, so a list can never drift from what it describes.
fn joined<T: AsRef<str>>(names: impl IntoIterator<Item = T>) -> String {
    names
        .into_iter()
        .map(|name| name.as_ref().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const DEFAULT_CONFIG: &str = r#"
[profiles.economy]
model = "gpt-5.6-luna"
effort = "medium"
[profiles.critic]
model = "gpt-6-astra"
effort = "high"
[profiles.deep]
model = "gpt-6-astra"
effort = "high"
"#;

    fn message(err: Error) -> String {
        match err {
            Error::UnsupportedProfile(detail) => detail,
            other => panic!("expected an unsupported_profile error, got {other:?}"),
        }
    }

    fn rejection(config: &str) -> String {
        message(Profiles::from_toml(config).unwrap_err())
    }

    /// Encodes a TOML basic string. Rust's `{:?}` is not a TOML encoder — it writes `\0` and
    /// `\u{a0}`, neither of which TOML accepts — and these tests need control characters and
    /// exotic whitespace to reach the validator rather than dying in the parser.
    fn toml_string(text: &str) -> String {
        let mut out = String::from("\"");
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 || c == '\u{7f}' => {
                    out.push_str(&format!("\\u{:04X}", c as u32))
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// A complete, otherwise valid document whose critic model is `model`.
    ///
    /// Critic rather than economy so that a rule which only ever looked at the first section would
    /// fail these tests instead of passing them.
    fn critic_model(model: &str) -> String {
        format!(
            "[profiles.economy]\nmodel = \"m\"\neffort = \"medium\"\n\
             [profiles.critic]\nmodel = {}\neffort = \"high\"\n\
             [profiles.deep]\nmodel = \"m\"\neffort = \"xhigh\"\n",
            toml_string(model)
        )
    }

    /// Renders a `Profiles` back into the documented config shape, for round-trip tests.
    fn render(profiles: &Profiles) -> String {
        let mut out = String::new();
        for profile in PROFILES {
            let spec = profiles.spec(profile);
            out.push_str(&format!(
                "[profiles.{}]\nmodel = {:?}\neffort = {:?}\n",
                profile.as_str(),
                spec.model,
                spec.effort.as_str()
            ));
        }
        out
    }

    #[test]
    fn defaults_match_the_policy_table_exactly() {
        let profiles = Profiles::defaults();
        assert_eq!(profiles.spec(Profile::Economy).model, "gpt-5.6-luna");
        assert_eq!(profiles.spec(Profile::Economy).effort, Effort::Medium);
        assert_eq!(profiles.spec(Profile::Critic).model, "gpt-6-astra");
        assert_eq!(profiles.spec(Profile::Critic).effort, Effort::High);
        assert_eq!(profiles.spec(Profile::Deep).model, "gpt-6-astra");
        assert_eq!(profiles.spec(Profile::Deep).effort, Effort::High);
    }

    #[test]
    fn profiles_share_astra_for_independent_review_roles() {
        let profiles = Profiles::defaults();
        let models: BTreeSet<&str> = PROFILES
            .iter()
            .map(|p| profiles.spec(*p).model.as_str())
            .collect();
        let efforts: BTreeSet<Effort> = PROFILES
            .iter()
            .map(|p| profiles.spec(*p).effort.clone())
            .collect();
        assert_eq!(models, BTreeSet::from(["gpt-5.6-luna", "gpt-6-astra"]));
        assert_eq!(
            efforts.len(),
            2,
            "both Astra review profiles intentionally use high: {efforts:?}"
        );
    }

    #[test]
    fn there_are_exactly_three_profiles_each_naming_itself() {
        for profile in PROFILES {
            // Exhaustive: a fourth profile stops this compiling, which is the point of writing it out.
            let expected = match profile {
                Profile::Economy => "economy",
                Profile::Critic => "critic",
                Profile::Deep => "deep",
            };
            assert_eq!(profile.as_str(), expected);
        }
        assert_eq!(PROFILES.len(), 3);
        assert_eq!(
            PROFILES
                .iter()
                .map(|p| p.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn effort_identifiers_round_trip_without_implied_depth_order() {
        for name in [
            "medium", "high", "xhigh", "quick", "deep", "max", "ultra", "none",
        ] {
            let effort = Effort::parse(name).unwrap();
            assert_eq!(effort.as_str(), name);
            assert_eq!(
                serde_json::from_str::<Effort>(&serde_json::to_string(&effort).unwrap()).unwrap(),
                effort
            );
        }
    }

    #[test]
    fn every_role_maps_to_the_documented_profile() {
        let expected: [(Role, Profile, &str); 11] = [
            (Role::FactFinder, Profile::Economy, "fact_finder"),
            (Role::ColdConsumer, Profile::Deep, "cold_consumer"),
            (Role::Implementer, Profile::Economy, "implementer"),
            (Role::Rework, Profile::Deep, "rework"),
            (Role::PlanCritic, Profile::Critic, "plan_critic"),
            (Role::UnitReviewer, Profile::Critic, "unit_reviewer"),
            (Role::FailureDiagnosis, Profile::Critic, "failure_diagnosis"),
            (Role::Recommender, Profile::Deep, "recommender"),
            (Role::PlanSynthesis, Profile::Deep, "plan_synthesis"),
            (Role::ConflictReplan, Profile::Deep, "conflict_replan"),
            (Role::FinalReview, Profile::Deep, "final_review"),
        ];
        for (role, profile, name) in expected {
            assert_eq!(role.profile(), profile, "{name} maps to the wrong profile");
            assert_eq!(role.as_str(), name);
            assert!(
                Role::ALL.contains(&role),
                "{name} is missing from Role::ALL"
            );
        }
        assert_eq!(expected.map(|(role, _, _)| role), Role::ALL);
    }

    #[test]
    fn role_all_lists_eleven_distinct_roles() {
        assert_eq!(Role::ALL.len(), 11);
        assert_eq!(Role::ALL.into_iter().collect::<BTreeSet<_>>().len(), 11);
        assert_eq!(
            Role::ALL
                .iter()
                .map(|r| r.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            11
        );
    }

    #[test]
    fn the_retry_uses_deep_without_an_extra_diagnosis_model() {
        let count = |profile: Profile| Role::ALL.iter().filter(|r| r.profile() == profile).count();
        assert_eq!(count(Profile::Economy), 2);
        assert_eq!(count(Profile::Critic), 3);
        assert_eq!(count(Profile::Deep), 6);
        assert_eq!(
            count(Profile::Economy) + count(Profile::Critic) + count(Profile::Deep),
            Role::ALL.len()
        );
    }

    #[test]
    fn every_wire_name_is_exactly_the_serialized_name() {
        for effort in EFFORTS {
            let json = format!("\"{}\"", effort.as_str());
            assert_eq!(serde_json::to_string(&effort).unwrap(), json);
            assert_eq!(serde_json::from_str::<Effort>(&json).unwrap(), effort);
        }
        for profile in PROFILES {
            let json = format!("\"{}\"", profile.as_str());
            assert_eq!(serde_json::to_string(&profile).unwrap(), json);
            assert_eq!(serde_json::from_str::<Profile>(&json).unwrap(), profile);
        }
        for role in Role::ALL {
            let json = format!("\"{}\"", role.as_str());
            assert_eq!(serde_json::to_string(&role).unwrap(), json);
            assert_eq!(serde_json::from_str::<Role>(&json).unwrap(), role);
        }
    }

    #[test]
    fn malformed_effort_identifiers_are_rejected() {
        for invalid in ["", " high", "high ", "a b", "a\nb"] {
            assert!(Effort::parse(invalid).is_err());
            assert!(
                serde_json::from_str::<Effort>(&serde_json::to_string(invalid).unwrap()).is_err()
            );
        }
    }

    #[test]
    fn an_unknown_role_name_is_not_deserialized_into_a_neighbouring_role() {
        assert!(serde_json::from_str::<Role>("\"planner\"").is_err());
        assert!(serde_json::from_str::<Role>("\"FactFinder\"").is_err());
        assert!(serde_json::from_str::<Role>("\"fact-finder\"").is_err());
    }

    #[test]
    fn effort_parse_accepts_exactly_the_three_allowed_words() {
        assert_eq!(Effort::parse("medium").unwrap(), Effort::Medium);
        assert_eq!(Effort::parse("high").unwrap(), Effort::High);
        assert_eq!(Effort::parse("xhigh").unwrap(), Effort::Xhigh);
    }

    #[test]
    fn effort_parse_round_trips_every_rendered_effort() {
        for effort in EFFORTS {
            assert_eq!(Effort::parse(effort.as_str()).unwrap(), effort);
            assert_eq!(
                Effort::parse(Effort::parse(effort.as_str()).unwrap().as_str()).unwrap(),
                effort
            );
        }
    }

    #[test]
    fn effort_names_need_catalog_support_in_addition_to_valid_syntax() {
        let catalog = crate::catalog::bundled();
        let requirements = &catalog.role_requirements[&Role::FactFinder];
        assert!(Effort::parse("future-effort").is_ok());
        assert!(!catalog.supports("gpt-5.6-luna", "future-effort", requirements));
    }

    #[test]
    fn the_documented_config_parses_to_the_policy_defaults() {
        assert_eq!(
            Profiles::from_toml(DEFAULT_CONFIG).unwrap(),
            Profiles::defaults()
        );
    }

    #[test]
    fn rendering_the_defaults_and_parsing_them_back_is_the_identity() {
        let defaults = Profiles::defaults();
        assert_eq!(Profiles::from_toml(&render(&defaults)).unwrap(), defaults);
    }

    #[test]
    fn a_configured_table_survives_a_render_and_parse_round_trip() {
        // Unusual but legal model ids, including a multi-byte grapheme, to prove the round trip is
        // not the defaults answering for themselves.
        let config = r#"
[profiles.economy]
model = "modèl-λ.1"
effort = "xhigh"
[profiles.critic]
model = "a"
effort = "medium"
[profiles.deep]
model = "gpt-6-astra:2026-09-04"
effort = "high"
"#;
        let parsed = Profiles::from_toml(config).unwrap();
        assert_eq!(parsed.spec(Profile::Economy).model, "modèl-λ.1");
        assert_eq!(parsed.spec(Profile::Economy).effort, Effort::Xhigh);
        assert_eq!(parsed.spec(Profile::Critic).model, "a");
        assert_eq!(parsed.spec(Profile::Deep).model, "gpt-6-astra:2026-09-04");
        assert_eq!(Profiles::from_toml(&render(&parsed)).unwrap(), parsed);
        assert_ne!(parsed, Profiles::defaults());
    }

    #[test]
    fn section_order_comments_and_crlf_do_not_change_the_result() {
        let config = "# 화합 profiles\r\n[profiles.deep]\r\nmodel = \"gpt-6-astra\"\r\n\
                      effort = \"high\"\r\n\r\n[profiles.economy] # cheapest\r\n\
                      model = \"gpt-5.6-luna\"\r\neffort = \"medium\"\r\n[profiles.critic]\r\n\
                      effort = \"high\"\r\nmodel = \"gpt-6-astra\"\r\n";
        assert_eq!(Profiles::from_toml(config).unwrap(), Profiles::defaults());
    }

    #[test]
    fn parsing_the_same_text_twice_yields_equal_tables() {
        let first = Profiles::from_toml(DEFAULT_CONFIG).unwrap();
        let second = Profiles::from_toml(DEFAULT_CONFIG).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_missing_profile_section_is_named_and_never_filled_in_from_the_defaults() {
        for (missing, kept) in [
            ("economy", ["critic", "deep"]),
            ("critic", ["economy", "deep"]),
            ("deep", ["economy", "critic"]),
        ] {
            let config = format!(
                "[profiles.{}]\nmodel = \"m\"\neffort = \"high\"\n[profiles.{}]\nmodel = \"m\"\neffort = \"high\"\n",
                kept[0], kept[1]
            );
            let detail = rejection(&config);
            assert!(
                detail.contains(&format!("[profiles.{missing}]")),
                "should have named the missing section, got {detail}"
            );
            assert!(
                detail.contains("missing the required section(s)"),
                "{detail}"
            );
            assert!(
                detail.contains("does not fill a missing profile in from its defaults"),
                "{detail}"
            );
        }
    }

    #[test]
    fn an_empty_document_reports_all_three_missing_sections_in_policy_order() {
        let detail = rejection("");
        assert_eq!(
            detail,
            "profile configuration is missing the required section(s) \
             [profiles.economy], [profiles.critic], [profiles.deep]; \
             Hwahap does not fill a missing profile in from its defaults"
        );
        assert_eq!(rejection("   \n# only a comment\n"), detail);
    }

    #[test]
    fn an_empty_profiles_table_is_missing_every_section() {
        let detail = rejection("[profiles]\n");
        assert!(detail.contains("[profiles.economy]"), "{detail}");
        assert!(detail.contains("[profiles.critic]"), "{detail}");
        assert!(detail.contains("[profiles.deep]"), "{detail}");
    }

    #[test]
    fn an_unknown_profile_section_is_rejected_before_the_missing_ones_are_counted() {
        let detail = rejection("[profiles.turbo]\nmodel = \"m\"\neffort = \"high\"\n");
        assert_eq!(
            detail,
            "profile configuration has unknown section(s) [profiles.turbo]; \
             the only profiles are economy, critic, deep"
        );
    }

    #[test]
    fn a_misspelled_profile_section_does_not_shadow_the_real_one() {
        let config = format!("{DEFAULT_CONFIG}[profiles.Deep]\nmodel = \"m\"\neffort = \"high\"\n");
        let detail = rejection(&config);
        assert!(detail.contains("[profiles.Deep]"), "{detail}");
        assert!(detail.contains("unknown section(s)"), "{detail}");
    }

    #[test]
    fn an_unknown_key_inside_a_profile_is_rejected() {
        let config = format!("{DEFAULT_CONFIG}[profiles.economy.overrides]\nseed = 1\n");
        let detail = rejection(&config);
        assert!(detail.contains("unknown field"), "{detail}");
        assert!(detail.contains("overrides"), "{detail}");
    }

    #[test]
    fn an_unknown_top_level_table_is_rejected_rather_than_ignored() {
        let config = format!("{DEFAULT_CONFIG}[network]\ntimeout = 5\n");
        let detail = rejection(&config);
        assert!(detail.contains("unknown field"), "{detail}");
        assert!(detail.contains("network"), "{detail}");
    }

    #[test]
    fn a_profile_missing_its_model_or_effort_key_is_rejected() {
        let without_model = "[profiles.economy]\neffort = \"medium\"\n\
                             [profiles.critic]\nmodel = \"m\"\neffort = \"high\"\n\
                             [profiles.deep]\nmodel = \"m\"\neffort = \"xhigh\"\n";
        let detail = rejection(without_model);
        assert!(detail.contains("missing field"), "{detail}");
        assert!(detail.contains("model"), "{detail}");

        let without_effort = "[profiles.economy]\nmodel = \"m\"\n\
                              [profiles.critic]\nmodel = \"m\"\neffort = \"high\"\n\
                              [profiles.deep]\nmodel = \"m\"\neffort = \"xhigh\"\n";
        let detail = rejection(without_effort);
        assert!(detail.contains("missing field"), "{detail}");
        assert!(detail.contains("effort"), "{detail}");
    }

    #[test]
    fn a_forbidden_effort_in_the_config_names_the_profile_and_the_allowed_efforts() {
        for forbidden in ["none", "low", "max"] {
            let config = format!(
                "[profiles.economy]\nmodel = \"m\"\neffort = \"{forbidden}\"\n\
                 [profiles.critic]\nmodel = \"m\"\neffort = \"high\"\n\
                 [profiles.deep]\nmodel = \"m\"\neffort = \"xhigh\"\n"
            );
            let detail = rejection(&config);
            assert!(detail.starts_with("[profiles.economy]: "), "{detail}");
            assert!(
                detail.contains("forbidden by the effort policy"),
                "{detail}"
            );
            assert!(detail.contains("medium, high, xhigh"), "{detail}");
        }
    }

    #[test]
    fn a_misspelled_effort_in_the_config_is_rejected_rather_than_rounded_to_a_neighbour() {
        for bad in [
            "Medium",
            "HIGH",
            " high",
            "highest",
            "extra-high",
            "",
            "high\u{0009}",
        ] {
            let config = format!(
                "[profiles.economy]\nmodel = \"m\"\neffort = \"medium\"\n\
                 [profiles.critic]\nmodel = \"m\"\neffort = {}\n\
                 [profiles.deep]\nmodel = \"m\"\neffort = \"xhigh\"\n",
                toml_string(bad)
            );
            let detail = rejection(&config);
            assert!(detail.starts_with("[profiles.critic]: "), "{detail}");
            assert!(detail.contains("is not a known effort"), "{detail}");
        }
    }

    #[test]
    fn an_empty_or_whitespace_only_model_is_rejected() {
        for blank in ["", " ", "\t", "   \n ", "\u{00a0}", "\u{3000}"] {
            let config = format!(
                "[profiles.economy]\nmodel = \"m\"\neffort = \"medium\"\n\
                 [profiles.critic]\nmodel = \"m\"\neffort = \"high\"\n\
                 [profiles.deep]\nmodel = {}\neffort = \"xhigh\"\n",
                toml_string(blank)
            );
            let detail = rejection(&config);
            assert_eq!(
                detail, "[profiles.deep]: model must not be empty",
                "input {blank:?}"
            );
        }
    }

    #[test]
    fn a_model_padded_with_whitespace_is_rejected_rather_than_trimmed() {
        let config = "[profiles.economy]\nmodel = \" gpt-5.6-luna\"\neffort = \"medium\"\n\
                      [profiles.critic]\nmodel = \"m\"\neffort = \"high\"\n\
                      [profiles.deep]\nmodel = \"m\"\neffort = \"xhigh\"\n";
        let detail = rejection(config);
        assert_eq!(
            detail,
            "[profiles.economy]: model \" gpt-5.6-luna\" must not have leading or trailing whitespace"
        );
    }

    #[test]
    fn a_model_carrying_control_characters_is_rejected_and_the_codepoint_is_named() {
        for (sneaky, codepoint) in [
            ("gpt\u{0000}-luna", 0x0000),
            ("gpt\nluna", 0x000A),
            ("gpt\rluna", 0x000D),
            ("gpt\u{0007}luna", 0x0007),
            ("gpt\u{001b}[31mluna", 0x001B),
            ("gpt\u{007f}luna", 0x007F),
            ("gpt\u{0085}luna", 0x0085),
        ] {
            let detail = rejection(&critic_model(sneaky));
            assert_eq!(
                detail,
                format!(
                    "[profiles.critic]: model {sneaky:?} must not contain the invisible \
                     character U+{codepoint:04X}"
                )
            );
        }
    }

    #[test]
    fn a_model_carrying_a_bidirectional_override_is_rejected() {
        // The trojan-source characters: they reorder everything printed after them, so a receipt
        // would show a model id that is not the one the adapter was given.
        for (sneaky, codepoint) in [
            ("gpt\u{202e}anul", 0x202E),
            ("gpt\u{202d}luna", 0x202D),
            ("gpt\u{202a}luna", 0x202A),
            ("gpt\u{2066}luna", 0x2066),
            ("gpt\u{2069}luna", 0x2069),
            ("gpt\u{200e}luna", 0x200E),
            ("gpt\u{061c}luna", 0x061C),
        ] {
            let detail = rejection(&critic_model(sneaky));
            assert_eq!(
                detail,
                format!(
                    "[profiles.critic]: model {sneaky:?} must not contain the invisible \
                     character U+{codepoint:04X}"
                )
            );
        }
    }

    #[test]
    fn a_model_carrying_a_zero_width_character_is_rejected() {
        for (sneaky, codepoint) in [
            ("gpt\u{200b}luna", 0x200B),
            ("gpt\u{200c}luna", 0x200C),
            ("gpt\u{200d}luna", 0x200D),
            ("gpt\u{2060}luna", 0x2060),
            ("gpt\u{feff}luna", 0xFEFF),
            ("gpt\u{00ad}luna", 0x00AD),
            ("gpt\u{180e}luna", 0x180E),
        ] {
            let detail = rejection(&critic_model(sneaky));
            assert_eq!(
                detail,
                format!(
                    "[profiles.critic]: model {sneaky:?} must not contain the invisible \
                     character U+{codepoint:04X}"
                )
            );
        }
    }

    #[test]
    fn a_model_made_only_of_invisible_characters_is_not_taken_for_a_real_id() {
        // `trim` does not touch these, so the empty check lets them through; without the invisible
        // check the run would ask the adapter for a model that prints as nothing at all.
        for sneaky in ["\u{feff}", "\u{200b}\u{200b}", "\u{00ad}"] {
            let detail = rejection(&critic_model(sneaky));
            assert!(
                detail.contains("must not contain the invisible character"),
                "{sneaky:?} was accepted as a model id: {detail}"
            );
        }
    }

    #[test]
    fn a_model_of_visible_non_ascii_characters_is_accepted_unchanged() {
        // The invisible check must not become a "letters and digits only" rule: model ids are
        // opaque strings and a deployment may legitimately name one in its own script.
        for good in [
            "modèl-λ.1",
            "gpt-5.6-루나",
            "модель",
            "gpt-5.6🙂",
            "e\u{0301}clair",
            "gpt 5.6",
        ] {
            let profiles = Profiles::from_toml(&critic_model(good)).unwrap();
            assert_eq!(profiles.spec(Profile::Critic).model, good);
        }
    }

    #[test]
    fn a_model_with_an_unbalanced_quote_never_reaches_validation() {
        let detail = rejection("[profiles.economy]\nmodel = \"unterminated\n");
        assert!(
            detail.starts_with("profile configuration is not valid TOML: "),
            "{detail}"
        );
    }

    #[test]
    fn an_absurdly_nested_document_is_rejected_by_the_parser_before_the_walk_recurses() {
        // `walk_value` recurses once per level, so what keeps it off the stack is the TOML parser
        // refusing the document first. Asserting the parser's own message is what would notice if
        // that stopped being true.
        for shape in ["{ a = ", "["] {
            let close = if shape == "[" { "]" } else { " }" };
            let deep = format!("a = {}1{}", shape.repeat(1_000), close.repeat(1_000));
            assert!(
                rejection(&deep).starts_with("profile configuration is not valid TOML: "),
                "{}",
                rejection(&deep)
            );
        }
        // Two levels are legal TOML, so this one really does reach the walk and come back.
        assert!(rejection("a = { a = { a = 1 } }").contains("unknown field `a`"));
    }

    #[test]
    fn a_duplicated_profile_section_is_rejected_instead_of_last_one_winning() {
        let config =
            format!("{DEFAULT_CONFIG}[profiles.economy]\nmodel = \"other\"\neffort = \"xhigh\"\n");
        let detail = rejection(&config);
        assert!(
            detail.starts_with("profile configuration is not valid TOML: "),
            "{detail}"
        );
    }

    #[test]
    fn a_non_string_model_or_effort_is_rejected() {
        let numeric_model = "[profiles.economy]\nmodel = 12\neffort = \"medium\"\n";
        assert!(
            rejection(numeric_model).contains("invalid type"),
            "{}",
            rejection(numeric_model)
        );
        let boolean_effort = "[profiles.economy]\nmodel = \"m\"\neffort = true\n";
        assert!(
            rejection(boolean_effort).contains("invalid type"),
            "{}",
            rejection(boolean_effort)
        );
    }

    #[test]
    fn a_top_level_model_is_named_as_configuration_skew() {
        let config = format!("model = \"gpt-6-astra\"\n{DEFAULT_CONFIG}");
        let detail = rejection(&config);
        assert_eq!(
            detail,
            "profile configuration sets model outside a [profiles.<name>] table; \
             model and effort must be set together in one of [profiles.economy], \
             [profiles.critic], [profiles.deep], because splitting them is exactly the \
             configuration skew the effort policy forbids"
        );
    }

    #[test]
    fn a_top_level_effort_is_named_as_configuration_skew() {
        let config = format!("effort = \"xhigh\"\n{DEFAULT_CONFIG}");
        let detail = rejection(&config);
        assert!(
            detail.starts_with("profile configuration sets effort outside"),
            "{detail}"
        );
    }

    #[test]
    fn model_and_effort_in_an_unrelated_table_are_both_reported() {
        let config =
            format!("{DEFAULT_CONFIG}[agent]\neffort = \"low\"\nmodel = \"gpt-6-astra\"\n");
        let detail = rejection(&config);
        assert!(
            detail.starts_with("profile configuration sets agent.effort, agent.model outside"),
            "{detail}"
        );
    }

    #[test]
    fn stray_settings_are_reported_in_sorted_order_not_in_traversal_order() {
        // `a-b` sorts after `a` as a key but before `a.model` as a path, so a report that just
        // followed the walk would come out in a different order than this one.
        let config = format!("{DEFAULT_CONFIG}[a]\nmodel = \"m\"\n[a-b]\nmodel = \"m\"\n");
        let detail = rejection(&config);
        assert!(
            detail.starts_with("profile configuration sets a-b.model, a.model outside"),
            "{detail}"
        );
    }

    #[test]
    fn a_setting_on_the_profiles_table_itself_is_skew_not_a_default() {
        let config = format!("[profiles]\neffort = \"high\"\n{DEFAULT_CONFIG}");
        let detail = rejection(&config);
        assert!(
            detail.starts_with("profile configuration sets profiles.effort outside"),
            "{detail}"
        );
    }

    #[test]
    fn a_setting_nested_below_a_profile_is_skew() {
        let config =
            format!("{DEFAULT_CONFIG}[profiles.deep.fallback]\nmodel = \"gpt-5.6-luna\"\n");
        let detail = rejection(&config);
        assert!(
            detail.starts_with("profile configuration sets profiles.deep.fallback.model outside"),
            "{detail}"
        );
    }

    #[test]
    fn a_setting_inside_an_array_of_tables_is_skew() {
        let config = format!("{DEFAULT_CONFIG}[[agents]]\nmodel = \"gpt-5.6-luna\"\n");
        let detail = rejection(&config);
        assert!(
            detail.starts_with("profile configuration sets agents.[0].model outside"),
            "{detail}"
        );
    }

    #[test]
    fn a_singular_profile_table_does_not_pass_for_the_real_one() {
        let config = "[profile.economy]\nmodel = \"gpt-5.6-luna\"\neffort = \"medium\"\n";
        let detail = rejection(config);
        assert!(
            detail.starts_with(
                "profile configuration sets profile.economy.effort, profile.economy.model outside"
            ),
            "{detail}"
        );
    }

    #[test]
    fn a_profile_named_model_is_an_unknown_section_not_skew() {
        // The scalar check exists so that `[profiles.model]` reports the real problem.
        let config =
            format!("{DEFAULT_CONFIG}[profiles.model]\nmodel = \"m\"\neffort = \"high\"\n");
        let detail = rejection(&config);
        assert!(
            detail.contains("unknown section(s) [profiles.model]"),
            "{detail}"
        );
    }

    #[test]
    fn a_stray_model_is_skew_whatever_type_its_value_has() {
        // The skew rule is about the KEY being in the wrong place. Letting a list or a table slip
        // through to serde's "unknown field `model`" would describe a typo, which is precisely the
        // reading this module exists to prevent.
        for value in [
            "[\"gpt-6-astra\"]",
            "{ id = \"gpt-6-astra\" }",
            "12",
            "true",
            "1979-05-27",
        ] {
            let config = format!("model = {value}\n{DEFAULT_CONFIG}");
            let detail = rejection(&config);
            assert!(
                detail.starts_with("profile configuration sets model outside"),
                "model = {value} gave {detail}"
            );
        }
    }

    #[test]
    fn a_model_table_in_an_unrelated_section_is_skew_rather_than_an_unknown_field() {
        let config = format!("{DEFAULT_CONFIG}[agent]\nmodel = {{ id = \"gpt-6-astra\" }}\n");
        assert!(
            rejection(&config).starts_with("profile configuration sets agent.model outside"),
            "{}",
            rejection(&config)
        );
        let table = format!("{DEFAULT_CONFIG}[effort]\nseconds = 3\n");
        assert!(
            rejection(&table).starts_with("profile configuration sets effort outside"),
            "{}",
            rejection(&table)
        );
    }

    #[test]
    fn configuration_skew_is_reported_before_every_other_complaint() {
        // One document that breaks four rules at once. Skew wins because it is the only one the
        // user cannot see by reading the section headings.
        let config = "model = \"gpt-6-astra\"\n\
                      [profiles.turbo]\nmodel = \"m\"\neffort = \"low\"\n";
        let detail = rejection(config);
        assert!(
            detail.starts_with("profile configuration sets model outside"),
            "{detail}"
        );
        assert!(!detail.contains("turbo"), "{detail}");
        assert!(!detail.contains("missing"), "{detail}");
    }

    #[test]
    fn an_unknown_section_is_reported_before_a_bad_value_in_a_known_one() {
        let config = "[profiles.economy]\nmodel = \"\"\neffort = \"low\"\n\
                      [profiles.critic]\nmodel = \"m\"\neffort = \"high\"\n\
                      [profiles.deep]\nmodel = \"m\"\neffort = \"xhigh\"\n\
                      [profiles.turbo]\nmodel = \"m\"\neffort = \"high\"\n";
        let detail = rejection(config);
        assert!(
            detail.contains("unknown section(s) [profiles.turbo]"),
            "{detail}"
        );
    }

    #[test]
    fn the_earliest_profile_in_policy_order_is_the_one_whose_value_is_reported() {
        let config = "[profiles.economy]\nmodel = \"m\"\neffort = \"low\"\n\
                      [profiles.critic]\nmodel = \"\"\neffort = \"high\"\n\
                      [profiles.deep]\nmodel = \"m\"\neffort = \"max\"\n";
        assert!(
            rejection(config).starts_with("[profiles.economy]: "),
            "{}",
            rejection(config)
        );
    }

    #[test]
    fn stray_paths_read_the_same_however_the_document_orders_them() {
        let first = format!("{DEFAULT_CONFIG}[zulu]\nmodel = \"m\"\n[alpha]\neffort = \"high\"\n");
        let second = format!("{DEFAULT_CONFIG}[alpha]\neffort = \"high\"\n[zulu]\nmodel = \"m\"\n");
        assert_eq!(rejection(&first), rejection(&second));
        assert!(
            rejection(&first)
                .starts_with("profile configuration sets alpha.effort, zulu.model outside"),
            "{}",
            rejection(&first)
        );
    }

    #[test]
    fn every_ordering_of_the_three_sections_parses_to_the_same_table() {
        let section = |name: &str, model: &str, effort: &str| {
            format!("[profiles.{name}]\nmodel = \"{model}\"\neffort = \"{effort}\"\n")
        };
        let parts = [
            section("economy", "gpt-5.6-luna", "medium"),
            section("critic", "gpt-6-astra", "high"),
            section("deep", "gpt-6-astra", "high"),
        ];
        let orders = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for order in orders {
            let config: String = order.iter().map(|i| parts[*i].as_str()).collect();
            assert_eq!(
                Profiles::from_toml(&config).unwrap(),
                Profiles::defaults(),
                "{config}"
            );
        }
    }

    #[test]
    fn two_tables_that_differ_in_one_field_are_not_equal() {
        let base = Profiles::from_toml(DEFAULT_CONFIG).unwrap();
        let other_model = Profiles::from_toml(&critic_model("gpt-6-astra-2")).unwrap();
        assert_ne!(base, other_model);
        let other_effort = "[profiles.economy]\nmodel = \"gpt-5.6-luna\"\neffort = \"medium\"\n\
                            [profiles.critic]\nmodel = \"gpt-6-astra\"\neffort = \"xhigh\"\n\
                            [profiles.deep]\nmodel = \"gpt-6-astra\"\neffort = \"xhigh\"\n";
        assert_ne!(base, Profiles::from_toml(other_effort).unwrap());
    }

    #[test]
    fn spec_returns_the_table_entry_for_every_profile() {
        let profiles = Profiles::from_toml(DEFAULT_CONFIG).unwrap();
        assert_eq!(profiles.spec(Profile::Economy).effort, Effort::Medium);
        assert_eq!(profiles.spec(Profile::Critic).effort, Effort::High);
        assert_eq!(profiles.spec(Profile::Deep).effort, Effort::High);
        assert_eq!(profiles.spec(Profile::Critic).model, "gpt-6-astra");
    }

    #[test]
    fn for_role_returns_the_spec_of_that_roles_profile() {
        let profiles = Profiles::defaults();
        for role in Role::ALL {
            assert_eq!(profiles.for_role(role), profiles.spec(role.profile()));
        }
        assert_eq!(profiles.for_role(Role::Implementer).model, "gpt-5.6-luna");
        assert_eq!(profiles.for_role(Role::UnitReviewer).model, "gpt-6-astra");
        assert_eq!(profiles.for_role(Role::Recommender).model, "gpt-6-astra");
    }

    #[test]
    fn resolving_the_same_role_twice_returns_the_identical_spec() {
        let profiles = Profiles::defaults();
        for role in Role::ALL {
            let first = profiles.for_role(role);
            let second = profiles.for_role(role);
            assert!(
                std::ptr::eq(first, second),
                "{} resolved to two places",
                role.as_str()
            );
            assert_eq!(
                serde_json::to_string(first).unwrap(),
                serde_json::to_string(second).unwrap()
            );
        }
    }

    #[test]
    fn a_profile_spec_round_trips_through_json() {
        let spec = ProfileSpec {
            model: "gpt-6-astra".to_string(),
            effort: Effort::High,
        };
        assert_eq!(
            serde_json::to_string(&spec).unwrap(),
            r#"{"model":"gpt-6-astra","effort":"high"}"#
        );
        assert_eq!(
            serde_json::from_str::<ProfileSpec>(&serde_json::to_string(&spec).unwrap()).unwrap(),
            spec
        );
    }

    #[test]
    fn a_very_long_model_id_is_carried_through_verbatim() {
        let long = "g".repeat(4096);
        let config = format!(
            "[profiles.economy]\nmodel = {}\neffort = \"medium\"\n\
             [profiles.critic]\nmodel = \"m\"\neffort = \"high\"\n\
             [profiles.deep]\nmodel = \"m\"\neffort = \"xhigh\"\n",
            toml_string(&long)
        );
        let profiles = Profiles::from_toml(&config).unwrap();
        assert_eq!(profiles.spec(Profile::Economy).model, long);
    }

    #[test]
    fn three_profiles_may_name_the_same_model_at_the_same_effort() {
        // Deliberate: the policy constrains which efforts exist, not how many models a deployment
        // has. Collapsing the table is the user's call, and it stays visible in every receipt.
        let config = "[profiles.economy]\nmodel = \"only-model\"\neffort = \"high\"\n\
                      [profiles.critic]\nmodel = \"only-model\"\neffort = \"high\"\n\
                      [profiles.deep]\nmodel = \"only-model\"\neffort = \"high\"\n";
        let profiles = Profiles::from_toml(config).unwrap();
        for role in Role::ALL {
            assert_eq!(profiles.for_role(role).model, "only-model");
            assert_eq!(profiles.for_role(role).effort, Effort::High);
        }
    }
}
