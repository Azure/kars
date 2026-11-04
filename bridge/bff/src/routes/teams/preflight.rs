// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::error::{AppError, AppResult};
use crate::kars::team::TeamRole;
use crate::routes::options::Options;
use crate::routes::tasks::{BlueprintDto, ExecutionPlanDto, ModelDto};

use super::{CreateRole, TeamModelRoutes, validation};

pub(super) fn validate_roster_plan(roles: &[CreateRole], plan: &ExecutionPlanDto) -> AppResult<()> {
    validation::reject_reserved_role_names("", roles)?;
    let names = roles
        .iter()
        .map(|role| role.name.trim())
        .collect::<std::collections::BTreeSet<_>>();
    let planned = plan
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if names.contains("")
        || names.len() != roles.len()
        || planned.len() != plan.roles.len()
        || names != planned
    {
        return Err(AppError::BadRequest(
            "execution_plan role names must exactly match the team roster, without blank or duplicate roles".into(),
        ));
    }
    Ok(())
}

/// Resolve the same defaults and complete roster overrides as Team creation.
/// This is a read-only projection, not an alternate Team admission path.
pub(crate) fn prepare(
    options: &Options,
    namespace: &str,
    blueprint: &mut BlueprintDto,
    roles: &mut [CreateRole],
    total_tokens: Option<i64>,
    qualification: bool,
) -> AppResult<Vec<TeamRole>> {
    let plan = blueprint.execution_plan.as_ref().ok_or_else(|| {
        AppError::BadRequest("a typed execution_plan is required for Team preflight".into())
    })?;
    crate::routes::compose::validate_execution_plan_for_launch(plan)
        .map_err(AppError::BadRequest)?;
    validate_roster_plan(roles, plan)?;
    validation::normalize_autonomous_runtime(&mut blueprint.runtime);
    for role in roles.iter_mut() {
        role.name = role.name.trim().to_string();
        validation::normalize_role_runtime(&mut role.runtime);
        role.model = role
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    if blueprint.model.is_none() {
        blueprint.model = options
            .models
            .iter()
            .find(|model| model.is_default)
            .or_else(|| options.models.first())
            .map(|model| ModelDto {
                provider: model.provider.clone(),
                deployment: model.deployment.clone(),
            });
    }
    let model = blueprint
        .model
        .as_ref()
        .map(|model| format!("{}::{}", model.provider, model.deployment));
    let fallbacks = validation::normalize_model_fallback_routes(
        &blueprint
            .model_fallbacks
            .iter()
            .map(|model| format!("{}::{}", model.provider, model.deployment))
            .collect::<Vec<_>>(),
        model.as_deref(),
    )?;
    blueprint.model_fallbacks = fallbacks
        .iter()
        .map(|route| {
            let (provider, deployment) = route.split_once("::").expect("normalized route");
            ModelDto {
                provider: provider.into(),
                deployment: deployment.into(),
            }
        })
        .collect();
    validation::validate_team_model_routes_for_attempt(
        options,
        TeamModelRoutes {
            namespace,
            total_tokens,
            runtime: blueprint.runtime.as_deref(),
            model: model.as_deref(),
            model_fallbacks: &fallbacks,
            roles,
            execution_plan: plan,
            mcp_servers: &blueprint.mcp_servers,
            memory: blueprint.memory.as_deref(),
        },
        qualification,
    )?;
    blueprint
        .skills
        .extend(roles.iter().flat_map(|role| role.skills.iter().cloned()));
    blueprint.skills.sort();
    blueprint.skills.dedup();
    let parent = blueprint.clone().into_crd();
    validation::build_roster(roles, blueprint.tool_policy.as_deref(), Some(&parent))?
        .into_iter()
        .map(|role| {
            serde_json::from_value(role)
                .map_err(|error| AppError::BadRequest(format!("invalid Team roster: {error}")))
        })
        .collect()
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
