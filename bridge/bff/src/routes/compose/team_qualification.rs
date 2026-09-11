use super::mission::option_named;
use super::{ComposeTeamProposal, is_non_autonomous_harness};

pub(super) fn default_model_route(options: &crate::routes::options::Options) -> Option<String> {
    options
        .models
        .iter()
        .find(|model| model.is_default)
        .or_else(|| options.models.first())
        .map(|model| format!("{}::{}", model.provider, model.deployment))
}

fn team_role_qualification_requirements(
    plan: &crate::routes::tasks::ExecutionPlanDto,
    role_name: &str,
) -> std::collections::BTreeSet<String> {
    let mut required =
        std::collections::BTreeSet::from(["team".to_string(), "telemetry".to_string()]);
    if let Some(role) = plan.roles.iter().find(|role| role.name == role_name) {
        for phase in &role.phases {
            required.extend(phase.capabilities.iter().cloned());
        }
    }
    required
}

fn qualify_role_resource(
    runtime: &str,
    provider: &str,
    deployment: &str,
    route: &str,
    label: &str,
    qualified: Result<bool, String>,
    detail: impl FnOnce() -> String,
) -> Result<(), String> {
    match qualified {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "{label} lacks retained resource qualification for {route}. {}",
            detail()
        )),
        Err(error) => Err(format!(
            "{label} could not be matched against qualification records for {runtime} · {provider}::{deployment}: {error}"
        )),
    }
}

fn team_proposal_qualification(
    proposal: &ComposeTeamProposal,
    options: &crate::routes::options::Options,
) -> Result<(), String> {
    let principal_runtime = "OpenClaw";
    let principal_route = proposal
        .model
        .split_once("::")
        .map(|(provider, deployment)| (provider.to_string(), deployment.to_string()))
        .or_else(|| {
            default_model_route(options).and_then(|route| {
                route
                    .split_once("::")
                    .map(|(provider, deployment)| (provider.to_string(), deployment.to_string()))
            })
        })
        .ok_or_else(|| "the team proposal has no launchable principal model route".to_string())?;
    let principal_blueprint = crate::routes::tasks::BlueprintDto {
        runtime: Some(principal_runtime.to_string()),
        model: Some(crate::routes::tasks::ModelDto {
            provider: principal_route.0.clone(),
            deployment: principal_route.1.clone(),
        }),
        model_fallbacks: Vec::new(),
        instructions: Some(proposal.instructions.clone()),
        tool_policy: None,
        mcp_servers: proposal.mcp_servers.clone(),
        egress: proposal
            .egress
            .iter()
            .map(|entry| crate::routes::tasks::EgressDto {
                host: entry.host.clone(),
                port: entry.port.map(i32::from),
            })
            .collect(),
        egress_mode: Some(proposal.egress_mode.clone()),
        isolation: None,
        memory: proposal.memory.clone(),
        skills: proposal
            .roles
            .iter()
            .flat_map(|role| role.skills.iter().cloned())
            .collect(),
        execution_plan: proposal.execution_plan.clone(),
    };
    let (required, max_parallel) =
        crate::routes::validate::qualification_requirements(&principal_blueprint, Some("team"));
    if !crate::routes::options::route_qualification(
        principal_runtime,
        &principal_route.0,
        &principal_route.1,
        &required,
        max_parallel,
        None,
    )? {
        let missing = crate::routes::options::route_qualification_gap(
            principal_runtime,
            &principal_route.0,
            &principal_route.1,
            &required,
            max_parallel,
            None,
        )?;
        return Err(format!(
            "{} lacks retained qualification for [{}] at max_parallel={max_parallel}",
            crate::routes::options::route_label(
                principal_runtime,
                &principal_route.0,
                &principal_route.1
            ),
            missing.into_iter().collect::<Vec<_>>().join(", "),
        ));
    }
    let principal_route_label = crate::routes::options::route_label(
        principal_runtime,
        &principal_route.0,
        &principal_route.1,
    );
    for server in &proposal.mcp_servers {
        let option = option_named(&options.mcp_servers, server)
            .ok_or_else(|| format!("MCP server `{server}` is not in the live options catalogue"))?;
        qualify_role_resource(
            principal_runtime,
            &principal_route.0,
            &principal_route.1,
            &principal_route_label,
            &format!("MCP server `{server}`"),
            crate::routes::options::mcp_server_qualified_for_route(
                principal_runtime,
                &principal_route.0,
                &principal_route.1,
                option,
            ),
            || {
                format!(
                    "Current schema digest: {}. Generic route records do not prove this server.",
                    option.tool_schema_digest.as_deref().unwrap_or("missing")
                )
            },
        )?;
    }
    if let Some(memory) = proposal.memory.as_deref() {
        let option = option_named(&options.memories, memory)
            .ok_or_else(|| format!("memory `{memory}` is not in the live options catalogue"))?;
        qualify_role_resource(
            principal_runtime,
            &principal_route.0,
            &principal_route.1,
            &principal_route_label,
            &format!("memory `{memory}`"),
            crate::routes::options::memory_binding_qualified_for_route(
                principal_runtime,
                &principal_route.0,
                &principal_route.1,
                option,
            ),
            || {
                format!(
                    "Current backend/digest: {}/{}. Generic route records do not prove this memory binding.",
                    option.backend.as_deref().unwrap_or("missing"),
                    option.compiled_digest.as_deref().unwrap_or("missing")
                )
            },
        )?;
    }
    let Some(plan) = proposal.execution_plan.as_ref() else {
        return Err("the team proposal has no typed execution plan".into());
    };
    let default_role_route = format!("{}::{}", principal_route.0, principal_route.1);
    for role in &proposal.roles {
        let route = if role.model.trim().is_empty() {
            default_role_route.as_str()
        } else {
            role.model.trim()
        };
        let (provider, deployment) = route
            .split_once("::")
            .ok_or_else(|| format!("role {} has no valid model route", role.name))?;
        let runtime = if role.runtime.trim().is_empty() {
            principal_runtime
        } else {
            role.runtime.trim()
        };
        let role_required = team_role_qualification_requirements(plan, &role.name);
        if !crate::routes::options::route_qualification(
            runtime,
            provider,
            deployment,
            &role_required,
            1,
            None,
        )? {
            let missing = crate::routes::options::route_qualification_gap(
                runtime,
                provider,
                deployment,
                &role_required,
                1,
                None,
            )?;
            return Err(format!(
                "role `{}` route {} lacks retained qualification for [{}]",
                role.name,
                crate::routes::options::route_label(runtime, provider, deployment),
                missing.into_iter().collect::<Vec<_>>().join(", "),
            ));
        }
        let role_route_label = crate::routes::options::route_label(runtime, provider, deployment);
        if role_required.contains("mcp") {
            for server in &proposal.mcp_servers {
                let option = option_named(&options.mcp_servers, server).ok_or_else(|| {
                    format!("MCP server `{server}` is not in the live options catalogue")
                })?;
                qualify_role_resource(
                    runtime,
                    provider,
                    deployment,
                    &role_route_label,
                    &format!("role `{}` MCP server `{server}`", role.name),
                    crate::routes::options::mcp_server_qualified_for_route(
                        runtime, provider, deployment, option,
                    ),
                    || {
                        format!(
                            "Current schema digest: {}. Generic route records do not prove this server.",
                            option.tool_schema_digest.as_deref().unwrap_or("missing")
                        )
                    },
                )?;
            }
        }
        if role_required.contains("memory")
            && let Some(memory) = proposal.memory.as_deref()
        {
            let option = option_named(&options.memories, memory)
                .ok_or_else(|| format!("memory `{memory}` is not in the live options catalogue"))?;
            qualify_role_resource(
                runtime,
                provider,
                deployment,
                &role_route_label,
                &format!("role `{}` memory `{memory}`", role.name),
                crate::routes::options::memory_binding_qualified_for_route(
                    runtime, provider, deployment, option,
                ),
                || {
                    format!(
                        "Current backend/digest: {}/{}. Generic route records do not prove this memory binding.",
                        option.backend.as_deref().unwrap_or("missing"),
                        option.compiled_digest.as_deref().unwrap_or("missing")
                    )
                },
            )?;
        }
        for skill in &role.skills {
            let option = option_named(&options.skills, skill)
                .ok_or_else(|| format!("skill `{skill}` is not in the approved live catalogue"))?;
            qualify_role_resource(
                runtime,
                provider,
                deployment,
                &role_route_label,
                &format!("role `{}` skill `{skill}`", role.name),
                crate::routes::options::skill_version_qualified_for_route(
                    runtime, provider, deployment, option,
                ),
                || {
                    format!(
                        "Current version digest: {}. Generic route records do not prove this approved skill version.",
                        option.version_digest.as_deref().unwrap_or("missing")
                    )
                },
            )?;
        }
    }
    Ok(())
}

pub(super) fn normalize_team_proposal_route(
    proposal: &mut ComposeTeamProposal,
    options: &crate::routes::options::Options,
) -> Result<(), String> {
    for role in &mut proposal.roles {
        if is_non_autonomous_harness(&role.runtime) {
            role.runtime = "OpenClaw".into();
        }
    }
    let initial_error = match team_proposal_qualification(proposal, options) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let original_model = proposal.model.clone();
    let original_role_routes = proposal
        .roles
        .iter()
        .map(|role| (role.runtime.clone(), role.model.clone()))
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    if let Some(default) = default_model_route(options) {
        candidates.push(default);
    }

    candidates.extend(
        options
            .models
            .iter()
            .map(|model| format!("{}::{}", model.provider, model.deployment)),
    );
    let mut seen = std::collections::HashSet::new();
    for route in candidates
        .into_iter()
        .filter(|route| seen.insert(route.clone()))
    {
        proposal.model = route.clone();
        for role in &mut proposal.roles {
            role.runtime.clear();
            role.model.clear();
        }
        if team_proposal_qualification(proposal, options).is_ok() {
            proposal.model_basis = Some(format!(
                "Bridge selected {route} because the complete team plan and its reviewed resources match one retained qualification record; the orchestrator's proposed route did not."
            ));
            proposal.expected_tokens_per_outcome = None;
            proposal.efficiency_sample_runs = 0;
            return Ok(());
        }
    }
    proposal.model = original_model;
    for (role, (runtime, model)) in proposal.roles.iter_mut().zip(original_role_routes) {
        role.runtime = runtime;
        role.model = model;
    }
    Err(initial_error)
}

pub(super) fn qualified_team_fallbacks(
    proposal: &ComposeTeamProposal,
    options: &crate::routes::options::Options,
) -> Vec<String> {
    let mut candidates = options
        .models
        .iter()
        .map(|model| format!("{}::{}", model.provider, model.deployment))
        .filter(|route| route != &proposal.model)
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .filter(|route| {
            let mut trial = proposal.clone();
            trial.model = route.clone();
            trial.model_fallbacks.clear();
            for role in &mut trial.roles {
                role.model = route.clone();
            }
            team_proposal_qualification(&trial, options).is_ok()
        })
        .take(8)
        .collect()
}
