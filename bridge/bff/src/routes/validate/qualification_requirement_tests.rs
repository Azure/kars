// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — qualification requirement regression tests.

use super::{blueprint_to_dto, qualification_requirements};

#[test]
fn stored_halt_blocks_preflight_without_changing_the_decision_or_task() {
    use super::{CheckStatus, ValidateResponse, check_stored_activation};
    use kube::ResourceExt;
    let mut task: crate::kars::task::KarsTask = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "mission", "namespace": "kars-system"},
        "spec": {"objective": "Retained work", "execution": {"launch": false},
            "envelope": {"tier": 1, "authorityCeiling": 1}}
    }))
    .unwrap();
    let mut ordinary = ValidateResponse {
        ok: true,
        checks: vec![],
    };
    check_stored_activation(&task, &mut ordinary);
    assert!(ordinary.ok);
    for decision in ["halted by operator", ""] {
        task.annotations_mut()
            .insert("kars.azure.com/halted".into(), decision.into());
        let before = serde_json::to_value(&task).unwrap();
        let mut result = ValidateResponse {
            ok: true,
            checks: vec![],
        };
        check_stored_activation(&task, &mut result);
        assert!(!result.ok);
        let check = result
            .checks
            .iter()
            .find(|check| check.id == "governed_halt")
            .unwrap();
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.detail.contains("Create a new Mission"));
        assert_eq!(serde_json::to_value(&task).unwrap(), before);
    }
}

#[test]
fn stored_activation_preserves_prior_failures_and_plan_failure_deduplication() {
    use super::{Check, CheckStatus, ValidateResponse, check_stored_activation};
    use kube::ResourceExt;
    let mut task: crate::kars::task::KarsTask = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "mission", "namespace": "kars-system"},
        "spec": {"objective": "Retained work", "execution": {"launch": false},
            "envelope": {"tier": 1, "authorityCeiling": 1}}
    }))
    .unwrap();
    let failure = |id: &str| Check {
        id: id.into(),
        label: "Existing failure".into(),
        status: CheckStatus::Fail,
        detail: "Retain this failure".into(),
    };
    let mut existing = ValidateResponse {
        ok: false,
        checks: vec![failure("model")],
    };
    check_stored_activation(&task, &mut existing);
    assert!(!existing.ok);
    assert_eq!(existing.checks.len(), 1);
    assert_eq!(existing.checks[0].detail, "Retain this failure");

    task.annotations_mut().insert(
        "kars.azure.com/mission-decomposition".into(),
        "execution-plan/v1".into(),
    );
    assert!(crate::kars::execution_plans::validate(&task, true).is_err());
    let mut activation = ValidateResponse {
        ok: true,
        checks: vec![],
    };
    check_stored_activation(&task, &mut activation);
    assert!(!activation.ok);
    assert_eq!(activation.checks.len(), 1);
    assert_eq!(activation.checks[0].id, "execution_activation");
    assert_eq!(activation.checks[0].status, CheckStatus::Fail);

    let mut duplicate = ValidateResponse {
        ok: false,
        checks: vec![failure("execution_plan")],
    };
    check_stored_activation(&task, &mut duplicate);
    assert!(!duplicate.ok);
    assert_eq!(duplicate.checks.len(), 1);
    assert_eq!(duplicate.checks[0].detail, "Retain this failure");
}

#[test]
fn persisted_draft_plan_preserves_create_preflight_requirements_and_all_plan_fields() {
    use crate::kars::task::TaskBlueprint;
    use crate::routes::tasks::{BlueprintDto, ExecutionPlanDto};
    let plan: ExecutionPlanDto = serde_json::from_value(serde_json::json!({
        "schema": "kars.execution-plan/v1",
        "roles": [
            {
                "name": "evidence",
                "objective": "Read public evidence.",
                "depends_on": [],
                "phases": [{
                    "name": "inspect",
                    "objective": "Read current check logs.",
                    "capabilities": ["network", "mcp"],
                    "required_tool_calls": [{
                        "name": "github_actions_job_logs",
                        "arguments": {"owner": "owner", "repo": "repo", "job_id": "42"}
                    }],
                    "min_tool_calls": 1,
                    "max_tool_calls": 4,
                    "fresh_context": true
                }],
                "budget_tokens": 1500
            },
            {
                "name": "review",
                "objective": "Review the evidence.",
                "depends_on": ["evidence"],
                "phases": [{
                    "name": "assess",
                    "objective": "Assess the handback.",
                    "capabilities": [],
                    "required_tool_calls": [],
                    "min_tool_calls": 0,
                    "max_tool_calls": 0,
                    "fresh_context": false
                }],
                "budget_tokens": 500
            }
        ],
        "max_parallel": 2,
        "synthesis": {
            "objective": "Produce the review.",
            "capabilities": ["network"],
            "max_tool_calls": 1
        },
        "deliverables": [
            {"name": "review.md", "media_type": "text/markdown"},
            {"name": "evidence.json", "media_type": null}
        ]
    }))
    .unwrap();
    let create = BlueprintDto {
        execution_plan: Some(plan.clone()),
        ..Default::default()
    };
    let stored = serde_json::to_value(TaskBlueprint {
        execution_plan: Some(plan.into_crd()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(stored["executionPlan"]["maxParallel"], 2);
    let persisted: TaskBlueprint = serde_json::from_value(stored).unwrap();
    let launch = blueprint_to_dto(&persisted);
    assert_eq!(
        serde_json::to_value(&launch.execution_plan).unwrap(),
        serde_json::to_value(&create.execution_plan).unwrap()
    );
    let required = qualification_requirements(&launch, Some("mission"));
    assert_eq!(
        required,
        qualification_requirements(&create, Some("mission"))
    );
    assert_eq!(required.1, 2);
    for capability in ["artifacts", "delegation", "mcp", "network", "telemetry"] {
        assert!(required.0.contains(capability));
    }
    assert!(!required.0.contains("single-agent"));
}

#[test]
fn legacy_draft_without_a_plan_still_requires_single_agent_qualification() {
    let launch = blueprint_to_dto(&crate::kars::task::TaskBlueprint::default());
    assert!(launch.execution_plan.is_none());
    let (required, parallel) = qualification_requirements(&launch, Some("mission"));
    assert_eq!(parallel, 1);
    assert!(required.contains("single-agent"));
    assert!(!required.contains("delegation"));
}

#[test]
fn team_preflight_requires_retained_team_evidence() {
    let blueprint = crate::routes::tasks::BlueprintDto::default();
    let (mission, _) = qualification_requirements(&blueprint, Some("mission"));
    let (team, _) = qualification_requirements(&blueprint, Some("team"));

    assert!(!mission.contains("team"));
    assert!(team.contains("team"));
}

#[test]
fn execution_plan_capabilities_flow_into_atomic_qualification_requirements() {
    let mut blueprint = crate::routes::tasks::BlueprintDto {
        execution_plan: Some(crate::routes::tasks::ExecutionPlanDto {
            schema: "kars.execution-plan/v1".into(),
            roles: vec![crate::routes::tasks::ExecutionRoleDto {
                name: "source-scout".into(),
                objective: "Discover exact URLs and fetch the source evidence.".into(),
                depends_on: Vec::new(),
                phases: vec![crate::routes::tasks::ExecutionPhaseDto {
                    name: "discover".into(),
                    objective: "Search and fetch authoritative sources.".into(),
                    capabilities: vec!["web-search".into(), "network".into()],
                    required_tool_calls: Vec::new(),
                    min_tool_calls: 1,
                    max_tool_calls: 4,
                    fresh_context: true,
                    execution_mode: None,
                }],
                budget_tokens: None,
            }],
            max_parallel: 1,
            synthesis: crate::routes::tasks::ExecutionSynthesisDto {
                objective: "Return the verified answer.".into(),
                capabilities: Vec::new(),
                max_tool_calls: 0,
                execution_mode: None,
            },
            deliverables: Vec::new(),
        }),
        ..Default::default()
    };

    let (required, max_parallel) = qualification_requirements(&blueprint, Some("mission"));
    assert_eq!(max_parallel, 1);
    assert!(required.contains("delegation"));
    assert!(required.contains("web-search"));
    assert!(required.contains("network"));

    let plan = blueprint.execution_plan.as_mut().unwrap();
    plan.roles.clear();
    plan.synthesis.execution_mode = Some("native".into());
    plan.synthesis.capabilities = vec!["network".into()];
    plan.synthesis.max_tool_calls = 8;
    let (required, max_parallel) = qualification_requirements(&blueprint, Some("mission"));
    assert_eq!(max_parallel, 1);
    assert!(required.contains("single-agent"));
    assert!(required.contains("network"));
    assert!(!required.contains("delegation"));
    assert!(!required.contains("web-search"));
}
