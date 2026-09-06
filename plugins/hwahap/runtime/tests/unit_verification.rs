use hwahap::{
    engine::{BuildRequest, BuildUnit},
    plan::Plan,
    validate::{approved_plan_blockers, build_blockers, freeze_blockers},
};

fn plan() -> Plan {
    BuildRequest {
        verification_inputs: vec![],
        user_instruction: "Build the specified contract".into(),
        objective: "Each unit proves its own acceptance".into(),
        base_branch: "main".into(),
        branch: "codex/coverage".into(),
        full_suite: "true".into(),
        units: (1..=2)
            .map(|n| BuildUnit {
                title: format!("Unit {n}"),
                acceptance: format!("Outcome {n}"),
                paths: vec![format!("file{n}")],
                test_command: "true".into(),
            })
            .collect(),
    }
    .plan("coverage", &"a".repeat(40))
    .unwrap()
}

#[test]
fn t01_t04_other_units_cannot_cover_a_missing_acceptance_at_any_gate() {
    let mut plan = plan();
    plan.units[0].acceptance_ids.push("A2".into());
    let before = serde_json::to_value(&plan).unwrap();
    for gate in [freeze_blockers, build_blockers, approved_plan_blockers] {
        let missing: Vec<_> = gate(&plan)
            .unwrap()
            .into_iter()
            .filter(|e| e.code == "uncovered_unit_acceptance")
            .map(|e| e.detail)
            .collect();
        assert_eq!(missing, ["U1 has no own test for acceptance A2"]);
    }
    assert_eq!(serde_json::to_value(&plan).unwrap(), before);
}

#[test]
fn t02_union_of_own_tests_and_shared_acceptance_are_valid() {
    let mut plan = plan();
    plan.units[0].acceptance_ids.push("A2".into());
    let mut own = plan.tests[1].clone();
    own.id = "T3".into();
    own.unit_id = "U1".into();
    plan.tests.push(own);
    assert!(build_blockers(&plan).unwrap().is_empty());
}

#[test]
fn t03_missing_tests_and_invalid_references_keep_deterministic_errors() {
    let mut plan = plan();
    plan.tests.remove(0);
    plan.tests[0].acceptance_ids.push("A1".into());
    plan.units[0].depends_on.push("U2".into());
    let errors = build_blockers(&plan).unwrap();
    for code in [
        "untested_unit",
        "test_outside_unit",
        "uncovered_unit_acceptance",
    ] {
        assert!(errors.iter().any(|e| e.code == code), "{errors:?}");
    }
    assert!(
        errors.iter().any(|e| e.code.contains("cycle")),
        "{errors:?}"
    );
    plan.units.reverse();
    plan.tests.reverse();
    assert_eq!(build_blockers(&plan).unwrap(), errors);
}

#[test]
fn probe_units_preserve_their_separate_verification_policy() {
    let mut plan = plan();
    plan.units[0].probe = true;
    plan.tests.remove(0);
    let errors = build_blockers(&plan).unwrap();
    assert!(!errors
        .iter()
        .any(|e| { matches!(e.code, "uncovered_unit_acceptance" | "untested_unit") }));
}
