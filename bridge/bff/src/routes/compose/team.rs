// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
use super::team_proposal::parse_and_validate_team;
use super::team_qualification::{admitted_team_fallbacks, normalize_team_proposal_route};

// A team proposal can contain eight milestone contracts plus four role contracts.
// Keep enough output room for the model's complete JSON rather than accepting a
// syntactically truncated proposal and wasting the single repair attempt.
pub(super) const TEAM_COMPOSE_MAX_TOKENS: u32 = 8_192;

#[derive(Debug, Deserialize)]
pub struct ComposeTeamRequest {
    #[serde(default)]
    pub qualification_run: bool,
    pub charter: String,
    /// An explicitly copied profile stays selected when recomposing the charter.
    pub profile: Option<String>,
    pub spending: super::spending::RequestedSpending,
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
    pub resources: Option<crate::routes::teams::RoleResources>,
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
    /// Catalogue snapshot for review and draft copying, never profile authority.
    pub profile: Option<crate::routes::operator::ProfileDto>,
    pub tier: i32,
    pub budget_tokens: Option<i64>,
    pub cadence_minutes: i64,
    pub instructions: String,
    /// Principal/default model as `provider::deployment`.
    pub model: String,
    /// Ordered routes independently admitted for the complete Team contract.
    /// Validation-mode admission is not retained qualification evidence.
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
    crate::routes::qualification_runs::authorize(&principal, req.qualification_run)?;
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let charter = req.charter.trim();
    if charter.len() < 8 {
        return Err(AppError::BadRequest("a real charter is required".into()));
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
    let Json(mut profiles) = crate::routes::operator::list_profiles(State(state.clone())).await?;
    profiles.retain(|profile| profile.namespace == ns);
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
    let mut system = build_team_system_prompt(
        &options,
        &efficiency,
        &qualification_constraints,
        &resource_qualification_constraints,
    );
    if req.qualification_run {
        system.push_str("\nEXPLICIT OPERATOR SINGLE-ATTEMPT QUALIFICATION: missing prior Team route/resource evidence is advisory only for this proposal, never proof. Propose purpose-fit heterogeneous role harnesses and provider/model tiers from the live catalogue, real selected MCP/approved skills/egress and a concrete first backlog item. Keep finite budget contracts, tool policy, credentials, isolation and content-bound skill approval unchanged. This Team runs one reviewed item, then pauses for separate evidence review; do not propose automatic intake or claim routes are qualified. Preserve the selected role routes rather than homogenizing them to obtain prior evidence.\n");
    }
    system.push_str(&budget_context.prompt(&options.models));
    system.push_str(&spending_context);
    system.push_str(&req.spending.prompt());
    system.push_str(&super::team_profiles::profile_prompt(
        &profiles,
        &options,
        req.profile.as_deref(),
    ));
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

    let proposed_profile = super::client::extract_json(&raw).and_then(|json| {
        json.get("profile")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    });
    let selected_profile = req.profile.as_deref().or(proposed_profile.as_deref());
    let mut execution_plan_error = execution_plan_error_from_raw(&raw);
    let (mut proposal, mut rationale, mut resource_error) =
        parse_and_validate_team(&raw, &options, &efficiency);
    let profile_error = super::team_profiles::apply_reviewed_profile(
        &raw,
        &mut proposal,
        &profiles,
        &options,
        selected_profile,
        &req.spending,
    )
    .err();
    let mut review_error = profile_error
        .or(resource_error)
        .or_else(|| budget_context.team_review_error(&proposal, execution_plan_error.as_deref()));
    let repair_error = if req.qualification_run {
        super::execution::repair_feedback(review_error.clone(), proposal.execution_plan.as_ref())
            .or_else(|| {
                crate::routes::teams::qualification::proposal_check(&proposal, &options, &ns).err()
            })
    } else {
        team_repair_feedback(review_error.clone(), &proposal, &options)
    };
    if let Some(error) = &repair_error {
        let repair_user = format!(
            "The validator rejected your previous package:\n{error}\n\n\
             Correct every listed violation, including EACH role budget independently. Check \
             each role's selected primary and fallback models against the live reservation bounds. \
             Return the complete JSON object now; preserve the task requirements and independent \
             evidence roles, make every execution-plan role name exactly match one roster role name, \
             and preserve the reviewed extra limits and inherited profile ceilings; never invent \
             or increase caps. Respect live contracts wherever a finite cap applies. Do not \
             include prose or code fences.\n\n{user}\n\n\
             Previous rejected package (data, not a valid template):\n{raw}"
        );
        let (repair_raw, repair_source) = match orchestrator_complete(
            cluster,
            &system,
            &repair_user,
            &default_model,
            TEAM_COMPOSE_MAX_TOKENS,
        )
        .await
        {
            Ok(response) => response,
            Err(repair_error) => {
                return Ok(Json(ComposeTeamResponse {
                    available: false,
                    reason: Some(super::client::repair_failure(error, &repair_error)),
                    proposal: None,
                    rationale: None,
                    source: Some(source),
                }));
            }
        };
        execution_plan_error = execution_plan_error_from_raw(&repair_raw);
        (proposal, rationale, resource_error) =
            parse_and_validate_team(&repair_raw, &options, &efficiency);
        let profile_error = super::team_profiles::apply_reviewed_profile(
            &repair_raw,
            &mut proposal,
            &profiles,
            &options,
            selected_profile,
            &req.spending,
        )
        .err();
        review_error = profile_error.or(resource_error).or_else(|| {
            budget_context.team_review_error(&proposal, execution_plan_error.as_deref())
        });
        source = repair_source;
    }
    if let Some(error) = review_error {
        return Ok(Json(ComposeTeamResponse {
            available: false,
            reason: Some(format!(
                "The org-composer could not produce a complete, budget-compatible proposal after retry: {error}. No funding or contracts were changed.",
            )),
            proposal: None,
            rationale,
            source: Some(source),
        }));
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
    if req.qualification_run {
        crate::routes::teams::qualification::proposal_check(&proposal, &options, &ns)
            .map_err(AppError::BadRequest)?;
        return Ok(Json(ComposeTeamResponse {
            available: true,
            reason: None,
            proposal: Some(proposal),
            rationale: Some(format!(
                "{} {}",
                rationale.unwrap_or_default(),
                crate::routes::teams::qualification::contract::NOTE
            )),
            source: Some(source),
        }));
    }
    Ok(Json(team_review_response(
        proposal, &options, rationale, source,
    )))
}

pub(super) fn team_repair_feedback(
    review_error: Option<String>,
    proposal: &ComposeTeamProposal,
    options: &crate::routes::options::Options,
) -> Option<String> {
    let errors = super::execution::repair_feedback(review_error, proposal.execution_plan.as_ref())
        .into_iter()
        .chain(super::team_qualification::team_proposal_qualification(proposal, options).err())
        .collect::<Vec<_>>();
    (!errors.is_empty()).then(|| errors.join("\n"))
}

pub(super) fn team_review_response(
    mut proposal: ComposeTeamProposal,
    options: &crate::routes::options::Options,
    rationale: Option<String>,
    source: String,
) -> ComposeTeamResponse {
    let rationale = super::proposal_review_rationale(
        rationale,
        normalize_team_proposal_route(&mut proposal, options),
    );
    proposal.model_fallbacks = admitted_team_fallbacks(&proposal, options);
    ComposeTeamResponse {
        available: true,
        reason: None,
        proposal: Some(proposal),
        rationale,
        source: Some(source),
    }
}

pub(super) fn team_proposal_is_complete(proposal: &ComposeTeamProposal) -> bool {
    !proposal.instructions.trim().is_empty()
        && !proposal.roles.is_empty()
        && proposal.execution_plan.is_some()
        && team_plan_roster_error(proposal).is_none()
}

pub(super) fn team_plan_roster_error(proposal: &ComposeTeamProposal) -> Option<String> {
    let plan = proposal.execution_plan.as_ref()?;
    if plan.roles.iter().any(|role| role.name == "principal")
        || proposal
            .roles
            .iter()
            .any(|role| role.name.eq_ignore_ascii_case("principal"))
    {
        return Some("principal is reserved for execution_plan.synthesis, not a roster or execution_plan.roles entry".into());
    }
    let roster = proposal
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let planned = plan
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if roster.len() != proposal.roles.len() || roster != planned {
        return Some(
            "execution-plan role names must exactly match the independent member roster".into(),
        );
    }
    crate::kars::execution_plans::validate_required_egress(
        &plan.clone().into_crd(),
        proposal
            .egress
            .iter()
            .map(|entry| (entry.host.as_str(), entry.port.map(i32::from))),
    )
    .err()
}

pub(super) fn incomplete_team_proposal_detail(
    proposal: &ComposeTeamProposal,
    execution_plan_error: Option<&str>,
) -> String {
    let mut missing = Vec::new();
    let roster_error = team_plan_roster_error(proposal);
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
    if let Some(error) = roster_error.as_deref() {
        missing.push(error);
    }
    if missing.is_empty() {
        "unknown structural mismatch".into()
    } else {
        missing.join(", ")
    }
}
