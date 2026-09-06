//! The MCP surface, asserted from outside the crate.
//!
//! These are the numbers the V3 design is pinned to. `tests/gates.sh` checks the same
//! things by grepping the source, which catches a change to how they are written; this file checks
//! what the server actually advertises, which catches a change to what they mean.

use hwahap::mcp::{Hwahap, INSTRUCTIONS};
use rmcp::ServerHandler;

#[test]
fn recheck_is_an_explicit_step_input() {
    let step = Hwahap::tool_router()
        .list_all()
        .into_iter()
        .find(|t| t.name == "hwahap_step")
        .unwrap();
    assert_eq!(
        step.input_schema["properties"]["recheck_pr"]["type"],
        "boolean"
    );
    assert!(INSTRUCTIONS.contains("recheck_pr:true alone"));
}

#[test]
fn the_server_advertises_exactly_three_tools() {
    let tools = Hwahap::tool_router().list_all();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, vec!["hwahap_ship", "hwahap_status", "hwahap_step"]);
}

#[test]
fn exactly_one_tool_is_read_only_and_it_is_the_one_that_reads() {
    let read_only: Vec<String> = Hwahap::tool_router()
        .list_all()
        .iter()
        .filter(|t| {
            t.annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or(false)
        })
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(read_only, vec!["hwahap_status".to_string()]);
}

#[test]
fn no_tool_that_would_hand_scheduling_back_to_the_host_exists() {
    // Each of these would ask the calling model to decide something the state machine decides.
    let names: Vec<String> = Hwahap::tool_router()
        .list_all()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    for forbidden in [
        "hwahap_plan",
        "hwahap_cycle",
        "hwahap_adjust",
        "hwahap_retry",
        "hwahap_create_unit",
        "hwahap_spawn_worker",
        "hwahap_integrate",
        "hwahap_abort",
    ] {
        assert!(
            !names.contains(&forbidden.to_string()),
            "{forbidden} exists"
        );
    }
}

#[test]
fn the_server_names_itself_rather_than_the_framework() {
    let info = Hwahap::new().get_info();
    assert_eq!(info.server_info.name, "hwahap");
    assert_ne!(info.server_info.name, "rmcp");
    assert_eq!(info.server_info.version, "4.0.0");
}

#[test]
fn the_instructions_are_advertised_and_teach_the_loop_in_their_first_paragraph() {
    let info = Hwahap::new().get_info();
    let instructions = info.instructions.expect("instructions must be advertised");
    assert_eq!(instructions, INSTRUCTIONS);

    let opening = instructions
        .split("\n\n")
        .next()
        .expect("at least one paragraph");
    for expected in [
        "hwahap_step",
        "continue",
        "await_user",
        "completed",
        "blocked",
        "CONFIRM PLAN",
        "SHIP",
        "verbatim",
    ] {
        assert!(
            opening.contains(expected),
            "the opening paragraph omits {expected:?}:\n{opening}"
        );
    }
}

#[test]
fn the_instructions_name_every_tool_and_forbid_inventing_a_confirmation() {
    for tool in Hwahap::tool_router().list_all() {
        assert!(
            INSTRUCTIONS.contains(tool.name.as_ref()),
            "the instructions never mention {}",
            tool.name
        );
    }
    assert!(INSTRUCTIONS.contains("Never compose, complete, or infer"));
    assert!(INSTRUCTIONS.contains("only the user may type one"));
}

#[test]
fn every_tool_carries_a_title_a_description_and_an_explicit_destructive_hint() {
    for tool in Hwahap::tool_router().list_all() {
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
        assert!(
            annotations.title.is_some(),
            "{} has no human title",
            tool.name
        );
        // Left unset, a host must assume the worst; Hwahap creates a branch and a draft pull
        // request and destroys nothing, so it says so.
        assert_eq!(
            annotations.destructive_hint,
            Some(false),
            "{} does not state whether it is destructive",
            tool.name
        );
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(
            description.len() > 40,
            "{} has a description too thin to choose by: {description:?}",
            tool.name
        );
    }
}

#[test]
fn the_step_tool_requires_only_the_repository_path() {
    let tool = Hwahap::step_tool_attr();
    let schema = serde_json::to_value(&tool.input_schema).expect("schema");
    let required: Vec<String> = schema["required"]
        .as_array()
        .expect("required")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    assert_eq!(
        required,
        vec!["cwd".to_string(), "host_session_id".to_string()]
    );

    let properties = schema["properties"].as_object().expect("properties");
    assert!(properties.contains_key("request"));
    assert!(properties.contains_key("user_input"));
    for key in [
        "plan_only",
        "build_confirmed",
        "adjust_build",
        "question_response",
    ] {
        assert!(properties.contains_key(key), "missing {key}");
    }
    assert!(
        Hwahap::step_tool_attr().output_schema.unwrap()["properties"]
            .get("question_batch")
            .is_some()
    );
}

#[test]
fn the_ship_tool_cannot_be_called_without_the_users_own_words() {
    let tool = Hwahap::ship_tool_attr();
    let schema = serde_json::to_value(&tool.input_schema).expect("schema");
    let required: Vec<String> = schema["required"]
        .as_array()
        .expect("required")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    assert!(
        required.contains(&"confirmation".to_string()),
        "{required:?}"
    );
    assert!(required.contains(&"cwd".to_string()), "{required:?}");
}

#[test]
fn the_status_tool_declares_a_structured_output_schema() {
    // The host renders progress from `structuredContent`; without an output schema it would have
    // only the text blob to parse.
    let tool = Hwahap::status_tool_attr();
    assert!(tool.output_schema.is_some());
    assert_eq!(tool.name, "hwahap_status");
}
