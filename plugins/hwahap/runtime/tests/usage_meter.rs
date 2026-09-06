use hwahap::{
    clock::FixedClock,
    cost::meter,
    plan::SCHEMA,
    state::{Run, RunState, Store},
};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Store, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .write_run(
            &FixedClock::new("2026-09-06T00:00:00Z"),
            &Run {
                schema: SCHEMA.into(),
                run_id: "run".into(),
                goal_id: "goal".into(),
                revision: 1,
                state: RunState::Inspecting,
                accepted_units: vec![],
                accepted_fingerprints: Default::default(),
                plan_digest: None,
                branch: String::new(),
                reviewed_head: None,
                seq: 0,
            },
        )
        .unwrap();
    let log = dir.path().join("session.jsonl");
    let text = format!(
        "{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":"parent"}}),
        json!({"type":"turn_context","payload":{"model":"gpt-6-astra"}})
    );
    std::fs::write(&log, text).unwrap();
    (dir, store, log)
}
fn event(path: &std::path::Path, input: u64, cached: u64, output: u64) {
    use std::io::Write;
    writeln!(std::fs::OpenOptions::new().append(true).open(path).unwrap(), "{}", json!({
        "type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{
        "input_tokens":input,"cached_input_tokens":cached,"output_tokens":output,"reasoning_output_tokens":output/2}}}})).unwrap();
}

#[test]
fn baseline_replay_cache_and_reasoning_are_counted_once_without_transcripts() {
    let (_dir, store, log) = fixture();
    event(&log, 100, 80, 10);
    meter::attach(&store, &log, false).unwrap();
    event(&log, 180, 140, 30);
    event(&log, 180, 140, 30); // repeated cumulative notification
    meter::attach(&store, &log, false).unwrap(); // baseline never moves on replay
    let value = meter::summary(&store).unwrap();
    assert_eq!(value["total"]["input_tokens"], 80);
    assert_eq!(value["total"]["cached_input_tokens"], 60);
    assert_eq!(value["total"]["output_tokens"], 20); // reasoning already included
    assert_eq!(meter::summary(&store).unwrap(), value);
    assert!(meter::attach(&store, &log, true).is_err());
    for entry in std::fs::read_dir(store.artifacts_path()).unwrap() {
        assert!(!std::fs::read_to_string(entry.unwrap().path())
            .unwrap()
            .contains("token_count"));
    }
}

#[test]
fn missing_reset_and_replaced_sources_are_unknown_instead_of_zero_usage() {
    let (_dir, store, log) = fixture();
    event(&log, 100, 80, 10);
    meter::attach(&store, &log, false).unwrap();
    event(&log, 90, 70, 9);
    let value = meter::summary(&store).unwrap();
    assert_eq!(value["observed_sessions"], 0);
    assert_eq!(value["unavailable_sessions"], json!(["parent"]));
    std::fs::remove_file(&log).unwrap();
    assert_eq!(
        meter::summary(&store).unwrap()["unavailable_sessions"],
        json!(["parent"])
    );
}

#[test]
fn explicit_whole_session_counts_model_changes_and_ignores_partial_writes() {
    use std::io::Write;
    let (_dir, store, log) = fixture();
    event(&log, 10, 5, 2);
    writeln!(
        std::fs::OpenOptions::new().append(true).open(&log).unwrap(),
        "{}",
        json!({"type":"turn_context","payload":{"model":"gpt-5.6-terra"}})
    )
    .unwrap();
    event(&log, 30, 15, 5);
    write!(
        std::fs::OpenOptions::new().append(true).open(&log).unwrap(),
        "{{partial"
    )
    .unwrap();
    meter::attach(&store, &log, true).unwrap();
    let value = meter::summary(&store).unwrap();
    assert_eq!(value["total"]["input_tokens"], 30);
    assert_eq!(value["by_model"]["gpt-5.6-terra"]["input_tokens"], 20);
    assert_eq!(value["by_model"]["gpt-6-astra"]["output_tokens"], 2);
}

#[test]
fn usage_cli_persists_measurements_but_show_does_not_write() {
    let (dir, store, log) = fixture();
    assert!(std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    let call = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_hwahap"))
            .arg("usage")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let cwd = dir.path().to_str().unwrap();
    event(&log, 100, 80, 10);
    call(&["attach", cwd, log.to_str().unwrap()]);
    let snapshot = std::fs::read(store.root().join("usage.json")).unwrap();
    event(&log, 120, 90, 13);
    let shown = call(&["show", cwd]);
    assert_eq!(shown["observed_session_usage"]["total"]["input_tokens"], 20);
    assert_eq!(
        std::fs::read(store.root().join("usage.json")).unwrap(),
        snapshot
    );
    let synced = call(&["sync", cwd]);
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.root().join("usage.json")).unwrap()).unwrap();
    assert_eq!(synced, saved);
    assert_eq!(
        synced["observed_session_usage"]["total"]["output_tokens"],
        3
    );
}

#[test]
fn explicit_rates_price_cached_input_once_and_never_claim_a_bill() {
    let (_dir, store, log) = fixture();
    meter::attach(&store, &log, false).unwrap();
    event(&log, 1_000_000, 700_000, 20_000);
    let card = json!({"currency":"test-units","source":"synthetic regression rates",
        "effective_date":"2026-09-06","assumptions":"flat test tariff",
        "per_million":{"gpt-6-astra":{"input":2.0,"cached_input":0.2,"cache_write_input":2.5,"output":10.0}}});
    std::fs::write(store.root().join("pricing.json"), card.to_string()).unwrap();
    let value = hwahap::cost::persist(&store).unwrap();
    assert!((value["cost_estimate"]["priced_subtotal"].as_f64().unwrap() - 0.94).abs() < 1e-9);
    assert!(value["cost_estimate"]["total_billed_cost"].is_null());
    let mut invalid = card;
    invalid["per_million"]["gpt-6-astra"]["input"] = json!(-1);
    std::fs::write(store.root().join("pricing.json"), invalid.to_string()).unwrap();
    let value = hwahap::cost::summary(&store).unwrap();
    assert_eq!(value["cost_estimate"]["status"], "invalid_configuration");
    assert_eq!(
        value["observed_session_usage"]["total"]["input_tokens"],
        1_000_000
    );
}

#[test]
fn no_counters_and_replaced_session_identity_are_unavailable() {
    let (_dir, store, log) = fixture();
    meter::attach(&store, &log, false).unwrap();
    let value = meter::summary(&store).unwrap();
    assert_eq!(value["observed_sessions"], 0);
    assert_eq!(value["unavailable_sessions"], json!(["parent"]));
    event(&log, 100, 80, 10);
    assert_eq!(meter::summary(&store).unwrap()["observed_sessions"], 1);
    let text = std::fs::read_to_string(&log)
        .unwrap()
        .replace("parent", "replacement");
    std::fs::write(&log, text).unwrap();
    assert_eq!(meter::summary(&store).unwrap()["observed_sessions"], 0);
}

#[test]
fn absent_write_rate_is_unpriced_until_explicitly_configured() {
    use std::io::Write;
    let (_dir, store, log) = fixture();
    meter::attach(&store, &log, false).unwrap();
    writeln!(std::fs::OpenOptions::new().append(true).open(&log).unwrap(), "{}", json!({
        "type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{
        "input_tokens":1_000_000,"cached_input_tokens":700_000,"cache_write_input_tokens":100_000,"output_tokens":20_000}}}})).unwrap();
    let mut card = json!({"currency":"test-units","source":"synthetic regression rates",
        "effective_date":"2026-09-06","assumptions":"flat test tariff",
        "per_million":{"gpt-6-astra":{"input":2.0,"cached_input":0.2,"output":10.0}}});
    let path = store.root().join("pricing.json");
    std::fs::write(&path, card.to_string()).unwrap();
    let value = hwahap::cost::summary(&store).unwrap();
    assert!(value["cost_estimate"]["priced_subtotal"].is_null());
    assert_eq!(
        value["cost_estimate"]["unpriced_models"],
        json!(["gpt-6-astra"])
    );
    card["per_million"]["gpt-6-astra"]["cache_write_input"] = json!(2.5);
    std::fs::write(&path, card.to_string()).unwrap();
    let value = hwahap::cost::summary(&store).unwrap();
    assert!((value["cost_estimate"]["priced_subtotal"].as_f64().unwrap() - 0.99).abs() < 1e-9);
    card["per_million"] = json!({});
    std::fs::write(&path, card.to_string()).unwrap();
    let value = hwahap::cost::summary(&store).unwrap();
    assert!(value["cost_estimate"]["priced_subtotal"].is_null());
}
