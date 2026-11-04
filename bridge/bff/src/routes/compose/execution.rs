// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::client::extract_json;
use super::{ComposeDelegation, ComposeDelegationRole};

fn valid_delegation_role(value: &serde_json::Value) -> Option<ComposeDelegationRole> {
    let name = value.get("name")?.as_str()?.trim().to_ascii_lowercase();
    let objective = value.get("objective")?.as_str()?.trim().to_string();
    if name.is_empty()
        || name.len() > 48
        || objective.len() < 20
        || objective.len() > 600
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return None;
    }
    Some(ComposeDelegationRole { name, objective })
}

pub(super) fn single_agent_delegation() -> ComposeDelegation {
    ComposeDelegation {
        mode: "single-agent".into(),
        roles: Vec::new(),
        max_parallel: 1,
    }
}

pub(super) fn delegation_from_execution_plan(
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> ComposeDelegation {
    if plan.roles.is_empty() {
        return single_agent_delegation();
    }
    ComposeDelegation {
        mode: "principal-specialists".into(),
        roles: plan
            .roles
            .iter()
            .map(|role| ComposeDelegationRole {
                name: role.name.clone(),
                objective: role.objective.clone(),
            })
            .collect(),
        max_parallel: plan.max_parallel,
    }
}

pub(crate) fn validate_execution_plan(
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> Result<(), String> {
    crate::kars::task::execution_plan::validate_execution_plan(&plan.clone().into_crd())
}

pub(crate) fn validate_execution_plan_for_launch(
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> Result<(), String> {
    crate::kars::task::execution_plan::validate_activation(
        Some(&plan.clone().into_crd()),
        None,
        true,
    )
}

pub(super) fn repair_feedback(
    review_error: Option<String>,
    plan: Option<&crate::routes::tasks::ExecutionPlanDto>,
) -> Option<String> {
    let activation_error = plan.and_then(|plan| validate_execution_plan_for_launch(plan).err());
    let errors = review_error
        .into_iter()
        .chain(activation_error)
        .collect::<Vec<_>>();
    (!errors.is_empty()).then(|| errors.join("\n"))
}

fn parse_execution_plan_result(
    json: &serde_json::Value,
) -> Result<crate::routes::tasks::ExecutionPlanDto, String> {
    super::budgets::validate_proposed_caps(json)?;
    let value = json
        .get("execution_plan")
        .ok_or_else(|| "execution_plan is missing".to_string())?;
    let plan = serde_json::from_value(value.clone())
        .map_err(|error| format!("execution_plan does not match the required schema: {error}"))?;
    validate_execution_plan(&plan)?;
    Ok(plan)
}

pub(super) fn parse_execution_plan(
    json: &serde_json::Value,
) -> Option<crate::routes::tasks::ExecutionPlanDto> {
    parse_execution_plan_result(json).ok()
}

pub(super) fn execution_plan_error_from_raw(raw: &str) -> Option<String> {
    let json =
        extract_json(raw).ok_or_else(|| "response did not contain a JSON object".to_string());
    match json {
        Ok(json) => parse_execution_plan_result(&json).err(),
        Err(error) => Some(error),
    }
}

pub(crate) fn validate_delegation(delegation: &ComposeDelegation) -> Result<(), String> {
    match delegation.mode.as_str() {
        "single-agent" if delegation.roles.is_empty() && delegation.max_parallel == 1 => Ok(()),
        "principal-specialists"
            if (2..=4).contains(&delegation.roles.len())
                && delegation.max_parallel >= 1
                && delegation.max_parallel <= delegation.roles.len() as i32
                && delegation.roles.iter().all(|role| {
                    let value = serde_json::json!({
                        "name": role.name,
                        "objective": role.objective,
                    });
                    valid_delegation_role(&value).is_some()
                }) =>
        {
            let unique = delegation
                .roles
                .iter()
                .map(|role| role.name.as_str())
                .collect::<std::collections::HashSet<_>>();
            (unique.len() == delegation.roles.len())
                .then_some(())
                .ok_or_else(|| "delegation role names must be unique".to_string())
        }
        "single-agent" => {
            Err("single-agent delegation must have no roles and max_parallel=1".into())
        }
        "principal-specialists" => Err(
            "principal-specialists delegation requires 2–4 unique valid leaf roles and a bounded max_parallel"
                .into(),
        ),
        _ => Err("delegation mode must be single-agent or principal-specialists".into()),
    }
}

pub(crate) fn delegation_budget_allocation(
    total_tokens: i64,
    role_count: usize,
) -> Result<(i64, i64), String> {
    if total_tokens <= 0 || total_tokens < role_count as i64 {
        return Err(
            "execution plans require a positive total budget with capacity for every role".into(),
        );
    }
    Ok((
        total_tokens,
        if role_count == 0 {
            0
        } else {
            total_tokens / role_count as i64
        },
    ))
}
