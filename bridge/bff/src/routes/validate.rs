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
use crate::routes::ownership::require_owned_task;
use crate::state::AppState;

mod envelope;
mod models;
mod network;
mod resources;

pub(crate) use models::qualification_requirements;

#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    #[serde(default)]
    pub blueprint: Option<crate::routes::tasks::BlueprintDto>,
    /// The autonomy tier the mission will run at (1..5). Validated so the launch
    /// gate actually covers the envelope the UI shows, not just the blueprint.
    #[serde(default)]
    pub tier: Option<i32>,
    /// The token budget cap, when set. Validated for sanity (positive, not
    /// absurdly small) so a mis-typed cap is caught before launch.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Structural launch surface. Team composition requires retained Team E2E
    /// evidence in addition to generic mission execution evidence.
    #[serde(default)]
    pub workload: Option<String>,
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
    Path(ns): Path<String>,
    Json(req): Json<ValidateRequest>,
) -> AppResult<Json<ValidateResponse>> {
    let cluster = require_cluster(&state)?;
    let bp = req.blueprint.unwrap_or_default();
    Ok(Json(
        run_checks(
            cluster,
            &ns,
            &bp,
            req.tier,
            req.budget_tokens,
            req.workload.as_deref(),
        )
        .await,
    ))
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
    let bp = task
        .spec
        .blueprint
        .as_ref()
        .map(blueprint_to_dto)
        .unwrap_or_default();
    let tier = Some(task.spec.envelope.tier);
    let budget_tokens = task.spec.envelope.budget.as_ref().and_then(|b| b.tokens);
    Ok(Json(
        run_checks(cluster, &ns, &bp, tier, budget_tokens, None).await,
    ))
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

/// Run the full §20 check suite against a package. Pure read + DNS.
async fn run_checks(
    cluster: &crate::kars::cluster::Cluster,
    namespace: &str,
    bp: &crate::routes::tasks::BlueprintDto,
    tier: Option<i32>,
    budget_tokens: Option<i64>,
    workload: Option<&str>,
) -> ValidateResponse {
    let mut checks: Vec<Check> = Vec::new();

    envelope::check_envelope(cluster, bp, tier, budget_tokens, &mut checks).await;

    resources::check_resources(cluster, namespace, bp, &mut checks).await;

    models::check_models(cluster, bp, budget_tokens, workload, &mut checks).await;

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
