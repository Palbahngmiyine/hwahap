//! Opt-in local Codex usage measurement. Never retain transcript text or count a session twice.
use crate::{
    canonical::Digest,
    pr_review::{read_evidence, save_evidence},
    state::Store,
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::Path};

#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_write_input_tokens: u64,
}
impl Tokens {
    fn difference(&self, before: &Self) -> Result<Self> {
        let sub = |a: u64, b: u64| {
            a.checked_sub(b).ok_or_else(|| {
                Error::Rejected("usage counters decreased; source is incomplete".into())
            })
        };
        let delta = Self {
            input_tokens: sub(self.input_tokens, before.input_tokens)?,
            cached_input_tokens: sub(self.cached_input_tokens, before.cached_input_tokens)?,
            output_tokens: sub(self.output_tokens, before.output_tokens)?,
            cache_write_input_tokens: sub(
                self.cache_write_input_tokens,
                before.cache_write_input_tokens,
            )?,
        };
        if delta
            .cached_input_tokens
            .checked_add(delta.cache_write_input_tokens)
            .is_none_or(|n| n > delta.input_tokens)
        {
            return Err(Error::Rejected("invalid input/cache counters".into()));
        }
        Ok(delta)
    }
    fn add(&mut self, other: &Self) -> Result<()> {
        for (a, b) in [
            (&mut self.input_tokens, other.input_tokens),
            (&mut self.cached_input_tokens, other.cached_input_tokens),
            (&mut self.output_tokens, other.output_tokens),
            (
                &mut self.cache_write_input_tokens,
                other.cache_write_input_tokens,
            ),
        ] {
            *a = a
                .checked_add(b)
                .ok_or_else(|| Error::Rejected("usage overflow".into()))?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Source {
    run_id: String,
    session_id: String,
    path: String,
    from_start: bool,
    baseline: BTreeMap<String, Tokens>,
}

fn scan(path: &Path) -> Result<(String, BTreeMap<String, Tokens>)> {
    // Bound even a growing or hostile log. Partial final writes are picked up at the next sync.
    const LIMIT: u64 = 64 * 1024 * 1024;
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|e| Error::io(path, e))?
        .take(LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(|e| Error::io(path, e))?;
    if text.len() as u64 > LIMIT {
        return Err(Error::Rejected("usage source exceeds 64 MiB".into()));
    }
    let (mut id, mut model) = (None, "unknown".to_owned());
    let (mut previous, mut models) = (Tokens::default(), BTreeMap::<String, Tokens>::new());
    for line in text
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
    {
        let event: serde_json::Value = serde_json::from_str(line)
            .map_err(|_| Error::Rejected("malformed usage source".into()))?;
        let payload = &event["payload"];
        match event["type"].as_str() {
            Some("session_meta") => {
                let next = payload["id"]
                    .as_str()
                    .ok_or_else(|| Error::Rejected("missing session identity".into()))?;
                if id.as_deref().is_some_and(|old| old != next) {
                    return Err(Error::Rejected("session identity changed".into()));
                }
                id = Some(next.to_owned());
            }
            Some("turn_context") => {
                model = payload["model"].as_str().unwrap_or("unknown").to_owned()
            }
            Some("event_msg") if payload["type"] == "token_count" => {
                let value = &payload["info"]["total_token_usage"];
                if value.is_null() {
                    continue;
                }
                let current: Tokens = serde_json::from_value(value.clone())
                    .map_err(|_| Error::Rejected("incomplete usage counters".into()))?;
                models
                    .entry(model.clone())
                    .or_default()
                    .add(&current.difference(&previous)?)?;
                previous = current;
            }
            _ => {}
        }
    }
    Ok((
        id.ok_or_else(|| Error::Rejected("usage source has no session metadata".into()))?,
        models,
    ))
}

/// Default baseline excludes past work, including history retained by reused children.
pub fn attach(store: &Store, path: &Path, from_start: bool) -> Result<String> {
    let run = store
        .read_run()?
        .ok_or_else(|| Error::Rejected("start a run before attaching usage".into()))?;
    let path = path.canonicalize().map_err(|e| Error::io(path, e))?;
    let (session_id, mut baseline) = scan(&path)?;
    let key = format!(
        "meter-source-{}.json",
        Digest::of_bytes(session_id.as_bytes()).challenge()
    );
    if let Some(old) = read_evidence::<Source>(store, &key)? {
        if old.run_id != run.run_id || Path::new(&old.path) != path || old.from_start != from_start
        {
            return Err(Error::Rejected(
                "usage source already attached with a different scope".into(),
            ));
        }
        return Ok(session_id);
    }
    if from_start {
        baseline.clear();
    }
    save_evidence(
        store,
        &key,
        &Source {
            run_id: run.run_id,
            session_id: session_id.clone(),
            path: path.to_string_lossy().into_owned(),
            from_start,
            baseline,
        },
    )?;
    Ok(session_id)
}

/// Read-only, live view. Session totals overlap dispatch totals and must never be added to them.
pub fn summary(store: &Store) -> Result<serde_json::Value> {
    let run_id = store.read_run()?.map(|run| run.run_id);
    let (mut total, mut models) = (Tokens::default(), BTreeMap::<String, Tokens>::new());
    let (mut registered, mut observed) = (0u64, 0u64);
    let mut unavailable = Vec::new();
    let dir = store.artifacts_path();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir).map_err(|e| Error::io(&dir, e))? {
            let path = entry.map_err(|e| Error::io(&dir, e))?.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| n.starts_with("meter-source-") && n.ends_with(".json"))
            else {
                continue;
            };
            let source: Source = read_evidence(store, name)?
                .ok_or_else(|| Error::Corrupt("usage source disappeared".into()))?;
            registered += 1;
            let result = (|| -> Result<BTreeMap<String, Tokens>> {
                let (id, current) = scan(Path::new(&source.path))?;
                if current.is_empty() {
                    return Err(Error::Rejected("usage source has no counters yet".into()));
                }
                if id != source.session_id || run_id.as_ref() != Some(&source.run_id) {
                    return Err(Error::Rejected("usage identity or run mismatch".into()));
                }
                let mut delta = BTreeMap::new();
                for model in current.keys().chain(source.baseline.keys()) {
                    delta.insert(
                        model.clone(),
                        current
                            .get(model)
                            .unwrap_or(&Tokens::default())
                            .difference(source.baseline.get(model).unwrap_or(&Tokens::default()))?,
                    );
                }
                Ok(delta)
            })();
            match result {
                Ok(delta) => {
                    for (model, tokens) in delta {
                        total.add(&tokens)?;
                        models.entry(model).or_default().add(&tokens)?;
                    }
                    observed += 1;
                }
                Err(_) => unavailable.push(source.session_id),
            }
        }
    }
    Ok(
        serde_json::json!({"scope":"attached sessions since baseline; from-start sources explicitly include history",
        "source":"local Codex token_count events; observed counters, not a verified bill",
        "not_additive_with_dispatch_usage":true,"registered_sessions":registered,"observed_sessions":observed,
        "unavailable_sessions":unavailable,"total":total,"by_model":models}),
    )
}
