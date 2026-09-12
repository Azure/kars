// kars Bridge BFF — qualification requirement regression tests.

use super::{blueprint_to_dto, qualification_requirements};

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
    let blueprint = crate::routes::tasks::BlueprintDto {
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
                }],
                budget_tokens: None,
            }],
            max_parallel: 1,
            synthesis: crate::routes::tasks::ExecutionSynthesisDto {
                objective: "Return the verified answer.".into(),
                capabilities: Vec::new(),
                max_tool_calls: 0,
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
}
