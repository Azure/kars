// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::egress::normalize_proposed_egress;
use super::execution::{execution_plan_error_from_raw, parse_execution_plan};
use super::prompts::{build_system_prompt, build_team_system_prompt};
use super::routing::{
    catalogue_has_model_key, orchestrator_quality_score, recommendation_is_actionable,
    should_strengthen_team_principal,
};
use super::team::{TEAM_COMPOSE_MAX_TOKENS, team_proposal_is_complete};
use super::team_proposal::{is_synthesis_only_team_role, parse_and_validate_team};
use super::{
    ComposeEgress, ComposeTeamProposal, delegation_budget_allocation, is_autonomous_harness,
    is_non_autonomous_harness, validate_execution_plan,
};
use crate::routes::efficiency::EfficiencyDto;
use crate::routes::options::{IsolationOption, ModelOption, Options, RefOption, RuntimeOption};

pub(super) fn test_options() -> Options {
    Options {
        per_run_qualification_available: true,
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

pub(super) fn test_efficiency() -> EfficiencyDto {
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
fn github_private_reads_use_the_scoped_app_route_in_mission_and_team_prompts() {
    for prompt in [
        build_system_prompt(&test_options(), &test_efficiency(), "", ""),
        build_team_system_prompt(&test_options(), &test_efficiency(), "", ""),
    ] {
        for requirement in [
            "both OpenClaw and Hermes can use native HTTP fetch",
            "prefer this route rather than adding GitHub MCP",
            "require current read-only binding authorization at preflight",
            "without custom headers or a body",
            "paginated /branches list",
            "Select no additional direct api.github.com egress",
            "Do not add `github-pr` or write authority for read-only work",
            "App repository selections neither grant nor restrict that MCP credential",
            "Preserve an explicit user request for MCP",
            "without broadening credentials",
            "Network/filesystem permissions and a write-enabled connection alone NEVER grant publication",
        ] {
            assert!(
                prompt.contains(requirement),
                "Missing GitHub contract: {requirement}"
            );
        }
        assert!(!prompt.contains(
            "Use an empty list only when the task needs no external access beyond the model"
        ));
    }
}

#[test]
fn team_composition_preserves_explicit_role_resources_and_blocks_malformed_bindings() {
    let (options, mut raw) = team_role_binding_fixture();
    let selected =
        serde_json::json!({"mcp_servers":["docs"],"egress":[{"host":"docs.example","port":443}]});
    raw["roles"][0]["resources"] = selected.clone();
    raw["roles"][1]["resources"] = serde_json::json!({"mcp_servers":[],"egress":[]});
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    let named = raw["roles"][0]["name"].as_str().unwrap();
    let member = proposal
        .roles
        .iter()
        .find(|role| role.name == named)
        .unwrap();
    assert_eq!(serde_json::to_value(&member.resources).unwrap(), selected);
    assert!(proposal.roles.iter().all(|role| role.resources.is_some()));
    raw["roles"][0]["resources"] = serde_json::json!({"unexpected":"permission"});
    let (proposal, reason, error) =
        parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    assert!(proposal.execution_plan.is_none());
    assert!(reason.unwrap().contains("invalid resource selections"));
    let feedback = super::team::team_repair_feedback(error, &proposal, &options).unwrap();
    assert!(feedback.contains("invalid resource selections"));
    let prompt = build_team_system_prompt(&options, &test_efficiency(), "", "");
    assert!(prompt.contains("subset of the Team selections"));
    assert!(prompt.contains("Every roster role must include `resources`"));
}

#[test]
fn native_mission_and_team_composition_preserves_network_without_scripted_calls() {
    let (options, mut raw) = team_role_binding_fixture();
    raw["egress"] = serde_json::json!([{"host":"example.com","port":443}]);
    for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
        for phase in role["phases"].as_array_mut().unwrap() {
            phase["execution_mode"] = serde_json::json!("native");
            phase["capabilities"] =
                serde_json::json!(["network", "filesystem-read", "filesystem-write"]);
            phase["min_tool_calls"] = serde_json::json!(0);
            phase["fresh_context"] = serde_json::json!(true);
            phase["required_tool_calls"] = serde_json::json!([]);
        }
    }
    raw["execution_plan"]["synthesis"]["execution_mode"] = serde_json::json!("native");
    raw["execution_plan"]["synthesis"]["capabilities"] =
        serde_json::json!(["filesystem-read", "filesystem-write"]);
    raw["execution_plan"]["synthesis"]["max_tool_calls"] = serde_json::json!(8);
    raw["model"] = serde_json::json!("github-copilot::gpt-5.6-sol");
    let (team, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    assert!(super::team::team_plan_roster_error(&team).is_none());
    assert!(team_proposal_is_complete(&team));
    let retained = serde_json::to_value(&team).unwrap();
    assert_eq!(
        retained["execution_plan"]["roles"][0]["phases"][0]["execution_mode"],
        "native"
    );
    assert_eq!(
        retained["execution_plan"]["roles"][0]["phases"][0]["required_tool_calls"],
        serde_json::json!([])
    );
    for runtime in ["Hermes", "OpenClaw"] {
        raw["model"] = serde_json::json!({"provider":"github-copilot","deployment":"gpt-5.6-sol"});
        raw["runtime"] = serde_json::json!(runtime);
        let (mission, _) = super::mission_proposal::parse_and_validate(
            &raw.to_string(),
            &options,
            &test_efficiency(),
        );
        let error = super::mission::mission_proposal_structure_error(&mission, &raw.to_string());
        assert!(error.is_none(), "{runtime}: {error:?}");
        let plan = mission.execution_plan.as_ref().unwrap();
        let activation = super::validate_execution_plan_for_launch(plan);
        assert!(activation.is_ok(), "{runtime}: {activation:?}");
        assert_eq!(
            plan.roles[0].phases[0].execution_mode.as_deref(),
            Some("native")
        );
        assert!(plan.roles[0].phases[0].required_tool_calls.is_empty());
        assert_eq!(plan.roles[0].phases[0].min_tool_calls, 0);
        assert!(
            plan.roles[0].phases[0]
                .capabilities
                .iter()
                .any(|capability| capability == "network")
        );
    }
}

#[test]
fn native_deliverable_composition_distinguishes_names_from_export_paths() {
    for prompt in [
        build_system_prompt(&test_options(), &test_efficiency(), "", ""),
        build_team_system_prompt(&test_options(), &test_efficiency(), "", ""),
    ] {
        for constraint in [
            "at most 16 unique basenames",
            "1–128 ASCII bytes matching ^[A-Za-z0-9][A-Za-z0-9._-]*$",
            "Never declare response.md, constructor or prototype",
            "for exports/report.md or /sandbox/exports/report.md, declare name=report.md",
            "Do not silently strip arbitrary requested directories or rename reserved files",
        ] {
            assert!(
                prompt.contains(constraint),
                "missing deliverable constraint: {constraint}"
            );
        }
    }
}

#[test]
fn native_memory_composition_preserves_binding_and_persistence_requirements() {
    for prompt in [
        build_system_prompt(&test_options(), &test_efficiency(), "", ""),
        build_team_system_prompt(&test_options(), &test_efficiency(), "", ""),
    ] {
        assert!(prompt.contains("memory search/update in both runtimes, independently of network"));
        assert!(prompt.contains("exact private/shared scope grants"));
        assert!(prompt.contains("Queued/in_progress update status is not persistence"));
        assert!(prompt.contains("completed is not human knowledge approval"));
        assert!(!prompt.contains(
            "persistent-memory tools and binary document extraction are not yet supported"
        ));
    }
}

#[test]
fn native_peer_composition_explains_existing_transport_without_waiving_evidence() {
    let options = test_options();
    let efficiency = test_efficiency();
    for prompt in [
        build_system_prompt(&options, &efficiency, "", ""),
        build_team_system_prompt(&options, &efficiency, "", ""),
    ] {
        for constraint in [
            "native OpenClaw and Hermes worker phases with `network`",
            "supported in reviewed Missions as well as Teams",
            "external MCP server or new communication transport is not required",
            "current assignment authority, tool policy, encryption and installed route qualification checks",
            "locators are not proof of current identity, readiness or permission",
            "avoid completion dependencies between those roles",
            "max_parallel large enough for the simultaneous participants within installed limits",
            "final synthesis cannot converse with already-finished workers",
            "send acknowledgement does not establish receipt or reply",
            "Never substitute simulated dialogue",
            "normal pre-flight and actual run evidence remain authoritative",
        ] {
            assert!(
                prompt.contains(constraint),
                "missing peer constraint: {constraint}"
            );
        }
    }
}

#[test]
fn native_single_agent_composition_requires_review_without_invented_roles() {
    let (options, mut raw) = team_role_binding_fixture();
    raw["execution_plan"]["roles"] = serde_json::json!([]);
    raw["execution_plan"]["max_parallel"] = serde_json::json!(1);
    raw["execution_plan"]["synthesis"]["execution_mode"] = serde_json::json!("native");
    raw["execution_plan"]["synthesis"]["capabilities"] =
        serde_json::json!(["filesystem-write", "network"]);
    raw["execution_plan"]["synthesis"]["max_tool_calls"] = serde_json::json!(16);
    raw["model"] = serde_json::json!({"provider":"github-copilot","deployment":"gpt-5.6-sol"});
    for runtime in ["OpenClaw", "Hermes"] {
        raw["runtime"] = serde_json::json!(runtime);
        let (mission, _) = super::mission_proposal::parse_and_validate(
            &raw.to_string(),
            &options,
            &test_efficiency(),
        );
        assert!(
            super::mission::mission_proposal_structure_error(&mission, &raw.to_string()).is_none()
        );
        assert_eq!(mission.delegation.mode, "single-agent");
        assert!(mission.delegation.roles.is_empty());
        let plan = mission.execution_plan.as_ref().unwrap();
        assert!(plan.roles.is_empty());
        super::validate_execution_plan_for_launch(plan).unwrap();
        let mut absent = raw.clone();
        absent["execution_plan"] = serde_json::Value::Null;
        let (mission, _) = super::mission_proposal::parse_and_validate(
            &absent.to_string(),
            &options,
            &test_efficiency(),
        );
        assert!(
            super::mission::mission_proposal_structure_error(&mission, &absent.to_string())
                .unwrap()
                .contains("explicit reviewed execution plan")
        );
    }
}

#[test]
fn native_mcp_composition_uses_network_and_preserves_binding_qualification() {
    let (mut options, mut raw) = team_role_binding_fixture();
    options.mcp_servers.push(RefOption {
        name: "repository-wiki".into(),
        discovered_tools: vec!["read_wiki_contents".into()],
        ..options.skills[0].clone()
    });
    options.tool_policies.push(RefOption {
        name: "research-tools".into(),
        ..options.skills[0].clone()
    });
    raw["mcp_servers"] = serde_json::json!(["repository-wiki"]);
    raw["tool_policy"] = serde_json::json!("research-tools");
    for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
        for phase in role["phases"].as_array_mut().unwrap() {
            phase["execution_mode"] = serde_json::json!("native");
            phase["capabilities"] =
                serde_json::json!(["filesystem-read", "filesystem-write", "network"]);
            phase["fresh_context"] = serde_json::json!(true);
            phase["min_tool_calls"] = serde_json::json!(0);
            phase["required_tool_calls"] = serde_json::json!([]);
        }
    }
    raw["execution_plan"]["synthesis"]["execution_mode"] = serde_json::json!("native");
    raw["execution_plan"]["synthesis"]["capabilities"] =
        serde_json::json!(["filesystem-read", "filesystem-write"]);
    raw["execution_plan"]["synthesis"]["max_tool_calls"] = serde_json::json!(4);
    let efficiency = test_efficiency();
    for prompt in [
        build_system_prompt(&options, &efficiency, "", ""),
        build_team_system_prompt(&options, &efficiency, "", ""),
    ] {
        assert!(prompt.contains("repository-wiki"));
        assert!(prompt.contains("read_wiki_contents"));
        assert!(prompt.contains("native MCP discovery/calls through the router"));
        assert!(prompt.contains("selected binding still requires MCP qualification"));
        assert!(!prompt.contains("select it and declare `mcp`"));
        assert!(!prompt.contains("Shell, MCP, persistent-memory tools"));
    }
    let check = |servers: Vec<String>, plan: crate::routes::tasks::ExecutionPlanDto| {
        assert_eq!(servers, vec!["repository-wiki"]);
        assert!(super::validate_execution_plan_for_launch(&plan).is_ok());
        assert!(
            plan.roles
                .iter()
                .flat_map(|role| &role.phases)
                .all(|phase| phase.required_tool_calls.is_empty())
        );
        let blueprint = crate::routes::tasks::BlueprintDto {
            mcp_servers: servers,
            execution_plan: Some(plan),
            ..Default::default()
        };
        let (required, _) = crate::routes::validate::qualification_requirements(&blueprint, None);
        assert!(required.contains("mcp"));
        assert!(required.contains("network"));
    };
    let (team, _, _) = parse_and_validate_team(&raw.to_string(), &options, &efficiency);
    assert!(team_proposal_is_complete(&team));
    check(team.mcp_servers, team.execution_plan.unwrap());
    for runtime in ["OpenClaw", "Hermes"] {
        raw["runtime"] = serde_json::json!(runtime);
        raw["model"] = serde_json::json!({"provider":"github-copilot","deployment":"gpt-5.6-sol"});
        let (mission, _) =
            super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &efficiency);
        assert_eq!(mission.tool_policy.as_deref(), Some("research-tools"));
        check(mission.mcp_servers, mission.execution_plan.unwrap());
    }
}

#[test]
fn mission_and_team_repair_reject_required_fetches_outside_proposed_egress() {
    let (options, mut raw) = team_role_binding_fixture();
    raw["egress"] = serde_json::json!([{"host":"freshworks.com","port":443}]);
    raw["execution_plan"]["roles"][0]["phases"][0]["required_tool_calls"] = serde_json::json!([
        {"name":"http_fetch","arguments":{"url":"https://support.freshdesk.com/support/solutions","method":"GET"}}
    ]);
    raw["execution_plan"]["roles"][0]["phases"][0]["capabilities"] =
        serde_json::json!(["network", "filesystem-write"]);
    raw["execution_plan"]["roles"][0]["phases"][0]["min_tool_calls"] = serde_json::json!(1);
    for covered in [false, true] {
        if covered {
            raw["egress"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({"host":"support.freshdesk.com","port":443}));
        }
        raw["model"] = serde_json::json!("github-copilot::gpt-5.6-sol");
        let (team, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
        let before = serde_json::to_value(&team).unwrap();
        let error = super::team::team_plan_roster_error(&team);
        assert_eq!(error.is_none(), covered, "{error:?}");
        assert_eq!(team_proposal_is_complete(&team), covered);
        assert_eq!(serde_json::to_value(&team).unwrap(), before);
        if let Some(error) = error {
            assert!(error.contains("support.freshdesk.com"));
        }
        raw["model"] = serde_json::json!({"provider":"github-copilot","deployment":"gpt-5.6-sol"});
        raw["runtime"] = serde_json::json!("Hermes");
        let (mission, _) = super::mission_proposal::parse_and_validate(
            &raw.to_string(),
            &options,
            &test_efficiency(),
        );
        let before = serde_json::to_value(&mission).unwrap();
        let error = super::mission::mission_proposal_structure_error(&mission, &raw.to_string());
        assert_eq!(error.is_none(), covered, "{error:?}");
        if let Some(error) = error {
            assert!(error.contains("support.freshdesk.com"));
        }
        assert_eq!(serde_json::to_value(&mission).unwrap(), before);
    }
}

#[test]
fn composition_retains_task_requirements_without_qualification() {
    const CHILD: &str = "KARS_TEST_COMPOSE_REVIEW_WITHOUT_QUALIFICATION";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "routes::compose::capability_tests::composition_retains_task_requirements_without_qualification",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("BRIDGE_ROUTE_QUALIFICATION_MODE", "required")
            .env("BRIDGE_QUALIFICATION_RECORDS_JSON", "[]")
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
    raw["instructions"] =
        serde_json::json!("Compare official public sources and independently verify the findings.");
    raw["egress"] = serde_json::json!([{"host":"docs.example.org", "port":443}]);
    raw["egress_mode"] = serde_json::json!("strict");
    raw["execution_plan"]["roles"][0]["phases"][0]["capabilities"] =
        serde_json::json!(["network", "filesystem-write"]);
    for role in raw["roles"].as_array_mut().unwrap() {
        role["skills"] = serde_json::json!([]);
    }
    for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
        role["budget_tokens"] = serde_json::json!(100000);
        for phase in role["phases"].as_array_mut().unwrap() {
            phase["fresh_context"] = serde_json::json!(true);
        }
    }
    raw["execution_plan"]["synthesis"]["capabilities"] =
        serde_json::json!(["filesystem-read", "filesystem-write"]);
    raw["execution_plan"]["synthesis"]["max_tool_calls"] = serde_json::json!(4);
    let (team, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    assert!(team_proposal_is_complete(&team));
    let expected_team = serde_json::to_value(&team).unwrap();
    let response = super::team::team_review_response(
        team,
        &options,
        Some("Independent public-source comparison.".into()),
        "unit-test-model".into(),
    );
    assert!(response.available);
    assert!(response.reason.is_none());
    assert!(
        response
            .rationale
            .as_deref()
            .unwrap()
            .starts_with("Launch blocked:")
    );
    assert_eq!(
        serde_json::to_value(response.proposal.as_ref().unwrap()).unwrap(),
        expected_team
    );
    assert_eq!(response.source.as_deref(), Some("unit-test-model"));

    raw["model"] = serde_json::json!({"provider":"github-copilot","deployment":"gpt-5.6-sol"});
    raw["runtime"] = serde_json::json!("OpenClaw");
    raw["budget_tokens"] = serde_json::json!(300000);
    let (mission, _) =
        super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &test_efficiency());
    assert!(super::mission::mission_proposal_structure_error(&mission, &raw.to_string()).is_none());
    let expected_mission = serde_json::to_value(&mission).unwrap();
    let response = super::mission::mission_review_response(
        mission,
        &options,
        false,
        Some("Independent public-source comparison.".into()),
        "unit-test-model".into(),
    );
    assert!(response.available);
    assert!(response.reason.is_none());
    let rationale = response.rationale.as_deref().unwrap();
    assert!(rationale.starts_with("Launch blocked:"), "{rationale}");
    assert!(rationale.contains("Independent public-source comparison."));
    assert_eq!(
        serde_json::to_value(response.proposal.as_ref().unwrap()).unwrap(),
        expected_mission
    );

    let required = [
        "artifacts",
        "delegation",
        "network",
        "filesystem-read",
        "filesystem-write",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert!(
        !crate::routes::options::route_admitted(
            "OpenClaw",
            "github-copilot",
            "gpt-5.6-sol",
            &required,
            1,
            Some(300000)
        )
        .unwrap()
    );
    assert!(!crate::routes::qualification_runs::admit(Ok(false), false).unwrap());
}

fn native_single_agent_plan_fixture() -> serde_json::Value {
    let mut plan = crate::kars::task::execution_plan::tests::filesystem_plan();
    plan.roles.clear();
    plan.synthesis.execution_mode = Some("native".into());
    serde_json::to_value(crate::routes::tasks::ExecutionPlanDto::from_crd(&plan)).unwrap()
}

#[test]
fn mission_composition_rejects_planless_and_malformed_output() {
    let options = test_options();
    let efficiency = test_efficiency();
    for plan in [None, Some(serde_json::Value::Null)] {
        let mut raw = serde_json::json!({
            "instructions":"Summarize the supplied text.",
            "model":{"provider":"github-copilot","deployment":"gpt-5.6-sol"}
        });
        if let Some(plan) = plan {
            raw["execution_plan"] = plan;
        }
        let (proposal, _) =
            super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &efficiency);
        assert!(
            super::mission::mission_proposal_structure_error(&proposal, &raw.to_string())
                .unwrap()
                .contains("explicit reviewed execution plan")
        );
    }
    for raw in [
        "not json",
        "{}",
        r#"{"instructions":"Research sources.","execution_plan":{"roles":"broken"}}"#,
    ] {
        let (proposal, _) = super::mission_proposal::parse_and_validate(raw, &options, &efficiency);
        assert!(
            super::mission::mission_proposal_structure_error(&proposal, raw).is_some(),
            "{raw}"
        );
    }
    assert_eq!(
        super::proposal_review_rationale(Some("Qualified proposal.".into()), Ok(())),
        Some("Qualified proposal.".into())
    );
}

#[test]
fn mission_composition_repairs_invalid_model_instead_of_accepting_default() {
    let options = test_options();
    let efficiency = test_efficiency();
    for model in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!("github-copilot::gpt-5.6-sol")),
        Some(serde_json::json!({"deployment":"gpt-5.6-sol"})),
        Some(serde_json::json!({"provider":"unavailable","deployment":"gpt-5.6-sol"})),
        Some(serde_json::json!({"provider":"github-copilot","deployment":"unavailable"})),
    ] {
        let mut raw = serde_json::json!({"instructions":"Compare the supplied evidence."});
        if let Some(model) = model {
            raw["model"] = model;
        }
        let (proposal, _) =
            super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &efficiency);
        let before = serde_json::to_value(&proposal).unwrap();
        let error = super::mission::mission_proposal_structure_error(&proposal, &raw.to_string())
            .expect("normalization must not hide an invalid model from repair");
        assert!(error.contains("model must explicitly name"), "{error}");
        assert_eq!(serde_json::to_value(&proposal).unwrap(), before);
    }
    let mut options = options;
    options.models.push(ModelOption {
        provider: "another-provider".into(),
        deployment: "stronger-model".into(),
        is_default: false,
        detail: None,
    });
    let raw = serde_json::json!({
        "instructions":"Compare the supplied evidence.",
        "execution_plan": native_single_agent_plan_fixture(),
        "model":{"provider":"another-provider","deployment":"stronger-model"}
    });
    let (proposal, _) =
        super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &efficiency);
    assert!(
        super::mission::mission_proposal_structure_error(&proposal, &raw.to_string()).is_none()
    );
    assert_eq!(proposal.model.unwrap().deployment, "stronger-model");
}

#[test]
fn composer_prompts_preserve_requirements_and_propose_network_and_budgets() {
    let options = test_options();
    let efficiency = test_efficiency();
    for prompt in [
        build_system_prompt(&options, &efficiency, "", ""),
        build_team_system_prompt(&options, &efficiency, "", ""),
    ] {
        assert!(prompt.contains("Composition is not launch authorization"));
        assert!(prompt.contains("task-required capabilities, destinations, resources, roles"));
        assert!(prompt.contains("reviewed extra spending fields"));
        assert!(prompt.contains("budget_tokens=null"));
        assert!(prompt.contains("no ADDITIONAL"));
        assert!(prompt.contains("GovernedInference accounting"));
        assert!(prompt.contains("official website/API hosts"));
        assert!(!prompt.contains("Prefer an empty list"));
        assert!(!prompt.contains("propose a supported alternative"));
    }
}

#[test]
fn mission_prompt_distinguishes_inherited_routes_from_team_role_routing() {
    let options = test_options();
    let efficiency = test_efficiency();
    let mission = build_system_prompt(&options, &efficiency, "", "");
    assert!(
        mission.contains("Mission workers inherit the principal's runtime, provider and model")
    );
    assert!(mission.contains("execution_plan.roles has no per-role runtime or model override"));
    assert!(mission.contains("requires a Team with explicit per-role routing"));
    assert!(mission.contains("Preserve that requirement as an unresolved blocker"));
    assert!(mission.contains("Model fallbacks are not per-role model assignment"));
    let team = build_team_system_prompt(&options, &efficiency, "", "");
    assert!(!team.contains("MISSION ROLE ROUTING:"));
    assert!(team.contains("Each role can run a DIFFERENT harness and model"));
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
        profile: None,
        tier: 3,
        budget_tokens: None,
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
        egress_mode: "strict".into(),
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

pub(super) fn team_role_binding_fixture() -> (Options, serde_json::Value) {
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
        "budget_tokens": 3000000,
        "model": "github-copilot::gpt-5.6-sol",
        "roles": [
            {
                "name": "source-reader",
                "system_prompt": "Read supplied sources and preserve precise supporting quotations.",
                "runtime": "Hermes",
                "model": "azure-foundry::gpt-5.4-mini",
                "skills": ["source-reading"],
                "resources": {"mcp_servers":[], "egress":[]}
            },
            {
                "name": "decision-writer",
                "system_prompt": "Write a decision document with cited evidence and explicit uncertainty.",
                "runtime": "OpenClaw",
                "model": "github-copilot::gpt-5.6-sol",
                "skills": ["decision-writing"],
                "resources": {"mcp_servers":[], "egress":[]}
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
    let (mut proposal, _, _) =
        parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
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
        assert!(!prompt.contains("null-plan exception"));
        assert!(prompt.contains("Every Mission requires an explicit execution_plan"));
        assert!(!prompt.contains("\"max_tool_calls\": 0"));
        assert!(!prompt.contains("\"capabilities\": []"));
        assert!(prompt.contains(
            "For decomposed work, synthesis requires filesystem-read and filesystem-write for handoffs and deliverables"
        ));
        assert!(prompt.contains("Set a positive max_tool_calls with task-appropriate headroom"));
        assert!(
            prompt.contains(
                "Each role, phase and synthesis objective must contain 20–1200 UTF-8 bytes"
            )
        );
        assert!(prompt.contains("Capabilities must be unique exact names"));
        assert!(!prompt.contains("} | null"));
    }
    let team = build_team_system_prompt(&options, &efficiency, "", "");
    assert!(team.contains("every requested output file by its exact name"));
    assert!(team.contains("\"name\":\"<exact requested safe filename>\""));
    assert!(team.contains("NOT a roster or execution_plan.roles entry"));
    assert!(team.contains("reviewed extra or profile Team-lifetime cap, or null to inherit"));
    assert!(team.contains("EVERY phase of a predecessor finishes"));
    assert!(team.contains(
        "post-review incorporation and final file validation in the principal synthesis"
    ));
    assert!(team.contains("each milestone runs the complete execution_plan"));
    assert!(team.contains("Use one milestone for one requested delivery"));
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
    let (mut proposal, _, _) =
        parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    proposal.model_fallbacks = vec!["azure-foundry::gpt-5.4-mini".into()];
    proposal.roles.reverse();
    let mut underfunded = proposal.clone();
    underfunded.budget_tokens = Some(2500);
    for role in &mut underfunded.execution_plan.as_mut().unwrap().roles {
        role.budget_tokens = Some(4000);
    }
    let feedback = super::team::team_repair_feedback(None, &underfunded, &options).unwrap();
    for expected in ["source-reader", "2500", "3000"] {
        assert!(feedback.contains(expected), "{feedback}");
    }
    underfunded.budget_tokens = Some(1999);
    let feedback = super::team::team_repair_feedback(None, &underfunded, &options).unwrap();
    assert!(
        feedback.contains("proposed lifetime token cap=Some(1999)"),
        "{feedback}"
    );
    let original = serde_json::to_value(&proposal).unwrap();
    let feedback =
        super::team::team_repair_feedback(Some("other review failure".into()), &proposal, &options)
            .unwrap();
    for expected in ["other review failure", "source-reader", "1999", "3000"] {
        assert!(feedback.contains(expected), "{feedback}");
    }
    assert_eq!(serde_json::to_value(&proposal).unwrap(), original);
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
        "budget_tokens":300000,
        "execution_plan": native_single_agent_plan_fixture()
    });
    let (mut mission, _) =
        super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &test_efficiency());
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
        );
        let (team, _, _) =
            parse_and_validate_team(&team_raw.to_string(), &options, &test_efficiency());
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
    );
    let (team, _, _) = parse_and_validate_team(&team_raw.to_string(), &options, &test_efficiency());
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
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    assert!(team_proposal_is_complete(&proposal));
    assert_eq!(proposal.roles.len(), 2);
    assert_eq!(
        serde_json::to_value(&proposal.roles[0]).unwrap(),
        serde_json::json!({
            "name": "source-reader",
            "system_prompt": raw["execution_plan"]["roles"][0]["objective"],
            "runtime": "",
            "model": "",
            "skills": [],
            "resources": {"mcp_servers":[], "egress":[]}
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
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
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
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
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
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    assert!(team_proposal_is_complete(&proposal));
    assert_eq!(serde_json::to_value(&proposal.roles).unwrap(), expected);
}

#[test]
fn team_role_binding_preserves_legacy_planless_roster() {
    let (options, mut raw) = team_role_binding_fixture();
    raw.as_object_mut().unwrap().remove("execution_plan");
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
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
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
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
}

#[test]
fn sparse_route_history_does_not_drive_execution_model_selection() {
    assert!(!recommendation_is_actionable(Some("gpt-5.6-sol"), true));
    assert!(recommendation_is_actionable(Some("gpt-5.6-sol"), false));
    assert!(!recommendation_is_actionable(None, false));
}

fn ambiguous_efficiency_fixture() -> (Options, EfficiencyDto) {
    let (mut options, _) = team_role_binding_fixture();
    options.models.push(ModelOption {
        provider: "azure-openai".into(),
        deployment: "gpt-5.4-mini".into(),
        is_default: false,
        detail: None,
    });
    let mut efficiency = test_efficiency();
    efficiency
        .routes
        .push(crate::routes::efficiency::RouteEfficiency {
            route: "gpt-5.4-mini".into(),
            harness: "Hermes".into(),
            runs: 10,
            verified_runs: 10,
            acceptance_rate: 1.0,
            success_rate: 1.0,
            tokens_per_outcome: Some(100),
            ..Default::default()
        });
    efficiency.recommended = Some("gpt-5.4-mini".into());
    efficiency.recommended_harness = Some("Hermes".into());
    efficiency.total_runs = 10;
    (options, efficiency)
}

#[test]
fn ambiguous_efficiency_does_not_pick_first_provider_or_adopt_its_harness() {
    let (mut options, efficiency) = ambiguous_efficiency_fixture();
    for _ in 0..2 {
        let (proposal, _) =
            super::mission_proposal::parse_and_validate("{}", &options, &efficiency);
        let model = proposal.model.unwrap();
        assert_eq!(model.provider, "github-copilot");
        assert_eq!(model.deployment, "gpt-5.6-sol");
        assert_eq!(proposal.runtime, "OpenClaw");
        assert!(
            proposal
                .model_basis
                .unwrap()
                .contains("shared by multiple providers")
        );
        options.models.reverse();
    }
}

#[test]
fn ambiguous_efficiency_preserves_explicit_provider_without_endorsement() {
    let (options, efficiency) = ambiguous_efficiency_fixture();
    for provider in ["azure-foundry", "azure-openai"] {
        let raw = serde_json::json!({"model":{"provider":provider,"deployment":"gpt-5.4-mini"}});
        let (proposal, _) =
            super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &efficiency);
        assert_eq!(proposal.model.unwrap().provider, provider);
        assert_eq!(proposal.runtime, "OpenClaw");
        assert_eq!(
            proposal.model_basis.as_deref(),
            Some("Chosen by the orchestrator for this objective.")
        );
    }
}

#[test]
fn unambiguous_efficiency_keeps_existing_route_and_harness_selection() {
    let (mut options, efficiency) = ambiguous_efficiency_fixture();
    options
        .models
        .retain(|model| model.provider != "azure-openai");
    let (proposal, _) = super::mission_proposal::parse_and_validate("{}", &options, &efficiency);
    assert_eq!(proposal.model.unwrap().provider, "azure-foundry");
    assert_eq!(proposal.runtime, "Hermes");
    assert!(proposal.model_basis.unwrap().contains("Best learned route"));
}

#[test]
fn orchestration_honors_each_provider_qualified_default_before_model_rankings() {
    let mut options = test_options();
    options.models = [
        ("azure-openai", "gpt-5.4"),
        ("github-copilot", "gpt-5.4"),
        ("github-copilot", "gpt-6-astra"),
        ("github-copilot", "gpt-6.1-sol"),
        ("github-copilot", "mai-code-1.1-flash"),
        ("local", "future-configured-model"),
    ]
    .into_iter()
    .map(|(provider, deployment)| ModelOption {
        provider: provider.into(),
        deployment: deployment.into(),
        is_default: false,
        detail: None,
    })
    .collect();
    let mut efficiency = test_efficiency();
    efficiency.recommended = Some("gpt-5.4".into());
    let selections: Vec<_> = options
        .models
        .iter()
        .map(|model| (model.provider.clone(), model.deployment.clone()))
        .collect();
    for (selected_provider, selected_deployment) in selections {
        options.default_model = Some(selected_deployment.clone());
        for model in &mut options.models {
            model.is_default =
                model.provider == selected_provider && model.deployment == selected_deployment;
        }
        for _ in 0..2 {
            let (provider, deployment, basis) =
                super::routing::select_orchestrator_route(&options, &efficiency).unwrap();
            assert_eq!(provider, selected_provider);
            assert_eq!(deployment, selected_deployment);
            assert!(basis.contains("configured default"));
            options.models.reverse();
        }
    }
}

#[test]
fn full_gpt54_orchestration_beats_mini_in_either_catalogue_order() {
    let (mut options, mut efficiency) = ambiguous_efficiency_fixture();
    options
        .models
        .retain(|model| model.deployment == "gpt-5.4-mini");
    options.models.push(ModelOption {
        provider: "azure-foundry".into(),
        deployment: "gpt-5.4".into(),
        is_default: false,
        detail: None,
    });
    efficiency.recommended_low_confidence = false;
    for ambiguous in [true, false] {
        if !ambiguous {
            options
                .models
                .retain(|model| model.provider != "azure-openai");
        }
        for _ in 0..2 {
            let (provider, deployment, _) =
                super::routing::select_orchestrator_route(&options, &efficiency).unwrap();
            assert_eq!(provider, "azure-foundry");
            assert_eq!(deployment, "gpt-5.4");
            options.models.reverse();
        }
    }
    options
        .models
        .retain(|model| model.deployment == "gpt-5.4-mini");
    assert_eq!(
        super::routing::select_orchestrator_route(&options, &efficiency)
            .unwrap()
            .1,
        "gpt-5.4-mini"
    );
}

#[test]
fn ambiguous_efficiency_adds_neither_frontier_nor_acceptance_bonus() {
    let (mut options, mut efficiency) = ambiguous_efficiency_fixture();
    for low_confidence in [false, true] {
        efficiency.recommended_low_confidence = low_confidence;
        for _ in 0..2 {
            let (provider, deployment, basis) =
                super::routing::select_orchestrator_route(&options, &efficiency).unwrap();
            assert_eq!(provider, "github-copilot");
            assert_eq!(deployment, "gpt-5.6-sol");
            assert!(!basis.contains("efficiency-frontier"));
            options.models.reverse();
        }
    }
}

#[test]
fn ambiguous_efficiency_is_not_exposed_as_team_route_measurements() {
    let (options, mut efficiency) = ambiguous_efficiency_fixture();
    efficiency.routes[0].harness = "OpenClaw".into();
    efficiency.recommended_harness = Some("OpenClaw".into());
    for provider in ["azure-foundry", "azure-openai"] {
        let raw = serde_json::json!({"model":format!("{provider}::gpt-5.4-mini")});
        let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &efficiency);
        assert_eq!(proposal.model, format!("{provider}::gpt-5.4-mini"));
        assert_eq!(proposal.expected_tokens_per_outcome, None);
        assert_eq!(proposal.efficiency_sample_runs, 0);
        assert!(
            proposal
                .model_basis
                .unwrap()
                .contains("cannot endorse this provider")
        );
    }
}

#[test]
fn principal_promotion_preserves_inherited_models_without_endorsing_ambiguous_history() {
    let (mut options, mut efficiency) = ambiguous_efficiency_fixture();
    for model in &mut options.models {
        if model.deployment == "gpt-5.4-mini" {
            model.deployment = "gpt-oss-120b".into();
        }
    }
    efficiency.routes[0].route = "gpt-oss-120b".into();
    efficiency.routes[0].harness = "OpenClaw".into();
    efficiency.recommended = Some("gpt-oss-120b".into());
    efficiency.recommended_harness = Some("OpenClaw".into());
    let raw = serde_json::json!({
        "model":"azure-foundry::gpt-oss-120b",
        "roles":[
            {"name":"reader","system_prompt":"Read all supplied source documents and report evidence."},
            {"name":"analyst","system_prompt":"Analyze the supplied evidence and report the findings."},
            {"name":"writer","system_prompt":"Write the requested deliverables from supplied evidence."}
        ]
    });
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &efficiency);
    assert_eq!(proposal.roles.len(), 3);
    assert_eq!(proposal.model, "github-copilot::gpt-5.6-sol");
    assert!(
        proposal
            .roles
            .iter()
            .all(|role| role.model == "azure-foundry::gpt-oss-120b")
    );
    let basis = proposal.model_basis.unwrap();
    assert!(!basis.contains("retain the qualified"));
    assert!(basis.contains("route qualification and budget admission still apply"));
}

#[test]
fn principal_promotion_preserves_explicit_and_inherited_member_models_and_budgets() {
    let (options, mut raw) = team_role_binding_fixture();
    raw["model"] = serde_json::json!("azure-foundry::gpt-5.4-mini");
    raw["roles"][0]["model"] = serde_json::json!("");
    raw["roles"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "independent-reviewer",
            "system_prompt": "Independently check the source evidence and calculations."
        }));
    let mut review = raw["execution_plan"]["roles"][1].clone();
    review["name"] = serde_json::json!("independent-reviewer");
    raw["execution_plan"]["roles"]
        .as_array_mut()
        .unwrap()
        .push(review);
    for (role, tokens) in raw["execution_plan"]["roles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip([900_000, 800_000, 900_000])
    {
        role["budget_tokens"] = serde_json::json!(tokens);
    }
    let (proposal, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    assert_eq!(proposal.model, "github-copilot::gpt-5.6-sol");
    assert_eq!(proposal.roles[0].model, "azure-foundry::gpt-5.4-mini");
    assert_eq!(proposal.roles[1].model, "github-copilot::gpt-5.6-sol");
    assert_eq!(proposal.roles[2].model, "azure-foundry::gpt-5.4-mini");
    assert_eq!(proposal.budget_tokens, Some(3_000_000));
    let plan = proposal.execution_plan.unwrap();
    assert_eq!(plan.roles.len(), 3);
    for (role, proposed) in plan
        .roles
        .iter()
        .zip(raw["execution_plan"]["roles"].as_array().unwrap())
    {
        assert_eq!(role.budget_tokens, proposed["budget_tokens"].as_i64());
    }
}

#[test]
fn ambiguous_efficiency_is_excluded_from_both_composer_prompts() {
    let (options, efficiency) = ambiguous_efficiency_fixture();
    for prompt in [
        build_system_prompt(&options, &efficiency, "", ""),
        build_team_system_prompt(&options, &efficiency, "", ""),
    ] {
        assert!(prompt.contains("historical measurements excluded from routing"));
        assert!(!prompt.contains("← recommended"));
        assert!(!prompt.contains("Prefer the recommended route's model"));
        assert!(!prompt.contains("100% accepted"));
    }
}

#[test]
fn sparse_efficiency_default_basis_does_not_claim_model_is_missing() {
    let (mut options, mut efficiency) = ambiguous_efficiency_fixture();
    options
        .models
        .retain(|model| model.provider != "azure-openai");
    efficiency.recommended_low_confidence = true;
    let (proposal, _) = super::mission_proposal::parse_and_validate("{}", &options, &efficiency);
    assert!(
        proposal
            .model_basis
            .unwrap()
            .contains("insufficient confidence")
    );
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
fn synthesis_repair_feedback_names_the_failed_field_without_changing_the_plan() {
    for (field, value, expected) in [
        (
            "objective",
            serde_json::json!("x".repeat(19)),
            "20–1200 UTF-8 bytes; received 19",
        ),
        (
            "objective",
            serde_json::json!("é".repeat(601)),
            "20–1200 UTF-8 bytes; received 1202",
        ),
        (
            "max_tool_calls",
            serde_json::json!(-1),
            "max_tool_calls must be nonnegative",
        ),
        (
            "capabilities",
            serde_json::json!(["network", "network"]),
            "capabilities must contain unique names",
        ),
        (
            "capabilities",
            serde_json::json!(["http_fetch"]),
            "capabilities must contain unique names",
        ),
    ] {
        let mut raw = serde_json::json!({"execution_plan": native_single_agent_plan_fixture()});
        raw["execution_plan"]["synthesis"][field] = value;
        let retained = raw.clone();
        let error = execution_plan_error_from_raw(&raw.to_string()).unwrap();
        assert!(error.contains(expected), "{error}");
        assert!(parse_execution_plan(&raw).is_none());
        assert_eq!(raw, retained);
    }
    for objective in ["x".repeat(20), "é".repeat(600)] {
        let mut raw = serde_json::json!({"execution_plan": native_single_agent_plan_fixture()});
        raw["execution_plan"]["synthesis"]["objective"] = serde_json::json!(objective);
        raw["execution_plan"]["synthesis"]["max_tool_calls"] = serde_json::json!(128);
        assert!(execution_plan_error_from_raw(&raw.to_string()).is_none());
    }
    let mut legacy = crate::kars::task::execution_plan::tests::filesystem_plan();
    legacy.synthesis.max_tool_calls = 128;
    legacy.synthesis.execution_mode = None;
    let raw = serde_json::json!({
        "execution_plan": crate::routes::tasks::ExecutionPlanDto::from_crd(&legacy)
    });
    let error = execution_plan_error_from_raw(&raw.to_string()).unwrap();
    assert!(error.contains("explicitly set execution_mode"), "{error}");
}

#[test]
fn research_prompts_distinguish_reviewed_fetch_from_search_and_quote_mermaid_labels() {
    let options = test_options();
    let efficiency = test_efficiency();
    let mission_prompt = build_system_prompt(&options, &efficiency, "  (none)", "  (none)");
    assert!(mission_prompt.contains("web-search"));
    assert!(mission_prompt.contains("quote every label"));

    let team_prompt = build_team_system_prompt(&options, &efficiency, "  (none)", "  (none)");
    assert!(team_prompt.contains("web-search"));
    assert!(team_prompt.contains("retained qualification stay atomic on one route"));
    for prompt in [&mission_prompt, &team_prompt] {
        assert!(prompt.contains("Hosted web-search remains unsupported under finite funding"));
        assert!(prompt.contains("Without network, required_tool_calls must be empty"));
        assert!(prompt.contains("Retain genuinely necessary unsupported capabilities"));
        assert!(prompt.contains("Propose the task's necessary egress hosts"));
        assert!(prompt.contains("normally set required_tool_calls=[]"));
        assert!(prompt.contains("not a scripted tool sequence"));
        assert!(prompt.contains("native agent loop chooses tool calls, source URLs and recovery"));
        assert!(prompt.contains(r#""execution_mode": "native""#));
        assert!(!prompt.contains("Reserve one tool call for each"));
        assert!(!prompt.contains("Read all supplied handoffs before fetching or writing outputs"));
        assert!(!prompt.contains("set it to at least 1 whenever"));
    }
}

#[test]
fn execution_plan_roundtrip_preserves_absent_and_explicit_role_caps() {
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
    .expect("valid plan without role caps");
    assert!(plan.roles.iter().all(|role| role.budget_tokens.is_none()));

    let mut explicit = plan.clone();
    explicit.roles[1].budget_tokens = Some(90_000);
    let raw = serde_json::json!({"budget_tokens": null, "execution_plan": explicit});
    let parsed = parse_execution_plan(&raw).unwrap();
    assert_eq!(parsed.roles[0].budget_tokens, None);
    assert_eq!(parsed.roles[1].budget_tokens, Some(90_000));
    assert_eq!(parsed.roles[2].budget_tokens, None);
    assert_eq!(serde_json::to_value(parsed).unwrap(), raw["execution_plan"]);
}

#[test]
fn single_agent_budget_preserves_parent_without_child_allocation() {
    assert_eq!(
        delegation_budget_allocation(600_000, 0).unwrap(),
        (600_000, 0)
    );
    assert!(delegation_budget_allocation(0, 0).is_err());
    assert!(delegation_budget_allocation(-1, 0).is_err());
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
    for mini in ["gpt-5.4-mini", "gpt_5_4_mini", "gpt-5-4-mini-2026-03-17"] {
        assert!(orchestrator_quality_score("gpt-5.4") > orchestrator_quality_score(mini));
    }
    assert!(orchestrator_quality_score("gpt-5.4-pro") > orchestrator_quality_score("gpt-5.4"));
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
fn proposed_egress_is_normalized_without_inventing_destinations() {
    let result = normalize_proposed_egress(
        [
            ("api.githubcopilot.com", Some(443)),
            ("example.openai.azure.com", Some(443)),
            ("example.services.ai.azure.com", Some(443)),
            ("API.GITHUB.COM", None),
            ("api.github.com", Some(443)),
            ("registry.npmjs.org", None),
            ("docs.example.org", Some(8443)),
        ]
        .into_iter()
        .map(|(host, port)| ComposeEgress {
            host: host.into(),
            port,
        })
        .collect(),
    );
    assert_eq!(
        result
            .iter()
            .map(|e| (e.host.as_str(), e.port))
            .collect::<Vec<_>>(),
        vec![
            ("api.github.com", Some(443)),
            ("docs.example.org", Some(8443)),
            ("registry.npmjs.org", Some(443)),
        ]
    );
    assert!(normalize_proposed_egress(Vec::new()).is_empty());
}

#[test]
fn mission_and_team_egress_come_from_the_proposal_not_intent_keywords() {
    let (options, mut raw) = team_role_binding_fixture();
    raw["instructions"] =
        serde_json::json!("Compare current official vendor sources and report verified findings.");
    raw["model"] = serde_json::json!({"provider":"github-copilot","deployment":"gpt-5.6-sol"});
    raw["runtime"] = serde_json::json!("OpenClaw");
    for endpoints in [
        serde_json::json!([]),
        serde_json::json!([{"host":"www.zendesk.com","port":443}]),
        serde_json::json!([{"host":"api.github.com","port":443},{"host":"registry.npmjs.org","port":443}]),
    ] {
        raw["egress"] = endpoints.clone();
        for intent in [
            "Compare public vendor pricing. No GitHub, repository access, Python pip, npm, Rust cargo, or vulnerability lookups.",
            "Maintain a TypeScript GitHub repository and its package.json; review Dependabot security advisories.",
            "Write a dependency-free Python invoice reconciliation tool; do not install packages.",
        ] {
            raw["instructions"] = serde_json::json!(intent);
            let (mission, _) = super::mission_proposal::parse_and_validate(
                &raw.to_string(),
                &options,
                &test_efficiency(),
            );
            let (team, _, _) =
                parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
            assert_eq!(
                serde_json::to_value(&mission.egress).unwrap(),
                endpoints,
                "Mission: {intent}"
            );
            assert_eq!(
                serde_json::to_value(&team.egress).unwrap(),
                endpoints,
                "Team: {intent}"
            );
        }
    }
}

#[test]
fn native_memory_composition_does_not_invent_or_drop_explicit_bindings() {
    let (mut options, mut raw) = team_role_binding_fixture();
    let mut binding = options.skills[0].clone();
    binding.name = "explicit-memory".into();
    binding.backend = Some("foundry".into());
    binding.readiness = Some("Ready".into());
    options.memories.push(binding);
    for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
        for phase in role["phases"].as_array_mut().unwrap() {
            phase["execution_mode"] = serde_json::json!("native");
            phase["capabilities"] =
                serde_json::json!(["memory", "filesystem-read", "filesystem-write"]);
            phase["fresh_context"] = serde_json::json!(true);
            phase["required_tool_calls"] = serde_json::json!([]);
        }
    }
    raw["execution_plan"]["synthesis"]["execution_mode"] = serde_json::json!("native");
    raw["execution_plan"]["synthesis"]["capabilities"] =
        serde_json::json!(["memory", "filesystem-read", "filesystem-write"]);
    raw["execution_plan"]["synthesis"]["max_tool_calls"] = serde_json::json!(8);
    for selection in [
        serde_json::Value::Null,
        serde_json::json!("explicit-memory"),
    ] {
        raw["memory"] = selection.clone();
        raw["model"] = serde_json::json!("github-copilot::gpt-5.6-sol");
        let (team, _, _) = parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
        assert_eq!(team.memory.as_deref(), selection.as_str());
        assert!(team.execution_plan.is_some());
        for runtime in ["Hermes", "OpenClaw"] {
            raw["runtime"] = serde_json::json!(runtime);
            raw["model"] =
                serde_json::json!({"provider":"github-copilot","deployment":"gpt-5.6-sol"});
            let (mission, _) = super::mission_proposal::parse_and_validate(
                &raw.to_string(),
                &options,
                &test_efficiency(),
            );
            assert_eq!(mission.memory.as_deref(), selection.as_str());
            assert!(
                super::mission::mission_proposal_structure_error(&mission, &raw.to_string())
                    .is_none()
            );
        }
    }
    for invalid in [
        serde_json::json!("missing-binding"),
        serde_json::json!(17),
        serde_json::json!(""),
    ] {
        raw["memory"] = invalid;
        raw["model"] = serde_json::json!("github-copilot::gpt-5.6-sol");
        let (team, reason, error) =
            parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
        assert!(team.execution_plan.is_none());
        assert!(reason.unwrap().contains("Explicit memory binding"));
        let feedback = super::team::team_repair_feedback(error, &team, &options).unwrap();
        assert!(feedback.contains("Explicit memory binding"));
        assert!(feedback.contains("memory=null"));
    }
    raw["memory"] = serde_json::json!("missing-binding");
    raw["model"] = serde_json::json!({"provider":"github-copilot","deployment":"gpt-5.6-sol"});
    let (mission, _) =
        super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &test_efficiency());
    assert_eq!(mission.memory.as_deref(), Some("missing-binding"));
    raw["memory"] = serde_json::json!(17);
    let (mission, _) =
        super::mission_proposal::parse_and_validate(&raw.to_string(), &options, &test_efficiency());
    assert!(
        super::mission::mission_proposal_structure_error(&mission, &raw.to_string())
            .unwrap()
            .contains("Explicit memory binding")
    );
}
