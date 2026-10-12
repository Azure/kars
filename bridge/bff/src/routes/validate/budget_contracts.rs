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
fn coverage(
    raw: &str,
    models: &[String],
    money_required: bool,
    now: i64,
    tokens: Option<i64>,
) -> Result<(), String> {
    let catalog: Catalog = serde_json::from_str(raw).map_err(|_| {
        "The operator inference-budget contract catalogue is malformed.".to_string()
    })?;
    catalog
        .validate(now)
        .map_err(|_| "The operator inference-budget contract catalogue is invalid.".to_string())?;
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
    let budget = budget.filter(|budget| {
        matches!(budget.scope, Some(BudgetScope::GovernedInference))
            && (budget.tokens.is_some_and(|cap| cap > 0)
                || budget.usd_micros.is_some_and(|cap| cap > 0))
    })?;
    let result = async {
        let data = cluster
            .configmap_data_result("kars-inference-budget-contracts")
            .await
            .map_err(|error| {
                tracing::warn!(%error, "Budget preflight could not read operator contracts");
                "Cannot read the operator inference-budget contract catalogue. Retry after restoring access."
                    .to_string()
            })?
            .ok_or_else(|| "The operator inference-budget contract catalogue is absent.".to_string())?;
        let raw = data
            .get("contracts.json")
            .ok_or_else(|| "The operator budget catalogue has no contracts.json.".to_string())?;
        let mut models = match blueprint.model.as_ref() {
            Some(model) => vec![model.deployment.clone()],
            None => vec![cluster.controller_models().await.0.ok_or_else(|| {
                "Cannot determine the inherited model for budget preflight.".to_string()
            })?],
        };
        models.extend(blueprint.model_fallbacks.iter().map(|model| model.deployment.clone()));
        coverage(
            raw,
            &models,
            budget.usd_micros.is_some_and(|cap| cap > 0),
            chrono::Utc::now().timestamp(),
            budget.tokens,
        )?;
        if let Some(plan) = &blueprint.execution_plan {
            for role in &plan.roles {
                coverage(
                    raw, &models, budget.usd_micros.is_some_and(|cap| cap > 0),
                    chrono::Utc::now().timestamp(),
                    plan.role_qualification_budget(&role.name, budget.tokens),
                ).map_err(|detail| format!("Role {:?}: {detail}", role.name))?;
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
            "Current contracts name the requested models and no declared cap is below \
             their minimum possible reservation. This is not proof that their \
             exact provider, endpoint and operation match, or that the broker, account, \
             credentials and reservation capacity are ready. The router still verifies \
             those at execution and fails closed when unavailable."
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

    #[test]
    fn connected_copilot_without_budget_contract_is_not_launchable() {
        assert!(
            coverage(
                &catalogue().to_string(),
                &["gpt-6.1-sol".into()],
                false,
                1,
                None
            )
            .is_err()
        );
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
                None
            )
            .is_err()
        );
        assert!(
            coverage(
                &catalogue().to_string(),
                &["gpt-5.4-mini".into()],
                false,
                2_000_000_000,
                None
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
            let budget = TaskBudget {
                scope: Some(BudgetScope::GovernedInference),
                tokens: Some(40_000),
                usd_micros: None,
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
            assert!(
                check(state.cluster().unwrap(), &BlueprintDto::default(), None)
                    .await
                    .is_none()
            );
        }
    }
}
