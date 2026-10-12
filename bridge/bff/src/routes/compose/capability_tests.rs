// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::egress::complete_egress_recommendation;
use super::execution::{
    apply_weighted_role_budget_floors, execution_plan_error_from_raw, parse_execution_plan,
    weighted_role_budget_floors,
};
use super::prompts::{build_system_prompt, build_team_system_prompt};
use super::routing::{
    catalogue_has_model_key, efficient_member_route_is_qualified, orchestrator_quality_score,
    recommendation_is_actionable, should_strengthen_team_principal,
};
use super::team::{TEAM_COMPOSE_MAX_TOKENS, team_proposal_is_complete};
use super::team_proposal::{is_synthesis_only_team_role, parse_and_validate_team};
use super::{
    ComposeEgress, ComposeTeamProposal, delegation_budget_allocation, is_autonomous_harness,
    is_non_autonomous_harness, validate_execution_plan,
};
use crate::routes::efficiency::EfficiencyDto;
use crate::routes::options::{IsolationOption, ModelOption, Options, RefOption, RuntimeOption};

fn test_options() -> Options {
    Options {
        models: vec![ModelOption {
            provider: "github-copilot".into(),
            deployment: "gpt-5.6-sol".into(),
            is_default: true,
            detail: None,
        }],
        default_model: Some("gpt-5.6-sol".into()),
        provider: None,
        runtimes: vec![RuntimeOption {
            kind: "OpenClaw".into(),
            label: "OpenClaw".into(),
            wired: true,
            status: "validated".into(),
            note: "ready".into(),
        }],
        isolation: vec![IsolationOption {
            value: "standard".into(),
            label: "Standard".into(),
            note: "sandboxed".into(),
        }],
        tool_policies: Vec::new(),
        mcp_servers: Vec::new(),
        mcp_profiles: Vec::new(),
        memories: Vec::new(),
        skills: Vec::new(),
    }
}

fn test_efficiency() -> EfficiencyDto {
    EfficiencyDto {
        routes: Vec::new(),
        recommended: None,
        recommended_harness: None,
        recommended_basis: None,
        recommended_low_confidence: false,
        total_runs: 0,
        priced: false,
    }
}

#[test]
fn autonomous_set_is_openclaw_hermes_byo() {
    for k in ["OpenClaw", "openclaw", "Hermes", "hermes", "BYO", "byo"] {
        assert!(is_autonomous_harness(k), "{k} should be autonomous");
        assert!(
            !is_non_autonomous_harness(k),
            "{k} should not be non-autonomous"
        );
    }
}

#[test]
fn synthesis_only_role_is_reserved_for_the_principal() {
    assert!(is_synthesis_only_team_role(
        "readiness-editor",
        "Reconcile specialist handbacks into one truthful readiness report."
    ));
    assert!(!is_synthesis_only_team_role(
        "ci-health-auditor",
        "Verify exact-head CI checks and return independent evidence."
    ));
}

#[test]
fn empty_team_proposal_is_not_reported_as_available() {
    let proposal = ComposeTeamProposal {
        tier: 3,
        cadence_minutes: 0,
        instructions: String::new(),
        model: String::new(),
        model_fallbacks: Vec::new(),
        model_basis: None,
        expected_tokens_per_outcome: None,
        efficiency_sample_runs: 0,
        mcp_servers: Vec::new(),
        memory: None,
        egress: Vec::new(),
        egress_mode: "learning".into(),
        engineering_enabled: false,
        engineering_signals: Vec::new(),
        engineering_poll_interval_seconds: 900,
        engineering_auto_run: false,
        roles: Vec::new(),
        execution_plan: None,
        milestones: Vec::new(),
    };
    assert!(!team_proposal_is_complete(&proposal));
}

fn team_role_binding_fixture() -> (Options, serde_json::Value) {
    let mut options = test_options();
    options.models.push(ModelOption {
        provider: "azure-foundry".into(),
        deployment: "gpt-5.4-mini".into(),
        is_default: false,
        detail: None,
    });
    options.runtimes.push(RuntimeOption {
        kind: "Hermes".into(),
        label: "Hermes".into(),
        wired: true,
        status: "validated".into(),
        note: "ready".into(),
    });
    options.skills = ["source-reading", "decision-writing"]
        .into_iter()
        .map(|name| RefOption {
            name: name.into(),
            namespace: "kars-system".into(),
            summary: None,
            mode: None,
            discovered_tools: Vec::new(),
            tool_schema_digest: None,
            compiled_digest: None,
            backend: None,
            readiness: None,
            version: None,
            recipe: None,
            version_digest: None,
            qualified_routes: Vec::new(),
        })
        .collect();
    let value = serde_json::json!({
        "instructions": "Coordinate source reading and decision writing for the reviewer.",
        "model": "github-copilot::gpt-5.6-sol",
        "roles": [
            {
                "name": "source-reader",
                "system_prompt": "Read supplied sources and preserve precise supporting quotations.",
                "runtime": "Hermes",
                "model": "azure-foundry::gpt-5.4-mini",
                "skills": ["source-reading"]
            },
            {
                "name": "decision-writer",
                "system_prompt": "Write a decision document with cited evidence and explicit uncertainty.",
                "runtime": "OpenClaw",
                "model": "github-copilot::gpt-5.6-sol",
                "skills": ["decision-writing"]
            }
        ],
        "execution_plan": {
            "schema": "kars.execution-plan/v1",
            "roles": [
                {
                    "name": "source-reader",
                    "objective": "Read the supplied source material and retain exact evidence.",
                    "phases": [{
                        "name": "collect",
                        "objective": "Collect the required source evidence without synthesis.",
                        "capabilities": ["filesystem-read"],
                        "max_tool_calls": 4
                    }]
                },
                {
                    "name": "decision-writer",
                    "objective": "Produce the requested decision from the retained source evidence.",
                    "depends_on": ["source-reader"],
                    "phases": [{
                        "name": "draft",
                        "objective": "Draft the decision using only retained dependency evidence.",
                        "capabilities": ["filesystem-write"],
                        "max_tool_calls": 2
                    }]
                }
            ],
            "max_parallel": 1,
            "synthesis": {
                "objective": "Reconcile the role handbacks into the final answer.",
                "capabilities": [],
                "max_tool_calls": 0
            },
            "deliverables": [{"name": "decision.md", "media_type": "text/markdown"}]
        }
    });
    (options, value)
}

#[test]
fn team_composer_rejects_a_principal_reintroduced_by_the_execution_plan() {
    let (options, mut raw) = team_role_binding_fixture();
    let mut principal = raw["execution_plan"]["roles"][1].clone();
    principal["name"] = serde_json::json!("principal");
    raw["execution_plan"]["roles"]
        .as_array_mut()
        .unwrap()
        .push(principal);
    let (mut proposal, _) =
        parse_and_validate_team(&raw.to_string(), &options, &test_efficiency(), "");
    assert!(!team_proposal_is_complete(&proposal));
    let error = super::team_qualification::normalize_team_proposal_route(&mut proposal, &options)
        .unwrap_err();
    assert!(
        error.contains("principal is reserved for execution_plan.synthesis"),
        "{error}"
    );
    assert_eq!(proposal.execution_plan.as_ref().unwrap().roles.len(), 3);
}

#[test]
fn composer_schema_does_not_teach_empty_synthesis_or_drop_requested_team_outputs() {
    let options = test_options();
    let efficiency = test_efficiency();
    for prompt in [
        build_system_prompt(&options, &efficiency, "", ""),
        build_team_system_prompt(&options, &efficiency, "", ""),
    ] {
        assert!(!prompt.contains("\"max_tool_calls\": 0"));
        assert!(!prompt.contains("\"capabilities\": []"));
        assert!(prompt.contains("covering all handoff reads and deliverable exports"));
    }
    let team = build_team_system_prompt(&options, &efficiency, "", "");
    assert!(team.contains("every requested output file by its exact name"));
    assert!(team.contains("\"name\":\"<exact requested safe filename>\""));
    assert!(team.contains("NOT a roster or execution_plan.roles entry"));
}

#[test]
fn team_role_budget_reaches_proposal_diagnostics_and_fallbacks() {
    const CHILD: &str = "KARS_TEST_COMPOSE_ROLE_BUDGET";
    if std::env::var_os(CHILD).is_none() {
        let records = ["OpenClaw", "Hermes"].into_iter().flat_map(|runtime| {
            [("github-copilot", "gpt-5.6-sol", 2000), ("azure-foundry", "gpt-5.4-mini", 3000)]
                .into_iter().map(move |(provider, deployment, minimum)| serde_json::json!({
                    "runtime": runtime, "provider": provider, "deployment": deployment,
                    "capabilities": ["single-agent", "team", "telemetry", "delegation", "artifacts", "filesystem-read", "filesystem-write"],
                    "max_parallel": 1, "min_total_tokens": minimum,
                    "evidence": {"task": "test-fixture", "run_id": "test-run", "digest": "sha256:test-fixture"}
                }))
        }).collect::<Vec<_>>();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "routes::compose::capability_tests::team_role_budget_reaches_proposal_diagnostics_and_fallbacks",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("BRIDGE_ROUTE_QUALIFICATION_MODE", "required")
            .env("BRIDGE_QUALIFICATION_RECORDS_JSON", serde_json::to_string(&records).unwrap())
            .env_remove("BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (options, mut raw) = team_role_binding_fixture();
    for role in raw["roles"].as_array_mut().unwrap() {
        role["skills"] = serde_json::json!([]);
    }
    for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
        for phase in role["phases"].as_array_mut().unwrap() {
            phase["capabilities"] = serde_json::json!(["filesystem-read", "filesystem-write"]);
            phase["fresh_context"] = serde_json::json!(true);
        }
    }
    raw["execution_plan"]["synthesis"]["capabilities"] =
        serde_json::json!(["filesystem-read", "filesystem-write"]);
    raw["execution_plan"]["synthesis"]["max_tool_calls"] = serde_json::json!(3);
    raw["execution_plan"]["roles"][0]["budget_tokens"] = serde_json::json!(1999);
    raw["execution_plan"]["roles"][1]["budget_tokens"] = serde_json::json!(4000);
    let (mut proposal, _) =
        parse_and_validate_team(&raw.to_string(), &options, &test_efficiency(), "");
    proposal.model_fallbacks = vec!["azure-foundry::gpt-5.4-mini".into()];
    proposal.roles.reverse();
    let original = serde_json::to_value(&proposal).unwrap();
    let error = super::team_qualification::normalize_team_proposal_route(&mut proposal, &options)
        .expect_err("no candidate fits the exact-name source-reader cap");
    assert!(error.contains("role `source-reader`"), "{error}");
    assert!(
        error.contains("[filesystem-read, filesystem-write, team, telemetry]"),
        "{error}"
    );
    assert_eq!(serde_json::to_value(&proposal).unwrap(), original);
    assert!(super::team_qualification::admitted_team_fallbacks(&proposal, &options).is_empty());

    proposal.execution_plan.as_mut().unwrap().roles[0].budget_tokens = Some(2000);
    let reviewed_plan = serde_json::to_value(&proposal.execution_plan).unwrap();
    super::team_qualification::normalize_team_proposal_route(&mut proposal, &options)
        .expect("candidate at its exact minimum is admitted without increasing the role cap");
    assert_eq!(proposal.model, "github-copilot::gpt-5.6-sol");
    assert!(proposal.roles.iter().all(|role| role.model.is_empty()));
    assert_eq!(
        proposal
            .roles
            .iter()
            .find(|role| role.name == "source-reader")
            .unwrap()
            .runtime,
        "Hermes"
    );
    assert_eq!(
        serde_json::to_value(&proposal.execution_plan).unwrap(),
        reviewed_plan
    );
    for (cap, admitted) in [(Some(2999), false), (Some(3000), true), (None, true)] {
        proposal.execution_plan.as_mut().unwrap().roles[0].budget_tokens = cap;
        let fallbacks = super::team_qualification::admitted_team_fallbacks(&proposal, &options);
        assert_eq!(
            fallbacks.contains(&"azure-foundry::gpt-5.4-mini".to_string()),
            admitted,
            "cap={cap:?}"
        );
    }
    proposal.model_fallbacks.clear();
    assert!(super::team_qualification::admitted_team_fallbacks(&proposal, &options).is_empty());
    proposal.model_fallbacks = vec![
        "github-copilot::gpt-5.6-sol".into(),
        "azure-foundry::not-installed".into(),
        "azure-foundry::gpt-5.4-mini".into(),
        "azure-foundry::gpt-5.4-mini".into(),
    ];
    assert_eq!(
        super::team_qualification::admitted_team_fallbacks(&proposal, &options),
        ["azure-foundry::gpt-5.4-mini"]
    );
    let raw = serde_json::json!({
        "model": {"provider":"github-copilot", "deployment":"gpt-5.6-sol"},
        "runtime":"OpenClaw",
        "budget_tokens":300000
    });
    let (mut mission, _) = super::mission_proposal::parse_and_validate(
        &raw.to_string(),
        &options,
        &test_efficiency(),
        "Write one local document.",
    );
    assert!(super::mission::admitted_mission_fallbacks(&mission, &options).is_empty());
    mission.model_fallbacks = [
        ("github-copilot", "gpt-5.6-sol"),
        ("azure-foundry", "not-installed"),
        ("azure-foundry", "gpt-5.4-mini"),
        ("azure-foundry", "gpt-5.4-mini"),
    ]
    .into_iter()
    .map(|(provider, deployment)| super::ComposeModel {
        provider: provider.into(),
        deployment: deployment.into(),
    })
    .collect();
    let admitted = super::mission::admitted_mission_fallbacks(&mission, &options);
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].deployment, "gpt-5.4-mini");
    mission.budget_tokens = Some(2999);
    assert!(super::mission::admitted_mission_fallbacks(&mission, &options).is_empty());
}

#[test]
fn composers_preserve_requested_fallback_order_and_default_to_none() {
    let (mut options, mut team_raw) = team_role_binding_fixture();
    options.models.push(ModelOption {
        provider: "azure-foundry".into(),
        deployment: "gpt-4.1".into(),
        is_default: false,
        detail: None,
    });
    let mut mission_raw = serde_json::json!({
        "model": {"provider":"github-copilot", "deployment":"gpt-5.6-sol"}
    });
    for empty in [None, Some(serde_json::json!([]))] {
        if let Some(empty) = empty {
            mission_raw["model_fallbacks"] = empty.clone();
            team_raw["model_fallbacks"] = empty;
        }
        let (mission, _) = super::mission_proposal::parse_and_validate(
            &mission_raw.to_string(),
            &options,
            &test_efficiency(),
            "",
        );
        let (team, _) =
            parse_and_validate_team(&team_raw.to_string(), &options, &test_efficiency(), "");
        assert!(mission.model_fallbacks.is_empty());
        assert!(team.model_fallbacks.is_empty());
    }
    mission_raw["model_fallbacks"] = serde_json::json!([
        {"provider":"azure-foundry", "deployment":"gpt-5.4-mini"},
        {"provider":"unknown", "deployment":"gpt-4.1"},
        {"provider":"azure-foundry", "deployment":"gpt-4.1"}
    ]);
    team_raw["model_fallbacks"] = serde_json::json!([
        "azure-foundry::gpt-5.4-mini",
        "unknown::gpt-4.1",
        "azure-foundry::gpt-4.1"
    ]);
    let (mission, _) = super::mission_proposal::parse_and_validate(
        &mission_raw.to_string(),
        &options,
        &test_efficiency(),
        "",
    );
    let (team, _) =
        parse_and_validate_team(&team_raw.to_string(), &options, &test_efficiency(), "");
    assert_eq!(
        mission
            .model_fallbacks
            .iter()
            .map(|model| { format!("{}::{}", model.provider, model.deployment) })
            .collect::<Vec<_>>(),
        ["azure-foundry::gpt-5.4-mini", "azure-foundry::gpt-4.1"]
    );
    assert_eq!(
        team.model_fallbacks,
        ["azure-foundry::gpt-5.4-mini", "azure-foundry::gpt-4.1"]
    );
}

#[test]
fn team_role_binding_does_not_inherit_an_unmatched_roles_configuration() {
    let (options, mut raw) = team_role_binding_fixture();
    raw["roles"][0]["name"] = serde_json::json!("unrelated-observer");
    let (proposal, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency(), "");
    assert!(team_proposal_is_complete(&proposal));
    assert_eq!(proposal.roles.len(), 2);
    assert_eq!(
        serde_json::to_value(&proposal.roles[0]).unwrap(),
        serde_json::json!({
            "name": "source-reader",
            "system_prompt": raw["execution_plan"]["roles"][0]["objective"],
            "runtime": "",
            "model": "",
            "skills": []
        })
    );
    assert_eq!(
        serde_json::to_value(&proposal.roles[1]).unwrap(),
        raw["roles"][1]
    );
}

#[test]
fn team_role_binding_preserves_configuration_by_name_in_plan_order() {
    let (options, mut raw) = team_role_binding_fixture();
    let expected = raw["roles"].clone();
    raw["roles"].as_array_mut().unwrap().reverse();
    raw["roles"][0]["name"] = serde_json::json!("  decision-writer  ");
    let (proposal, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency(), "");
    assert!(team_proposal_is_complete(&proposal));
    assert_eq!(serde_json::to_value(&proposal.roles).unwrap(), expected);
    assert_eq!(
        proposal.execution_plan.unwrap().roles[1].depends_on,
        vec!["source-reader"]
    );
}

#[test]
fn team_role_binding_missing_roster_uses_each_planned_objective_and_defaults() {
    let (options, mut raw) = team_role_binding_fixture();
    raw.as_object_mut().unwrap().remove("roles");
    let (proposal, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency(), "");
    assert!(team_proposal_is_complete(&proposal));
    assert_eq!(proposal.roles.len(), 2);
    let plan = proposal.execution_plan.as_ref().unwrap();
    for (role, planned) in proposal.roles.iter().zip(&plan.roles) {
        assert_eq!(role.name, planned.name);
        assert_eq!(role.system_prompt, planned.objective);
        assert!(role.runtime.is_empty());
        assert!(role.model.is_empty());
        assert!(role.skills.is_empty());
    }
}

#[test]
fn team_role_binding_blank_matched_prompt_uses_its_own_objective() {
    let (options, mut raw) = team_role_binding_fixture();
    let mut expected = raw["roles"].clone();
    raw["roles"][0]["system_prompt"] = serde_json::json!(" \n\t ");
    expected[0]["system_prompt"] = raw["execution_plan"]["roles"][0]["objective"].clone();
    let (proposal, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency(), "");
    assert!(team_proposal_is_complete(&proposal));
    assert_eq!(serde_json::to_value(&proposal.roles).unwrap(), expected);
}

#[test]
fn team_role_binding_preserves_legacy_planless_roster() {
    let (options, mut raw) = team_role_binding_fixture();
    raw.as_object_mut().unwrap().remove("execution_plan");
    let (proposal, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency(), "");
    assert!(proposal.execution_plan.is_none());
    assert_eq!(serde_json::to_value(&proposal.roles).unwrap(), raw["roles"]);
}

#[test]
fn team_mcp_duplicates_do_not_discard_later_installed_servers() {
    let (mut options, mut raw) = team_role_binding_fixture();
    let reference = options.skills[0].clone();
    options.mcp_servers = (0..10)
        .map(|index| RefOption {
            name: format!("mcp-{index}"),
            ..reference.clone()
        })
        .collect();
    raw["mcp_servers"] = serde_json::json!([
        null,
        17,
        "not-installed",
        "mcp-0",
        " mcp-0 ",
        "mcp-1",
        "mcp-2",
        "mcp-1",
        "mcp-3",
        "mcp-4",
        "mcp-5",
        "mcp-6",
        "mcp-7",
        "mcp-8",
        "mcp-9"
    ]);
    let (proposal, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency(), "");
    assert_eq!(
        proposal.mcp_servers,
        (0..8)
            .map(|index| format!("mcp-{index}"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn team_composer_budget_covers_full_milestone_contract() {
    const { assert!(TEAM_COMPOSE_MAX_TOKENS >= 8_192) };
}

#[test]
fn collaborative_principal_uses_frontier_when_available() {
    assert!(should_strengthen_team_principal(
        3,
        "gpt-oss-120b",
        "gpt-5.6-sol"
    ));
    assert!(!should_strengthen_team_principal(
        2,
        "gpt-oss-120b",
        "gpt-5.6-sol"
    ));
    assert!(!should_strengthen_team_principal(
        3,
        "gpt-oss-120b",
        "gpt-oss-120b"
    ));
    assert!(!efficient_member_route_is_qualified(2, 1.0));
    assert!(!efficient_member_route_is_qualified(3, 0.66));
    assert!(efficient_member_route_is_qualified(3, 0.67));
}

#[test]
fn sparse_route_history_does_not_drive_execution_model_selection() {
    assert!(!recommendation_is_actionable(Some("gpt-5.6-sol"), true));
    assert!(recommendation_is_actionable(Some("gpt-5.6-sol"), false));
    assert!(!recommendation_is_actionable(None, false));
}

#[test]
fn bootstrap_only_adapters_are_non_autonomous() {
    for k in [
        "Anthropic",
        "OpenAIAgents",
        "MicrosoftAgentFramework",
        "LangGraph",
        "PydanticAi",
    ] {
        assert!(!is_autonomous_harness(k), "{k} should NOT be autonomous");
        assert!(is_non_autonomous_harness(k), "{k} should be non-autonomous");
    }
}

#[test]
fn empty_harness_is_not_treated_as_non_autonomous() {
    // Empty = "inherit default" — must not trigger a correction.
    assert!(!is_non_autonomous_harness(""));
    assert!(!is_non_autonomous_harness("   "));
}

#[test]
fn arbitrary_execution_plan_is_preserved_without_role_rewrites() {
    let value = serde_json::json!({
        "execution_plan": {
            "schema": "kars.execution-plan/v1",
            "roles": [
                {
                    "name": "source-reader",
                    "objective": "Read the supplied source material and retain exact evidence.",
                    "depends_on": [],
                    "phases": [{
                        "name": "collect",
                        "objective": "Collect the required source evidence without synthesis.",
                        "capabilities": ["filesystem-read"],
                        "max_tool_calls": 4,
                        "fresh_context": true
                    }]
                },
                {
                    "name": "decision-writer",
                    "objective": "Produce the requested decision from the retained source evidence.",
                    "depends_on": ["source-reader"],
                    "phases": [{
                        "name": "draft",
                        "objective": "Draft the decision using only retained dependency evidence.",
                        "capabilities": [],
                        "max_tool_calls": 0,
                        "fresh_context": true
                    }]
                }
            ],
            "max_parallel": 1,
            "synthesis": {
                "objective": "Reconcile the role handbacks into the final answer.",
                "capabilities": [],
                "max_tool_calls": 0
            },
            "deliverables": [{"name":"decision.md","media_type":"text/markdown"}]
        }
    });
    let plan = parse_execution_plan(&value).expect("valid plan");
    assert_eq!(
        plan.roles
            .iter()
            .map(|role| role.name.as_str())
            .collect::<Vec<_>>(),
        vec!["source-reader", "decision-writer"]
    );
    assert_eq!(plan.roles[1].depends_on, vec!["source-reader"]);
}

#[test]
fn execution_plan_rejects_unknown_capability_and_cycles() {
    let mut plan = parse_execution_plan(&serde_json::json!({
        "execution_plan": {
            "schema": "kars.execution-plan/v1",
            "roles": [{
                "name": "one",
                "objective": "Perform one arbitrary evidence task for the mission.",
                "depends_on": [],
                "phases": [{
                    "name": "work",
                    "objective": "Perform the arbitrary evidence task completely.",
                    "capabilities": ["shell"],
                    "max_tool_calls": 2,
                    "fresh_context": true
                }]
            }],
            "max_parallel": 1,
            "synthesis": {
                "objective": "Return the final mission answer from the handback.",
                "capabilities": [],
                "max_tool_calls": 0
            },
            "deliverables": []
        }
    }))
    .expect("valid baseline");
    plan.roles[0].phases[0].capabilities = vec!["repository-security".into()];
    assert!(validate_execution_plan(&plan).is_err());
    plan.roles[0].phases[0].capabilities = vec!["shell".into()];
    plan.roles[0].depends_on = vec!["one".into()];
    assert!(validate_execution_plan(&plan).is_err());
}

#[test]
fn launch_validation_rejects_structurally_valid_but_unexecutable_plans_without_rewriting() {
    let value = serde_json::json!({
        "schema": "kars.execution-plan/v1",
        "roles": [{
            "name": "note-author",
            "objective": "Write the requested local Markdown checklist.",
            "depends_on": [],
            "budget_tokens": 400000,
            "phases": [{
                "name": "draft",
                "objective": "Write and verify the requested Markdown file.",
                "capabilities": ["filesystem-read", "filesystem-write"],
                "min_tool_calls": 0,
                "max_tool_calls": 2,
                "required_tool_calls": [],
                "fresh_context": true
            }]
        }],
        "max_parallel": 1,
        "synthesis": {
            "objective": "Read the authored file and attach the final deliverable.",
            "capabilities": ["filesystem-read", "filesystem-write"],
            "max_tool_calls": 2
        },
        "deliverables": [{"name": "beta-smoke-check.md", "media_type": "text/markdown"}]
    });
    let supported = serde_json::from_value(value.clone()).unwrap();
    assert!(super::validate_execution_plan_for_launch(&supported).is_ok());
    for (pointer, unsupported) in [
        ("/roles/0/phases/0/fresh_context", serde_json::json!(false)),
        (
            "/roles/0/phases/0/capabilities",
            serde_json::json!(["filesystem-write"]),
        ),
        ("/roles/0/phases/0/max_tool_calls", serde_json::json!(0)),
        ("/roles/0/phases/0/capabilities", serde_json::json!(["mcp"])),
        ("/synthesis/capabilities", serde_json::json!([])),
        ("/synthesis/max_tool_calls", serde_json::json!(0)),
    ] {
        let mut candidate = value.clone();
        *candidate.pointer_mut(pointer).unwrap() = unsupported;
        let plan: crate::routes::tasks::ExecutionPlanDto =
            serde_json::from_value(candidate).unwrap();
        let before = serde_json::to_value(&plan).unwrap();
        assert!(validate_execution_plan(&plan).is_ok(), "{pointer}");
        assert!(
            super::validate_execution_plan_for_launch(&plan)
                .unwrap_err()
                .contains("TypedPlanExecutionUnavailable"),
            "{pointer}"
        );
        assert_eq!(serde_json::to_value(&plan).unwrap(), before);
    }
}

#[test]
fn execution_plan_parse_error_identifies_the_exact_repair() {
    let raw = serde_json::json!({
        "execution_plan": {
            "schema": "kars.execution-plan/v1",
            "roles": [{
                "name": "triage",
                "objective": "Triage",
                "phases": [{
                    "name": "inspect",
                    "objective": "Inspect the repository backlog and retain exact evidence.",
                    "capabilities": ["filesystem-read"],
                    "max_tool_calls": 1
                }]
            }],
            "max_parallel": 1,
            "synthesis": {
                "objective": "Present the verified maintenance recommendation to the reviewer.",
                "capabilities": [],
                "max_tool_calls": 0
            }
        }
    })
    .to_string();

    assert_eq!(
        execution_plan_error_from_raw(&raw).as_deref(),
        Some("role triage has an invalid objective")
    );
}

#[test]
fn research_prompts_require_web_search_and_quoted_mermaid_labels() {
    let options = test_options();
    let efficiency = test_efficiency();
    let mission_prompt = build_system_prompt(&options, &efficiency, "  (none)", "  (none)");
    assert!(mission_prompt.contains("web-search"));
    assert!(mission_prompt.contains("quote every label"));

    let team_prompt = build_team_system_prompt(&options, &efficiency, "  (none)", "  (none)");
    assert!(team_prompt.contains("web-search"));
    assert!(team_prompt.contains("qualification stays atomic"));
}

#[test]
fn weighted_budget_distribution_funds_scout_and_preserves_larger_explicit_roles() {
    let plan = parse_execution_plan(&serde_json::json!({
        "execution_plan": {
            "schema": "kars.execution-plan/v1",
            "roles": [
                {
                    "name": "source-scout",
                    "objective": "Discover the authoritative URLs and fetch evidence.",
                    "depends_on": [],
                    "phases": [{
                        "name": "discover",
                        "objective": "Search and fetch the exact URLs with evidence.",
                        "capabilities": ["web-search", "network"],
                        "min_tool_calls": 1,
                        "max_tool_calls": 32,
                        "fresh_context": true
                    }]
                },
                {
                    "name": "analyst",
                    "objective": "Inspect the retained sources and extract facts.",
                    "depends_on": ["source-scout"],
                    "phases": [
                        {
                            "name": "inspect",
                            "objective": "Inspect the retained source bundle.",
                            "capabilities": [],
                            "max_tool_calls": 0,
                            "fresh_context": true
                        },
                        {
                            "name": "summarize",
                            "objective": "Summarize the retained evidence only.",
                            "capabilities": [],
                            "max_tool_calls": 0,
                            "fresh_context": true
                        }
                    ]
                },
                {
                    "name": "reporter",
                    "objective": "Draft the downstream report from retained evidence.",
                    "depends_on": ["analyst"],
                    "phases": [
                        {
                            "name": "outline",
                            "objective": "Outline the downstream report.",
                            "capabilities": [],
                            "max_tool_calls": 0,
                            "fresh_context": true
                        },
                        {
                            "name": "draft",
                            "objective": "Draft the downstream report.",
                            "capabilities": [],
                            "max_tool_calls": 0,
                            "fresh_context": true
                        }
                    ]
                }
            ],
            "max_parallel": 1,
            "synthesis": {
                "objective": "Return the final answer from the retained evidence.",
                "capabilities": [],
                "max_tool_calls": 0
            },
            "deliverables": []
        }
    }))
    .expect("valid weighted plan");
    let floors = weighted_role_budget_floors(320_000, &plan);
    assert_eq!(floors.iter().sum::<i64>(), 320_000);
    assert!(floors[0] > floors[1] * 4);
    assert_eq!(floors[1], floors[2]);

    let mut explicit = plan.clone();
    explicit.roles[1].budget_tokens = Some(90_000);
    let (changed, updated_total) = apply_weighted_role_budget_floors(&mut explicit, 320_000);
    assert!(changed);
    assert_eq!(explicit.roles[1].budget_tokens, Some(90_000));
    assert!(updated_total > 320_000);
    assert!(explicit.roles[0].budget_tokens.expect("scout budget") > 200_000);
}

#[test]
fn decomposed_budget_preserves_the_parent_ceiling() {
    assert_eq!(
        delegation_budget_allocation(600_000, 3).expect("allocation"),
        (600_000, 200_000)
    );
    assert_eq!(
        delegation_budget_allocation(400_000, 3).expect("allocation"),
        (400_000, 133_333)
    );
    assert_eq!(
        delegation_budget_allocation(260_000, 3).expect("allocation"),
        (260_000, 86_666)
    );
    assert_eq!(
        delegation_budget_allocation(3, 3).expect("minimum allocation"),
        (3, 1)
    );
    assert!(delegation_budget_allocation(2, 3).is_err());
}

#[test]
fn orchestration_quality_prefers_reasoning_frontier_models() {
    assert!(orchestrator_quality_score("gpt-5.6-sol") > orchestrator_quality_score("gpt-oss-120b"));
    assert!(orchestrator_quality_score("claude-opus-4.8").is_some());
    assert!(orchestrator_quality_score("text-embedding-3-small").is_none());
    assert!(orchestrator_quality_score("gpt-image-1").is_none());
}

#[test]
fn model_assignment_requires_an_exact_catalogue_pair() {
    let models = vec![
        ModelOption {
            provider: "github-copilot".into(),
            deployment: "shared-name".into(),
            is_default: true,
            detail: None,
        },
        ModelOption {
            provider: "local-inference".into(),
            deployment: "local-only".into(),
            is_default: false,
            detail: None,
        },
    ];
    assert!(catalogue_has_model_key(
        &models,
        "github-copilot::shared-name"
    ));
    assert!(!catalogue_has_model_key(
        &models,
        "local-inference::shared-name"
    ));
    assert!(!catalogue_has_model_key(&models, "shared-name"));
}

#[test]
fn repository_egress_is_inferred_and_provider_hosts_are_removed() {
    let result = complete_egress_recommendation(
        vec![ComposeEgress {
            host: "api.githubcopilot.com".into(),
            port: Some(443),
        }],
        "Maintain a TypeScript GitHub repository and its package.json",
        &["github".into()],
    );
    let hosts = result
        .iter()
        .map(|endpoint| endpoint.host.as_str())
        .collect::<Vec<_>>();
    assert!(!hosts.contains(&"api.githubcopilot.com"));
    assert!(hosts.contains(&"api.github.com"));
    assert!(hosts.contains(&"raw.githubusercontent.com"));
    assert!(hosts.contains(&"patch-diff.githubusercontent.com"));
    assert!(hosts.contains(&"registry.npmjs.org"));
}

#[test]
fn dependabot_security_review_does_not_guess_wrong_package_registry() {
    let result = complete_egress_recommendation(
        vec![ComposeEgress {
            host: "registry.npmjs.org".into(),
            port: Some(443),
        }],
        "Review the newest Dependabot pull request and relevant security advisories",
        &["github".into()],
    );
    let hosts = result
        .iter()
        .map(|endpoint| endpoint.host.as_str())
        .collect::<Vec<_>>();
    assert!(!hosts.contains(&"registry.npmjs.org"));
    assert!(!hosts.contains(&"pypi.org"));
    assert!(hosts.contains(&"api.osv.dev"));
    assert!(hosts.contains(&"api.github.com"));
}
