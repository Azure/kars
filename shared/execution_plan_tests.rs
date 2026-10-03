// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::json;

pub fn plan() -> ExecutionPlan {
    serde_json::from_value(json!({
        "schema":"kars.execution-plan/v1", "maxParallel":1,
        "roles":[{"name":"writer","objective":"Write a useful reviewed handbook.",
            "dependsOn":[],"budgetTokens":500000,
            "phases":[{"name":"draft","objective":"Write a useful Markdown handbook.",
                "capabilities":["filesystem-write","mcp"],"minToolCalls":1,"maxToolCalls":3,
                "freshContext":true,"requiredToolCalls":[{"name":"github_actions_job_logs",
                    "arguments":{"owner":"example","repo":"handbook","job_id":"42","tail_lines":"20"}}]}]}],
        "synthesis":{"objective":"Return the reviewed final handbook.","capabilities":[],"maxToolCalls":0},
        "deliverables":[{"name":"handbook.md","mediaType":"text/markdown"}]
    })).unwrap()
}

#[test]
fn drafts_validate_but_current_executor_cannot_activate_them() {
    let plan = plan();
    assert!(validate_activation(Some(&plan), Some("execution-plan/v1"), false).is_ok());
    assert!(
        validate_activation(Some(&plan), None, true)
            .unwrap_err()
            .contains("TypedPlanExecutionUnavailable")
    );
    for active in [false, true] {
        assert!(validate_activation(None, None, active).is_ok());
        assert!(
            validate_activation(None, Some("execution-plan/v1"), active)
                .unwrap_err()
                .contains("ReviewedExecutionPlanMissing")
        );
        assert!(validate_activation(Some(&plan), Some("future/v2"), active).is_err());
    }
}

#[test]
fn bounded_plan_rejects_cycles_invalid_limits_and_unbounded_text() {
    let original = serde_json::to_value(plan()).unwrap();
    for (path, value) in [
        ("/schema", json!("unknown")),
        ("/roles", json!([])),
        ("/maxParallel", json!(2)),
        ("/roles/0/dependsOn", json!(["writer"])),
        ("/roles/0/budgetTokens", json!(0)),
        ("/roles/0/phases/0/minToolCalls", json!(4)),
        ("/roles/0/phases/0/maxToolCalls", json!(33)),
        ("/roles/0/phases/0/capabilities", json!(["mcp", "mcp"])),
        (
            "/roles/0/phases/0/requiredToolCalls/0/arguments/owner",
            json!("x".repeat(4097)),
        ),
        (
            "/roles/0/phases/0/requiredToolCalls/0/arguments/tail_lines",
            json!("2001"),
        ),
        ("/deliverables/0/mediaType", json!("x".repeat(129))),
        ("/deliverables/0/mediaType", json!("text/markdown\n")),
        ("/deliverables/0/name", json!("../handbook.md")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(path).unwrap() = value;
        let changed = serde_json::from_value(changed).unwrap();
        assert!(validate_execution_plan(&changed).is_err(), "{path}");
    }
}

#[test]
fn deliverable_names_match_bounded_runtime_attachments() {
    for name in ["a".into(), "Handbook_v2.final-md".into(), "a".repeat(128)] {
        let mut candidate = plan();
        candidate.deliverables[0].name = name;
        assert!(validate_execution_plan(&candidate).is_ok());
    }
    for name in [
        "",
        "-handbook",
        "_handbook",
        ".handbook",
        "response.md",
        "constructor",
        "prototype",
        "__proto__",
        "../handbook",
        "dir/handbook",
        "dir\\handbook",
        "é.md",
    ]
    .into_iter()
    .map(String::from)
    .chain(["a".repeat(129)])
    {
        let mut candidate = plan();
        candidate.deliverables[0].name = name.clone();
        assert!(validate_execution_plan(&candidate).is_err(), "{name}");
    }
}

#[test]
fn required_call_arguments_enforce_exact_size_boundaries() {
    let mut bounded = plan();
    let arguments = &mut bounded.roles[0].phases[0].required_tool_calls[0].arguments;
    arguments.insert("k".repeat(128), "v".repeat(4096));
    for key in ["extra-a", "extra-b", "extra-c"] {
        arguments.insert(key.into(), "value".into());
    }
    assert_eq!(arguments.len(), 8);
    assert!(validate_execution_plan(&bounded).is_ok());
    for (key, value) in [
        ("ninth".into(), "value".into()),
        ("".into(), "value".into()),
        ("k".repeat(129), "value".into()),
        ("owner".into(), "v".repeat(4097)),
        ("owner".into(), " ".into()),
        ("tail_lines".into(), "0".into()),
    ] {
        let mut candidate = if key == "ninth" {
            bounded.clone()
        } else {
            plan()
        };
        candidate.roles[0].phases[0].required_tool_calls[0]
            .arguments
            .insert(key.clone(), value);
        assert!(validate_execution_plan(&candidate).is_err(), "{key}");
    }
}

#[test]
fn dependencies_accept_dags_but_reject_unknown_duplicate_and_cyclic_edges() {
    let mut original = plan();
    for name in ["reviewer", "editor"] {
        let mut role = original.roles[0].clone();
        role.name = name.into();
        role.depends_on = vec!["writer".into()];
        original.roles.push(role);
    }
    original.roles[2].depends_on.push("reviewer".into());
    original.max_parallel = 3;
    assert!(validate_execution_plan(&original).is_ok());
    for (index, dependencies) in [
        (1, vec!["unknown"]),
        (1, vec!["writer", "writer"]),
        (0, vec!["editor"]),
        (0, vec!["writer"]),
    ] {
        let mut changed = original.clone();
        changed.roles[index].depends_on = dependencies.into_iter().map(String::from).collect();
        assert!(validate_execution_plan(&changed).is_err());
    }
}

#[test]
fn omitted_defaults_roundtrip_without_creating_authority() {
    let value = json!({
        "schema":"kars.execution-plan/v1", "maxParallel":1,
        "roles":[{"name":"writer","objective":"Write a useful reviewed handbook.",
            "phases":[{"name":"draft","objective":"Draft a useful reviewed handbook.","maxToolCalls":0}]}],
        "synthesis":{"objective":"Return the reviewed final handbook.","maxToolCalls":0}
    });
    let original: ExecutionPlan = serde_json::from_value(value).unwrap();
    assert!(validate_execution_plan(&original).is_ok());
    assert!(original.roles[0].budget_tokens.is_none());
    assert!(original.roles[0].depends_on.is_empty());
    assert!(original.deliverables.is_empty());
    assert_eq!(original.roles[0].phases[0].min_tool_calls, 0);
    assert!(!original.roles[0].phases[0].fresh_context);
    assert!(original.roles[0].phases[0].required_tool_calls.is_empty());
    assert_eq!(
        serde_json::from_value::<ExecutionPlan>(serde_json::to_value(&original).unwrap()).unwrap(),
        original
    );
}

#[test]
fn unknown_plan_fields_are_not_silently_discarded() {
    let mut value = serde_json::to_value(plan()).unwrap();
    value["futureAuthority"] = json!(true);
    assert!(serde_json::from_value::<ExecutionPlan>(value).is_err());
}
