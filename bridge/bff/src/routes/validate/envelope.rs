// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — envelope pre-flight checks.

use super::{Check, CheckStatus};
use crate::kars::task::{BudgetScope, TaskBudget};

fn check_budget(budget: Option<&TaskBudget>) -> Check {
    let tokens = budget.and_then(|b| b.tokens);
    let usd_micros = budget.and_then(|b| b.usd_micros);
    let scope = budget.and_then(|b| b.scope);
    let (status, detail) = if tokens.is_some_and(|b| b <= 0) {
        (
            CheckStatus::Fail,
            "The token cap must be a positive number of tokens.",
        )
    } else if usd_micros.is_some_and(|b| b < 0) {
        (CheckStatus::Fail, "The currency cap cannot be negative.")
    } else if tokens.is_some_and(|b| b > 0) || usd_micros.is_some_and(|b| b > 0) {
        if !matches!(scope, Some(BudgetScope::GovernedInference)) {
            (
                CheckStatus::Fail,
                "A positive aggregate launch budget requires explicit GovernedInference scope. A legacy planning budget is not an enforced cap. First enrollment requires a new Task or Team UID; do not remove the cap to bypass this check.",
            )
        } else {
            (
                CheckStatus::Warn,
                "GovernedInference scope is explicit, but this preflight does not verify durable broker readiness, provider contracts, available reservation capacity, or the bound account. Admission, materialization, or dispatch may still reject execution. Limits cover the Task tree or Team UID lifetime, not each run; enforcement is not confirmed here.",
            )
        }
    } else {
        (
            CheckStatus::Warn,
            "No positive aggregate inference cap is set. Autonomy tiers and cluster defaults are not proof of a hard aggregate spending limit.",
        )
    };
    Check {
        id: "budget".into(),
        label: tokens.map_or_else(
            || "Inference budget".into(),
            |b| format!("Inference token budget {b}"),
        ),
        status,
        detail: detail.into(),
    }
}

pub(super) async fn check_envelope(
    cluster: &crate::kars::cluster::Cluster,
    bp: &crate::routes::tasks::BlueprintDto,
    tier: Option<i32>,
    budget: Option<&TaskBudget>,
    checks: &mut Vec<Check>,
) {
    // 0. Envelope sanity — the autonomy tier and budget the operator will launch
    //    with. The UI presents validation as covering the whole package, so the
    //    gate must actually check the envelope, not only the blueprint.
    if let Some(t) = tier {
        checks.push(if (1..=5).contains(&t) {
            Check {
                id: "tier".into(),
                label: format!("Autonomy tier {t}"),
                status: CheckStatus::Pass,
                detail: "Within the valid range (1–5). Sub-roles are capped one tier below.".into(),
            }
        } else {
            Check {
                id: "tier".into(),
                label: "Autonomy tier".into(),
                status: CheckStatus::Fail,
                detail: format!("Tier {t} is out of range — must be 1–5."),
            }
        });
    }
    let runtime = bp.runtime.as_deref().unwrap_or("OpenClaw");
    checks.push(check_budget(budget));
    if let Some(plan) = bp.execution_plan.as_ref() {
        checks.push(match crate::routes::compose::validate_execution_plan(plan) {
            Ok(()) => Check {
                id: "execution_plan".into(),
                label: format!(
                    "Execution plan · {} role{} · up to {} in parallel",
                    plan.roles.len(),
                    if plan.roles.len() == 1 { "" } else { "s" },
                    plan.max_parallel
                ),
                status: CheckStatus::Pass,
                detail:
                    "Role dependencies, phases, capabilities, tool-call bounds, synthesis, and deliverables are valid."
                        .into(),
            },
            Err(error) => Check {
                id: "execution_plan".into(),
                label: "Execution plan is invalid".into(),
                status: CheckStatus::Fail,
                detail: error,
            },
        });
    }

    // 0b. Runtime image and registry credentials. This is a real launch blocker:
    // allowing a configured private image without a matching pull secret produces
    // an immediate ImagePullBackOff before the agent can execute anything.
    let runnable_runtimes = cluster.runnable_runtimes().await;
    checks.push(if runnable_runtimes.contains(runtime) {
        Check {
            id: "runtime".into(),
            label: format!("Runtime “{runtime}” can start on this cluster"),
            status: CheckStatus::Pass,
            detail: "The runtime image is configured and its registry is covered by the controller's pull credentials.".into(),
        }
    } else {
        Check {
            id: "runtime".into(),
            label: format!("Runtime “{runtime}” cannot start on this cluster"),
            status: CheckStatus::Fail,
            detail: "The runtime image is missing or its private registry is not covered by the controller's pull credentials. Launch is blocked to prevent ImagePullBackOff.".into(),
        }
    });
    if runtime != "OpenClaw" && !bp.skills.is_empty() {
        checks.push(Check {
            id: "runtime_skills".into(),
            label: format!("Runtime “{runtime}” cannot mount file skills"),
            status: CheckStatus::Fail,
            detail: "Controller-mounted file skills are currently supported only by OpenClaw; remove them or use OpenClaw instead.".into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::validate::ValidateRequest;
    use serde_json::json;

    #[test]
    fn positive_legacy_caps_fail_instead_of_promising_enforcement() {
        for tokens in [1, 499, 500, 20_000, i64::MAX] {
            let budget: TaskBudget = serde_json::from_value(json!({"tokens": tokens})).unwrap();
            let check = check_budget(Some(&budget));
            assert_eq!(check.status, CheckStatus::Fail);
            assert!(check.detail.contains("GovernedInference"));
            assert!(check.detail.contains("new Task or Team UID"));
        }
    }

    #[test]
    fn currency_only_legacy_cap_also_requires_scope() {
        let budget: TaskBudget = serde_json::from_value(json!({"usdMicros": 100})).unwrap();
        assert_eq!(check_budget(Some(&budget)).status, CheckStatus::Fail);
    }

    #[test]
    fn explicit_scope_never_proves_broker_or_account_readiness() {
        for limits in [
            json!({"tokens": 1}),
            json!({"tokens": 20_000}),
            json!({"usdMicros": 100}),
        ] {
            let mut budget: TaskBudget = serde_json::from_value(limits).unwrap();
            budget.scope = Some(BudgetScope::GovernedInference);
            let check = check_budget(Some(&budget));
            assert_eq!(check.status, CheckStatus::Warn);
            for detail in [
                "broker readiness",
                "provider contracts",
                "reservation capacity",
                "bound account",
                "UID lifetime",
                "enforcement is not confirmed",
            ] {
                assert!(check.detail.contains(detail), "missing {detail}");
            }
        }
    }

    #[test]
    fn invalid_limits_fail_even_with_explicit_scope() {
        for limits in [
            json!({"tokens": -1}),
            json!({"tokens": 0}),
            json!({"usdMicros": -1}),
        ] {
            let mut budget: TaskBudget = serde_json::from_value(limits).unwrap();
            budget.scope = Some(BudgetScope::GovernedInference);
            assert_eq!(check_budget(Some(&budget)).status, CheckStatus::Fail);
        }
    }

    #[test]
    fn missing_limits_do_not_imply_cluster_spending_protection() {
        for value in [
            json!(null),
            json!({}),
            json!({"usdMicros": 0}),
            json!({"scope": "GovernedInference"}),
        ] {
            let budget: Option<TaskBudget> = serde_json::from_value(value).unwrap();
            let check = check_budget(budget.as_ref());
            assert_eq!(check.status, CheckStatus::Warn);
            assert!(
                check
                    .detail
                    .contains("not proof of a hard aggregate spending limit")
            );
        }
    }

    #[test]
    fn package_and_stored_budget_shapes_produce_the_same_check() {
        for scope in [json!(null), json!("GovernedInference")] {
            for (tokens, currency) in [
                (Some(20_000), None),
                (None, Some(100)),
                (Some(20_000), Some(100)),
                (None, None),
            ] {
                let request: ValidateRequest = serde_json::from_value(json!({
                    "budget_scope": scope, "budget_tokens": tokens, "budget_usd_micros": currency
                }))
                .unwrap();
                let stored: TaskBudget = serde_json::from_value(json!({
                    "scope": scope, "tokens": tokens, "usdMicros": currency
                }))
                .unwrap();
                assert_eq!(
                    serde_json::to_value(request.budget()).unwrap(),
                    serde_json::to_value(&stored).unwrap()
                );
                assert_eq!(
                    serde_json::to_value(check_budget(Some(&request.budget()))).unwrap(),
                    serde_json::to_value(check_budget(Some(&stored))).unwrap()
                );
            }
        }
    }

    #[test]
    fn legacy_requests_do_not_acquire_scope_and_unknown_scope_is_rejected() {
        let request: ValidateRequest =
            serde_json::from_value(json!({"budget_tokens": 20_000})).unwrap();
        assert!(request.budget().scope.is_none());
        assert_eq!(
            check_budget(Some(&request.budget())).status,
            CheckStatus::Fail
        );
        assert!(
            serde_json::from_value::<ValidateRequest>(json!({"budget_scope": "PerRun"})).is_err()
        );
    }
}
