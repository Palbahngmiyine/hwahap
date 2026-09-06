use super::*;
use std::collections::BTreeMap;
const INTENT: &str = "archive-intent.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    run_id: String,
    files: BTreeMap<String, Digest>,
    worktree: Option<(String, String)>,
}
fn owned(name: &str) -> bool {
    matches!(
        name,
        "run.json"
            | "events.jsonl"
            | "plan.json"
            | "plan.md"
            | "report.md"
            | "usage.json"
            | "verification.json"
    ) || (name.starts_with("native-pool-") && name.ends_with(".json"))
}
fn valid_name(name: &str) -> bool {
    let parts: Vec<_> = name.split('/').collect();
    !parts.iter().any(|p| {
        p.is_empty()
            || matches!(*p, "." | "..")
            || (p.contains(['\\', '\0']) || (cfg!(windows) && p.contains(':')))
    }) && ((parts.len() == 1 && owned(name)) || (parts.len() > 1 && parts[0] == "artifacts"))
}
fn manifest_name(root: &Path, path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path
        .strip_prefix(root)
        .map_err(|e| Error::Corrupt(e.to_string()))?
        .components()
    {
        let std::path::Component::Normal(part) = component else {
            return Err(Error::Corrupt("invalid archive manifest path".into()));
        };
        parts.push(
            part.to_str()
                .ok_or_else(|| Error::Rejected("archive path is not UTF-8".into()))?,
        );
    }
    let name = parts.join("/");
    if !valid_name(&name) {
        return Err(Error::Corrupt("invalid archive manifest path".into()));
    }
    Ok(name)
}
fn regular_ancestors(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|e| Error::Corrupt(e.to_string()))?;
    let mut current = root.to_path_buf();
    for part in relative.components() {
        current.push(part);
        match current.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::Rejected(
                    "archive path contains a symbolic link".into(),
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&current, e)),
        }
    }
    Ok(())
}
fn collect(root: &Path, path: &Path, files: &mut BTreeMap<String, Digest>) -> Result<()> {
    let metadata = path.symlink_metadata().map_err(|e| Error::io(path, e))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Rejected(
            "archive state contains a symbolic link".into(),
        ));
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| Error::io(path, e))? {
            collect(root, &entry.map_err(|e| Error::io(path, e))?.path(), files)?;
        }
    } else if metadata.is_file() {
        files.insert(
            manifest_name(root, path)?,
            Digest::of_bytes(&std::fs::read(path).map_err(|e| Error::io(path, e))?),
        );
    } else {
        return Err(Error::Rejected(
            "archive requires regular state files".into(),
        ));
    }
    Ok(())
}
impl Store {
    pub fn verify_archive(&self, run_id: &str) -> Result<()> {
        if run_id.is_empty() || run_id.contains(['/', '\\']) || matches!(run_id, "." | "..") {
            return Err(Error::Rejected("invalid archive run ID".into()));
        }
        let target = self.root.join("archive").join(run_id);
        regular_ancestors(&self.root, &target.join("manifest.json"))?;
        let manifest: Manifest = serde_json::from_slice(
            &std::fs::read(target.join("manifest.json")).map_err(|e| Error::io(&target, e))?,
        )
        .map_err(|e| Error::Corrupt(e.to_string()))?;
        if manifest.run_id != run_id
            || !manifest.files.contains_key("run.json")
            || !manifest.files.contains_key("events.jsonl")
        {
            return Err(Error::Corrupt("archive identity is incomplete".into()));
        }
        for (name, expected) in &manifest.files {
            if !valid_name(name) {
                return Err(Error::Corrupt("invalid archive manifest path".into()));
            }
            let path = target.join(name);
            regular_ancestors(&self.root, &path)?;
            if Digest::of_bytes(&std::fs::read(&path).map_err(|e| Error::io(&path, e))?)
                != *expected
            {
                return Err(Error::Corrupt("archived file hash differs".into()));
            }
        }
        let archived = Store {
            root: target.clone(),
        };
        archived.verify_chain()?;
        let completion = Store {
            root: target.join("completion"),
        };
        completion.verify_chain()?;
        let evidence = serde_json::json!({"run_id":run_id,"manifest":Digest::of(&manifest)?});
        let events = completion.read_events()?;
        if events.len() != 1 || events[0].kind != "archived" || events[0].data != evidence {
            return Err(Error::Corrupt("archive completion differs".into()));
        }
        Ok(())
    }

    pub fn archive_pending(&self) -> bool {
        self.root.join(INTENT).exists()
    }
    pub fn resume_archive(&self) -> Result<()> {
        self.archive(&crate::clock::SystemClock)
    }
    pub fn archive(&self, clock: &dyn Clock) -> Result<()> {
        let intent = self.root.join(INTENT);
        let manifest: Manifest = if intent.exists() {
            serde_json::from_slice(&std::fs::read(&intent).map_err(|e| Error::io(&intent, e))?)
                .map_err(|e| Error::Corrupt(e.to_string()))?
        } else {
            let run = self
                .read_run()?
                .ok_or_else(|| Error::Rejected("archive requires a run".into()))?;
            crate::verification::require_stopped(self)?;
            if crate::native::orphan(self)?.is_some() {
                return Err(Error::Rejected(
                    "archive requires native stop acknowledgment".into(),
                ));
            }
            self.verify_chain()?;
            self.append_event(
                clock,
                "archive_requested",
                serde_json::json!({"run_id":run.run_id}),
            )?;
            let mut files = BTreeMap::new();
            for entry in std::fs::read_dir(&self.root).map_err(|e| Error::io(&self.root, e))? {
                let entry = entry.map_err(|e| Error::io(&self.root, e))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if owned(&name) || name == "artifacts" {
                    collect(&self.root, &entry.path(), &mut files)?;
                }
            }
            let worktree = if self.worktree_path().exists() {
                let git = crate::git::Git::open(&self.worktree_path())?;
                if git.current_branch()? != run.branch || run.branch.is_empty() {
                    return Err(Error::Rejected(
                        "archive worktree ownership differs from this run".into(),
                    ));
                }
                let owner = crate::git::Git::open(self.root.parent().expect("repository parent"))?;
                let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
                if git.run(&args)? != owner.run(&args)? {
                    return Err(Error::Rejected(
                        "archive worktree belongs to another repository".into(),
                    ));
                }
                Some((run.branch.clone(), git.head_sha()?))
            } else {
                None
            };
            let manifest = Manifest {
                run_id: run.run_id,
                files,
                worktree,
            };
            self.write_atomic(
                &intent,
                &serde_json::to_string(&manifest).map_err(|e| Error::Corrupt(e.to_string()))?,
            )?;
            manifest
        };
        // Earlier Windows candidates persisted native separators before failing validation.
        // Normalize only on Windows, where these names identify the same filesystem paths.
        #[cfg(windows)]
        let manifest = {
            let mut normalized = BTreeMap::new();
            for (name, digest) in manifest.files {
                let name = name.replace('\\', "/");
                if !valid_name(&name) || normalized.insert(name, digest).is_some() {
                    return Err(Error::Corrupt("invalid archive manifest paths".into()));
                }
            }
            Manifest {
                files: normalized,
                ..manifest
            }
        };
        if manifest.run_id.is_empty()
            || manifest.run_id.contains(['/', '\\'])
            || matches!(manifest.run_id.as_str(), "." | "..")
            || !manifest.files.contains_key("run.json")
            || !manifest.files.contains_key("events.jsonl")
            || manifest.files.keys().any(|p| !valid_name(p))
        {
            return Err(Error::Corrupt("invalid archive manifest paths".into()));
        }
        #[cfg(windows)]
        self.write_atomic(
            &intent,
            &serde_json::to_string(&manifest).map_err(|e| Error::Corrupt(e.to_string()))?,
        )?;
        let target = self.root.join("archive").join(&manifest.run_id);
        regular_ancestors(&self.root, &target)?;
        std::fs::create_dir_all(&target).map_err(|e| Error::io(&target, e))?;
        let run_path = if self.run_path().exists() {
            self.run_path()
        } else {
            target.join("run.json")
        };
        let run: Run =
            serde_json::from_slice(&std::fs::read(&run_path).map_err(|e| Error::io(&run_path, e))?)
                .map_err(|e| Error::Corrupt(e.to_string()))?;
        if run.run_id != manifest.run_id {
            return Err(Error::Corrupt("archive run identity differs".into()));
        }
        for (name, digest) in &manifest.files {
            let source = self.root.join(name);
            let destination = target.join(name);
            regular_ancestors(&self.root, &source)?;
            regular_ancestors(&self.root, &destination)?;
            let matches = |path: &Path| -> Result<bool> {
                let meta = path.symlink_metadata().map_err(|e| Error::io(path, e))?;
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Ok(false);
                }
                Ok(
                    Digest::of_bytes(&std::fs::read(path).map_err(|e| Error::io(path, e))?)
                        == *digest,
                )
            };
            if destination.exists() && !matches(&destination)? {
                return Err(Error::Corrupt("archived file hash differs".into()));
            }
            if source.exists() {
                if !matches(&source)? {
                    return Err(Error::Corrupt("source changed during archive".into()));
                }
                if destination.exists() {
                    std::fs::remove_file(&source).map_err(|e| Error::io(&source, e))?;
                } else {
                    std::fs::create_dir_all(destination.parent().expect("parent"))
                        .map_err(|e| Error::io(&destination, e))?;
                    std::fs::rename(&source, &destination).map_err(|e| Error::io(&source, e))?;
                }
            } else if !destination.exists() {
                return Err(Error::Corrupt(
                    "archive file is missing from both locations".into(),
                ));
            }
        }
        if let Some((branch, head)) = &manifest.worktree {
            let source = self.worktree_path();
            let destination = target.join("worktree");
            regular_ancestors(&self.root, &source)?;
            regular_ancestors(&self.root, &destination)?;
            if source.exists() {
                if destination.exists() {
                    return Err(Error::Rejected(
                        "both archive worktree locations exist".into(),
                    ));
                }
                let git = crate::git::Git::open(&source)?;
                if git.current_branch()? != *branch || git.head_sha()? != *head {
                    return Err(Error::Rejected("worktree changed during archive".into()));
                }
                git.run(&[
                    "worktree",
                    "move",
                    source
                        .to_str()
                        .ok_or_else(|| Error::Rejected("worktree path is not UTF-8".into()))?,
                    destination
                        .to_str()
                        .ok_or_else(|| Error::Rejected("archive path is not UTF-8".into()))?,
                ])?;
            }
            let git = crate::git::Git::open(&destination)?;
            if git.current_branch()? != *branch || git.head_sha()? != *head {
                return Err(Error::Corrupt("archived worktree identity differs".into()));
            }
        }
        let mut remaining = BTreeMap::new();
        for entry in std::fs::read_dir(&self.root).map_err(|e| Error::io(&self.root, e))? {
            let entry = entry.map_err(|e| Error::io(&self.root, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if owned(&name) || name == "artifacts" {
                collect(&self.root, &entry.path(), &mut remaining)?;
            }
        }
        if !remaining.is_empty() {
            return Err(Error::Rejected(
                "state changed during archive; preserve and reconcile its manifest".into(),
            ));
        }
        let mut archived = BTreeMap::new();
        for name in manifest.files.keys() {
            collect(&target, &target.join(name), &mut archived)?;
        }
        if archived != manifest.files {
            return Err(Error::Corrupt(
                "archive manifest verification failed".into(),
            ));
        }
        // Completion has its own journal, leaving every archived byte covered by the manifest.
        self.write_atomic(
            &target.join("manifest.json"),
            &serde_json::to_string(&manifest).map_err(|e| Error::Corrupt(e.to_string()))?,
        )?;
        let completed = Store {
            root: target.join("completion"),
        };
        completed.verify_chain()?;
        let evidence =
            serde_json::json!({"run_id":manifest.run_id,"manifest":Digest::of(&manifest)?});
        if completed.read_events()?.is_empty() {
            completed.append_event(clock, "archived", evidence.clone())?;
        }
        if completed
            .read_events()?
            .last()
            .is_none_or(|e| e.kind != "archived" || e.data != evidence)
        {
            return Err(Error::Corrupt("archive completion differs".into()));
        }
        std::fs::remove_file(&intent).map_err(|e| Error::io(&intent, e))
    }
}
