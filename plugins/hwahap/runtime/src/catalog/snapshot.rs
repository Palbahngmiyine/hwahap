use super::*;
use crate::{clock::Clock, state::Store};

pub fn configured(store: &Store) -> Result<Catalog> {
    let config = crate::config::Config::load(store.root())?;
    if config.legacy_profiles {
        return Err(Error::Rejected("legacy [profiles]: convert models and efforts to model-catalog.json (or catalog_path); the catalog applies to new runs".into()));
    }
    let explicit = config.catalog_path.is_some();
    let path = store.root().join(
        config
            .catalog_path
            .as_deref()
            .unwrap_or("model-catalog.json"),
    );
    let catalog: Catalog = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Rejected(format!("catalog {}: {e}", path.display())))?,
        Err(error) if !explicit && error.kind() == std::io::ErrorKind::NotFound => bundled(),
        Err(error) => return Err(Error::io(&path, error)),
    };
    catalog.validate()?;
    Ok(catalog)
}

pub fn snapshot(store: &Store, run_id: &str) -> Result<CatalogSnapshot> {
    store.verify_chain()?;
    let value = store
        .read_events()?
        .into_iter()
        .find(|e| e.kind == "catalog_snapshot" && e.data["run_id"] == run_id)
        .ok_or_else(|| {
            Error::Rejected(
                "run has no pinned model catalog; preserve it and start a new run".into(),
            )
        })?;
    let snapshot: CatalogSnapshot =
        serde_json::from_value(value.data).map_err(|e| Error::Corrupt(e.to_string()))?;
    snapshot.validate(run_id)?;
    Ok(snapshot)
}

pub fn pin(store: &Store, clock: &dyn Clock, run_id: &str) -> Result<CatalogSnapshot> {
    if store
        .read_events()?
        .iter()
        .any(|e| e.kind == "catalog_snapshot" && e.data["run_id"] == run_id)
    {
        return snapshot(store, run_id);
    }
    let snapshot = CatalogSnapshot::new(run_id, configured(store)?)?;
    store.append_event(clock, "catalog_snapshot", serde_json::json!(snapshot))?;
    Ok(snapshot)
}
