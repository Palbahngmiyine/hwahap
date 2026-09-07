use crate::{canonical::Digest, Error, Result};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

pub fn digest(root: &Path, inputs: &[String]) -> Result<Digest> {
    Digest::of(&collect(root, inputs)?)
}

fn collect(root: &Path, inputs: &[String]) -> Result<Vec<serde_json::Value>> {
    let root = root.canonicalize().map_err(|e| Error::io(root, e))?;
    let mut entries = Vec::new();
    let paths: BTreeSet<_> = inputs.iter().collect();
    for relative in paths {
        validate_path(relative)?;
        let path = Path::new(relative);
        visit(&root, path, &mut BTreeSet::new(), &mut entries)?;
    }
    let mut links = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for entry in &entries {
        let path = root.join(
            entry[0]
                .as_str()
                .ok_or_else(|| Error::Internal("input path serialization".into()))?,
        );
        let target = resolve_links(&root, &path, &mut BTreeSet::new(), &mut links)?;
        if !target.starts_with(&root) {
            return Err(Error::Rejected("input resolves outside source".into()));
        }
        targets.insert(target);
    }
    for link in links {
        let meta = std::fs::symlink_metadata(&link).map_err(|e| Error::io(&link, e))?;
        entries.push(serde_json::json!([
            link.strip_prefix(&root).unwrap(),
            "resolution_link",
            std::fs::read_link(&link).map_err(|e| Error::io(&link, e))?,
            permissions(&meta)
        ]));
    }
    for target in targets {
        entries.push(serde_json::json!([
            target.strip_prefix(&root).unwrap(),
            "resolved_target"
        ]));
    }
    Ok(entries)
}

fn resolve_links(
    root: &Path,
    path: &Path,
    active: &mut BTreeSet<std::path::PathBuf>,
    links: &mut BTreeSet<std::path::PathBuf>,
) -> Result<std::path::PathBuf> {
    let mut cursor = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            // A Windows verbatim drive prefix becomes a filesystem path at RootDir.
            Component::Prefix(_) => {
                cursor.push(component.as_os_str());
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                cursor.pop();
                continue;
            }
            _ => cursor.push(component.as_os_str()),
        }
        let meta = match std::fs::symlink_metadata(&cursor) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io(&cursor, e)),
        };
        if meta.file_type().is_symlink() {
            if !active.insert(cursor.clone()) {
                return Err(Error::Rejected("cyclic input resolution".into()));
            }
            if cursor.starts_with(root) {
                links.insert(cursor.clone());
            }
            let target = std::fs::read_link(&cursor).map_err(|e| Error::io(&cursor, e))?;
            let target = if target.is_absolute() {
                target
            } else {
                cursor.parent().unwrap().join(target)
            };
            let resolved = resolve_links(root, &target, active, links)?;
            active.remove(&cursor);
            cursor = resolved;
        }
    }
    Ok(cursor)
}

fn permissions(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        u32::from(meta.permissions().readonly())
    }
}

/// Preserve declared paths, followed targets and intermediate symlinks during cleanup.
pub fn cleanup_exclusions(root: &Path, inputs: &[String]) -> Result<Vec<String>> {
    let root = root.canonicalize().map_err(|e| Error::io(root, e))?;
    let mut paths: BTreeSet<String> = inputs.iter().cloned().collect();
    for entry in collect(&root, inputs)? {
        paths.insert(
            entry[0]
                .as_str()
                .ok_or_else(|| Error::Internal("input path serialization".into()))?
                .to_string(),
        );
    }

    Ok(paths
        .into_iter()
        .map(|path| {
            let mut pattern = String::from("/");
            for ch in path.chars() {
                if matches!(ch, '\\' | '*' | '?' | '[' | ']' | '!' | '#' | ' ') {
                    pattern.push('\\');
                }
                pattern.push(ch);
            }
            pattern
        })
        .collect())
}

pub fn validate_path(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if relative.trim().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || path.starts_with(".hwahap")
        || path.starts_with(".git")
    {
        return Err(Error::Rejected(
            "verification inputs require source-relative paths".into(),
        ));
    }
    Ok(())
}

fn visit(
    root: &Path,
    relative: &Path,
    ancestors: &mut BTreeSet<std::path::PathBuf>,
    entries: &mut Vec<serde_json::Value>,
) -> Result<()> {
    let path = root.join(relative);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut ancestor = path.parent();
            while let Some(parent) = ancestor {
                match parent.canonicalize() {
                    Ok(resolved) => {
                        if !resolved.starts_with(root)
                            || resolved.starts_with(root.join(".hwahap"))
                            || resolved.starts_with(root.join(".git"))
                        {
                            return Err(Error::Rejected(
                                "missing verification input crosses source boundary".into(),
                            ));
                        }
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        ancestor = parent.parent()
                    }
                    Err(e) => return Err(Error::io(parent, e)),
                }
            }
            entries.push(serde_json::json!([relative, "missing"]));
            return Ok(());
        }
        Err(e) => return Err(Error::io(&path, e)),
    };
    let canonical = path.canonicalize().map_err(|e| Error::io(&path, e))?;
    if !canonical.starts_with(root)
        || canonical.starts_with(root.join(".hwahap"))
        || canonical.starts_with(root.join(".git"))
    {
        return Err(Error::Rejected(
            "verification input resolves outside the source boundary".into(),
        ));
    }
    let mode = permissions(&meta);
    if meta.file_type().is_symlink() {
        entries.push(serde_json::json!([
            relative,
            "symlink",
            std::fs::read_link(&path).map_err(|e| Error::io(&path, e))?,
            mode
        ]));
    }
    let target = std::fs::metadata(&path).map_err(|e| Error::io(&path, e))?;
    let mode = permissions(&target);
    if path.is_dir() {
        if !ancestors.insert(canonical.clone()) {
            return Err(Error::Rejected("cyclic verification input".into()));
        }
        entries.push(serde_json::json!([relative, "directory", mode]));
        let mut children = std::fs::read_dir(&path)
            .map_err(|e| Error::io(&path, e))?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| Error::io(&path, e))?;
        children.sort();
        for child in children {
            visit(root, &relative.join(child), ancestors, entries)?;
        }
        ancestors.remove(&canonical);
    } else if path.is_file() {
        let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
        entries.push(serde_json::json!([
            relative,
            "file",
            mode,
            Digest::of_bytes(&bytes)
        ]));
    } else {
        return Err(Error::Rejected(
            "verification inputs must be files or directories".into(),
        ));
    }
    Ok(())
}
