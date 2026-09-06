use super::*;
use common::{step, Reply, Script};
use hwahap::profile::Role;

pub async fn plan_ready(f: &Fixture) -> hwahap::engine::StepOutcome {
    f.engine()
        .start_planning("Generate an output file", true)
        .unwrap();
    confirm_ready(f).await
}

pub async fn confirm_ready(f: &Fixture) -> hwahap::engine::StepOutcome {
    let engine = f.engine();
    let facts = serde_json::json!({"facts":[{"id":"F1","question":"What exists?","answer":"seed file","sources":["src/existing.txt:1"]}]});
    let mut decisions = serde_json::json!({
        "decisions":(1..=2).map(|n| serde_json::json!({
            "id":format!("C{n}"),"surface":"S1","kind":if n == 1 {"decision"} else {"scenario"},
            "question":if n == 1 {"Replace generated output?"} else {"Replace existing output?"},
            "alternatives":[{"id":"ALT1","value":"replace"},{"id":"ALT2","value":"append"}],
            "recommendation":{"mode":"no_recommendation","rationale":["User preference"]},"depends_on":[]
        })).collect::<Vec<_>>(),
        "not_applicable":(2..=12).map(|n| serde_json::json!({"surface":format!("S{n}"),"reason":"No such behavior"})).collect::<Vec<_>>()
    });
    if !Store::open(&f.repo)
        .unwrap()
        .read_plan()
        .unwrap()
        .unwrap()
        .decisions
        .is_empty()
    {
        decisions["decisions"] = serde_json::json!([]);
    }
    let structure = serde_json::json!({
        "requirements":[{"id":"R1","statement":"replace output","decision_ids":["C1","C2"]}],
        "acceptance":[{"id":"A1","requirement_ids":["R1"],"observable":"output exists"}],
        "units":[{"id":"U1","title":"write output","paths":["output"],"acceptance_ids":["A1"],"depends_on":[],"probe":false}],
        "tests":[{"id":"T1","command":"test -f output","acceptance_ids":["A1"],"unit_id":"U1"}],
        "full_suite":"test -f output"
    });
    let script = Script::new(vec![
        step(Role::FactFinder, Reply::say(facts.to_string())),
        step(Role::Recommender, Reply::say(decisions.to_string())),
        step(
            Role::Recommender,
            Reply::say(r#"{"decisions":[],"not_applicable":[]}"#),
        ),
        step(Role::PlanSynthesis, Reply::say(structure.to_string())),
        step(Role::ColdConsumer, Reply::say(r#"{"verdict":"pass"}"#)),
        step(Role::PlanCritic, Reply::say(r#"{"verdict":"pass"}"#)),
    ]);
    engine.step_with(&script, None, None).await.unwrap();
    let mut answers = vec!["C1=ALT1".to_string(), "C2=ALT1".to_string()];
    answers.extend((2..=12).map(|n| format!("S{n}=NA")));
    engine
        .step_with(&script, None, Some(&answers.join("\n")))
        .await
        .unwrap();
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "proving"
    );
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "awaiting_confirmation"
    );
    let plan = Store::open(&f.repo).unwrap().read_plan().unwrap().unwrap();
    let ready = engine
        .step_with(
            &script,
            None,
            Some(&format!("CONFIRM PLAN {}", plan.challenge().unwrap())),
        )
        .await
        .unwrap();
    assert_eq!(ready.state, "plan_ready");
    ready
}
