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
use super::team_proposal::is_synthesis_only_team_role;
use super::{
    ComposeEgress, ComposeTeamProposal, delegation_budget_allocation, is_autonomous_harness,
    is_non_autonomous_harness, validate_execution_plan,
};
use crate::routes::efficiency::EfficiencyDto;
use crate::routes::options::{IsolationOption, ModelOption, Options, RuntimeOption};

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
