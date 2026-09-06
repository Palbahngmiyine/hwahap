//! Configuration: native execution limits and a replaceable model catalog.
//!
//! Everything here has a working default, so an unconfigured repository still runs. What cannot be
//! defaulted is rejected rather than guessed.

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::profile::Profiles;

/// The file read from `<repo>/.hwahap/config.toml`, if it exists.
pub const CONFIG_FILE: &str = "config.toml";

/// Everything Hwahap needs beyond the plan itself.
#[derive(Debug, Clone)]
pub struct Config {
    pub profiles: Profiles,
    pub catalog_path: Option<String>,
    pub legacy_profiles: bool,
    /// How long a single test command may run before it counts as failed.
    pub test_timeout_secs: u64,
    pub native_max_calls: u64,
    pub native_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    /// Retained only to report migration to the catalog configuration;
    /// duplicating its rules here is how a config that passes one check and fails the other gets
    /// created.
    #[serde(default)]
    profiles: Option<toml::Value>,
    #[serde(default)]
    catalog_path: Option<String>,
    #[serde(default)]
    limits: Option<LimitsSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsSection {
    #[serde(default)]
    test_timeout_secs: Option<u64>,
    #[serde(default)]
    native_max_calls: Option<u64>,
    #[serde(default)]
    native_timeout_secs: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            profiles: Profiles::defaults(),
            catalog_path: None,
            legacy_profiles: false,
            test_timeout_secs: 1_800,
            native_max_calls: 64,
            native_timeout_secs: 180,
        }
    }
}

impl Config {
    /// Resolve the same execution profiles in the engine, broker, receipts and cost records.
    pub fn for_run(store: &crate::state::Store) -> Result<Self> {
        let mut config = Self::load(store.root())?;
        if store.archive_pending() {
            return Ok(config);
        }
        if let Some(run) = store.read_run()? {
            crate::catalog::snapshot(store, &run.run_id)?;
        } else {
            crate::catalog::configured(store)?;
        }
        config.profiles = Profiles::defaults();
        if store
            .read_plan()?
            .is_some_and(|plan| plan.execution_authorization.is_some())
        {
            config.profiles = config.profiles.direct_build()?;
        }
        Ok(config)
    }
    /// Reads `<hwahap_dir>/config.toml`, falling back to the defaults when it is absent.
    pub fn load(hwahap_dir: &Path) -> Result<Config> {
        let path = hwahap_dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Config::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// Parses a configuration document.
    pub fn parse(text: &str) -> Result<Config> {
        let document: Document = toml::from_str(text)
            .map_err(|e| Error::Rejected(format!("{CONFIG_FILE} is not valid: {e}")))?;

        let mut config = Config {
            catalog_path: document.catalog_path,
            legacy_profiles: document.profiles.is_some(),
            ..Config::default()
        };
        if let Some(limits) = document.limits {
            for (name, value, target) in [
                (
                    "native_max_calls",
                    limits.native_max_calls,
                    &mut config.native_max_calls,
                ),
                (
                    "native_timeout_secs",
                    limits.native_timeout_secs,
                    &mut config.native_timeout_secs,
                ),
            ] {
                if let Some(value) = value {
                    if value == 0 {
                        return Err(Error::Rejected(format!("[limits] {name} must be positive")));
                    }
                    *target = value;
                }
            }
            if let Some(seconds) = limits.test_timeout_secs {
                if seconds == 0 {
                    return Err(Error::Rejected(
                        "[limits] test_timeout_secs is 0, which would fail every test immediately"
                            .into(),
                    ));
                }
                config.test_timeout_secs = seconds;
            }
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Effort, Profile};

    #[test]
    fn the_defaults_are_the_policy_defaults_and_native_limits() {
        let config = Config::default();
        assert_eq!(config.profiles.spec(Profile::Economy).model, "gpt-5.6-luna");
        assert_eq!(
            config.profiles.spec(Profile::Economy).effort,
            Effort::Medium
        );
        assert_eq!(config.profiles.spec(Profile::Critic).model, "gpt-6-astra");
        assert_eq!(config.profiles.spec(Profile::Critic).effort, Effort::High);
        assert_eq!(config.profiles.spec(Profile::Deep).model, "gpt-6-astra");
        assert_eq!(config.profiles.spec(Profile::Deep).effort, Effort::High);
        assert_eq!(config.native_max_calls, 64);
        assert_eq!(config.native_timeout_secs, 180);
        assert_eq!(config.test_timeout_secs, 1_800);
    }

    #[test]
    fn an_empty_document_yields_the_defaults() {
        assert_eq!(
            Config::parse("")
                .unwrap()
                .profiles
                .spec(Profile::Deep)
                .model,
            Config::default().profiles.spec(Profile::Deep).model
        );
    }

    #[test]
    fn a_missing_file_yields_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.test_timeout_secs, 1_800);
    }

    #[test]
    fn unknown_config_fields_are_rejected() {
        let error = Config::parse("[adapter]\ncommand = \"codex-acp\"\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn native_limits_are_positive_and_configurable() {
        for key in ["native_max_calls", "native_timeout_secs"] {
            assert!(Config::parse(&format!("[limits]\n{key} = 0")).is_err());
        }
        let config =
            Config::parse("[limits]\nnative_max_calls = 12\nnative_timeout_secs = 60").unwrap();
        assert_eq!(
            (config.native_max_calls, config.native_timeout_secs),
            (12, 60)
        );
    }

    #[test]
    fn legacy_profiles_are_detected_for_catalog_migration() {
        let config =
            Config::parse("[profiles.economy]\nmodel='replacement'\neffort='quick'\n").unwrap();
        assert!(config.legacy_profiles);
        let dir = tempfile::tempdir().unwrap();
        let store = crate::state::Store::open(dir.path()).unwrap();
        std::fs::create_dir_all(store.root()).unwrap();
        std::fs::write(
            store.root().join("config.toml"),
            "[profiles.economy]\nmodel='replacement'\neffort='quick'\n",
        )
        .unwrap();
        let error = crate::catalog::configured(&store).unwrap_err().to_string();
        assert!(error.contains("convert models and efforts"), "{error}");
    }

    #[test]
    fn an_unknown_top_level_section_is_rejected_rather_than_ignored() {
        let err = Config::parse("[nonsense]\nx = 1\n").unwrap_err();
        assert!(err.to_string().contains("not valid"), "{err}");
    }

    #[test]
    fn a_model_set_outside_a_profile_is_rejected() {
        let err = Config::parse("model = \"gpt-6-astra\"\n").unwrap_err();
        assert!(err.to_string().contains("not valid"), "{err}");
    }

    #[test]
    fn a_zero_test_timeout_is_rejected() {
        let err = Config::parse("[limits]\ntest_timeout_secs = 0\n").unwrap_err();
        assert!(err.to_string().contains("would fail every test"), "{err}");
    }

    #[test]
    fn a_test_timeout_is_applied() {
        assert_eq!(
            Config::parse("[limits]\ntest_timeout_secs = 60\n")
                .unwrap()
                .test_timeout_secs,
            60
        );
    }

    #[test]
    fn a_malformed_document_names_the_file() {
        let err = Config::parse("this is not toml").unwrap_err();
        assert!(err.to_string().contains(CONFIG_FILE), "{err}");
    }

    #[test]
    fn a_config_file_on_disk_is_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[limits]\ntest_timeout_secs = 42\n",
        )
        .unwrap();
        assert_eq!(Config::load(dir.path()).unwrap().test_timeout_secs, 42);
    }
}
