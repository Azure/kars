// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars::task::{BudgetScope, TaskBudget};
use crate::routes::options::ModelOption;
use crate::routes::validate::{CheckStatus, budget_contracts};
use serde_json::{Value, json};

fn configured_test(name: &str) -> bool {
    const CHILD: &str = "KARS_TEAM_PREFLIGHT_TEST";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("routes::teams::preflight::tests::{name}"),
            "--nocapture",
        ])
        .env(CHILD, name)
        .env("BRIDGE_ROUTE_QUALIFICATION_MODE", "required")
        .env("BRIDGE_QUALIFICATION_RECORDS_JSON", "[]")
        .env_remove("BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

fn fixture() -> (Options, BlueprintDto, Vec<CreateRole>) {
    let options = Options {
        per_run_qualification_available: true,
        models: vec![
            ModelOption {
                provider: "github-copilot".into(),
                deployment: "gpt-5.4".into(),
                is_default: true,
                detail: None,
            },
            ModelOption {
                provider: "foundry".into(),
                deployment: "gpt-5.4-mini".into(),
                is_default: false,
                detail: None,
            },
        ],
        default_model: None,
        provider: None,
        runtimes: vec![],
        isolation: vec![],
        tool_policies: vec![],
        mcp_servers: vec![],
        mcp_profiles: vec![],
        memories: vec![],
        skills: vec![],
    };
    let blueprint = serde_json::from_value(json!({
        "runtime": "OpenClaw", "model": {"provider":"github-copilot", "deployment":"gpt-5.4"},
        "tool_policy":"kars-default",
        "execution_plan": {
            "schema":"kars.execution-plan/v1", "max_parallel":2,
            "roles": [
                {"name":"researcher", "objective":"Research the reviewed source material", "budget_tokens":1600000,
                 "phases":[{"name":"research", "objective":"Write a grounded research handoff document", "execution_mode":"native", "capabilities":["filesystem-write"], "max_tool_calls":4, "fresh_context":true}]},
                {"name":"reviewer", "objective":"Review the research against its original evidence", "depends_on":["researcher"], "budget_tokens":900000,
                 "phases":[{"name":"review", "objective":"Read and verify the retained research handoff", "execution_mode":"native", "capabilities":["filesystem-read", "filesystem-write"], "max_tool_calls":4, "fresh_context":true}]}
            ],
            "synthesis":{"objective":"Combine the reviewed research into a final report", "execution_mode":"native", "capabilities":["filesystem-read", "filesystem-write"], "max_tool_calls":4},
            "deliverables":[{"name":"report.md", "media_type":"text/markdown"}]
        }
    })).unwrap();
    // Reverse roster order to prove name binding, not positional budget matching.
    let roles = serde_json::from_value(json!([
        {"name":"reviewer", "runtime":"Hermes", "model":"foundry::gpt-5.4-mini"},
        {"name":"researcher", "runtime":"OpenClaw", "model":"foundry::gpt-5.4-mini"}
    ]))
    .unwrap();
    (options, blueprint, roles)
}

fn prepare_fixture(
    blueprint: &mut BlueprintDto,
    roles: &mut [CreateRole],
) -> AppResult<Vec<TeamRole>> {
    prepare(
        &fixture().0,
        "work",
        blueprint,
        roles,
        Some(4_800_000),
        true,
    )
}

#[test]
fn team_preflight_uses_creation_roster_and_default_route_semantics() {
    if !configured_test("team_preflight_uses_creation_roster_and_default_route_semantics") {
        return;
    }
    let (_, mut blueprint, mut roles) = fixture();
    blueprint.model = None;
    let roster = prepare_fixture(&mut blueprint, &mut roles).unwrap();
    assert_eq!(blueprint.model.as_ref().unwrap().provider, "github-copilot");
    assert_eq!(roster[0].name, "reviewer");
    let member = roster[0].blueprint.as_ref().unwrap();
    assert_eq!(member.runtime.as_deref(), Some("Hermes"));
    assert_eq!(member.model.as_ref().unwrap().provider, "foundry");
    assert_eq!(member.model.as_ref().unwrap().deployment, "gpt-5.4-mini");
    assert_eq!(member.tool_policy.as_deref(), Some("kars-default"));
    assert_eq!(
        serde_json::to_value(&roster).unwrap(),
        json!(
            validation::build_roster(
                &roles,
                blueprint.tool_policy.as_deref(),
                Some(&blueprint.clone().into_crd()),
            )
            .unwrap()
        )
    );
}

#[test]
fn team_preflight_and_create_share_explicit_member_resources() {
    if !configured_test("team_preflight_and_create_share_explicit_member_resources") {
        return;
    }
    let (_, mut blueprint, mut roles) = fixture();
    blueprint.egress = serde_json::from_value(json!([{"host":"docs.example","port":443}])).unwrap();
    for role in &mut roles {
        role.resources =
            Some(serde_json::from_value(json!({"egress":[{"host":"docs.example"}]})).unwrap());
    }
    let prepared = prepare_fixture(&mut blueprint, &mut roles).unwrap();
    let created: Vec<TeamRole> = serde_json::from_value(json!(
        validation::build_roster(
            &roles,
            blueprint.tool_policy.as_deref(),
            Some(&blueprint.clone().into_crd())
        )
        .unwrap()
    ))
    .unwrap();
    // CRD serialization omits empty vectors; compare the materialized semantics.
    assert_eq!(
        serde_json::to_value(&prepared).unwrap(),
        serde_json::to_value(&created).unwrap()
    );
    for member in &prepared {
        let bp = member.blueprint.as_ref().unwrap();
        assert_eq!(bp.egress[0].host, "docs.example");
        assert_eq!(bp.egress[0].port, Some(443));
    }
    roles[0].resources.as_mut().unwrap().egress[0].host = "unreviewed.example".into();
    assert!(
        prepare_fixture(&mut blueprint, &mut roles)
            .unwrap_err()
            .to_string()
            .contains("subset")
    );
}

#[test]
fn team_preflight_rejects_unknown_provider_model_and_nonmatching_rosters() {
    if !configured_test("team_preflight_rejects_unknown_provider_model_and_nonmatching_rosters") {
        return;
    }
    for route in [
        "unknown::gpt-5.4-mini",
        "github-copilot::gpt-5.4-mini",
        "foundry::missing",
        "gpt-5.4-mini",
        "foundry::",
    ] {
        let (_, mut blueprint, mut roles) = fixture();
        roles[0].model = Some(route.into());
        assert!(
            prepare_fixture(&mut blueprint, &mut roles)
                .unwrap_err()
                .to_string()
                .contains("model route")
        );
    }
    for variant in 0..5 {
        let (_, mut blueprint, mut roles) = fixture();
        match variant {
            0 => {
                roles.pop();
            }
            1 => roles.push(roles[0].clone()),
            2 => roles[0].name = "unknown".into(),
            3 => roles[0].name = "principal".into(),
            _ => roles[0].name = " ".into(),
        }
        assert!(prepare_fixture(&mut blueprint, &mut roles).is_err());
    }
    let (_, mut blueprint, mut roles) = fixture();
    blueprint.execution_plan = None;
    assert!(prepare_fixture(&mut blueprint, &mut roles).is_err());
}

fn catalog_state() -> crate::state::AppState {
    use axum::{
        body::Body,
        http::{Request, Response},
    };
    use tower::service_fn;
    let contracts = [
        ("github-copilot", "gpt-5.4", 922000),
        ("azure-openai", "gpt-5.4-mini", 272000),
    ]
    .into_iter()
    .map(|(provider, model, input)| {
        json!({
            "id":model, "version":"v1", "validUntil":"2030-01-01T00:00:00Z",
            "providerId":provider, "endpoint":"https://models.example", "model":model,
            "operation":"ChatCompletions", "outputField":"MaxTokens", "maximumInputTokens":input,
            "maximumOutputTokens":50, "maximumWireBytes":4096, "outputBoundIncludesReasoning":true
        })
    })
    .collect::<Vec<Value>>();
    let raw = json!({"version":"test-v1", "contracts":contracts}).to_string();
    let service = service_fn(move |request: Request<kube::client::Body>| {
        assert_eq!(request.method(), "GET");
        assert_eq!(
            request.uri().path(),
            "/api/v1/namespaces/kars-system/configmaps/kars-inference-budget-contracts"
        );
        let raw = raw.clone();
        async move {
            Ok::<_, std::io::Error>(Response::builder().header("content-type", "application/json")
                .body(Body::from(json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{}, "data":{"contracts.json":raw}}).to_string())).unwrap())
        }
    });
    crate::state::AppState::for_test_client(kube::Client::new(service, "work"), "work")
}

#[tokio::test]
async fn team_preflight_binds_foundry_role_cap_while_mission_inherits_copilot() {
    if !configured_test("team_preflight_binds_foundry_role_cap_while_mission_inherits_copilot") {
        return;
    }
    let state = catalog_state();
    let cluster = state.cluster().unwrap();
    let (_, mut blueprint, mut roles) = fixture();
    let roster = prepare_fixture(&mut blueprint, &mut roles).unwrap();
    let budget = TaskBudget {
        scope: Some(BudgetScope::GovernedInference),
        tokens: Some(4_800_000),
        usd_micros: None,
    };
    let team =
        budget_contracts::check_with_roster(cluster, &blueprint, Some(&budget), Some(&roster))
            .await
            .unwrap();
    assert_eq!(team.status, CheckStatus::Warn, "{}", team.detail);
    let mission = budget_contracts::check_with_roster(cluster, &blueprint, Some(&budget), None)
        .await
        .unwrap();
    assert_eq!(mission.status, CheckStatus::Fail);
    assert!(
        mission.detail.contains("reviewer") && mission.detail.contains("922001"),
        "{}",
        mission.detail
    );

    let mut insufficient = blueprint.clone();
    insufficient.execution_plan.as_mut().unwrap().roles[1].budget_tokens = Some(272000);
    let result =
        budget_contracts::check_with_roster(cluster, &insufficient, Some(&budget), Some(&roster))
            .await
            .unwrap();
    assert_eq!(result.status, CheckStatus::Fail);
    assert!(result.detail.contains("reviewer") && result.detail.contains("272001"));

    let mut inherited = roles.clone();
    inherited[0].model = None;
    let inherited = prepare_fixture(&mut blueprint.clone(), &mut inherited).unwrap();
    let result =
        budget_contracts::check_with_roster(cluster, &blueprint, Some(&budget), Some(&inherited))
            .await
            .unwrap();
    assert_eq!(result.status, CheckStatus::Fail);
    assert!(result.detail.contains("reviewer") && result.detail.contains("922001"));

    for malformed in [vec![], vec![roster[0].clone(), roster[0].clone()]] {
        assert_eq!(
            budget_contracts::check_with_roster(
                cluster,
                &blueprint,
                Some(&budget),
                Some(&malformed)
            )
            .await
            .unwrap()
            .status,
            CheckStatus::Fail
        );
    }
    let mut priced = budget.clone();
    priced.usd_micros = Some(1_000_000);
    assert_eq!(
        budget_contracts::check_with_roster(cluster, &blueprint, Some(&priced), Some(&roster))
            .await
            .unwrap()
            .status,
        CheckStatus::Fail
    );
}

#[tokio::test]
async fn team_preflight_retains_member_and_principal_fallback_contract_checks() {
    if !configured_test("team_preflight_retains_member_and_principal_fallback_contract_checks") {
        return;
    }
    let state = catalog_state();
    let cluster = state.cluster().unwrap();
    let (_, mut blueprint, mut roles) = fixture();
    let mut roster = prepare_fixture(&mut blueprint, &mut roles).unwrap();
    let budget = TaskBudget {
        scope: Some(BudgetScope::GovernedInference),
        tokens: Some(4_800_000),
        usd_micros: None,
    };
    roster[0]
        .blueprint
        .as_mut()
        .unwrap()
        .model_fallbacks
        .push(crate::kars::task::TaskModel {
            provider: "github-copilot".into(),
            deployment: "gpt-5.4".into(),
        });
    let result =
        budget_contracts::check_with_roster(cluster, &blueprint, Some(&budget), Some(&roster))
            .await
            .unwrap();
    assert_eq!(result.status, CheckStatus::Fail);
    assert!(result.detail.contains("reviewer") && result.detail.contains("922001"));
    blueprint.model_fallbacks.push(ModelDto {
        provider: "foundry".into(),
        deployment: "unenrolled".into(),
    });
    let result =
        budget_contracts::check_with_roster(cluster, &blueprint, Some(&budget), Some(&roster))
            .await
            .unwrap();
    assert_eq!(result.status, CheckStatus::Fail);
    assert!(result.detail.contains("unenrolled"));
}
