use hwahap::planning_review::{FindingStatus, PlanningReviewResult};
use serde_json::{json, Value};
fn finding(id: &str, kind: &str) -> Value {
    json!({"id":id,"parent_id":null,"kind":kind,"targets":["U1"],
        "evidence":["T1 omits A2"],"expected":"cover A2 in U1 tests",
        "status":"open","depends_on":[]})
}
fn fail(items: Vec<Value>) -> String {
    json!({"verdict":"fail","findings":items}).to_string()
}
#[test]
fn t05_typed_contract_keeps_unit_review_separate() {
    assert!(PlanningReviewResult::parse(r#"{"verdict":"fail","findings":["gap"]}"#, &[]).is_err());
    assert!(
        hwahap::agentresult::ReviewResult::parse(&fail(vec![finding("CC1", "structure")])).is_err()
    );
    assert!(PlanningReviewResult::parse(&fail(vec![finding("CC1", "structure")]), &[]).is_ok());
}
#[test]
fn t06_resolution_needs_known_identity_and_fresh_evidence() {
    let mut f = finding("CC1", "structure");
    let mut known = PlanningReviewResult::parse(&fail(vec![f.clone()]), &[])
        .unwrap()
        .findings;
    f["status"] = json!("resolved");
    let pass = json!({"verdict":"pass","findings":[f.clone()]}).to_string();
    assert!(PlanningReviewResult::parse(&pass, &[]).is_err());
    assert!(PlanningReviewResult::parse(&pass, &known).is_err());
    known[0].status = FindingStatus::Resolved;
    assert!(PlanningReviewResult::parse(&pass, &known).is_ok());
    assert!(PlanningReviewResult::parse(r#"{"verdict":"pass","findings":[]}"#, &known).is_err());
    f["expected"] = json!("drop A2");
    assert!(PlanningReviewResult::parse(
        &json!({"verdict":"pass","findings":[f]}).to_string(),
        &known
    )
    .is_err());
}
#[test]
fn t08_mixed_links_reject_cycles_and_allow_choice_before_structure() {
    let a = finding("CC1", "choice");
    let mut b = finding("CC2", "structure");
    b["depends_on"] = json!(["CC1"]);
    assert!(PlanningReviewResult::parse(&fail(vec![a.clone(), b.clone()]), &[]).is_ok());
    let mut a = a;
    a["depends_on"] = json!(["CC2"]);
    assert!(PlanningReviewResult::parse(&fail(vec![a, b]), &[]).is_err());
}

#[test]
fn t15_budget_is_run_wide_and_resumes_an_unfinished_reservation() {
    use hwahap::{
        canonical::Digest, clock::FixedClock, planning_review::reserve_structure_attempt,
        state::Store,
    };
    let tmp = tempfile::tempdir().unwrap();
    let clock = FixedClock::new("2026-09-07T00:00:00Z");
    let store = Store::open(tmp.path()).unwrap();
    let ids = vec!["CC1".into()];
    for n in 1..=3 {
        let digest = Digest::of(&n).unwrap();
        assert_eq!(
            reserve_structure_attempt(&store, &clock, "run", &digest, &ids).unwrap(),
            n
        );
        let reopened = Store::open(tmp.path()).unwrap();
        assert_eq!(
            reserve_structure_attempt(&reopened, &clock, "run", &digest, &ids).unwrap(),
            n
        );
        store
            .append_event(
                &clock,
                "planning_structure_finished",
                json!({"run_id":"run","attempt":n}),
            )
            .unwrap();
    }
    assert!(reserve_structure_attempt(
        &store,
        &clock,
        "run",
        &Digest::of(&4).unwrap(),
        &["split-child".into()]
    )
    .is_err());
    assert_eq!(
        reserve_structure_attempt(&store, &clock, "new-run", &Digest::of(&4).unwrap(), &ids)
            .unwrap(),
        1
    );
}

mod common;
use common::{step, Fixture, Reply, Script};
use hwahap::{profile::Role, state::Store};
const PASS: &str = r#"{"verdict":"pass","findings":[]}"#;
fn facts() -> String {
    serde_json::json!({
        "facts": [{
            "id": "F1",
            "question": "what does the repository already contain?",
            "answer": "one seed file under src/",
            "sources": ["src/existing.txt:1"]
        }]
    })
    .to_string()
}

fn decisions() -> String {
    let not_applicable: Vec<serde_json::Value> = (2..=12)
        .map(|n| {
            serde_json::json!({
                "surface": format!("S{n}"),
                "reason": "this change has no surface here"
            })
        })
        .collect();
    serde_json::json!({
        "decisions": [
            {
                "id": "C1",
                "surface": "S1",
                "kind": "decision",
                "question": "Should the generated file be appended to or replaced?",
                "alternatives": [
                    {"id": "ALT1", "value": "replace it every run"},
                    {"id": "ALT2", "value": "append to it"}
                ],
                "recommendation": {
                    "mode": "recommended",
                    "choice": "ALT1",
                    "rationale": ["a replaced file is reproducible"],
                    "evidence": ["F1"],
                    "tradeoffs": ["history is lost"],
                    "impact": ["files"],
                    "confidence": "high"
                },
                "depends_on": []
            },
            {
                "id": "C2",
                "surface": "S1",
                "kind": "scenario",
                "question": "What must happen when the documentation directory does not exist?",
                "alternatives": [
                    {"id": "ALT1", "value": "create it"},
                    {"id": "ALT2", "value": "fail with a typed error"}
                ],
                "recommendation": {
                    "mode": "no_recommendation",
                    "rationale": ["both are defensible and the user must choose"]
                },
                "depends_on": []
            }
        ],
        "not_applicable": not_applicable
    })
    .to_string()
}

fn all_answers() -> String {
    let mut lines = vec!["C1=REC".to_string(), "C2=ALT1".to_string()];
    lines.extend((2..=12).map(|n| format!("S{n}=NA")));
    lines.join("\n")
}

fn structure() -> String {
    serde_json::json!({
        "requirements": [
            {"id": "R1", "statement": "a generated file exists", "decision_ids": ["C1"]},
            {"id": "R2", "statement": "the documentation directory is created", "decision_ids": ["C2"]}
        ],
        "acceptance": [
            {"id": "A1", "requirement_ids": ["R1"], "observable": "src/added.txt exists"},
            {"id": "A2", "requirement_ids": ["R2"], "observable": "docs/added.md exists"}
        ],
        "units": [
            {"id": "U1", "title": "generate the file", "paths": ["src/"],
             "acceptance_ids": ["A1"], "depends_on": [], "probe": false},
            {"id": "U2", "title": "document it", "paths": ["docs/"],
             "acceptance_ids": ["A2"], "depends_on": ["U1"], "probe": false}
        ],
        "tests": [
            {"id": "T1", "command": "test -f src/added.txt", "acceptance_ids": ["A1"], "unit_id": "U1"},
            {"id": "T2", "command": "test -f docs/added.md", "acceptance_ids": ["A2"], "unit_id": "U2"}
        ],
        "full_suite": "test -f src/added.txt && test -f docs/added.md"
    })
    .to_string()
}

async fn setup(fixture: &Fixture, script: &Script) {
    let engine = fixture.engine();
    engine
        .start_planning("Add a generated file and document it", true)
        .unwrap();
    engine.step_with(script, None, None).await.unwrap();
    engine
        .step_with(script, None, Some(&all_answers()))
        .await
        .unwrap();
    assert_eq!(
        engine.step_with(script, None, None).await.unwrap().state,
        "proving"
    );
}
fn initial() -> Vec<common::Step> {
    vec![
        step(Role::FactFinder, Reply::say(facts())),
        step(Role::Recommender, Reply::say(decisions())),
        step(
            Role::Recommender,
            Reply::say(r#"{"decisions":[],"not_applicable":[]}"#),
        ),
        step(Role::PlanSynthesis, Reply::say(structure())),
    ]
}
fn resolved(mut f: Value) -> String {
    f["status"] = json!("resolved");
    f["evidence"] = json!(["Current T1 covers A1; T2 covers A2; choices unchanged"]);
    json!({"verdict":"pass","findings":[f]}).to_string()
}
#[tokio::test]
async fn t05_structure_only_repairs_once_without_new_decisions_and_reviews_twice() {
    let fixture = Fixture::new();
    let f = finding("CC1", "structure");
    let mut steps = initial();
    let mut broken: Value = serde_json::from_str(&structure()).unwrap();
    broken["units"][0]["acceptance_ids"] = json!(["A1", "A2"]);
    steps[3].reply = Reply::say(broken.to_string());
    let mut repaired = broken;
    repaired["tests"][0]["acceptance_ids"] = json!(["A1", "A2"]);
    repaired["tests"][0]["command"] = json!("test -f src/added.txt && test -f docs/added.md");
    steps.extend([
        step(Role::ColdConsumer, Reply::say(fail(vec![f.clone()]))),
        step(Role::PlanCritic, Reply::say(PASS)),
        step(Role::PlanSynthesis, Reply::say(repaired.to_string())),
        step(Role::ColdConsumer, Reply::say(resolved(f.clone()))),
        step(Role::PlanCritic, Reply::say(resolved(f))),
    ]);
    let script = Script::new(steps);
    setup(&fixture, &script).await;
    let result = fixture
        .engine()
        .step_with(&script, None, None)
        .await
        .unwrap();
    assert_eq!(result.state, "proving");
    let store = Store::open(&fixture.repo).unwrap();
    let proposed = store.read_plan().unwrap().unwrap();
    assert_eq!(proposed.decisions.len(), 2);
    assert_eq!(proposed.decomposition_history.len(), 1);
    assert!(!hwahap::validate::freeze_blockers(&proposed)
        .unwrap()
        .is_empty());
    let result = fixture
        .engine()
        .step_with(&script, None, None)
        .await
        .unwrap();
    assert_eq!(result.state, "awaiting_confirmation", "{}", result.message);
    let final_plan = store.read_plan().unwrap().unwrap();
    assert_ne!(
        final_plan.reviews.critic.as_ref().unwrap().plan_digest,
        proposed.reviews.critic.as_ref().unwrap().plan_digest
    );
    assert!(hwahap::validate::freeze_blockers(&final_plan)
        .unwrap()
        .is_empty());
    for role in [
        Role::PlanSynthesis,
        Role::ColdConsumer,
        Role::PlanCritic,
        Role::Recommender,
    ] {
        assert_eq!(
            script.roles().iter().filter(|r| **r == role).count(),
            2,
            "{role:?}"
        );
    }
    let closure = store
        .read_events()
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "planning_findings_resolved")
        .unwrap();
    assert_eq!(
        closure.data["reviewed"],
        json!(final_plan.review_digest().unwrap())
    );
    assert_eq!(closure.data["reviews"], json!(final_plan.reviews));
    let fingerprint = final_plan.unit_fingerprint("U1").unwrap();
    let mut changed = final_plan.clone();
    changed.decomposition_history[0]
        .evidence
        .push("another rationale".into());
    assert_eq!(fingerprint, changed.unit_fingerprint("U1").unwrap());
    assert_ne!(
        final_plan.review_digest().unwrap(),
        changed.review_digest().unwrap()
    );
}

fn certificate(items: &[Value]) -> String {
    let items: Vec<_> = items
        .iter()
        .cloned()
        .map(|mut f| {
            f["status"] = json!("resolved");
            f["evidence"] = json!(["Verified current candidate and supplied resolution evidence"]);
            f
        })
        .collect();
    json!({"verdict":"pass","findings":items}).to_string()
}
#[tokio::test]
async fn t08_mixed_choice_waits_for_real_answer_before_structure() {
    let fixture = Fixture::new();
    let mut choice = finding("CC1", "choice");
    choice["parent_id"] = json!("CC2");
    let mut structure_finding = finding("CC2", "structure");
    structure_finding["depends_on"] = json!(["CC1"]);
    let findings = vec![choice, structure_finding];
    let mut followup: Value = serde_json::from_str(&decisions()).unwrap();
    followup["decisions"] = json!([followup["decisions"][1].clone()]);
    followup["decisions"][0]["id"] = json!("C3");
    followup["not_applicable"] = json!([]);
    let mut repaired: Value = serde_json::from_str(&structure()).unwrap();
    repaired["requirements"][1]["decision_ids"] = json!(["C2", "C3"]);
    let mut steps = initial();
    steps.extend([
        step(Role::ColdConsumer, Reply::say(fail(findings.clone()))),
        step(Role::PlanCritic, Reply::say(PASS)),
        step(Role::Recommender, Reply::say(followup.to_string())),
        step(
            Role::Recommender,
            Reply::say(r#"{"decisions":[],"not_applicable":[]}"#),
        ),
        step(Role::PlanSynthesis, Reply::say(repaired.to_string())),
        step(Role::ColdConsumer, Reply::say(certificate(&findings))),
        step(Role::PlanCritic, Reply::say(certificate(&findings))),
    ]);
    let script = Script::new(steps);
    setup(&fixture, &script).await;
    assert_eq!(
        fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "deciding"
    );
    assert_eq!(
        script
            .roles()
            .iter()
            .filter(|r| **r == Role::PlanSynthesis)
            .count(),
        1
    );
    let store = Store::open(&fixture.repo).unwrap();
    let waiting = store.read_plan().unwrap().unwrap();
    assert!(waiting.decision("C3").unwrap().answer.is_none());
    assert_eq!(
        hwahap::dialogue::QuestionBatch::derive(&waiting)
            .unwrap()
            .unwrap()
            .questions[0]
            .id,
        "C3"
    );
    fixture
        .engine()
        .step_with(&script, None, None)
        .await
        .unwrap();
    assert_eq!(
        store.read_plan().unwrap().unwrap().decomposition_history,
        waiting.decomposition_history
    );
    fixture
        .engine()
        .step_with(&script, None, Some("C3=ALT1"))
        .await
        .unwrap();
    let mut last = String::new();
    for _ in 0..5 {
        let result = fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap();
        last = result.state;
        if last == "awaiting_confirmation" {
            break;
        }
    }
    assert_eq!(last, "awaiting_confirmation");
    let plan = store.read_plan().unwrap().unwrap();
    assert!(plan
        .decomposition_history
        .iter()
        .any(|h| h.action == "choice_answered" && h.evidence.join("").contains("C3=ALT1")));
    assert_eq!(
        script
            .roles()
            .iter()
            .filter(|r| **r == Role::PlanSynthesis)
            .count(),
        2
    );
}

#[tokio::test]
async fn t07_fact_routes_to_current_source_and_blocker_waits_for_evidence() {
    for kind in ["fact", "blocker"] {
        let fixture = Fixture::new();
        let issue = finding("CC1", kind);
        let mut steps = initial();
        steps.extend([
            step(Role::ColdConsumer, Reply::say(fail(vec![issue.clone()]))),
            step(Role::PlanCritic, Reply::say(PASS)),
        ]);
        if kind == "fact" {
            let mut more: Value = serde_json::from_str(&facts()).unwrap();
            more["facts"][0]["id"] = json!("F2");
            steps.extend([
                step(Role::FactFinder, Reply::say(more.to_string())),
                step(
                    Role::ColdConsumer,
                    Reply::say(certificate(std::slice::from_ref(&issue))),
                ),
                step(Role::PlanCritic, Reply::say(certificate(&[issue]))),
            ]);
        }
        let script = Script::new(steps);
        setup(&fixture, &script).await;
        let result = fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap();
        let store = Store::open(&fixture.repo).unwrap();
        if kind == "fact" {
            assert_eq!(store.read_plan().unwrap().unwrap().facts.len(), 2);
            assert_eq!(
                fixture
                    .engine()
                    .step_with(&script, None, None)
                    .await
                    .unwrap()
                    .state,
                "awaiting_confirmation"
            );
        } else {
            assert_eq!(result.state, "plan_conflict");
            assert_eq!(result.next, "await_user");
            let before = store.read_plan().unwrap().unwrap();
            fixture
                .engine()
                .step_with(&script, None, None)
                .await
                .unwrap();
            assert_eq!(store.read_plan().unwrap().unwrap(), before);
            assert_eq!(before.planning_findings[0].status, FindingStatus::Open);
        }
        assert_eq!(
            script
                .roles()
                .iter()
                .filter(|r| **r == Role::Recommender)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn t15_invalid_structure_retains_findings_and_validation_after_three_attempts() {
    let fixture = Fixture::new();
    let mut steps = initial();
    steps.extend([
        step(
            Role::ColdConsumer,
            Reply::say(fail(vec![finding("CC1", "structure")])),
        ),
        step(Role::PlanCritic, Reply::say(PASS)),
    ]);
    let mut bad: Value = serde_json::from_str(&structure()).unwrap();
    bad["tests"] = json!([]);
    for _ in 0..3 {
        steps.push(step(Role::PlanSynthesis, Reply::say(bad.to_string())));
    }
    let script = Script::new(steps);
    setup(&fixture, &script).await;
    for _ in 0..3 {
        assert_eq!(
            fixture
                .engine()
                .step_with(&script, None, None)
                .await
                .unwrap()
                .state,
            "proving"
        );
    }
    assert_eq!(
        fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "blocked"
    );
    let store = Store::open(&fixture.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    assert_eq!(plan.planning_findings[0].status, FindingStatus::Open);
    assert_eq!(plan.decomposition_history.len(), 3);
    assert!(!plan
        .decomposition_history
        .last()
        .unwrap()
        .validation
        .is_empty());
    assert_eq!(
        store
            .read_events()
            .unwrap()
            .iter()
            .filter(|e| e.kind == "planning_structure_attempt")
            .count(),
        3
    );
    assert_eq!(plan.decisions.len(), 2);
}

#[test]
fn t06_two_reviewers_reopening_one_issue_preserve_identity_and_supersede_old_resolution() {
    use hwahap::planning_review::merge_open;
    let mut plan = hwahap::plan::Plan::new("run", "main", "goal");
    let findings = PlanningReviewResult::parse(&fail(vec![finding("CC1", "choice")]), &[])
        .unwrap()
        .findings;
    merge_open(&mut plan, &findings).unwrap();
    plan.planning_findings[0].status = FindingStatus::Resolved;
    merge_open(&mut plan, &[findings[0].clone(), findings[0].clone()]).unwrap();
    assert_eq!(plan.planning_findings.len(), 1);
    assert_eq!(plan.decomposition_history.len(), 1);
    assert_eq!(plan.decomposition_history[0].action, "reopened");
    assert_eq!(plan.planning_findings[0].status, FindingStatus::Open);
}

#[tokio::test]
async fn t07_fact_with_invalid_current_source_keeps_finding_open() {
    let fixture = Fixture::new();
    let mut steps = initial();
    let mut bad: Value = serde_json::from_str(&facts()).unwrap();
    bad["facts"][0]["id"] = json!("F2");
    bad["facts"][0]["sources"] = json!(["missing.txt:1"]);
    steps.extend([
        step(
            Role::ColdConsumer,
            Reply::say(fail(vec![finding("CC1", "fact")])),
        ),
        step(Role::PlanCritic, Reply::say(PASS)),
        step(Role::FactFinder, Reply::say(bad.to_string())),
    ]);
    let script = Script::new(steps);
    setup(&fixture, &script).await;
    assert!(fixture
        .engine()
        .step_with(&script, None, None)
        .await
        .is_err());
    let plan = Store::open(&fixture.repo)
        .unwrap()
        .read_plan()
        .unwrap()
        .unwrap();
    assert_eq!(plan.planning_findings[0].status, FindingStatus::Open);
    assert_eq!(plan.facts.len(), 1);
}

#[tokio::test]
async fn t15_interrupted_parent_repair_resumes_same_attempt_after_engine_restart() {
    let fixture = Fixture::new();
    let issue = finding("CC1", "structure");
    let mut steps = initial();
    steps.extend([
        step(Role::ColdConsumer, Reply::say(fail(vec![issue.clone()]))),
        step(Role::PlanCritic, Reply::say(PASS)),
        step(
            Role::PlanSynthesis,
            Reply::Fail("interrupted dispatch".into()),
        ),
    ]);
    let script = Script::new(steps);
    setup(&fixture, &script).await;
    assert!(fixture
        .engine()
        .step_with(&script, None, None)
        .await
        .is_err());
    let store = Store::open(&fixture.repo).unwrap();
    assert_eq!(
        store
            .read_events()
            .unwrap()
            .iter()
            .filter(|e| e.kind == "planning_structure_attempt")
            .count(),
        1
    );
    script.extend(vec![
        step(Role::PlanSynthesis, Reply::say(structure())),
        step(
            Role::ColdConsumer,
            Reply::say(certificate(std::slice::from_ref(&issue))),
        ),
        step(Role::PlanCritic, Reply::say(certificate(&[issue]))),
    ]);
    assert_eq!(
        fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "proving"
    );
    assert_eq!(
        store
            .read_events()
            .unwrap()
            .iter()
            .filter(|e| e.kind == "planning_structure_attempt")
            .count(),
        1
    );
    assert_eq!(
        fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "awaiting_confirmation"
    );
}

#[tokio::test]
async fn t07_blocker_resolution_requires_user_evidence_and_two_fresh_reviews() {
    let fixture = Fixture::new();
    let issue = finding("CC1", "blocker");
    let mut steps = initial();
    steps.extend([
        step(Role::ColdConsumer, Reply::say(fail(vec![issue.clone()]))),
        step(Role::PlanCritic, Reply::say(PASS)),
    ]);
    let script = Script::new(steps);
    setup(&fixture, &script).await;
    assert_eq!(
        fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "plan_conflict"
    );
    let store = Store::open(&fixture.repo).unwrap();
    script.extend(vec![
        step(
            Role::Recommender,
            Reply::say(r#"{"decisions":[],"not_applicable":[]}"#),
        ),
        step(Role::PlanSynthesis, Reply::say(structure())),
        step(
            Role::ColdConsumer,
            Reply::say(certificate(std::slice::from_ref(&issue))),
        ),
        step(Role::PlanCritic, Reply::say(certificate(&[issue]))),
    ]);
    fixture
        .engine()
        .step_with(
            &script,
            None,
            Some("U1 테스트가 A2도 검증하도록 승인합니다."),
        )
        .await
        .unwrap();
    let mut state = String::new();
    for _ in 0..6 {
        state = fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state;
        if state == "awaiting_confirmation" {
            break;
        }
    }
    assert_eq!(state, "awaiting_confirmation");
    let plan = store.read_plan().unwrap().unwrap();
    assert!(plan
        .decomposition_history
        .iter()
        .any(|h| h.action == "blocker_evidence" && h.evidence[0].contains("승인합니다")));
    assert!(hwahap::render::plan_markdown(&plan)
        .unwrap()
        .contains("<details><summary>지적과 해결 근거</summary>"));
}

#[tokio::test]
async fn t08_structure_precedes_dependent_blocker_or_choice_and_fresh_reviews_close_both() {
    for kind in ["blocker", "choice"] {
        let fixture = Fixture::new();
        let mut parent = finding("CC1", kind);
        let mut child = finding("CC2", "structure");
        if kind == "blocker" {
            child["parent_id"] = json!("CC1");
        } else {
            parent["depends_on"] = json!(["CC2"]);
        }
        let issues = vec![parent, child];
        let mut steps = initial();
        steps.extend([
            step(Role::ColdConsumer, Reply::say(fail(issues.clone()))),
            step(Role::PlanCritic, Reply::say(PASS)),
            step(Role::PlanSynthesis, Reply::say(structure())),
        ]);
        let script = Script::new(steps);
        setup(&fixture, &script).await;
        let result = fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap();
        assert_eq!(result.state, "proving", "{kind}: {}", result.message);
        let store = Store::open(&fixture.repo).unwrap();
        let repaired = store.read_plan().unwrap().unwrap();
        assert_eq!(repaired.planning_findings[0].status, FindingStatus::Open);
        assert_eq!(
            repaired.planning_findings[1].status,
            FindingStatus::Resolved
        );
        assert_eq!(repaired.decomposition_history[0].finding_ids, vec!["CC2"]);
        let mut final_structure: Value = serde_json::from_str(&structure()).unwrap();
        if kind == "choice" {
            let mut followup: Value = serde_json::from_str(&decisions()).unwrap();
            followup["decisions"] = json!([followup["decisions"][1].clone()]);
            followup["decisions"][0]["id"] = json!("C3");
            followup["not_applicable"] = json!([]);
            script.extend(vec![step(
                Role::Recommender,
                Reply::say(followup.to_string()),
            )]);
            assert_eq!(
                fixture
                    .engine()
                    .step_with(&script, None, None)
                    .await
                    .unwrap()
                    .state,
                "deciding"
            );
            fixture
                .engine()
                .step_with(&script, None, Some("C3=ALT1"))
                .await
                .unwrap();
            final_structure["requirements"][1]["decision_ids"] = json!(["C2", "C3"]);
        } else {
            assert_eq!(
                fixture
                    .engine()
                    .step_with(&script, None, None)
                    .await
                    .unwrap()
                    .state,
                "plan_conflict"
            );
            fixture
                .engine()
                .step_with(
                    &script,
                    None,
                    Some("The fixture owner grants the required permission."),
                )
                .await
                .unwrap();
        }
        script.extend(vec![
            step(
                Role::Recommender,
                Reply::say(r#"{"decisions":[],"not_applicable":[]}"#),
            ),
            step(Role::PlanSynthesis, Reply::say(final_structure.to_string())),
            step(Role::ColdConsumer, Reply::say(certificate(&issues))),
            step(Role::PlanCritic, Reply::say(certificate(&issues))),
        ]);
        let mut state = String::new();
        for _ in 0..6 {
            state = fixture
                .engine()
                .step_with(&script, None, None)
                .await
                .unwrap()
                .state;
            if state == "awaiting_confirmation" {
                break;
            }
        }
        assert_eq!(state, "awaiting_confirmation");
        let closed = store.read_plan().unwrap().unwrap();
        assert!(hwahap::validate::freeze_blockers(&closed)
            .unwrap()
            .is_empty());
        assert_eq!(closed.planning_findings.len(), 2);
        assert_eq!(script.remaining(), 0);
    }
}

#[tokio::test]
async fn t08_only_ready_structural_children_are_marked_provisionally_resolved() {
    let fixture = Fixture::new();
    let parent = finding("CC1", "structure");
    let mut child = finding("CC2", "structure");
    child["parent_id"] = json!("CC1");
    let issues = vec![parent, child];
    let mut steps = initial();
    steps.extend([
        step(Role::ColdConsumer, Reply::say(fail(issues.clone()))),
        step(Role::PlanCritic, Reply::say(PASS)),
        step(Role::PlanSynthesis, Reply::say(structure())),
        step(Role::PlanSynthesis, Reply::say(structure())),
        step(Role::ColdConsumer, Reply::say(certificate(&issues))),
        step(Role::PlanCritic, Reply::say(certificate(&issues))),
    ]);
    let script = Script::new(steps);
    setup(&fixture, &script).await;
    fixture
        .engine()
        .step_with(&script, None, None)
        .await
        .unwrap();
    let store = Store::open(&fixture.repo).unwrap();
    let child_done = store.read_plan().unwrap().unwrap();
    assert_eq!(child_done.planning_findings[0].status, FindingStatus::Open);
    assert_eq!(
        child_done.planning_findings[1].status,
        FindingStatus::Resolved
    );
    assert_eq!(child_done.decomposition_history[0].finding_ids, vec!["CC2"]);
    fixture
        .engine()
        .step_with(&script, None, None)
        .await
        .unwrap();
    let parent_done = store.read_plan().unwrap().unwrap();
    assert_eq!(
        parent_done.decomposition_history[1].finding_ids,
        vec!["CC1"]
    );
    assert!(!hwahap::validate::freeze_blockers(&parent_done)
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .engine()
            .step_with(&script, None, None)
            .await
            .unwrap()
            .state,
        "awaiting_confirmation"
    );
    assert_eq!(script.remaining(), 0);
}
