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
fn unsupported_plan_tool_allowlists_fail_closed_instead_of_discarding_authority() {
    let plan = filesystem_plan();
    validate_retained_input_execution(Some(&plan), 1).unwrap();
    for field in ["tool_allowlist", "toolAllowlist"] {
        for tools in [
            json!([]),
            json!(["file_read"]),
            json!(["file_write"]),
            json!(["*"]),
            json!(["file_*"]),
        ] {
            for location in ["", "/roles/0", "/roles/0/phases/0", "/synthesis"] {
                let mut supplied = serde_json::to_value(&plan).unwrap();
                supplied.pointer_mut(location).unwrap()[field] = tools.clone();
                let original = supplied.clone();
                let error = serde_json::from_value::<ExecutionPlan>(supplied.clone()).unwrap_err();
                assert!(error.to_string().contains("unknown field"), "{error}");
                assert_eq!(supplied, original);
            }
        }
    }
}

#[test]
fn retained_revision_inputs_require_explicit_read_authority_without_mutation() {
    let mut plan = filesystem_plan();
    plan.roles[0].phases[0].execution_mode = Some("native".into());
    plan.roles[0].phases[0].capabilities = vec!["network".into(), "filesystem-write".into()];
    plan.synthesis.execution_mode = Some("native".into());
    let mut researcher = plan.roles[0].clone();
    researcher.name = "benchmark-research".into();
    plan.roles.push(researcher);
    let original = plan.clone();
    validate_activation(Some(&plan), None, true).unwrap();
    validate_retained_input_execution(Some(&plan), 0).unwrap();
    let error = validate_retained_input_execution(Some(&plan), 1).unwrap_err();
    assert!(error.starts_with("This run cannot start because"));
    assert!(error.ends_with("[ReviewedInputCapabilityRequired]"));
    for text in [
        "ReviewedInputCapabilityRequired",
        "writer",
        "benchmark-research",
        "permission to read",
        "Edit",
        "previous work",
    ] {
        assert!(error.contains(text), "{error}");
    }
    assert_eq!(
        plan, original,
        "validation must not widen the reviewed plan"
    );
    for role in &mut plan.roles {
        role.phases[0].capabilities.push("filesystem-read".into());
    }
    let reviewed = plan.clone();
    validate_retained_input_execution(Some(&plan), 16).unwrap();
    assert_eq!(plan, reviewed);
    plan.roles[1].phases[0].max_tool_calls = 0;
    assert!(
        validate_retained_input_execution(Some(&plan), 1)
            .unwrap_err()
            .contains("benchmark-research")
    );
    plan = reviewed;
    plan.synthesis
        .capabilities
        .retain(|capability| capability != "filesystem-read");
    assert!(
        validate_retained_input_execution(Some(&plan), 1)
            .unwrap_err()
            .contains("principal synthesis")
    );
    assert!(validate_retained_input_execution(None, 1).is_err());
    assert!(validate_retained_input_execution(None, 0).is_ok());
}

#[test]
fn retained_input_allowance_includes_base_files_and_preserves_zero_denials() {
    let mut plan = filesystem_plan();
    // Scripted execution needs one read per input in addition to its handoff.
    assert!(validate_retained_input_execution(Some(&plan), 2).is_ok());
    assert!(validate_retained_input_execution(Some(&plan), 3).is_err());
    plan.roles[0].phases[0].execution_mode = Some("native".into());
    plan.synthesis.execution_mode = Some("native".into());
    plan.roles[0].phases[0].max_tool_calls = 1;
    assert!(validate_retained_input_execution(Some(&plan), 3).is_ok());
    plan.roles[0].phases[0].max_tool_calls = 0;
    assert!(validate_retained_input_execution(Some(&plan), 3).is_err());
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
fn reviewed_exact_public_fetch_activates_without_dropping_requirements() {
    let mut value = serde_json::to_value(filesystem_plan()).unwrap();
    value["roles"][0]["phases"][0]["capabilities"] =
        json!(["filesystem-read", "filesystem-write", "network"]);
    value["roles"][0]["phases"][0]["requiredToolCalls"] = json!([
        {"name":"http_fetch","arguments":{"method":"GET","url":"https://www.example.com/pricing?q=public%20plans"}}
    ]);
    let candidate: ExecutionPlan = serde_json::from_value(value.clone()).unwrap();
    let original = candidate.clone();
    assert!(validate_activation(Some(&candidate), None, true).is_ok());
    assert_eq!(candidate, original);
    for (path, replacement) in [
        ("/roles/0/phases/0/maxToolCalls", json!(1)),
        ("/roles/0/phases/0/requiredToolCalls", json!([])),
        (
            "/roles/0/phases/0/capabilities",
            json!(["filesystem-read", "filesystem-write"]),
        ),
        (
            "/roles/0/phases/0/requiredToolCalls/0/arguments/method",
            json!("POST"),
        ),
        (
            "/roles/0/phases/0/requiredToolCalls/0/name",
            json!("web_search"),
        ),
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(path).unwrap() = replacement;
        let changed = serde_json::from_value(changed).unwrap();
        assert!(
            validate_activation(Some(&changed), None, true).is_err(),
            "{path}"
        );
    }
    let mut duplicate = candidate.clone();
    duplicate.roles[0].phases[0]
        .required_tool_calls
        .push(candidate.roles[0].phases[0].required_tool_calls[0].clone());
    assert!(validate_activation(Some(&duplicate), None, true).is_err());
    let mut headers = candidate.clone();
    headers.roles[0].phases[0].required_tool_calls[0]
        .arguments
        .insert("headers".into(), "{}".into());
    assert!(validate_activation(Some(&headers), None, true).is_err());
}

#[test]
fn native_headroom_is_reviewed_not_clipped_to_legacy_bounds() {
    for maximum in [33, 128, i32::MAX] {
        let mut plan = filesystem_plan();
        plan.roles[0].phases[0].execution_mode = Some("native".into());
        plan.roles[0].phases[0].min_tool_calls = maximum;
        plan.roles[0].phases[0].max_tool_calls = maximum;
        plan.synthesis.execution_mode = Some("native".into());
        plan.synthesis.max_tool_calls = maximum;
        assert!(validate_activation(Some(&plan), None, true).is_ok());
        let approved = plan.clone();
        plan.roles[0].phases[0].execution_mode = None;
        assert!(validate_execution_plan(&plan).is_err());
        plan = approved.clone();
        plan.synthesis.execution_mode = None;
        assert!(validate_execution_plan(&plan).is_err());
        plan = approved;
        plan.roles[0].phases[0].max_tool_calls = -1;
        assert!(validate_execution_plan(&plan).is_err());
    }
}

#[test]
fn native_execution_separates_permissions_from_prescribed_tool_calls() {
    let mut native = filesystem_plan();
    let phase = &mut native.roles[0].phases[0];
    phase.execution_mode = Some("native".into());
    phase.capabilities = vec!["filesystem-write".into(), "network".into()];
    phase.min_tool_calls = 0;
    phase.max_tool_calls = 1;
    native.synthesis.execution_mode = Some("native".into());
    native.synthesis.max_tool_calls = 1;
    let original = native.clone();
    assert!(validate_activation(Some(&native), None, true).is_ok());
    assert_eq!(native, original);
    native.roles[0].phases[0].execution_mode = None;
    assert!(validate_activation(Some(&native), None, true).is_err());
    native = original.clone();
    native.roles[0].phases[0].fresh_context = false;
    assert!(
        validate_activation(Some(&native), None, true)
            .unwrap_err()
            .contains("cross-phase conversation reuse is not supported")
    );
    native = original.clone();
    native.synthesis.execution_mode = None;
    assert!(validate_activation(Some(&native), None, true).is_err());
    for capability in ["shell", "web-search", "mcp"] {
        native = original.clone();
        native.roles[0].phases[0]
            .capabilities
            .push(capability.into());
        assert!(
            validate_activation(Some(&native), None, true).is_err(),
            "{capability}"
        );
    }
    native = original.clone();
    native.roles[0].phases[0].execution_mode = Some("future".into());
    assert!(validate_execution_plan(&native).is_err());
    native = original;
    native.synthesis.execution_mode = Some("future".into());
    assert!(validate_execution_plan(&native).is_err());
}

#[test]
fn native_single_agent_uses_explicit_principal_permissions_without_child_roles() {
    let mut single = filesystem_plan();
    single.roles.clear();
    single.synthesis.execution_mode = Some("native".into());
    single.synthesis.capabilities = vec!["filesystem-write".into(), "network".into()];
    single.synthesis.max_tool_calls = 16;
    let original = single.clone();
    assert!(validate_activation(Some(&single), None, true).is_ok());
    assert_eq!(single, original);
    single.synthesis.execution_mode = None;
    assert!(validate_activation(Some(&single), None, true).is_err());
    single = original.clone();
    single.max_parallel = 2;
    assert!(validate_activation(Some(&single), None, true).is_err());
    single = original.clone();
    single.synthesis.max_tool_calls = 0;
    assert!(validate_activation(Some(&single), None, true).is_err());
    single = original.clone();
    single.synthesis.capabilities = vec!["network".into()];
    assert!(validate_activation(Some(&single), None, true).is_err());
    for capability in ["shell", "web-search", "mcp"] {
        single = original.clone();
        single.synthesis.capabilities.push(capability.into());
        assert!(validate_activation(Some(&single), None, true).is_err());
    }
    single = original;
    single.deliverables.clear();
    single.synthesis.capabilities.clear();
    assert!(validate_activation(Some(&single), None, true).is_ok());
}

#[test]
fn github_pr_requires_native_authority_and_never_decomposed_synthesis() {
    let mut plan = filesystem_plan();
    plan.roles[0].phases[0]
        .capabilities
        .push("github-pr".into());
    assert!(validate_activation(Some(&plan), None, true).is_err());
    plan.roles[0].phases[0].execution_mode = Some("native".into());
    assert!(validate_activation(Some(&plan), None, true).is_ok());
    plan.synthesis.capabilities.push("github-pr".into());
    plan.synthesis.execution_mode = Some("native".into());
    assert!(validate_activation(Some(&plan), None, true).is_err());
    plan.roles.clear();
    assert!(validate_activation(Some(&plan), None, true).is_ok());
    plan.synthesis.execution_mode = None;
    assert!(validate_activation(Some(&plan), None, true).is_err());
}

#[test]
fn python_execute_requires_explicit_native_file_and_network_authority() {
    let mut plan = filesystem_plan();
    plan.roles[0].phases[0].execution_mode = Some("native".into());
    plan.roles[0].phases[0].capabilities = [
        "filesystem-read",
        "filesystem-write",
        "network",
        "memory",
        "github-pr",
        "python-execute",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let original = plan.clone();
    assert!(validate_activation(Some(&plan), None, true).is_ok());
    assert_eq!(plan, original);
    for missing in ["filesystem-read", "filesystem-write", "network"] {
        let mut rejected = original.clone();
        rejected.roles[0].phases[0]
            .capabilities
            .retain(|c| c != missing);
        assert!(
            validate_activation(Some(&rejected), None, true).is_err(),
            "{missing}"
        );
    }
    plan.roles[0].phases[0].execution_mode = None;
    assert!(validate_activation(Some(&plan), None, true).is_err());
    plan = original.clone();
    plan.synthesis.execution_mode = Some("native".into());
    plan.synthesis.capabilities = original.roles[0].phases[0].capabilities.clone();
    assert!(validate_activation(Some(&plan), None, true).is_err());
    plan.roles.clear();
    assert!(validate_activation(Some(&plan), None, true).is_ok());
    for missing in ["filesystem-read", "filesystem-write", "network"] {
        let mut rejected = plan.clone();
        rejected.synthesis.capabilities.retain(|c| c != missing);
        assert!(
            validate_activation(Some(&rejected), None, true).is_err(),
            "{missing}"
        );
    }
    plan.synthesis.execution_mode = None;
    assert!(validate_activation(Some(&plan), None, true).is_err());
}

#[test]
fn memory_requires_native_authority_for_roles_and_synthesis() {
    let mut plan = filesystem_plan();
    plan.roles[0].phases[0].capabilities.push("memory".into());
    assert!(validate_activation(Some(&plan), None, true).is_err());
    plan.roles[0].phases[0].execution_mode = Some("native".into());
    assert!(validate_activation(Some(&plan), None, true).is_ok());
    plan.synthesis.capabilities.push("memory".into());
    assert!(validate_activation(Some(&plan), None, true).is_err());
    plan.synthesis.execution_mode = Some("native".into());
    assert!(validate_activation(Some(&plan), None, true).is_ok());
    plan.roles.clear();
    assert!(validate_activation(Some(&plan), None, true).is_ok());
    plan.synthesis.execution_mode = None;
    assert!(validate_activation(Some(&plan), None, true).is_err());
}

#[test]
fn native_execution_mode_requires_explicit_supported_nonnull_authority() {
    let legacy = serde_json::to_value(filesystem_plan()).unwrap();
    assert!(
        legacy["roles"][0]["phases"][0]
            .get("executionMode")
            .is_none()
    );
    assert!(legacy["synthesis"].get("executionMode").is_none());
    for target in ["/roles/0/phases/0", "/synthesis"] {
        for invalid in [json!(null), json!("future"), json!(true), json!(1)] {
            let mut changed = legacy.clone();
            changed.pointer_mut(target).unwrap()["executionMode"] = invalid;
            assert!(serde_json::from_value::<ExecutionPlan>(changed).is_err());
        }
        let mut changed = legacy.clone();
        changed.pointer_mut(target).unwrap()["executionMode"] = json!("native");
        let parsed: ExecutionPlan = serde_json::from_value(changed.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), changed);
    }
}

#[test]
fn native_execution_preserves_explicit_requirements_and_retained_input_permissions() {
    let mut native = filesystem_plan();
    native.roles[0].phases[0].execution_mode = Some("native".into());
    native.roles[0].phases[0]
        .capabilities
        .push("network".into());
    native.roles[0].phases[0].required_tool_calls = serde_json::from_value(json!([
        {"name":"http_fetch","arguments":{"method":"GET","url":"https://example.org/required"}}
    ]))
    .unwrap();
    assert!(validate_filesystem_execution(&native).is_ok());
    native.roles[0].phases[0]
        .capabilities
        .retain(|c| c != "network");
    assert!(validate_filesystem_execution(&native).is_err());
    native.roles[0].phases[0].required_tool_calls.clear();
    let mut reviewer = native.roles[0].clone();
    reviewer.name = "reviewer".into();
    reviewer.depends_on = vec!["writer".into()];
    reviewer.phases[0].capabilities = vec!["filesystem-write".into()];
    native.roles.push(reviewer);
    assert!(
        validate_filesystem_execution(&native)
            .unwrap_err()
            .contains("needs filesystem-read")
    );
    native.roles[1].phases[0]
        .capabilities
        .push("filesystem-read".into());
    assert!(validate_filesystem_execution(&native).is_ok());
}

#[test]
fn reviewed_fetch_urls_are_exact_public_https_without_normalization() {
    assert!(reviewed_fetch_url(
        "https://www.example.com/pricing?q=public%20plans"
    ));
    for url in [
        "http://example.com/",
        "https://example.com",
        "https://EXAMPLE.com/",
        "https://example.com:443/",
        "https://user@example.com/",
        "https://127.0.0.1/",
        "https://[::1]/",
        "https://x.local/",
        "https://x.internal/",
        "https://example.com/#part",
        "https://example.com/a/../b",
        "https://example.com/%2e/b",
        "https://example.com/%no",
        "https://example.com/white space",
        "https://example.com/é",
    ] {
        assert!(!reviewed_fetch_url(url), "{url}");
    }
}

#[test]
fn hosted_search_remains_outside_finite_funding() {
    let mut candidate = plan();
    candidate.roles[0].phases[0].required_tool_calls[0].name = "web_search".into();
    let original = candidate.clone();
    assert!(
        validate_activation(Some(&candidate), None, true)
            .unwrap_err()
            .contains("hosted web-search is outside the finite funding contract")
    );
    assert_eq!(candidate, original);
}

pub fn filesystem_plan() -> ExecutionPlan {
    let mut candidate = plan();
    let capabilities = vec!["filesystem-read".into(), "filesystem-write".into()];
    candidate.roles[0].phases[0].capabilities = capabilities.clone();
    candidate.roles[0].phases[0].required_tool_calls.clear();
    candidate.synthesis.capabilities = capabilities;
    candidate.synthesis.max_tool_calls = 4;
    candidate
}

#[test]
fn csv_deliverables_activate_unchanged_for_reviewed_and_native_execution() {
    for native in [false, true] {
        let mut candidate = filesystem_plan();
        candidate.deliverables[0] = ExecutionDeliverable {
            name: "comparison.csv".into(),
            media_type: Some("text/csv".into()),
        };
        if native {
            candidate.roles[0].phases[0].execution_mode = Some("native".into());
            candidate.synthesis.execution_mode = Some("native".into());
        }
        let original = candidate.clone();
        assert!(validate_activation(Some(&candidate), Some("execution-plan/v1"), true).is_ok());
        assert_eq!(candidate, original);
        candidate.deliverables[0].media_type = Some("application/pdf".into());
        assert!(
            validate_activation(Some(&candidate), None, true)
                .unwrap_err()
                .contains("media type application/pdf")
        );
    }
}

#[test]
fn installed_filesystem_subset_activates_without_narrowing_unsupported_plans() {
    let original = filesystem_plan();
    assert!(validate_activation(Some(&original), Some("execution-plan/v1"), true).is_ok());
    let original = serde_json::to_value(original).unwrap();
    for (path, value) in [
        ("/roles/0/phases/0/freshContext", json!(false)),
        ("/roles/0/phases/0/capabilities", json!(["filesystem-read"])),
        (
            "/roles/0/phases/0/capabilities",
            json!(["filesystem-read", "shell"]),
        ),
        ("/roles/0/phases/0/minToolCalls", json!(0)),
        (
            "/synthesis/capabilities",
            json!(["filesystem-read", "web-search"]),
        ),
        ("/synthesis/maxToolCalls", json!(0)),
        ("/deliverables/0/mediaType", json!("application/pdf")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(path).unwrap() = value;
        if path.ends_with("minToolCalls") {
            *changed
                .pointer_mut("/roles/0/phases/0/maxToolCalls")
                .unwrap() = json!(0);
        }
        let changed = serde_json::from_value(changed).unwrap();
        assert!(
            validate_activation(Some(&changed), None, false).is_ok(),
            "{path}"
        );
        assert!(
            validate_activation(Some(&changed), None, true)
                .unwrap_err()
                .contains("TypedPlanExecutionUnavailable"),
            "{path}"
        );
    }
}

#[test]
fn installed_execution_reports_exact_blockers_without_tool_arguments_or_plan_mutation() {
    let mut candidate = plan();
    candidate.roles[0].phases[0].fresh_context = false;
    candidate.roles[0].phases[0].required_tool_calls[0]
        .arguments
        .insert("owner".into(), "private-argument-value".into());
    candidate.deliverables[0].media_type = Some("application/pdf".into());
    let original = candidate.clone();
    let error = validate_filesystem_execution(&candidate).unwrap_err();
    for expected in [
        "role writer phase draft requires freshContext=true",
        "role writer phase draft requests capabilities [\"filesystem-write\", \"mcp\"]",
        "role writer phase draft requires external tool calls [\"github_actions_job_logs\"]",
        "synthesis requests capabilities []",
        "synthesis needs a positive maxToolCalls",
        "deliverable handbook.md requests media type application/pdf",
    ] {
        assert!(error.contains(expected), "{error}");
    }
    assert!(!error.contains("private-argument-value"));
    assert_eq!(candidate, original);
}

#[test]
fn installed_objective_bounds_match_trimmed_runtime_contracts() {
    let original = serde_json::to_value(filesystem_plan()).unwrap();
    for path in [
        "/roles/0/objective",
        "/roles/0/phases/0/objective",
        "/synthesis/objective",
    ] {
        for objective in [
            "                    ",
            "   Too short to run   ",
            "\u{feff}  Too short to run  \u{feff}",
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = json!(objective);
            let changed = serde_json::from_value(changed).unwrap();
            assert!(validate_activation(Some(&changed), None, false).is_ok());
            assert!(
                validate_activation(Some(&changed), None, true)
                    .unwrap_err()
                    .contains("TypedPlanExecutionUnavailable"),
                "{path}"
            );
        }
        let mut changed = original.clone();
        *changed.pointer_mut(path).unwrap() = json!(format!("  {}  ", "é".repeat(10)));
        let changed = serde_json::from_value(changed).unwrap();
        assert!(
            validate_activation(Some(&changed), None, true).is_ok(),
            "{path}"
        );
    }
}

#[test]
fn installed_role_budgets_require_exact_wire_integers() {
    let mut candidate = filesystem_plan();
    candidate.roles[0].budget_tokens = Some(9_007_199_254_740_991);
    assert!(validate_filesystem_execution(&candidate).is_ok());
    candidate.roles[0].budget_tokens = Some(9_007_199_254_740_992);
    assert!(validate_activation(Some(&candidate), None, false).is_ok());
    assert!(
        validate_filesystem_execution(&candidate)
            .unwrap_err()
            .contains("TypedPlanExecutionUnavailable")
    );
}

#[test]
fn installed_tool_allowances_cover_every_handoff_read_and_explicit_export() {
    let mut candidate = filesystem_plan();
    candidate.roles[0].phases[0].max_tool_calls = 1;
    candidate.synthesis.max_tool_calls = 2;
    assert!(validate_filesystem_execution(&candidate).is_ok());
    candidate.synthesis.max_tool_calls = 1;
    assert!(
        validate_filesystem_execution(&candidate)
            .unwrap_err()
            .contains("synthesis needs at least 2")
    );
    candidate.synthesis.max_tool_calls = 4;

    let mut reviewer = candidate.roles[0].clone();
    reviewer.name = "reviewer".into();
    reviewer.depends_on = vec!["writer".into()];
    reviewer.phases[0].name = "review".into();
    candidate.roles.push(reviewer);
    assert!(
        validate_filesystem_execution(&candidate)
            .unwrap_err()
            .contains("phase review needs at least 2")
    );
    candidate.roles[1].phases[0].max_tool_calls = 2;
    let mut later = candidate.roles[1].phases[0].clone();
    later.name = "refine".into();
    later.max_tool_calls = 1;
    candidate.roles[1].phases.push(later);
    assert!(
        validate_filesystem_execution(&candidate)
            .unwrap_err()
            .contains("phase refine needs at least 2")
    );
    candidate.roles[1].phases[1].max_tool_calls = 2;
    let mut deliverable = candidate.deliverables[0].clone();
    deliverable.name = "checklist.md".into();
    candidate.deliverables.push(deliverable);
    assert!(validate_filesystem_execution(&candidate).is_ok());
    candidate.synthesis.max_tool_calls = 3;
    assert!(
        validate_filesystem_execution(&candidate)
            .unwrap_err()
            .contains("synthesis needs at least 4")
    );
    // Drafts retain the proposed limits for review; validation never silently widens them.
    assert!(validate_execution_plan(&candidate).is_ok());
    assert_eq!(candidate.synthesis.max_tool_calls, 3);
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
