// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Check, CheckStatus};
use crate::inference_budget_contract::catalog::Catalog;
use crate::kars::cluster::Cluster;
use crate::kars::task::{BudgetScope, TaskBudget};
use crate::routes::tasks::BlueprintDto;

// This is a necessary-condition check, not a second provider resolver. Named
// providers and legacy defaults can have different accounting IDs for the same
// model; the router remains authoritative for the exact destination and operation.
pub(crate) fn parse_catalog(raw: &str, now: i64) -> Result<Catalog, String> {
    let catalog: Catalog = serde_json::from_str(raw).map_err(|_| {
        "The operator inference-budget contract catalogue is malformed.".to_string()
    })?;
    catalog
        .validate(now)
        .map_err(|_| "The operator inference-budget contract catalogue is invalid.".to_string())?;
    Ok(catalog)
}

pub(crate) async fn read_catalog(cluster: &Cluster) -> Result<String, String> {
    let data = cluster
        .configmap_data_result("kars-inference-budget-contracts")
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Cannot read operator budget contracts");
            "Cannot read the operator inference-budget contract catalogue. Retry after restoring access."
                .to_string()
        })?
        .ok_or_else(|| "The operator inference-budget contract catalogue is absent.".to_string())?;
    data.get("contracts.json")
        .cloned()
        .ok_or_else(|| "The operator budget catalogue has no contracts.json.".to_string())
}

pub(crate) fn coverage(
    raw: &str,
    models: &[String],
    money_required: bool,
    now: i64,
    tokens: Option<i64>,
) -> Result<(), String> {
    let catalog = parse_catalog(raw, now)?;
    if !money_required && tokens.is_none_or(|cap| cap <= 0) {
        return Ok(());
    }
    for model in models {
        let contracts = catalog
            .contracts
            .iter()
            .filter(|contract| {
                contract.model == *model
                    && contract.validate(now).is_ok()
                    && (!money_required || contract.maximum_price.is_some())
            })
            .collect::<Vec<_>>();
        if contracts.is_empty() {
            return Err(format!(
                "No current operator budget contract{} covers model {model:?}. \
                 This includes the requested primary and fallback models. \
                 An operator must enroll supported bounds{} before launch; \
                 connecting a provider does not enroll a budget contract. \
                 Do not remove the cap to bypass this failure.",
                if money_required {
                    " with maximum prices"
                } else {
                    ""
                },
                if money_required {
                    " and maximum prices"
                } else {
                    ""
                },
            ));
        }
        if let Some(cap) = tokens.filter(|cap| *cap > 0) {
            let minimum = contracts
                .iter()
                .filter_map(|contract| contract.maximum_input_tokens.checked_add(1))
                .min()
                .ok_or_else(|| {
                    "The operator contract reservation overflows token accounting.".to_string()
                })?;
            if (cap as u64) < minimum {
                return Err(format!(
                    "The {cap}-token cap cannot reserve even one request for model {model:?}. \
                     Current operator contracts require at least {minimum} tokens \
                     (the complete input bound plus one output token); the actual output \
                     allowance and earlier usage require additional headroom. Review the \
                     role allocation within the existing lifetime budget or select a compatible \
                     model. Do not reset the account, remove its cap or weaken the contract."
                ));
            }
        }
    }
    Ok(())
}

pub(super) async fn check(
    cluster: &Cluster,
    blueprint: &BlueprintDto,
    budget: Option<&TaskBudget>,
) -> Option<Check> {
    check_with_roster(cluster, blueprint, budget, None).await
}

async fn model_names(cluster: &Cluster, blueprint: &BlueprintDto) -> Result<Vec<String>, String> {
    let mut models = match blueprint.model.as_ref() {
        Some(model) => vec![model.deployment.clone()],
        None => vec![cluster.controller_models().await.0.ok_or_else(|| {
            "Cannot determine the inherited model for budget preflight.".to_string()
        })?],
    };
    models.extend(
        blueprint
            .model_fallbacks
            .iter()
            .map(|model| model.deployment.clone()),
    );
    Ok(models)
}

pub(crate) async fn check_with_roster(
    cluster: &Cluster,
    blueprint: &BlueprintDto,
    budget: Option<&TaskBudget>,
    roster: Option<&[crate::kars::team::TeamRole]>,
) -> Option<Check> {
    // Scope enrolls accounting; finite effective ceilings also require spending contracts.
    let budget =
        budget.filter(|budget| matches!(budget.scope, Some(BudgetScope::GovernedInference)))?;
    let result = async {
        let raw = read_catalog(cluster).await?;
        let models = model_names(cluster, blueprint).await?;
        coverage(
            &raw,
            &models,
            budget.usd_micros.is_some_and(|cap| cap > 0),
            chrono::Utc::now().timestamp(),
            budget.tokens,
        )?;
        if let Some(plan) = &blueprint.execution_plan {
            if let Some(roster) = roster {
                let names = roster
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<std::collections::BTreeSet<_>>();
                let planned = plan
                    .roles
                    .iter()
                    .map(|role| role.name.as_str())
                    .collect::<std::collections::BTreeSet<_>>();
                if names != planned
                    || names.len() != roster.len()
                    || planned.len() != plan.roles.len()
                {
                    return Err(
                        "Team budget preflight requires an exact, unique roster/plan binding."
                            .into(),
                    );
                }
            }
            for role in &plan.roles {
                let member =
                    roster.and_then(|entries| entries.iter().find(|entry| entry.name == role.name));
                // Explicit Team blueprints are complete overrides. Mission roles
                // have no roster and continue inheriting the principal route.
                let role_models = if let Some(member_blueprint) =
                    member.and_then(|entry| entry.blueprint.as_ref())
                {
                    model_names(cluster, &super::blueprint_to_dto(member_blueprint)).await?
                } else {
                    models.clone()
                };
                let mut cap = plan.role_qualification_budget(&role.name, budget.tokens);
                if let Some(limit) = member
                    .and_then(|entry| entry.envelope.as_ref())
                    .and_then(|envelope| envelope.budget.as_ref())
                    .and_then(|budget| budget.tokens)
                    .filter(|limit| *limit > 0)
                {
                    cap = Some(cap.map_or(limit, |cap| cap.min(limit)));
                }
                coverage(
                    &raw,
                    &role_models,
                    budget.usd_micros.is_some_and(|cap| cap > 0)
                        || member
                            .and_then(|entry| entry.envelope.as_ref())
                            .and_then(|envelope| envelope.budget.as_ref())
                            .and_then(|budget| budget.usd_micros)
                            .is_some_and(|cap| cap > 0),
                    chrono::Utc::now().timestamp(),
                    cap,
                )
                .map_err(|detail| format!("Role {:?}: {detail}", role.name))?;
            }
        }
        Ok::<(), String>(())
    }
    .await;
    Some(Check {
        id: "budget_contracts".into(),
        label: "Operator budget contracts and minimum reservation capacity".into(),
        status: if result.is_ok() {
            CheckStatus::Warn
        } else {
            CheckStatus::Fail
        },
        detail: result.err().unwrap_or_else(|| {
            "Declared finite ceilings have model-contract coverage and minimum reservation \
             capacity. Routes with no declared ceiling do not require a spending contract. \
             This is not runtime authorization: the broker checks current funding and every \
             effective ancestor before allowing record-only usage; capped work still requires \
             an exact provider, endpoint and operation contract. Credentials and runtime \
             readiness are checked separately, and actual usage remains recorded."
                .into()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn catalogue() -> Value {
        json!({
            "version": "test-v1",
            "contracts": [{
                "id": "foundry-chat", "version": "v1",
                "validUntil": "2030-01-01T00:00:00Z",
                "providerId": "azure-openai",
                "endpoint": "https://models.example",
                "model": "gpt-5.4-mini", "operation": "ChatCompletions",
                "outputField": "MaxTokens",
                "maximumInputTokens": 100,
                "maximumOutputTokens": 50,
                "maximumWireBytes": 4096,
                "outputBoundIncludesReasoning": true
            }]
        })
    }

    fn state_with_catalogue(raw: String) -> crate::state::AppState {
        use axum::{
            body::Body,
            http::{Request, Response},
        };
        let service = tower::service_fn(move |request: Request<kube::client::Body>| {
            let raw = raw.clone();
            async move {
                assert_eq!(request.method(), "GET");
                assert_eq!(
                    request.uri().path(),
                    "/api/v1/namespaces/kars-system/configmaps/kars-inference-budget-contracts"
                );
                Ok::<_, std::io::Error>(
                    Response::builder()
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"apiVersion":"v1","kind":"ConfigMap",
                        "metadata":{},"data":{"contracts.json":raw}})
                            .to_string(),
                        ))
                        .unwrap(),
                )
            }
        });
        crate::state::AppState::for_test_client(kube::Client::new(service, "work"), "work")
    }

    fn blueprint() -> BlueprintDto {
        serde_json::from_value(json!({
            "model":{"provider":"azure-foundry","deployment":"gpt-5.4-mini"},
            "execution_plan":{"schema":"kars.execution-plan/v1","max_parallel":1,
                "roles":[{"name":"researcher","objective":"Inspect independent evidence.",
                    "phases":[{"name":"research","objective":"Review the retained evidence.",
                        "execution_mode":"native","fresh_context":true,"max_tool_calls":3}]}],
                "synthesis":{"objective":"Write an evidence-based conclusion.",
                    "execution_mode":"native","max_tool_calls":3}}
        }))
        .unwrap()
    }

    fn scope_only_budget() -> TaskBudget {
        TaskBudget {
            scope: Some(BudgetScope::GovernedInference),
            tokens: None,
            usd_micros: None,
        }
    }

    #[tokio::test]
    async fn scope_only_preflight_does_not_require_model_spending_contracts() {
        let budget = scope_only_budget();
        let bp = blueprint();
        let mut expired = catalogue();
        expired["contracts"][0]["validUntil"] = json!("2000-01-01T00:00:00Z");
        let mut uncovered = catalogue();
        uncovered["contracts"][0]["model"] = json!("another-model");
        for (raw, expected) in [
            (catalogue().to_string(), CheckStatus::Warn),
            (expired.to_string(), CheckStatus::Warn),
            (uncovered.to_string(), CheckStatus::Warn),
            (
                json!({"version":"test","contracts":[]}).to_string(),
                CheckStatus::Warn,
            ),
            ("malformed".into(), CheckStatus::Fail),
        ] {
            let state = state_with_catalogue(raw);
            let result = check(state.cluster().unwrap(), &bp, Some(&budget))
                .await
                .unwrap();
            assert_eq!(result.status, expected, "{}", result.detail);
            assert_eq!(result.id, "budget_contracts");
        }
        let state = state_with_catalogue(catalogue().to_string());
        for fallback in [false, true] {
            let mut bp = bp.clone();
            let missing = crate::routes::tasks::ModelDto {
                provider: "azure-foundry".into(),
                deployment: "missing-model".into(),
            };
            if fallback {
                bp.model_fallbacks.push(missing);
            } else {
                bp.model = Some(missing);
            }
            let result = check(state.cluster().unwrap(), &bp, Some(&budget))
                .await
                .unwrap();
            assert_eq!(result.status, CheckStatus::Warn);
            let finite = TaskBudget {
                tokens: Some(1000),
                ..budget.clone()
            };
            let result = check(state.cluster().unwrap(), &bp, Some(&finite))
                .await
                .unwrap();
            assert_eq!(result.status, CheckStatus::Fail);
            assert!(result.detail.contains("missing-model"));
        }
        for budget in [
            None,
            Some(TaskBudget {
                scope: None,
                tokens: None,
                usd_micros: None,
            }),
            Some(TaskBudget {
                scope: None,
                tokens: Some(1000),
                usd_micros: None,
            }),
        ] {
            assert!(
                check(state.cluster().unwrap(), &bp, budget.as_ref())
                    .await
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn scope_only_preflight_preserves_role_routes_bindings_and_finite_ceilings() {
        use crate::kars::team::TeamRole;
        let state = state_with_catalogue(catalogue().to_string());
        let cluster = state.cluster().unwrap();
        let mut bp = blueprint();
        let budget = scope_only_budget();
        let roster = vec![TeamRole {
            name: "researcher".into(),
            ..Default::default()
        }];
        for (aggregate, role, member, expected) in [
            (None, None, None, CheckStatus::Warn),
            (Some(100), None, None, CheckStatus::Fail),
            (Some(101), None, None, CheckStatus::Warn),
            (None, Some(100), None, CheckStatus::Fail),
            (None, Some(101), None, CheckStatus::Warn),
            (None, None, Some(100), CheckStatus::Fail),
            (None, None, Some(101), CheckStatus::Warn),
            (Some(101), Some(1000), Some(100), CheckStatus::Fail),
        ] {
            let mut budget = budget.clone();
            budget.tokens = aggregate;
            bp.execution_plan.as_mut().unwrap().roles[0].budget_tokens = role;
            let mut roster = roster.clone();
            if let Some(tokens) = member {
                roster[0].envelope = Some(
                    serde_json::from_value(json!({"tier":2,
                    "authorityCeiling":2,"budget":{"scope":"GovernedInference","tokens":tokens}}))
                    .unwrap(),
                );
            }
            let before = format!("{bp:?} {budget:?} {roster:?}");
            let result = check_with_roster(cluster, &bp, Some(&budget), Some(&roster))
                .await
                .unwrap();
            assert_eq!(result.status, expected, "{}", result.detail);
            assert_eq!(format!("{bp:?} {budget:?} {roster:?}"), before);
        }
        bp.execution_plan.as_mut().unwrap().roles[0].budget_tokens = None;
        for invalid in [
            vec![],
            vec![roster[0].clone(), roster[0].clone()],
            vec![TeamRole {
                name: "unknown".into(),
                ..Default::default()
            }],
        ] {
            let result = check_with_roster(cluster, &bp, Some(&budget), Some(&invalid))
                .await
                .unwrap();
            assert_eq!(result.status, CheckStatus::Fail);
            assert!(result.detail.contains("exact, unique roster/plan binding"));
        }
        for field in ["model", "modelFallbacks"] {
            bp.execution_plan.as_mut().unwrap().roles[0].budget_tokens = None;
            let mut roster = roster.clone();
            let mut member =
                json!({"model":{"provider":"azure-foundry","deployment":"gpt-5.4-mini"}});
            let missing = json!({"provider":"azure-foundry","deployment":"missing-model"});
            member[field] = if field == "model" {
                missing
            } else {
                json!([missing])
            };
            roster[0].blueprint = Some(serde_json::from_value(member).unwrap());
            assert_eq!(
                check_with_roster(cluster, &bp, Some(&budget), Some(&roster))
                    .await
                    .unwrap()
                    .status,
                CheckStatus::Warn
            );
            bp.execution_plan.as_mut().unwrap().roles[0].budget_tokens = Some(1000);
            let result = check_with_roster(cluster, &bp, Some(&budget), Some(&roster))
                .await
                .unwrap();
            assert_eq!(result.status, CheckStatus::Fail);
            assert!(
                result.detail.contains("Role \"researcher\"")
                    && result.detail.contains("missing-model")
            );
        }
    }

    #[tokio::test]
    async fn scope_only_preflight_keeps_explicit_currency_price_requirements() {
        let bp = blueprint();
        for tokens in [None, Some(1000)] {
            let budget = TaskBudget {
                tokens,
                usd_micros: Some(500000),
                ..scope_only_budget()
            };
            for priced in [false, true] {
                let mut catalog = catalogue();
                if priced {
                    catalog["contracts"][0]["maximumPrice"] =
                        json!({"kind":"perRequest","maximumMicros":100});
                }
                let state = state_with_catalogue(catalog.to_string());
                let result = check(state.cluster().unwrap(), &bp, Some(&budget))
                    .await
                    .unwrap();
                assert_eq!(
                    result.status,
                    if priced {
                        CheckStatus::Warn
                    } else {
                        CheckStatus::Fail
                    }
                );
                if !priced {
                    assert!(result.detail.contains("maximum prices"));
                }
            }
            let mut catalog = catalogue();
            let mut unpriced = catalog["contracts"][0].clone();
            unpriced["id"] = json!("unpriced-role");
            unpriced["model"] = json!("role-model");
            catalog["contracts"][0]["maximumPrice"] =
                json!({"kind":"perRequest","maximumMicros":100});
            catalog["contracts"].as_array_mut().unwrap().push(unpriced);
            let state = state_with_catalogue(catalog.to_string());
            let roster = vec![
                serde_json::from_value(json!({"name":"researcher",
                "blueprint":{"model":{"provider":"azure-foundry","deployment":"role-model"}}}))
                .unwrap(),
            ];
            let result =
                check_with_roster(state.cluster().unwrap(), &bp, Some(&budget), Some(&roster))
                    .await
                    .unwrap();
            assert_eq!(result.status, CheckStatus::Fail);
            assert!(
                result.detail.contains("Role \"researcher\"")
                    && result.detail.contains("maximum prices")
            );
        }
    }

    #[test]
    fn connected_copilot_needs_spending_contract_only_for_finite_limits() {
        for (tokens, money, allowed) in [
            (None, false, true),
            (Some(1000), false, false),
            (None, true, false),
        ] {
            assert_eq!(
                coverage(
                    &catalogue().to_string(),
                    &["gpt-6.1-sol".into()],
                    money,
                    1,
                    tokens
                )
                .is_ok(),
                allowed
            );
        }
    }

    #[test]
    fn existing_model_coverage_does_not_imply_price_coverage() {
        let raw = catalogue().to_string();
        assert!(coverage(&raw, &["gpt-5.4-mini".into()], false, 1, None).is_ok());
        assert!(coverage(&raw, &["gpt-5.4-mini".into()], true, 1, None).is_err());
    }

    #[test]
    fn missing_fallback_expired_and_malformed_contracts_fail() {
        assert!(
            coverage(
                &catalogue().to_string(),
                &["gpt-5.4-mini".into(), "gpt-6.1-sol".into()],
                false,
                1,
                Some(1000)
            )
            .is_err()
        );
        assert!(
            coverage(
                &catalogue().to_string(),
                &["gpt-5.4-mini".into()],
                false,
                2_000_000_000,
                Some(1000)
            )
            .is_err()
        );
        assert!(coverage("{", &["gpt-5.4-mini".into()], false, 1, None).is_err());
        let mut invalid = catalogue();
        invalid["contracts"][0]["maximumInputTokens"] = json!(0);
        assert!(
            coverage(
                &invalid.to_string(),
                &["gpt-5.4-mini".into()],
                false,
                1,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn impossible_reservation_is_rejected_without_guessing_prompt_tokens() {
        let mut catalog = catalogue();
        catalog["contracts"][0]["maximumInputTokens"] = json!(272_000);
        let raw = catalog.to_string();
        let models = ["gpt-5.4-mini".into()];
        for cap in [150_000, 272_000] {
            let error = coverage(&raw, &models, false, 1, Some(cap)).unwrap_err();
            assert!(error.contains("272001") && error.contains("existing lifetime budget"));
        }
        for cap in [272_001, 600_000] {
            assert!(coverage(&raw, &models, false, 1, Some(cap)).is_ok());
        }
        assert!(coverage(&raw, &models, false, 1, None).is_ok());
    }

    #[tokio::test]
    async fn unreadable_absent_and_empty_catalogues_fail_explicitly() {
        use axum::{
            body::Body,
            http::{Request, Response},
        };
        use tower::service_fn;
        for status in [403, 404, 200] {
            let service = service_fn(move |request: Request<kube::client::Body>| async move {
                assert_eq!(request.method(), "GET");
                assert_eq!(
                    request.uri().path(),
                    "/api/v1/namespaces/kars-system/configmaps/kars-inference-budget-contracts"
                );
                let body = if status == 200 {
                    json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{},"data":{}})
                } else {
                    json!({"apiVersion":"v1","kind":"Status","status":"Failure",
                        "reason": if status == 403 { "Forbidden" } else { "NotFound" },
                        "code":status,"message":"test read failure"})
                };
                Ok::<_, std::io::Error>(
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
            });
            let state =
                crate::state::AppState::for_test_client(kube::Client::new(service, "work"), "work");
            for tokens in [None, Some(40_000)] {
                let budget = TaskBudget {
                    tokens,
                    ..scope_only_budget()
                };
                let result = check(
                    state.cluster().unwrap(),
                    &BlueprintDto::default(),
                    Some(&budget),
                )
                .await
                .unwrap();
                assert_eq!(result.status, CheckStatus::Fail);
                let expected = match status {
                    403 => "Cannot read",
                    404 => "absent",
                    _ => "contracts.json",
                };
                assert!(result.detail.contains(expected), "{}", result.detail);
            }
            assert!(
                check(state.cluster().unwrap(), &BlueprintDto::default(), None)
                    .await
                    .is_none()
            );
        }
    }
}
