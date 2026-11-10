// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — pre-flight validation gate (design note §20).
//
// The riskiest moment is the handoff from an edited package to a running agent
// with real tools, network, and authority. This endpoint validates the package
// against the LIVE cluster before a single agent starts, and returns an
// itemised pass/fail — never a black-box "go". It checks what can be checked
// honestly from the BFF today: the referenced ToolPolicy / McpServer /
// KarsMemory exist, the model is one the cluster serves, and each egress host
// resolves. Deeper in-sandbox usability probes (a live MCP handshake, a
// tool-invocation probe, RBAC-delegation) are a named next step and are
// reported as such rather than faked.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::{BudgetScope, TaskBudget};
use crate::routes::ownership::require_owned_task;
use crate::state::AppState;

pub(crate) mod budget_contracts;
mod envelope;
mod github;
mod models;
mod network;
mod resources;

pub(crate) use models::qualification_requirements;

#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    #[serde(default)]
    pub qualification_run: bool,
    #[serde(default)]
    pub blueprint: Option<crate::routes::tasks::BlueprintDto>,
    /// The autonomy tier the mission will run at (1..5). Validated so the launch
    /// gate actually covers the envelope the UI shows, not just the blueprint.
    #[serde(default)]
    pub tier: Option<i32>,
    #[serde(default)]
    pub delegation_depth: Option<i32>,
    /// Aggregate inference limits and their explicit accounting scope. Presence
    /// alone does not establish that the durable broker can enforce them.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    #[serde(default)]
    pub budget_usd_micros: Option<i64>,
    #[serde(default)]
    pub budget_scope: Option<BudgetScope>,
    /// Structural launch surface. Team composition requires retained Team E2E
    /// evidence in addition to generic mission execution evidence.
    #[serde(default)]
    pub workload: Option<String>,
    /// Team creation role inputs, keyed exactly to the reviewed execution plan.
    /// Absent for Missions, whose workers inherit their principal route.
    #[serde(default)]
    pub team_roles: Option<Vec<crate::routes::teams::CreateRole>>,
    /// The same selected repository set submitted to creation. Never a binding:
    /// authority is resolved from the authenticated principal and live grant.
    #[serde(default)]
    pub git_write_repos: Option<Vec<String>>,
}

impl ValidateRequest {
    fn budget(&self) -> TaskBudget {
        TaskBudget {
            scope: self.budget_scope,
            tokens: self.budget_tokens,
            usd_micros: self.budget_usd_micros,
        }
    }
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    Warn,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    /// True only when there are no failing checks — the launch gate.
    pub ok: bool,
    pub checks: Vec<Check>,
}

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

/// `POST /api/namespaces/:ns/validate` — validate a launch package against live
/// cluster state. Pure read + DNS; never mutates anything.
pub async fn validate_package(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(req): Json<ValidateRequest>,
) -> AppResult<Json<ValidateResponse>> {
    crate::routes::qualification_runs::authorize(&principal, req.qualification_run)?;
    let cluster = require_cluster(&state)?;
    let budget = req.budget();
    let mut bp = req.blueprint.unwrap_or_default();
    let roster = async {
        match (req.workload.as_deref(), req.team_roles) {
            (Some("team"), Some(mut roles)) => {
                let options = crate::routes::options::build_options(cluster).await?;
                let roster = crate::routes::teams::preflight::prepare(
                    &options, &ns, &mut bp, &mut roles, budget.tokens, req.qualification_run,
                )?;
                let runtimes = cluster.runnable_runtimes().await;
                for member in &roster {
                    let runtime = member.blueprint.as_ref().and_then(|bp| bp.runtime.as_deref())
                        .or(bp.runtime.as_deref()).unwrap_or("OpenClaw");
                    if !runtimes.contains(runtime) {
                        return Err(AppError::BadRequest(format!("Role {:?}: runtime {runtime:?} is not configured as runnable", member.name)));
                    }
                }
                Ok(Some(roster))
            }
            (Some("team"), None) => Err(AppError::BadRequest("Team preflight requires the reviewed team_roles roster".into())),
            (_, Some(_)) => Err(AppError::BadRequest("team_roles is only valid for workload team; Mission roles inherit the principal route".into())),
            _ => Ok(None),
        }
    }.await;
    let roster = match roster {
        Ok(roster) => roster,
        Err(error) => {
            return Ok(Json(ValidateResponse {
                ok: false,
                checks: vec![Check {
                    id: "team_roster".into(),
                    label: "Reviewed Team member routes".into(),
                    status: CheckStatus::Fail,
                    detail: error.to_string(),
                }],
            }));
        }
    };
    let mut result = run_checks(
        cluster,
        &ns,
        &bp,
        req.tier,
        Some(&budget),
        req.workload.as_deref(),
        req.qualification_run,
        roster.as_deref(),
    )
    .await;
    let github_checks = github::check(
        cluster,
        &ns,
        &principal,
        req.git_write_repos.as_deref(),
        github::write_enabled(req.workload.as_deref(), &bp),
        bp.egress
            .iter()
            .map(|entry| {
                // Team creation trims host entries before persisting its blueprint.
                if req.workload.as_deref() == Some("team") {
                    entry.host.trim()
                } else {
                    entry.host.as_str()
                }
            })
            .chain(
                roster
                    .iter()
                    .flatten()
                    .filter_map(|role| role.blueprint.as_ref())
                    .flat_map(|blueprint| &blueprint.egress)
                    .map(|entry| entry.host.as_str()),
            ),
        None,
    )
    .await;
    for check in github_checks {
        result.ok &= check.status != CheckStatus::Fail;
        result.checks.push(check);
    }
    let spending = crate::routes::budgets::preflight_check(cluster, &ns, &principal.name).await;
    result.ok &= spending.status != CheckStatus::Fail;
    result.checks.push(spending);
    if let Some(check) =
        envelope::check_team_delegation_depth(req.workload.as_deref(), req.delegation_depth)
    {
        result.ok &= check.status != CheckStatus::Fail;
        result.checks.push(check);
    }
    Ok(Json(result))
}

/// `POST /api/namespaces/:ns/tasks/:name/validate` — validate a task's *own*
/// stored blueprint. This protects the launch gate for a draft created
/// earlier: launching it from the mission detail re-runs the same §20 checks
/// against the package the controller would actually materialize.
pub async fn validate_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<ValidateResponse>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let qualification_run = crate::routes::qualification_runs::authorize_stored(&task, &principal)?;
    if qualification_run {
        crate::routes::qualification_runs::check_resources(cluster, &task).await?;
    }
    let bp = task
        .spec
        .blueprint
        .as_ref()
        .map(blueprint_to_dto)
        .unwrap_or_default();
    let tier = Some(task.spec.envelope.tier);
    let mut result = run_checks(
        cluster,
        &ns,
        &bp,
        tier,
        task.spec.envelope.budget.as_ref(),
        None,
        qualification_run,
        None,
    )
    .await;
    let budget_user = task
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kars.azure.com/created-by"))
        .map(String::as_str)
        .unwrap_or("unattributed");
    let spending = crate::routes::budgets::preflight_check(cluster, &ns, budget_user).await;
    result.ok &= spending.status != CheckStatus::Fail;
    result.checks.push(spending);
    for check in github::check_stored(cluster, &ns, &principal, &task).await {
        result.ok &= check.status != CheckStatus::Fail;
        result.checks.push(check);
    }
    check_stored_activation(&task, &mut result);
    Ok(Json(result))
}

fn check_stored_activation(task: &crate::kars::task::KarsTask, result: &mut ValidateResponse) {
    use kube::ResourceExt;
    if task.annotations().contains_key("kars.azure.com/halted") {
        result.checks.push(Check {
            id: "governed_halt".into(),
            label: "Mission permanently halted".into(),
            status: CheckStatus::Fail,
            detail: "Create a new Mission to continue this work. The governed halt cannot be reversed by launch or revision retry; the original results and accounting remain retained.".into(),
        });
        result.ok = false;
    }
    if let Err(detail) = crate::kars::execution_plans::validate(task, true) {
        if !result
            .checks
            .iter()
            .any(|check| check.id == "execution_plan" && check.status == CheckStatus::Fail)
        {
            result.checks.push(Check {
                id: "execution_activation".into(),
                label: "Stored execution package cannot be activated".into(),
                status: CheckStatus::Fail,
                detail,
            });
        }
        result.ok = false;
    }
}

/// Project the typed task blueprint into the validation DTO (the checks operate
/// on the same shape the create path accepts).
fn blueprint_to_dto(b: &crate::kars::task::TaskBlueprint) -> crate::routes::tasks::BlueprintDto {
    use crate::routes::tasks::{BlueprintDto, EgressDto, ModelDto};
    BlueprintDto {
        runtime: b.runtime.clone(),
        model: b.model.as_ref().map(|m| ModelDto {
            provider: m.provider.clone(),
            deployment: m.deployment.clone(),
        }),
        model_fallbacks: b
            .model_fallbacks
            .iter()
            .map(|m| ModelDto {
                provider: m.provider.clone(),
                deployment: m.deployment.clone(),
            })
            .collect(),
        instructions: b.instructions.clone(),
        tool_policy: b.tool_policy.clone(),
        mcp_servers: b.mcp_servers.clone(),
        egress: b
            .egress
            .iter()
            .map(|e| EgressDto {
                host: e.host.clone(),
                port: e.port,
            })
            .collect(),
        egress_mode: b.egress_mode.clone(),
        isolation: b.isolation.clone(),
        memory: b.memory.clone(),
        skills: b.skills.clone(),
        execution_plan: b
            .execution_plan
            .as_ref()
            .map(crate::routes::tasks::ExecutionPlanDto::from_crd),
    }
}

pub(crate) async fn require_task_checks(
    cluster: &crate::kars::cluster::Cluster,
    task: &crate::kars::task::KarsTask,
    qualification_run: bool,
) -> AppResult<()> {
    use kube::ResourceExt;
    if qualification_run {
        crate::routes::qualification_runs::check_resources(cluster, task).await?;
    }
    let blueprint = task
        .spec
        .blueprint
        .as_ref()
        .map(blueprint_to_dto)
        .unwrap_or_default();
    let result = run_checks(
        cluster,
        &task.namespace().unwrap_or_default(),
        &blueprint,
        Some(task.spec.envelope.tier),
        task.spec.envelope.budget.as_ref(),
        None,
        qualification_run,
        None,
    )
    .await;
    if result.ok {
        Ok(())
    } else {
        Err(AppError::BadRequest(
            result
                .checks
                .into_iter()
                .filter(|check| check.status == CheckStatus::Fail)
                .map(|check| format!("{}: {}", check.id, check.detail))
                .collect::<Vec<_>>()
                .join("; "),
        ))
    }
}

pub(crate) async fn require_budget_contracts(
    cluster: &crate::kars::cluster::Cluster,
    task: &crate::kars::task::KarsTask,
) -> AppResult<()> {
    let blueprint = task
        .spec
        .blueprint
        .as_ref()
        .map(blueprint_to_dto)
        .unwrap_or_default();
    if let Some(check) =
        budget_contracts::check(cluster, &blueprint, task.spec.envelope.budget.as_ref()).await
        && check.status == CheckStatus::Fail
    {
        return Err(AppError::BadRequest(check.detail));
    }
    Ok(())
}

pub(crate) async fn require_team_budget_contracts(
    cluster: &crate::kars::cluster::Cluster,
    team: &crate::kars::team::KarsTeam,
) -> AppResult<()> {
    let blueprint = team
        .spec
        .blueprint
        .as_ref()
        .map(blueprint_to_dto)
        .unwrap_or_default();
    if let Some(check) = budget_contracts::check_with_roster(
        cluster,
        &blueprint,
        team.spec.envelope.budget.as_ref(),
        Some(&team.spec.roster),
    )
    .await
        && check.status == CheckStatus::Fail
    {
        return Err(AppError::BadRequest(check.detail));
    }
    Ok(())
}

/// Run the full §20 check suite against a package. Pure read + DNS.
#[allow(clippy::too_many_arguments)]
async fn run_checks(
    cluster: &crate::kars::cluster::Cluster,
    namespace: &str,
    bp: &crate::routes::tasks::BlueprintDto,
    tier: Option<i32>,
    budget: Option<&TaskBudget>,
    workload: Option<&str>,
    qualification_run: bool,
    roster: Option<&[crate::kars::team::TeamRole]>,
) -> ValidateResponse {
    let mut checks: Vec<Check> = Vec::new();
    let budget_tokens = budget.and_then(|b| b.tokens);
    if qualification_run {
        let mut reviewed = bp.clone();
        if workload == Some("team") {
            // Standing Teams already govern their explicit egress mode in Core.
            reviewed.egress_mode = None;
        }
        let package = crate::routes::qualification_runs::validate_package(&reviewed, budget_tokens);
        checks.push(Check {
            id: "qualification_run".into(),
            label: "Explicit operator qualification run — not qualified evidence".into(),
            status: if package.is_ok() {
                CheckStatus::Warn
            } else {
                CheckStatus::Fail
            },
            detail: package.err().unwrap_or_else(|| {
                if workload == Some("team") {
                    crate::routes::teams::qualification::contract::NOTE.into()
                } else {
                    crate::routes::qualification_runs::NOTE.into()
                }
            }),
        });
    }

    checks.push(envelope::check_workspace(cluster, namespace).await);
    envelope::check_envelope(cluster, bp, tier, budget, workload, &mut checks).await;
    if let Some(check) = budget_contracts::check_with_roster(cluster, bp, budget, roster).await {
        checks.push(check);
    }

    resources::check_resources(cluster, namespace, bp, &mut checks).await;

    models::check_models(
        cluster,
        bp,
        budget_tokens,
        workload,
        qualification_run,
        &mut checks,
    )
    .await;

    network::check_egress(bp, &mut checks).await;

    if checks.is_empty() {
        checks.push(Check {
            id: "baseline".into(),
            label: "Package is launch-ready".into(),
            status: CheckStatus::Pass,
            detail: "No external tools, services, memory, or custom egress to validate — the mission runs on the model alone within its envelope.".into(),
        });
    }

    let ok = !checks.iter().any(|c| c.status == CheckStatus::Fail);
    ValidateResponse { ok, checks }
}

#[cfg(test)]
mod qualification_requirement_tests;
