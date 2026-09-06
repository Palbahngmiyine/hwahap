//! Explicit flat-rate estimates for measured tokens. Never substitute an estimate for a bill.
use super::meter::Tokens;
use crate::{state::Store, Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Card {
    currency: String,
    source: String,
    effective_date: String,
    assumptions: String,
    per_million: BTreeMap<String, Rates>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rates {
    input: f64,
    cached_input: f64,
    cache_write_input: Option<f64>,
    output: f64,
}

pub fn estimate(store: &Store, measured: &serde_json::Value) -> Result<serde_json::Value> {
    let path = store.root().join("pricing.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({
            "status":"unconfigured", "priced_subtotal":null, "total_billed_cost":null}))
        }
        Err(e) => return Err(Error::io(&path, e)),
    };
    let card: Card = serde_json::from_str(&text)
        .map_err(|e| Error::Rejected(format!("invalid pricing.json: {e}")))?;
    if [
        &card.currency,
        &card.source,
        &card.effective_date,
        &card.assumptions,
    ]
    .iter()
    .any(|s| s.trim().is_empty())
        || card.per_million.values().any(|r| {
            [
                r.input,
                r.cached_input,
                r.cache_write_input.unwrap_or(0.0),
                r.output,
            ]
            .iter()
            .any(|n| !n.is_finite() || *n < 0.0)
        })
    {
        return Err(Error::Rejected(
            "pricing requires provenance, assumptions and finite nonnegative rates".into(),
        ));
    }
    let tokens: BTreeMap<String, Tokens> = serde_json::from_value(measured["by_model"].clone())
        .map_err(|e| Error::Corrupt(e.to_string()))?;
    let (mut subtotal, mut unpriced, mut by_model) = (0.0f64, Vec::new(), BTreeMap::new());
    for (model, tokens) in tokens {
        let Some(rate) = card.per_million.get(&model) else {
            unpriced.push(model);
            continue;
        };
        if tokens.cache_write_input_tokens > 0 && rate.cache_write_input.is_none() {
            unpriced.push(model);
            continue;
        }
        let uncached = tokens
            .input_tokens
            .checked_sub(tokens.cached_input_tokens)
            .and_then(|n| n.checked_sub(tokens.cache_write_input_tokens))
            .ok_or_else(|| Error::Corrupt("invalid metered cache counts".into()))?;
        let cost = (uncached as f64 * rate.input
            + tokens.cached_input_tokens as f64 * rate.cached_input
            + tokens.cache_write_input_tokens as f64 * rate.cache_write_input.unwrap_or(0.0)
            + tokens.output_tokens as f64 * rate.output)
            / 1_000_000.0;
        subtotal += cost;
        if !subtotal.is_finite() {
            return Err(Error::Rejected("estimated cost overflow".into()));
        }
        by_model.insert(model, cost);
    }
    Ok(
        serde_json::json!({"status":"estimate", "currency":card.currency,"source":card.source,
        "effective_date":card.effective_date,"assumptions":card.assumptions,
        "basis":"flat configured rates applied to observed session deltas; excludes unobserved work, tool fees and unconfigured surcharges",
        "priced_subtotal":if by_model.is_empty() {None} else {Some(subtotal)},
        "by_model":by_model,"unpriced_models":unpriced,"unavailable_sessions":measured["unavailable_sessions"],
        "total_billed_cost":null}),
    )
}
