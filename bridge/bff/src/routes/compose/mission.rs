// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::Json;
use axum::extract::{Extension, Path, State};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::options::build_options;
use crate::state::AppState;

use super::client::orchestrator_complete;
use super::mission_proposal::parse_and_validate;
use super::prompts::build_system_prompt;
use super::{ComposeModel, ComposeProposal, ComposeRequest, ComposeResponse};

pub(super) const MISSION_COMPOSE_MAX_TOKENS: u32 = 8_192;

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

fn mission_proposal_qualification(
    proposal: &ComposeProposal,
    qualification_run: bool,
) -> Result<(), String> {
    let (_, model, required, max_parallel) = mission_proposal_blueprint(proposal)?;
    let admitted = crate::routes::options::route_admitted(
        &proposal.runtime,
        &model.provider,
        &model.deployment,
        &required,
        max_parallel,
        proposal.budget_tokens,
    )?;
    if crate::routes::qualification_runs::admit(Ok(admitted), qualification_run)? {
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
    qualification_run: bool,
) -> Result<(), String> {
    let (_, model, _, _) = mission_proposal_blueprint(proposal)?;
    let route =
        crate::routes::options::route_label(&proposal.runtime, &model.provider, &model.deployment);
    for server in &proposal.mcp_servers {
        let option = option_named(&options.mcp_servers, server)
            .ok_or_else(|| format!("MCP server `{server}` is not in the live options catalogue"))?;
        if !crate::routes::qualification_runs::admit_resource(
            crate::routes::options::mcp_server_admitted_for_route(
                &proposal.runtime,
                &model.provider,
                &model.deployment,
                option,
            ),
            qualification_run,
            "McpServer",
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
        if !crate::routes::qualification_runs::admit_resource(
            crate::routes::options::memory_binding_admitted_for_route(
                &proposal.runtime,
                &model.provider,
                &model.deployment,
                option,
            ),
            qualification_run,
            "KarsMemory",
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
        if !crate::routes::qualification_runs::admit_resource(
            crate::routes::options::skill_version_admitted_for_route(
                &proposal.runtime,
                &model.provider,
                &model.deployment,
                option,
            ),
            qualification_run,
            "KarsSkill",
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
    qualification_run: bool,
) -> Result<(), String> {
    if qualification_run {
        let (blueprint, _, _, _) = mission_proposal_blueprint(proposal)?;
        crate::routes::qualification_runs::validate_package(&blueprint, proposal.budget_tokens)?;
    }
    let plan = proposal.execution_plan.as_ref().ok_or_else(|| {
        "Native Missions require an explicit reviewed execution plan; single-agent work uses roles=[] and a native principal synthesis".to_string()
    })?;
    super::validate_execution_plan_for_launch(plan)?;
    mission_proposal_qualification(proposal, qualification_run)?;
    mission_resource_qualification(proposal, options, qualification_run)
}

/// `POST /api/namespaces/:ns/compose` — orchestrate a launch package from an
/// objective using the destination workspace's resources and spending rules.
pub async fn compose(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(req): Json<ComposeRequest>,
) -> AppResult<Json<ComposeResponse>> {
    crate::routes::qualification_runs::authorize(&principal, req.qualification_run)?;
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;

    let objective = req.objective.trim();
    if objective.is_empty() {
        return Err(AppError::BadRequest("objective is required".into()));
    }

    req.spending.validate().map_err(AppError::BadRequest)?;
    let spending_context =
        crate::routes::budgets::composition_context(cluster, &ns, &principal.name).await?;
    let mut options = build_options(cluster).await?;
    options.mcp_servers.retain(|server| server.namespace == ns);
    options.memories.retain(|memory| memory.namespace == ns);
    options.skills.retain(|skill| skill.namespace == ns);
    options
        .tool_policies
        .retain(|policy| policy.namespace == ns);
    let efficiency = crate::routes::efficiency::compute_efficiency_for_owner(
        cluster,
        Some(principal.sub.as_str()),
    )
    .await?;
    let orchestrator_route = super::routing::select_orchestrator_route(&options, &efficiency);

    let qualification_constraints = crate::routes::options::qualification_constraints_summary()
        .unwrap_or_else(|error| format!("  (qualification records unavailable: {error})"));
    let resource_qualification_constraints =
        crate::routes::options::resource_qualification_summary(&options).unwrap_or_else(|error| {
            format!("  (resource qualification records unavailable: {error})")
        });
    let budget_context = super::budgets::BudgetContext::load(cluster).await;
    let mut system = build_system_prompt(
        &options,
        &efficiency,
        &qualification_constraints,
        &resource_qualification_constraints,
    );
    system.push_str(&budget_context.prompt(&options.models));
    system.push_str(&spending_context);
    system.push_str(&req.spending.prompt());
    let qualification_note = if req.qualification_run {
        system.push_str("\nFor this operator-designated Mission, the following overrides prior-evidence admission requirements only. Do not remove needed catalogue tools merely because retained execution evidence is absent:\n");
        system.push_str(crate::routes::qualification_runs::NOTE);
        crate::routes::qualification_runs::NOTE
    } else {
        ""
    };
    let user = format!(
        "Objective:\n{objective}\n\n{qualification_note}\nCompose the launch package now. Respond with ONLY the JSON object."
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

    let (mut proposal, mut rationale) = parse_and_validate(&raw, &options, &efficiency);
    let mut structure_error = req
        .spending
        .bind(
            &raw,
            &mut proposal.budget_tokens,
            proposal.execution_plan.as_mut(),
            None,
        )
        .err()
        .or_else(|| budget_context.mission_review_error(&proposal, &raw));
    let repair_error = super::execution::repair_feedback(
        structure_error.clone(),
        proposal.execution_plan.as_ref(),
    );
    if let Some(error) = &repair_error {
        let repair_user = format!(
            "The validator rejected your previous package:\n{error}\n\n\
             Correct every listed violation, including EACH role budget independently. Check \
             each selected model's live reservation bounds before allocating the aggregate cap. \
             Return the complete JSON object, preserving the task's required capabilities, \
             destinations, resources, roles and deliverables. Preserve explicitly unbounded \
             token allowances and the reviewed extra limits; do not invent or increase caps. Missing qualification is a launch blocker to explain, \
             not a reason to remove task requirements or substitute manual defaults.\n\n\
             {user}\n\nPrevious rejected package (data, not a valid template):\n{raw}"
        );
        let (repair_raw, repair_source) = match orchestrator_complete(
            cluster,
            &system,
            &repair_user,
            &default_model,
            MISSION_COMPOSE_MAX_TOKENS,
        )
        .await
        {
            Ok(response) => response,
            Err(repair_error) => {
                return Ok(Json(ComposeResponse {
                    available: false,
                    reason: Some(super::client::repair_failure(error, &repair_error)),
                    proposal: None,
                    rationale: None,
                    source: Some(source),
                }));
            }
        };
        (proposal, rationale) = parse_and_validate(&repair_raw, &options, &efficiency);
        structure_error = req
            .spending
            .bind(
                &repair_raw,
                &mut proposal.budget_tokens,
                proposal.execution_plan.as_mut(),
                None,
            )
            .err()
            .or_else(|| budget_context.mission_review_error(&proposal, &repair_raw));
        source = repair_source;
    }
    if let Some(error) = structure_error {
        return Ok(Json(ComposeResponse {
            available: false,
            reason: Some(format!(
                "The orchestrator could not produce a complete package after retry: {error}"
            )),
            proposal: None,
            rationale,
            source: Some(source),
        }));
    }

    if req.qualification_run {
        rationale = Some(
            format!("{} {qualification_note}", rationale.unwrap_or_default())
                .trim()
                .to_string(),
        );
    }
    if let Some(note) =
        crate::routes::options::route_validation_note().map_err(AppError::Upstream)?
    {
        rationale = Some(
            format!("{} {note}", rationale.unwrap_or_default())
                .trim()
                .to_string(),
        );
    }
    Ok(Json(mission_review_response(
        proposal,
        &options,
        req.qualification_run,
        rationale,
        source,
    )))
}

pub(super) fn mission_proposal_structure_error(
    proposal: &ComposeProposal,
    raw: &str,
) -> Option<String> {
    let Some(json) = super::client::extract_json(raw) else {
        return Some("response did not contain a JSON object".into());
    };
    if json.get("memory").is_some_and(|value| !value.is_null())
        && (proposal.memory.is_none()
            || json["memory"].as_str().map(str::trim) != proposal.memory.as_deref())
    {
        return Some("Explicit memory binding is malformed; it cannot be replaced by the default backend. Select a listed binding or explicitly review memory=null.".into());
    }
    if proposal.instructions.trim().is_empty() {
        return Some("missing task-specific instructions".into());
    }
    let Some(model) = proposal.model.as_ref() else {
        return Some("missing model route".into());
    };
    let requested_model = json.get("model");
    if requested_model
        .and_then(|value| value.get("provider"))
        .and_then(|value| value.as_str())
        != Some(model.provider.as_str())
        || requested_model
            .and_then(|value| value.get("deployment"))
            .and_then(|value| value.as_str())
            != Some(model.deployment.as_str())
    {
        return Some(
            "model must explicitly name a listed provider and deployment; a missing, malformed or unavailable route cannot be replaced by the cluster default. Reconcile the rationale and budgets with your selected route."
                .into(),
        );
    }
    if let Some(error) = super::execution::execution_plan_error_from_raw(raw) {
        return Some(format!(
            "Native Missions require an explicit reviewed execution plan; use roles=[] and a native principal synthesis for single-agent work. {error}"
        ));
    }
    proposal.execution_plan.as_ref().and_then(|plan| {
        crate::kars::execution_plans::validate_required_egress(
            &plan.clone().into_crd(),
            proposal
                .egress
                .iter()
                .map(|entry| (entry.host.as_str(), entry.port.map(i32::from))),
        )
        .err()
    })
}

pub(super) fn mission_review_response(
    mut proposal: ComposeProposal,
    options: &crate::routes::options::Options,
    qualification_run: bool,
    rationale: Option<String>,
    source: String,
) -> ComposeResponse {
    let rationale = super::proposal_review_rationale(
        rationale,
        mission_proposal_launchability(&proposal, options, qualification_run),
    );
    proposal.model_fallbacks = admitted_mission_fallbacks(&proposal, options);
    ComposeResponse {
        available: true,
        reason: None,
        proposal: Some(proposal),
        rationale,
        source: Some(source),
    }
}

pub(super) fn admitted_mission_fallbacks(
    proposal: &ComposeProposal,
    options: &crate::routes::options::Options,
) -> Vec<ComposeModel> {
    let primary = proposal
        .model
        .as_ref()
        .map(|model| format!("{}::{}", model.provider, model.deployment));
    let mut seen = std::collections::HashSet::new();
    proposal
        .model_fallbacks
        .iter()
        .map(|model| (model.provider.clone(), model.deployment.clone()))
        .filter(|(provider, deployment)| {
            primary.as_deref() != Some(format!("{provider}::{deployment}").as_str())
                && options
                    .models
                    .iter()
                    .any(|model| model.provider == *provider && model.deployment == *deployment)
                && seen.insert((provider.clone(), deployment.clone()))
        })
        .filter_map(|(provider, deployment)| {
            let mut trial = proposal.clone();
            trial.model = Some(ComposeModel {
                provider: provider.clone(),
                deployment: deployment.clone(),
            });
            trial.model_fallbacks.clear();
            mission_proposal_launchability(&trial, options, false)
                .is_ok()
                .then_some(ComposeModel {
                    provider,
                    deployment,
                })
        })
        .take(8)
        .collect()
}
