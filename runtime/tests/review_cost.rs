#![cfg(unix)]
mod common;
use common::{git, security_review, step, Fixture, Reply, Script};
use hwahap::engine::{BuildRequest, BuildUnit};
use hwahap::pr_review::ReviewProgress;
use hwahap::profile::Role;
use hwahap::state::Store;

async fn draft() -> Fixture {
    let f = Fixture::new();
    git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let engine = f.engine();
    engine
        .start_build(&BuildRequest {
            user_instruction: "Implement without planning".into(),
            objective: "Create a checked feature".into(),
            base_branch: "main".into(),
            branch: "codex/security-check".into(),
            full_suite: "test -f feature.txt".into(),
            units: vec![BuildUnit {
                title: "Feature".into(),
                acceptance: "feature exists".into(),
                paths: vec!["feature.txt".into()],
                test_command: "test -f feature.txt".into(),
            }],
        })
        .unwrap();
    let script = Script::new(vec![
        step(
            Role::Implementer,
            Reply::write(
                &[("feature.txt", "ready\n")],
                r#"{"status":"completed","summary":"Created feature","conflict":null}"#,
            ),
        ),
        step(
            Role::UnitReviewer,
            Reply::say(r#"{"verdict":"pass","findings":[]}"#),
        ),
    ]);
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "final_verifying"
    );
    assert_eq!(
        engine.step_with(&script, None, None).await.unwrap().state,
        "pr_review"
    );
    f
}

#[tokio::test]
async fn completed_native_reviews_refresh_the_final_published_cost_report() {
    use hwahap::native::{NativeCompletion, NativeRegistration, NativeSessions};
    use hwahap::profile::Profiles;
    use hwahap::session::TokenUsage;
    use std::sync::Arc;
    let f = draft().await;
    let store = Store::open(&f.repo).unwrap();
    let report_path = store.root().join("report.md");
    let before = std::fs::read_to_string(&report_path).unwrap();
    let published_cost: serde_json::Value = serde_json::from_str(
        before
            .split("```json\n")
            .nth(1)
            .unwrap()
            .split("\n```")
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(published_cost["total"]["requests"], 0);
    let binding = ReviewProgress::load(&store).unwrap().unwrap().binding;
    let sessions = Arc::new(NativeSessions::new(
        store.clone(),
        Profiles::defaults(),
        64,
        20,
    ));
    let engine = f.engine();
    let runner = sessions.clone();
    let task = tokio::spawn(async move { engine.step_with(&*runner, None, None).await });
    let mut previous = String::new();
    for (n, role) in ["unit_reviewer", "final_review"].into_iter().enumerate() {
        let dispatch = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(d) = sessions.dispatch().unwrap() {
                    if d.dispatch_id != previous {
                        break d;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(dispatch.role, role);
        let agent_id = format!("independent-reviewer-{n}");
        sessions
            .register(&NativeRegistration {
                dispatch_id: dispatch.dispatch_id.clone(),
                agent_id: agent_id.clone(),
            })
            .unwrap();
        let report = if n == 0 {
            serde_json::json!({"binding":binding,"findings":[],"security":security_review(),"evidence":["checked actual published bytes"]})
        } else {
            serde_json::json!({"binding":binding,"assessments":[],"additional_findings":[],"security":security_review(),"evidence":["independently checked published bytes"]})
        };
        sessions
            .complete(NativeCompletion {
                dispatch_id: dispatch.dispatch_id.clone(),
                agent_id,
                final_message:
                    serde_json::json!({"dispatch_id":dispatch.dispatch_id,"result":report})
                        .to_string(),
                agent_stopped: true,
                reported_usage: Some(TokenUsage {
                    input_tokens: 100,
                    output_tokens: 20,
                    cached_input_tokens: 30,
                }),
            })
            .unwrap();
        previous = dispatch.dispatch_id;
    }
    let done = task.await.unwrap().unwrap();
    assert_eq!(done.state, "awaiting_adjust_or_ship");
    sessions.finish().unwrap();
    let live = hwahap::cost::summary(&store).unwrap();
    assert_eq!(live["total"]["requests"], 2);
    assert_eq!(live["total"]["completions"], 2);
    assert_eq!(live["total"]["reported_input_tokens"], 200);
    assert_eq!(live["total"]["reported_output_tokens"], 40);
    let after = std::fs::read_to_string(&report_path).unwrap();
    assert_ne!(after, before);
    let published: serde_json::Value = serde_json::from_str(
        after
            .split("```json\n")
            .nth(1)
            .unwrap()
            .split("\n```")
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(published["total"]["requests"], 2);
    assert_eq!(published["total"]["reported_input_tokens"], 200);
    assert_eq!(published["total"]["reported_output_tokens"], 40);
    assert!(
        f.dir.path().join("pr-edited").exists(),
        "PR metadata was refreshed after reviews"
    );
    let ship = format!(
        "SHIP {}",
        store
            .read_plan()
            .unwrap()
            .unwrap()
            .digest()
            .unwrap()
            .challenge()
    );
    assert_eq!(f.engine().ship(&ship).unwrap().state, "shipped");
    let after = std::fs::read_to_string(&report_path).unwrap();
    assert_ne!(after, before);
    let published: serde_json::Value = serde_json::from_str(
        after
            .split("```json\n")
            .nth(1)
            .unwrap()
            .split("\n```")
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(published["total"]["requests"], 2);
    assert_eq!(published["total"]["reported_input_tokens"], 200);
    assert_eq!(published["total"]["reported_output_tokens"], 40);
    assert!(f.dir.path().join("pr-edited").exists());
}
