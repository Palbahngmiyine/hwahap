use super::*;
use crate::state::Store;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judgment {
    Confirmed,
    Refuted,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    pub finding_id: String,
    pub judgment: Judgment,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefenseReport {
    pub binding: ReviewBinding,
    pub assessments: Vec<Assessment>,
    pub additional_findings: Vec<Finding>,
    pub security: SecurityReview,
    pub evidence: Vec<String>,
}

impl DefenseReport {
    pub fn validate(&self, attack: &AttackReport, expected: &ReviewBinding) -> Result<()> {
        attack.validate(expected)?;
        if &self.binding != expected {
            return Err(Error::Rejected("defense has stale PR binding".into()));
        }
        validate_evidence(&self.evidence)?;
        let expected_ids: BTreeSet<_> = attack.findings.iter().map(|f| f.id.trim()).collect();
        let mut assessed = BTreeSet::new();
        for a in &self.assessments {
            validate_evidence(&a.evidence)?;
            if !expected_ids.contains(a.finding_id.trim()) || !assessed.insert(a.finding_id.trim())
            {
                return Err(Error::Rejected(
                    "unknown or duplicate defense assessment".into(),
                ));
            }
        }
        if assessed != expected_ids {
            return Err(Error::Rejected("defense omitted attack findings".into()));
        }
        let mut all = expected_ids;
        for f in &self.additional_findings {
            f.validate()?;
            if !all.insert(f.id.trim()) {
                return Err(Error::Rejected(
                    "duplicate additional defense finding".into(),
                ));
            }
        }
        self.security.validate(
            &attack
                .findings
                .iter()
                .chain(&self.additional_findings)
                .collect::<Vec<_>>(),
        )
    }
    pub fn unresolved(&self) -> bool {
        self.assessments
            .iter()
            .any(|a| a.judgment == Judgment::Unresolved)
    }
    pub fn repair_findings(&self, attack: &AttackReport) -> Vec<Finding> {
        attack
            .findings
            .iter()
            .filter(|f| {
                self.assessments.iter().any(|a| {
                    a.finding_id.trim() == f.id.trim() && a.judgment == Judgment::Confirmed
                })
            })
            .cloned()
            .chain(self.additional_findings.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStage {
    Attack,
    Defense,
    Repair,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewProgress {
    pub binding: ReviewBinding,
    pub round: u32,
    pub stage: ReviewStage,
    pub repairs: u32,
}

impl ReviewProgress {
    pub fn artifact(&self, team: &str) -> Result<String> {
        if !matches!(team, "attack" | "defense" | "repair") {
            return Err(Error::Rejected("invalid review artifact team".into()));
        }
        Ok(format!(
            "pr-{}-{}-{team}.json",
            Digest::of(&self.binding)?.challenge(),
            self.round
        ))
    }
    pub fn save(&self, store: &Store) -> Result<()> {
        store.write_artifact(
            "pr-review.json",
            &serde_json::to_string_pretty(self).map_err(|e| Error::Internal(e.to_string()))?,
        )
    }
    pub fn load(store: &Store) -> Result<Option<Self>> {
        read_evidence(store, "pr-review.json")
    }
}

pub fn read_evidence<T: serde::de::DeserializeOwned>(
    store: &Store,
    name: &str,
) -> Result<Option<T>> {
    if name.contains('/') || name.contains('\\') || name.contains("..") || name.starts_with('.') {
        return Err(Error::Rejected("invalid review artifact name".into()));
    }
    let path = store.artifacts_path().join(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| Error::Corrupt(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, e)),
    }
}

pub fn save_evidence<T: Serialize>(store: &Store, name: &str, value: &T) -> Result<()> {
    let value = serde_json::to_value(value).map_err(|e| Error::Internal(e.to_string()))?;
    if let Some(previous) = read_evidence::<serde_json::Value>(store, name)? {
        if previous != value {
            return Err(Error::Corrupt(
                "review evidence cannot be overwritten".into(),
            ));
        }
        return Ok(());
    }
    publish_once(store, name, &value)
}

fn publish_once(store: &Store, name: &str, value: &serde_json::Value) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = store.artifacts_path();
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let (tmp, mut file) = loop {
        let tmp = dir.join(format!(
            ".review-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::io(tmp, e)),
        }
    };
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| Error::Internal(e.to_string()))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| Error::io(&tmp, e))?;
        // A hard link publishes the complete file atomically and never replaces a winner.
        match std::fs::hard_link(&tmp, dir.join(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_evidence::<serde_json::Value>(store, name)?.as_ref() == Some(value) {
                    Ok(())
                } else {
                    Err(Error::Corrupt(
                        "review evidence cannot be overwritten".into(),
                    ))
                }
            }
            Err(e) => Err(Error::io(dir.join(name), e)),
        }
    })();
    drop(file);
    let _ = std::fs::remove_file(tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_different_reports_have_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|n| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (n, save_evidence(&store, "race.json", &n).is_ok())
                })
            })
            .collect();
        let winners: Vec<_> = workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|(_, ok)| *ok)
            .collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(
            read_evidence::<u32>(&store, "race.json").unwrap(),
            Some(winners[0].0)
        );
    }
    #[test]
    fn defense_requires_every_identity_and_bound_evidence() {
        let attack = crate::pr_review::tests::report();
        let mut d = DefenseReport {
            binding: attack.binding.clone(),
            assessments: vec![Assessment {
                finding_id: "A1".into(),
                judgment: Judgment::Confirmed,
                evidence: vec!["independently reproduced".into()],
            }],
            additional_findings: vec![],
            security: security::checked(),
            evidence: vec!["checked".into()],
        };
        assert!(d.validate(&attack, &attack.binding).is_ok());
        assert_eq!(d.repair_findings(&attack).len(), 1);
        d.assessments.push(d.assessments[0].clone());
        assert!(d.validate(&attack, &attack.binding).is_err());
        d.assessments.clear();
        assert!(d.validate(&attack, &attack.binding).is_err());
        d.assessments.push(Assessment {
            finding_id: "unknown".into(),
            judgment: Judgment::Unresolved,
            evidence: vec!["not reproduced".into()],
        });
        assert!(d.unresolved());
        assert!(d.validate(&attack, &attack.binding).is_err());
    }
    #[test]
    fn saved_attack_survives_restart_and_cannot_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let attack = crate::pr_review::tests::report();
        let progress = ReviewProgress {
            binding: attack.binding.clone(),
            round: 1,
            stage: ReviewStage::Defense,
            repairs: 0,
        };
        let name = progress.artifact("attack").unwrap();
        save_evidence(&store, &name, &attack).unwrap();
        progress.save(&store).unwrap();
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(ReviewProgress::load(&reopened).unwrap(), Some(progress));
        assert_eq!(
            read_evidence::<AttackReport>(&reopened, &name).unwrap(),
            Some(attack.clone())
        );
        save_evidence(&reopened, &name, &attack).unwrap();
        let mut changed = attack;
        changed.findings.clear();
        assert!(save_evidence(&reopened, &name, &changed).is_err());
    }
}
