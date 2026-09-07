#![cfg(unix)]
mod common;
use common::{git, step, Fixture, Reply, Script};
use hwahap::{
    approval::*,
    canonical::Digest,
    engine::{BuildRequest, BuildUnit},
    profile::Role,
    state::Store,
};

fn approved(f: &Fixture) -> ApprovedPlanRequest {
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let markdown = "Create feature.txt containing ready.";
    let instruction = "승인한 계획대로 구현을 진행해줘.";
    let plan_digest = Digest::of_bytes(markdown.as_bytes()).to_string();
    ApprovedPlanRequest {
        approval: PlanApproval {
            reference: Some(ReferencedApproval {
                source_reference: "conversation:approved-plan-turn".into(),
                disposition: ApprovalDisposition::Approved,
                plan_digest: plan_digest.clone(),
                implementation_request_digest: Digest::of_bytes(instruction.as_bytes()).to_string(),
            }),
            markdown: markdown.into(),
            markdown_digest: plan_digest,
            implementation_request: instruction.into(),
            source_head: git(&f.repo, &["rev-parse", "HEAD"]),
        },
        contract: BuildRequest {
            task_profiles: Default::default(),
            verification_inputs: vec![],
            user_instruction: instruction.into(),
            objective: "Write feature".into(),
            base_branch: "main".into(),
            branch: "codex/flow".into(),
            full_suite: "test -f feature.txt".into(),
            units: vec![BuildUnit {
                title: "feature".into(),
                acceptance: "feature.txt contains ready".into(),
                paths: vec!["feature.txt".into()],
                test_command: "test \"$(cat feature.txt)\" = ready".into(),
            }],
        },
        replaces_plan_digest: None,
    }
}
#[tokio::test]
async fn t04_referenced_approval_preserves_request_and_enters_build_after_independent_translation_review(
) {
    let f = Fixture::new();
    let input = approved(&f);
    let engine = f.engine();
    assert_eq!(
        engine.register_approved_plan(&input).unwrap().state,
        "proving"
    );
    let review = Script::new(vec![
        step(
            Role::ColdConsumer,
            Reply::say(r#"{"verdict":"pass","findings":[]}"#),
        ),
        step(
            Role::PlanCritic,
            Reply::say(r#"{"verdict":"pass","findings":[]}"#),
        ),
    ]);
    assert_eq!(
        engine.step_with(&review, None, None).await.unwrap().state,
        "coding"
    );
    let store = Store::open(&f.repo).unwrap();
    let plan = store.read_plan().unwrap().unwrap();
    assert_eq!(
        plan.frozen.unwrap().answer_text,
        input.approval.implementation_request
    );
    assert_eq!(
        plan.approved_plan.unwrap().reference,
        input.approval.reference
    );
    let events = store.read_events().unwrap();
    engine.register_approved_plan(&input).unwrap();
    assert_eq!(store.read_events().unwrap(), events);
}
#[test]
fn t04_referenced_approval_preserves_scope_restrictions() {
    for instruction in [
        "Implement the approved plan. Do not implement unrelated features.",
        "승인한 계획대로 구현해줘. 범위 밖 기능은 구현하지 마.",
    ] {
        let f = Fixture::new();
        let mut input = approved(&f);
        input.contract.user_instruction = instruction.into();
        input.approval.implementation_request = instruction.into();
        input
            .approval
            .reference
            .as_mut()
            .unwrap()
            .implementation_request_digest = Digest::of_bytes(instruction.as_bytes()).to_string();
        assert_eq!(
            f.engine().register_approved_plan(&input).unwrap().state,
            "proving"
        );
        let plan = Store::open(&f.repo).unwrap().read_plan().unwrap().unwrap();
        assert_eq!(
            plan.approved_plan.unwrap().implementation_request,
            instruction
        );
    }
}

#[test]
fn t04_referenced_approval_rejects_refusal_and_changed_bindings_before_start() {
    let f = Fixture::new();
    let original = approved(&f);
    for disposition in [
        ApprovalDisposition::NotApproved,
        ApprovalDisposition::Rejected,
        ApprovalDisposition::Cancelled,
    ] {
        let mut input = original.clone();
        input.approval.reference.as_mut().unwrap().disposition = disposition;
        assert!(f.engine().register_approved_plan(&input).is_err());
    }
    for instruction in [
        "아직 구현하지 마",
        "This plan is not approved",
        "cancel this plan",
    ] {
        let mut input = original.clone();
        input.contract.user_instruction = instruction.into();
        input.approval.implementation_request = instruction.into();
        input
            .approval
            .reference
            .as_mut()
            .unwrap()
            .implementation_request_digest = Digest::of_bytes(instruction.as_bytes()).to_string();
        assert!(f.engine().register_approved_plan(&input).is_err());
    }
    for change in 0..3 {
        let mut input = original.clone();
        match change {
            0 => input.approval.markdown.push_str(" Delete another file."),
            1 => {
                input.approval.reference.as_mut().unwrap().plan_digest = Digest::zero().to_string()
            }
            _ => input.approval.implementation_request.push_str(" changed"),
        }
        assert!(f.engine().register_approved_plan(&input).is_err());
    }
    assert!(Store::open(&f.repo).unwrap().read_run().unwrap().is_none());
    assert!(!f.worktree().exists());
}

#[tokio::test]
async fn t16_native_request_binds_model_identity_decision_and_compacts_registered_progress() {
    use hwahap::{engine::Sessions, native::*, session::SessionSpec};
    use std::sync::Arc;
    let f = Fixture::new();
    let request = approved(&f);
    f.engine().start_build(&request.contract).unwrap();
    let store = Store::open(&f.repo).unwrap();
    common::fixture_native_observation(&store, "parent");
    let broker =
        Arc::new(NativeSessions::new(store.clone(), 8, 10).with_host_session_id("parent".into()));
    let runner = broker.clone();
    let spec = SessionSpec {
        cwd: f.worktree(),
        role: Role::Implementer,
        unit: Some("U1".into()),
        assessment: None,
        prompt: "Bound native fixture evidence. ".repeat(1500),
    };
    let task = tokio::spawn(async move { runner.run(&spec).await });
    let dispatch = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(d) = broker.dispatch().unwrap() {
                break d;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        (&*dispatch.model, &*dispatch.effort),
        ("gpt-5.6-luna", "medium")
    );
    let report = |dispatch| {
        hwahap::mcp::RunReport::from(NativeProgress {
            outcome: f.engine().status().unwrap(),
            dispatch: Some(dispatch),
        })
    };
    let offered = serde_json::to_vec(&report(dispatch.clone())).unwrap();
    let mut status = report(dispatch.clone());
    status.compact_native();
    assert!(status.native_dispatch.as_ref().unwrap().brief.is_empty());
    assert!(serde_json::to_vec(&status).unwrap().len() < offered.len() / 5);
    let artifact = store
        .artifacts_path()
        .join(format!("native-request-{}.json", dispatch.dispatch_id));
    let immutable = std::fs::read(&artifact).unwrap();
    let mut registration = NativeRegistration {
        dispatch_id: dispatch.dispatch_id.clone(),
        agent_id: "worker".into(),
        decision_digest: Some("changed".into()),
    };
    assert!(broker.register(&registration).is_err());
    registration.decision_digest = Some(dispatch.decision.digest.clone());
    broker.register(&registration).unwrap();
    let progress = report(broker.dispatch().unwrap().unwrap());
    assert!(progress.native_dispatch.as_ref().unwrap().brief.is_empty());
    assert_eq!(
        progress.native_brief.as_ref().unwrap().prompt_digest,
        dispatch.prompt_digest
    );
    assert!(serde_json::to_vec(&progress).unwrap().len() < offered.len() / 5);
    assert_eq!(std::fs::read(&artifact).unwrap(), immutable);
    let mut completion = NativeCompletion { decision_digest: Some("changed".into()), dispatch_id: dispatch.dispatch_id.clone(), agent_id: "worker".into(), final_message: serde_json::json!({"dispatch_id":dispatch.dispatch_id,"result":{"status":"completed","summary":"native fixture","conflict":null}}).to_string(), agent_stopped: true, reported_usage: None };
    assert!(broker.complete(completion.clone()).is_err());
    completion.decision_digest = Some(dispatch.decision.digest.clone());
    broker.complete(completion).unwrap();
    let result = task.await.unwrap().unwrap();
    let hwahap::session::SessionReceipt::Native(receipt) = result.receipt;
    assert_eq!(receipt.agent_id, "worker");
    assert_eq!(receipt.model_requested, dispatch.model);
    assert_eq!(receipt.effort_requested.as_str(), dispatch.effort);
    assert_eq!(receipt.decision.unwrap().digest, dispatch.decision.digest);
    assert!(receipt.reported_usage.is_none());
    broker.finish().unwrap();
}

fn two_units(f: &Fixture) -> ApprovedPlanRequest {
    let mut request = approved(f);
    request.contract.units.push(BuildUnit {
        title: "dependent copy".into(),
        acceptance: "copy.txt contains ready".into(),
        paths: vec!["copy.txt".into()],
        test_command: "test \"$(cat copy.txt)\" = ready".into(),
    });
    request.contract.full_suite = "test -f feature.txt && test -f copy.txt".into();
    request.approval.markdown =
        "Create feature.txt and its dependent copy.txt, both containing ready.".into();
    request.approval.markdown_digest =
        Digest::of_bytes(request.approval.markdown.as_bytes()).to_string();
    request.approval.reference.as_mut().unwrap().plan_digest =
        request.approval.markdown_digest.clone();
    request
}
const PASS: &str = r#"{"verdict":"pass","findings":[]}"#;
async fn normal_plan(f: &Fixture, request: &ApprovedPlanRequest) {
    let mut structure = request
        .contract
        .plan("fixture", &request.approval.source_head)
        .unwrap();
    for requirement in &mut structure.requirements {
        requirement.decision_ids = vec!["C1".into()];
    }
    structure.requirements[1].decision_ids = vec!["C2".into()];
    let facts = serde_json::json!({"facts":[{"id":"F1","question":"Existing source?","answer":"A seed file exists.","sources":["src/existing.txt:1"]}]}).to_string();
    let choices = serde_json::json!({"decisions":[{"id":"C1","surface":"S1","kind":"decision","question":"기능 파일과 복사본을 만들까요?","alternatives":[{"id":"ALT1","value":"두 파일 생성"},{"id":"ALT2","value":"기능 파일만 생성"}],"recommendation":{"mode":"no_recommendation","rationale":["사용자가 범위를 선택합니다."]},"depends_on":[]}],"not_applicable":(2..=12).map(|n|serde_json::json!({"surface":format!("S{n}"),"reason":"Local fixture file scope"})).collect::<Vec<_>>()}).to_string();
    let mut choices: serde_json::Value = serde_json::from_str(&choices).unwrap();
    choices["decisions"].as_array_mut().unwrap().push(serde_json::json!({"id":"C2","surface":"S1","kind":"scenario","question":"복사본이 없으면 어떻게 할까요?","alternatives":[{"id":"ALT1","value":"복사본 생성"},{"id":"ALT2","value":"오류 반환"}],"recommendation":{"mode":"no_recommendation","rationale":["사용자가 동작을 선택합니다."]},"depends_on":[]}));
    let choices = choices.to_string();
    let structure = serde_json::json!({"requirements":structure.requirements,"acceptance":structure.acceptance,"units":structure.units,"tests":structure.tests,"full_suite":structure.full_suite}).to_string();
    let script = Script::new(vec![
        step(Role::FactFinder, Reply::say(facts)),
        step(Role::Recommender, Reply::say(choices)),
        step(
            Role::Recommender,
            Reply::say(r#"{"decisions":[],"not_applicable":[]}"#),
        ),
        step(Role::PlanSynthesis, Reply::say(structure)),
        step(Role::ColdConsumer, Reply::say(PASS)),
        step(Role::PlanCritic, Reply::say(PASS)),
    ]);
    let engine = f.engine();
    engine
        .step_with(&script, Some("Create feature and dependent copy"), None)
        .await
        .unwrap();
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "deciding"
    );
    let answers = ["C1=ALT1".into(), "C2=ALT1".into()]
        .into_iter()
        .chain((2..=12).map(|n| format!("S{n}=NA")))
        .collect::<Vec<String>>()
        .join("\n");
    engine
        .step_with(&script, None, Some(&answers))
        .await
        .unwrap();
    engine.step_with(&script, None, None).await.unwrap();
    let proved = engine.step_with(&script, None, None).await.unwrap();
    assert_eq!(proved.state, "awaiting_confirmation", "{}", proved.message);
    let plan = Store::open(&f.repo).unwrap().read_plan().unwrap().unwrap();
    assert_eq!(
        engine
            .step_with(
                &script,
                None,
                Some(&format!(
                    "CONFIRM PLAN {}",
                    plan.digest().unwrap().challenge()
                ))
            )
            .await
            .unwrap()
            .state,
        "coding"
    );
}
fn author(path: &str, value: &str) -> common::Step {
    step(
        Role::Implementer,
        Reply::write(
            &[(path, value)],
            r#"{"status":"completed","summary":"fixture written"}"#,
        ),
    )
}
async fn finish_candidate(f: &Fixture, adjusted: bool) -> hwahap::engine::StepOutcome {
    let mut steps = vec![
        author("feature.txt", if adjusted { "ready\n" } else { "ready" }),
        step(Role::UnitReviewer, Reply::say(PASS)),
    ];
    if !adjusted {
        steps.push(author("copy.txt", "ready"));
    }
    steps.extend([
        step(Role::UnitReviewer, Reply::say(PASS)),
        step(Role::UnitReviewer, Reply::PrAttack),
        step(Role::FinalReview, Reply::pr_defense()),
    ]);
    let script = Script::new(steps);
    let engine = f.engine();
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "final_verifying"
    );
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "pr_review"
    );
    let result = engine.step_with(&script, None, None).await.unwrap();
    assert_eq!(result.state, "awaiting_adjust_or_ship");
    assert_eq!(script.remaining(), 0);
    if adjusted {
        assert!(script
            .calls()
            .iter()
            .all(|c| c.role != Role::Implementer || c.unit.as_deref() == Some("U1")));
    }
    result
}
#[tokio::test]
async fn t09_t13_t17_all_entry_paths_adjust_revalidate_review_and_ship_current_candidate() {
    for mode in 0..3 {
        let f = Fixture::new();
        let request = two_units(&f);
        let engine = f.engine();
        match mode {
            0 => normal_plan(&f, &request).await,
            1 => {
                engine.start_build(&request.contract).unwrap();
            }
            _ => {
                engine.register_approved_plan(&request).unwrap();
                let review = Script::new(vec![
                    step(Role::ColdConsumer, Reply::say(PASS)),
                    step(Role::PlanCritic, Reply::say(PASS)),
                ]);
                engine.step_with(&review, None, None).await.unwrap();
            }
        }
        finish_candidate(&f, false).await;
        let store = Store::open(&f.repo).unwrap();
        let plan = store.read_plan().unwrap().unwrap();
        let before_head = git(&f.worktree(), &["rev-parse", "HEAD"]);
        engine
            .adjust_build(&hwahap::engine::AdjustBuildRequest {
                user_instruction: "Correct feature formatting under the same contract".into(),
                contract_digest: plan.digest().unwrap().to_string(),
                unit_ids: vec!["U1".into()],
            })
            .unwrap();
        assert!(engine
            .ship(&format!("SHIP {}", plan.digest().unwrap().challenge()))
            .is_err());
        finish_candidate(&f, true).await;
        assert_ne!(git(&f.worktree(), &["rev-parse", "HEAD"]), before_head);
        let events = store.read_events().unwrap();
        assert!(events.iter().any(|e| e.kind == "unit_attempt_started"
            && e.data["unit_id"] == "U2"
            && e.data["kind"] == "revalidation"));
        let hwahap::state::RunState::AwaitingAdjustOrShip { challenge, .. } =
            store.read_run().unwrap().unwrap().state
        else {
            panic!("missing reviewed candidate")
        };
        std::fs::write(f.worktree().join("feature.txt"), "tampered").unwrap();
        assert!(engine.ship(&format!("SHIP {challenge}")).is_err());
        git(&f.worktree(), &["restore", "feature.txt"]);
        assert_eq!(
            engine.ship(&format!("SHIP {challenge}")).unwrap().state,
            "shipped"
        );
    }
}
#[tokio::test]
async fn t04_entry_contract_rejections_precede_authorship() {
    let direct = Fixture::new();
    let mut invalid = two_units(&direct);
    invalid.contract.units[0].test_command.clear();
    assert!(direct.engine().start_build(&invalid.contract).is_err());
    assert!(!direct.worktree().exists());
    let imported = Fixture::new();
    let mut invalid = two_units(&imported);
    invalid.contract.units[0].paths.clear();
    assert!(imported.engine().register_approved_plan(&invalid).is_err());
    assert!(!imported.worktree().exists());
    let planned = Fixture::new();
    let request = two_units(&planned);
    normal_plan(&planned, &request).await;
    let store = Store::open(&planned.repo).unwrap();
    let mut changed = store.read_plan().unwrap().unwrap();
    changed.full_suite.clear();
    store.write_plan(&changed).unwrap();
    let empty = Script::new(vec![]);
    let rejected = planned
        .engine()
        .step_with(&empty, None, None)
        .await
        .unwrap();
    assert_eq!(rejected.state, "blocked", "{}", rejected.message);
    assert!(empty.calls().is_empty());
    assert!(!planned.worktree().join("feature.txt").exists());
}
