// ─── Team orchestrator: charter → org chart ──────────────────────────────────
//
// The symmetric counterpart to the mission orchestrator. From a standing-team
// charter it proposes a full org chart — a roster of member roles, each with a
// purpose-fit harness and model — informed by the SAME efficiency frontier the
// mission composer uses. This is the bread-and-butter: different roles can run
// different harnesses/models, chosen by what actually performs. A human reviews
// and edits before the team is created.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::options::build_options;
use crate::state::AppState;

use super::ComposeEgress;
use super::client::orchestrator_complete;
use super::execution::execution_plan_error_from_raw;
use super::prompts::build_team_system_prompt;
use super::routing::select_orchestrator_route;
use super::team_proposal::parse_and_validate_team;
use super::team_qualification::{normalize_team_proposal_route, qualified_team_fallbacks};

// A team proposal can contain eight milestone contracts plus four role contracts.
// Keep enough output room for the model's complete JSON rather than accepting a
// syntactically truncated proposal and wasting the single repair attempt.
pub(super) const TEAM_COMPOSE_MAX_TOKENS: u32 = 8_192;

#[derive(Debug, Deserialize)]
pub struct ComposeTeamRequest {
    pub charter: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeTeamRole {
    pub name: String,
    pub system_prompt: String,
    /// Harness kind (validated against real wired runtimes) or empty for the
    /// team default.
    pub runtime: String,
    /// Model as `provider::deployment` (validated) or empty for team default.
    pub model: String,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeTeamMilestone {
    pub id: String,
    pub title: String,
    pub description: String,
    pub owner_role: Option<String>,
    pub depends_on: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub review_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeTeamProposal {
    pub tier: i32,
    pub cadence_minutes: i64,
    pub instructions: String,
    /// Principal/default model as `provider::deployment`.
    pub model: String,
    /// Ordered routes that independently qualify the complete Team contract.
    pub model_fallbacks: Vec<String>,
    /// Evidence-backed reason for the principal model choice.
    pub model_basis: Option<String>,
    /// Historical cost of the selected route, when the efficiency graph has
    /// enough retained outcomes to measure it.
    pub expected_tokens_per_outcome: Option<i64>,
    pub efficiency_sample_runs: i64,
    pub mcp_servers: Vec<String>,
    pub memory: Option<String>,
    pub egress: Vec<ComposeEgress>,
    pub egress_mode: String,
    pub engineering_enabled: bool,
    pub engineering_signals: Vec<String>,
    pub engineering_poll_interval_seconds: i64,
    pub engineering_auto_run: bool,
    pub roles: Vec<ComposeTeamRole>,
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
    pub milestones: Vec<ComposeTeamMilestone>,
}

#[derive(Debug, Serialize)]
pub struct ComposeTeamResponse {
    pub available: bool,
    pub reason: Option<String>,
    pub proposal: Option<ComposeTeamProposal>,
    pub rationale: Option<String>,
    pub source: Option<String>,
}

/// `POST /api/namespaces/:ns/compose-team` — orchestrate an org chart from a
/// charter. Same honesty contract as the mission composer: absent/failing
/// orchestrator returns `available:false` with a reason so the UI falls back to
/// manual composition.
pub async fn compose_team(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(req): Json<ComposeTeamRequest>,
) -> AppResult<Json<ComposeTeamResponse>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let charter = req.charter.trim();
    if charter.len() < 8 {
        return Err(AppError::BadRequest("a real charter is required".into()));
    }

    let mut options = build_options(cluster).await?;
    options.mcp_servers.retain(|server| server.namespace == ns);
    options.memories.retain(|memory| memory.namespace == ns);
    options.skills.retain(|skill| skill.namespace == ns);
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
    let system = build_team_system_prompt(
        &options,
        &efficiency,
        &qualification_constraints,
        &resource_qualification_constraints,
    );
    let user = format!(
        "Team charter:\n{charter}\n\nCompose the org chart now. Respond with ONLY the JSON object."
    );

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
        return Ok(Json(ComposeTeamResponse {
            available: false,
            reason: Some(
                "No inference models are configured on this cluster, so the org-composer can't run. Add an inference provider in the Operator Console, or shape the org manually below.".into(),
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
        return Ok(Json(ComposeTeamResponse {
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
        TEAM_COMPOSE_MAX_TOKENS,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return Ok(Json(ComposeTeamResponse {
                available: false,
                reason: Some(format!(
                    "The org-composer could not compose ({e}). Shape the org manually below — the same building blocks the orchestrator would use."
                )),
                proposal: None,
                rationale: None,
                source: None,
            }));
        }
    };

    let mut execution_plan_error = execution_plan_error_from_raw(&raw);
    let (mut proposal, mut rationale) =
        parse_and_validate_team(&raw, &options, &efficiency, charter);
    if !team_proposal_is_complete(&proposal) {
        let repair_user = format!(
            "{user}\n\nYour previous response was incomplete. The exact structural problem was: {}. \
             Return the complete JSON object now; preserve a small roster of independent evidence \
             roles, make every execution-plan role name exactly match one roster role name, and do \
             not include prose or code fences.",
            incomplete_team_proposal_detail(&proposal, execution_plan_error.as_deref())
        );
        if let Ok((repair_raw, repair_source)) = orchestrator_complete(
            cluster,
            &system,
            &repair_user,
            &default_model,
            TEAM_COMPOSE_MAX_TOKENS,
        )
        .await
        {
            execution_plan_error = execution_plan_error_from_raw(&repair_raw);
            (proposal, rationale) =
                parse_and_validate_team(&repair_raw, &options, &efficiency, charter);
            source = repair_source;
        }
    }
    if !team_proposal_is_complete(&proposal) {
        return Ok(Json(ComposeTeamResponse {
            available: false,
            reason: Some(format!(
                "The org-composer returned an incomplete proposal twice (missing: {}). Shape the org manually below rather than treating generic fallback roles as an AI recommendation.",
                incomplete_team_proposal_detail(&proposal, execution_plan_error.as_deref()),
            )),
            proposal: None,
            rationale: None,
            source: Some(source),
        }));
    }
    if let Err(error) = normalize_team_proposal_route(&mut proposal, &options) {
        let repair_user = format!(
            "{user}\n\nYour previous response was not launchable: {error}\n\
             Recompose it so the principal route, every role route, and every selected MCP \
             server, memory binding, and approved skill fit retained qualification evidence at \
             the CURRENT digest. Generic route records do not prove a specific resource.\n\n\
             QUALIFIED EXECUTION RECORDS:\n{qualification_constraints}\n\n\
             RESOURCE QUALIFICATION RECORDS:\n{resource_qualification_constraints}\n\n\
             Return ONLY the complete JSON object."
        );
        if let Ok((repair_raw, repair_source)) = orchestrator_complete(
            cluster,
            &system,
            &repair_user,
            &default_model,
            TEAM_COMPOSE_MAX_TOKENS,
        )
        .await
        {
            (proposal, rationale) =
                parse_and_validate_team(&repair_raw, &options, &efficiency, charter);
            source = repair_source;
        }
    }
    if let Err(error) = normalize_team_proposal_route(&mut proposal, &options) {
        return Ok(Json(ComposeTeamResponse {
            available: false,
            reason: Some(format!(
                "The org-composer could not produce a launchable team after retry: {error}"
            )),
            proposal: None,
            rationale: None,
            source: Some(source),
        }));
    }
    proposal.model_fallbacks = qualified_team_fallbacks(&proposal, &options);
    Ok(Json(ComposeTeamResponse {
        available: true,
        reason: None,
        proposal: Some(proposal),
        rationale,
        source: Some(source),
    }))
}

pub(super) fn team_proposal_is_complete(proposal: &ComposeTeamProposal) -> bool {
    !proposal.instructions.trim().is_empty()
        && !proposal.roles.is_empty()
        && proposal.execution_plan.is_some()
}

fn incomplete_team_proposal_detail(
    proposal: &ComposeTeamProposal,
    execution_plan_error: Option<&str>,
) -> String {
    let mut missing = Vec::new();
    if proposal.instructions.trim().is_empty() {
        missing.push("instructions");
    }
    if proposal.roles.is_empty() {
        missing.push("independent roles");
    }
    if proposal.execution_plan.is_none() {
        missing.push(
            execution_plan_error
                .unwrap_or("valid execution_plan with role names exactly matching the roster"),
        );
    }
    if missing.is_empty() {
        "unknown structural mismatch".into()
    } else {
        missing.join(", ")
    }
}
