use axum::Json;
use axum::extract::{Extension, State};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::options::build_options;
use crate::state::AppState;

use super::client::orchestrator_complete;
use super::execution::apply_weighted_role_budget_floors;
use super::mission_proposal::parse_and_validate;
use super::prompts::build_system_prompt;
use super::routing::select_orchestrator_route;
use super::{ComposeModel, ComposeProposal, ComposeRequest, ComposeResponse};

const MISSION_COMPOSE_MAX_TOKENS: u32 = 4_096;

fn mission_proposal_blueprint(
    proposal: &ComposeProposal,
) -> Result<
    (
        crate::routes::tasks::BlueprintDto,
        &ComposeModel,
        std::collections::BTreeSet<String>,
        i32,
    ),
    String,
> {
    let model = proposal
        .model
        .as_ref()
        .ok_or_else(|| "the proposal has no model route".to_string())?;
    let blueprint = crate::routes::tasks::BlueprintDto {
        runtime: Some(proposal.runtime.clone()),
        model: Some(crate::routes::tasks::ModelDto {
            provider: model.provider.clone(),
            deployment: model.deployment.clone(),
        }),
        model_fallbacks: proposal
            .model_fallbacks
            .iter()
            .map(|model| crate::routes::tasks::ModelDto {
                provider: model.provider.clone(),
                deployment: model.deployment.clone(),
            })
            .collect(),
        instructions: Some(proposal.instructions.clone()),
        tool_policy: proposal.tool_policy.clone(),
        mcp_servers: proposal.mcp_servers.clone(),
        egress: proposal
            .egress
            .iter()
            .map(|endpoint| crate::routes::tasks::EgressDto {
                host: endpoint.host.clone(),
                port: endpoint.port.map(i32::from),
            })
            .collect(),
        egress_mode: Some("strict".into()),
        isolation: Some(proposal.isolation.clone()),
        memory: proposal.memory.clone(),
        skills: proposal.skills.clone(),
        execution_plan: proposal.execution_plan.clone(),
    };
    let (required, max_parallel) =
        crate::routes::validate::qualification_requirements(&blueprint, None);
    Ok((blueprint, model, required, max_parallel))
}

fn apply_mission_budget_floor(proposal: &mut ComposeProposal) -> Result<Option<i64>, String> {
    let (_, model, required, max_parallel) = mission_proposal_blueprint(proposal)?;
    let minimum = crate::routes::options::route_minimum_tokens(
        &proposal.runtime,
        &model.provider,
        &model.deployment,
        &required,
        max_parallel,
    )?;
    let Some(minimum) = minimum else {
        return Ok(None);
    };

    let mut changed = false;
    let mut required_total = minimum;
    if let Some(plan) = proposal.execution_plan.as_mut()
        && !plan.roles.is_empty()
    {
        let (role_budgets_changed, role_budget_total) =
            apply_weighted_role_budget_floors(plan, minimum.max(plan.roles.len() as i64));
        changed |= role_budgets_changed;
        required_total = required_total.max(role_budget_total);
    }
    if proposal
        .budget_tokens
        .is_none_or(|current| current < required_total)
    {
        proposal.budget_tokens = Some(required_total);
        changed = true;
    }
    Ok(changed.then_some(required_total))
}

fn mission_proposal_qualification(proposal: &ComposeProposal) -> Result<(), String> {
    let (_, model, required, max_parallel) = mission_proposal_blueprint(proposal)?;
    let qualified = crate::routes::options::route_qualification(
        &proposal.runtime,
        &model.provider,
        &model.deployment,
        &required,
        max_parallel,
        proposal.budget_tokens,
    )?;
    if qualified {
        return Ok(());
    }
    let missing = crate::routes::options::route_qualification_gap(
        &proposal.runtime,
        &model.provider,
        &model.deployment,
        &required,
        max_parallel,
        proposal.budget_tokens,
    )?;
    Err(format!(
        "{} · {}::{} lacks retained qualification for [{}] at max_parallel={max_parallel}",
        proposal.runtime,
        model.provider,
        model.deployment,
        missing.into_iter().collect::<Vec<_>>().join(", "),
    ))
}

pub(super) fn option_named<'a>(
    options: &'a [crate::routes::options::RefOption],
    name: &str,
) -> Option<&'a crate::routes::options::RefOption> {
    options.iter().find(|option| option.name == name)
}

fn mission_resource_qualification(
    proposal: &ComposeProposal,
    options: &crate::routes::options::Options,
) -> Result<(), String> {
    let (_, model, _, _) = mission_proposal_blueprint(proposal)?;
    let route =
        crate::routes::options::route_label(&proposal.runtime, &model.provider, &model.deployment);
    for server in &proposal.mcp_servers {
        let option = option_named(&options.mcp_servers, server)
            .ok_or_else(|| format!("MCP server `{server}` is not in the live options catalogue"))?;
        if !crate::routes::options::mcp_server_qualified_for_route(
            &proposal.runtime,
            &model.provider,
            &model.deployment,
            option,
        )? {
            return Err(format!(
                "MCP server `{server}` lacks retained resource qualification for {route} at current schema {}. Generic route records do not prove this server.",
                option.tool_schema_digest.as_deref().unwrap_or("missing"),
            ));
        }
    }
    if let Some(memory) = proposal.memory.as_deref() {
        let option = option_named(&options.memories, memory)
            .ok_or_else(|| format!("memory `{memory}` is not in the live options catalogue"))?;
        if !crate::routes::options::memory_binding_qualified_for_route(
            &proposal.runtime,
            &model.provider,
            &model.deployment,
            option,
        )? {
            return Err(format!(
                "Memory `{memory}` lacks retained resource qualification for {route} at backend {} / compiled digest {}. Generic route records do not prove this binding.",
                option.backend.as_deref().unwrap_or("missing"),
                option.compiled_digest.as_deref().unwrap_or("missing"),
            ));
        }
    }
    for skill in &proposal.skills {
        let option = option_named(&options.skills, skill)
            .ok_or_else(|| format!("skill `{skill}` is not in the approved live catalogue"))?;
        if !crate::routes::options::skill_version_qualified_for_route(
            &proposal.runtime,
            &model.provider,
            &model.deployment,
            option,
        )? {
            return Err(format!(
                "Skill `{skill}` lacks retained resource qualification for {route} at current version digest {}. Generic route records do not prove this approved version.",
                option.version_digest.as_deref().unwrap_or("missing"),
            ));
        }
    }
    Ok(())
}

fn mission_proposal_launchability(
    proposal: &ComposeProposal,
    options: &crate::routes::options::Options,
) -> Result<(), String> {
    mission_proposal_qualification(proposal)?;
    mission_resource_qualification(proposal, options)
}

/// `POST /api/namespaces/:ns/compose` — orchestrate a launch package from an
/// objective. The `ns` is accepted for symmetry with the other task routes but
/// composition reads cluster-wide building blocks.
pub async fn compose(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<ComposeRequest>,
) -> AppResult<Json<ComposeResponse>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;

    let objective = req.objective.trim();
    if objective.is_empty() {
        return Err(AppError::BadRequest("objective is required".into()));
    }

    let options = build_options(cluster).await?;
    let efficiency = crate::routes::efficiency::compute_efficiency_for_owner(
        cluster,
        Some(principal.sub.as_str()),
    )
    .await;
    let orchestrator_route = select_orchestrator_route(&options, &efficiency);

    let qualification_constraints = crate::routes::options::qualification_constraints_summary()
        .unwrap_or_else(|error| format!("  (qualification records unavailable: {error})"));
    let resource_qualification_constraints =
        crate::routes::options::resource_qualification_summary(&options).unwrap_or_else(|error| {
            format!("  (resource qualification records unavailable: {error})")
        });
    let system = build_system_prompt(
        &options,
        &efficiency,
        &qualification_constraints,
        &resource_qualification_constraints,
    );
    let user = format!(
        "Objective:\n{objective}\n\nCompose the launch package now. Respond with ONLY the JSON object."
    );

    // Resolve the model used to CALL the orchestrator. An explicit
    // BRIDGE_ORCHESTRATOR_* env triple overrides (and carries its own model), so
    // it makes `default_model` irrelevant. Otherwise we need a real cluster
    // model — never a fabricated one, which would fail opaquely on a non-Anthropic
    // cluster. When there is neither, say so honestly instead of guessing.
    let env_orchestrator = std::env::var("BRIDGE_ORCHESTRATOR_ENDPOINT")
        .is_ok_and(|v| !v.trim().is_empty())
        && std::env::var("BRIDGE_ORCHESTRATOR_TOKEN").is_ok_and(|v| !v.trim().is_empty())
        && std::env::var("BRIDGE_ORCHESTRATOR_MODEL").is_ok_and(|v| !v.trim().is_empty());
    let pinned_orchestrator_model = cluster.bridge_orchestrator_model().await;
    let resolved_model = orchestrator_route
        .as_ref()
        .map(|(_, deployment, _)| deployment.clone())
        .or(pinned_orchestrator_model)
        .or_else(|| {
            options
                .models
                .iter()
                .find(|m| m.is_default)
                .or_else(|| options.models.first())
                .map(|m| m.deployment.clone())
        });
    if resolved_model.is_none() && !env_orchestrator {
        return Ok(Json(ComposeResponse {
            available: false,
            reason: Some(
                "No inference models are configured on this cluster, so the AI composer can't run. Add an inference provider in the Operator Console, or compose the package manually below.".into(),
            ),
            proposal: None,
            rationale: None,
            source: None,
        }));
    }
    let default_model = resolved_model.unwrap_or_default();
    if !env_orchestrator
        && let Some((provider, deployment, _)) = &orchestrator_route
        && let Err(error) = cluster
            .configure_bridge_orchestrator_model(provider, deployment)
            .await
    {
        return Ok(Json(ComposeResponse {
            available: false,
            reason: Some(format!(
                "The best orchestrator route ({provider}/{deployment}) could not be configured: {error}"
            )),
            proposal: None,
            rationale: None,
            source: None,
        }));
    }

    let (raw, mut source) = match orchestrator_complete(
        cluster,
        &system,
        &user,
        &default_model,
        MISSION_COMPOSE_MAX_TOKENS,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            // A reachable-but-failing orchestrator is reported honestly, not
            // papered over with a fabricated package.
            return Ok(Json(ComposeResponse {
                available: false,
                reason: Some(format!(
                    "The orchestrator could not compose a package ({e}). Compose it manually below — every field is the same one the orchestrator would propose."
                )),
                proposal: None,
                rationale: None,
                source: None,
            }));
        }
    };

    let (mut proposal, mut rationale) = parse_and_validate(&raw, &options, &efficiency, objective);
    if let Ok(Some(minimum)) = apply_mission_budget_floor(&mut proposal) {
        let note = format!(
            "The token budget was raised to the retained qualification floor of {minimum} tokens."
        );
        rationale = Some(match rationale {
            Some(existing) if !existing.trim().is_empty() => format!("{existing} {note}"),
            _ => note,
        });
    }
    if let Err(error) = mission_proposal_launchability(&proposal, &options) {
        let repair_user = format!(
            "{user}\n\nYour previous proposal was not launchable: {error}\n\
             Recompose it so the complete runtime/model/capability/max_parallel requirement fits \
             ONE qualified execution record below. Qualification records do not compose. If a \
             selected MCP server, memory binding, or approved skill is used, it MUST have a \
             retained resource-scoped qualification record at the CURRENT digest on the chosen \
             route — generic route records do not count. If a requested binary or file-writing \
             deliverable needs an unqualified capability, choose a launchable text/JSON \
             alternative and represent diagrams inline with quoted Mermaid flowchart labels \
             whenever they contain parser-sensitive punctuation.\n\n\
             QUALIFIED EXECUTION RECORDS:\n{qualification_constraints}\n\n\
             RESOURCE QUALIFICATION RECORDS:\n{resource_qualification_constraints}\n\n\
             Return ONLY the complete JSON object."
        );
        if let Ok((repair_raw, repair_source)) = orchestrator_complete(
            cluster,
            &system,
            &repair_user,
            &default_model,
            MISSION_COMPOSE_MAX_TOKENS,
        )
        .await
        {
            (proposal, rationale) =
                parse_and_validate(&repair_raw, &options, &efficiency, objective);
            let _ = apply_mission_budget_floor(&mut proposal);
            source = repair_source;
        }
    }
    if let Err(error) = mission_proposal_launchability(&proposal, &options) {
        return Ok(Json(ComposeResponse {
            available: false,
            reason: Some(format!(
                "The orchestrator could not produce a launchable package after retry: {error}"
            )),
            proposal: None,
            rationale: None,
            source: Some(source),
        }));
    }

    proposal.model_fallbacks = qualified_mission_fallbacks(&proposal, &options);
    Ok(Json(ComposeResponse {
        available: true,
        reason: None,
        proposal: Some(proposal),
        rationale,
        source: Some(source),
    }))
}

fn qualified_mission_fallbacks(
    proposal: &ComposeProposal,
    options: &crate::routes::options::Options,
) -> Vec<ComposeModel> {
    let primary = proposal
        .model
        .as_ref()
        .map(|model| format!("{}::{}", model.provider, model.deployment));
    let mut candidates = options
        .models
        .iter()
        .map(|model| (model.provider.clone(), model.deployment.clone()))
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .filter(|(provider, deployment)| {
            primary.as_deref() != Some(format!("{provider}::{deployment}").as_str())
        })
        .filter_map(|(provider, deployment)| {
            let mut trial = proposal.clone();
            trial.model = Some(ComposeModel {
                provider: provider.clone(),
                deployment: deployment.clone(),
            });
            trial.model_fallbacks.clear();
            mission_proposal_launchability(&trial, options)
                .is_ok()
                .then_some(ComposeModel {
                    provider,
                    deployment,
                })
        })
        .take(8)
        .collect()
}
