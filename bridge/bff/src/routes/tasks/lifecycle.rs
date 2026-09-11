use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::{Api, PostParams};
use serde::Deserialize;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::KarsTask;
use crate::routes::ownership::{require_owned_task, require_owned_task_or_output};
use crate::state::AppState;

use super::mapping::to_detail;
use super::{TaskDetailDto, map_kube_err, require_cluster};

/// `DELETE /api/namespaces/:ns/tasks/:name` — delete a mission and sweep its
/// persisted artifacts (deliverable, files, trace, review), so a deleted mission
/// leaves no orphaned ConfigMaps behind on the Artifacts page or as output-only
/// history. Mirrors the team-delete sweep. Idempotent-ish: 404 for unknowns.
pub async fn delete_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let has_cr = require_owned_task_or_output(cluster, &ns, &name, &principal)
        .await?
        .is_some();
    if has_cr {
        cluster
            .delete_task(&ns, &name)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    } else {
        // CR already gone — just sweep the leftover ConfigMaps.
        cluster.sweep_mission_artifacts(&name).await;
    }
    Ok(Json(serde_json::json!({
        "deleted": true,
        "note": "Mission deleted. Its sandbox, deliverable, files, trace, and review record were removed."
    })))
}

/// Per-mission promote request body.
#[derive(Debug, Deserialize)]
pub struct PromoteMissionRequest {
    pub tier: i32,
}

/// `POST /api/namespaces/:ns/tasks/:name/promote` — request a per-mission tier
/// promotion (§12). Patches `spec.requestedTier`; the controller opens a human
/// `KarsApproval` and widens the envelope only once approved. The BFF never
/// widens an envelope directly.
pub async fn promote_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(body): Json<PromoteMissionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    require_owned_task(cluster, &ns, &name, &principal).await?;
    if !(1..=5).contains(&body.tier) {
        return Err(AppError::BadRequest("tier must be in 1..5".into()));
    }
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let patch = serde_json::json!({ "spec": { "requestedTier": body.tier } });
    api.patch(
        &name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(patch),
    )
    .await
    .map_err(map_kube_err)?;
    Ok(Json(serde_json::json!({
        "requested": true,
        "tier": body.tier,
        "note": "A human approval has been opened. This mission is promoted only once it is approved."
    })))
}

/// Governed emergency-stop request.
#[derive(Debug, Deserialize)]
pub struct HaltRequest {
    /// Why the operator is halting — recorded on the governed decision so the
    /// stop is attestable ("who halted this, when, and why"), not anonymous.
    pub reason: Option<String>,
}

/// `POST /api/namespaces/:ns/tasks/:name/halt` — governed emergency-stop.
///
/// A one-click halt that STOPS a running mission/agent without destroying its
/// record: it flips `spec.execution.launch` to false (the controller's teardown
/// reconcile then deletes the sandbox + InferencePolicy, so the agent is removed
/// from the mesh and can no longer receive or answer delegated work) and stamps
/// a governed decision annotation (`kars.azure.com/halted` = operator/reason/at)
/// so the halt itself is a durable, attestable record. The deliverable, trace,
/// and receipt remain — unlike DELETE, which removes everything. No major agent
/// platform ships a governed kill; kars can, because it owns the K8s control
/// plane (to stop) and the governance record (to attest).
pub async fn halt_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(body): Json<HaltRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    require_owned_task(cluster, &ns, &name, &principal).await?;
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("operator emergency-stop");
    let at = chrono::Utc::now().to_rfc3339();
    let decision = format!("halted by operator at {at}: {reason}");
    // Un-launch (controller tears down the running sandbox) AND record the
    // governed decision atomically in one merge patch.
    let patch = serde_json::json!({
        "metadata": { "annotations": { "kars.azure.com/halted": decision } },
        "spec": { "execution": { "launch": false } },
    });
    api.patch(
        &name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(patch),
    )
    .await
    .map_err(map_kube_err)?;
    Ok(Json(serde_json::json!({
        "halted": true,
        "at": at,
        "reason": reason,
        "note": "The agent's sandbox is being torn down; the mission record, deliverable, and audit trail are retained. The halt is recorded as a governed decision.",
    })))
}

/// Replicate request — how many identical runs to launch for reliability (pass^k).
#[derive(Debug, Deserialize)]
pub struct ReplicateRequest {
    /// Number of additional identical runs to create (2–5). Each becomes a
    /// distinct KarsTask sharing this task's exact objective + envelope, so the
    /// efficiency frontier can compute pass^k reliability across them.
    pub count: u32,
    /// When true, each clone is launched immediately; when false, they are
    /// created as ready-to-run packages the caller launches. Default true.
    #[serde(default = "default_true")]
    pub launch: bool,
}

fn default_true() -> bool {
    true
}

/// `POST /api/namespaces/:ns/tasks/:name/replicate` — the pass^k runner.
///
/// Clones a mission's EXACT package (objective + envelope + blueprint) into
/// `count` distinct sibling tasks so they run independently and the efficiency
/// engine can measure pass^k reliability (fraction of the repeated package
/// accepted on EVERY attempt). Honest: this creates real, governed runs — the
/// same package, nothing weakened — not a simulated repeat.
pub async fn replicate_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<ReplicateRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let count = req.count.clamp(1, 5);
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let source = require_owned_task(cluster, &ns, &name, &principal).await?;
    if source
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.credential_bindings.as_ref())
        .is_some_and(|bindings| {
            bindings
                .sources
                .iter()
                .any(|source| source.scope != "workspace")
        })
    {
        return Err(AppError::BadRequest("Independent replicas cannot inherit another target's credential UID; use an approved workspace source or stage per-replica credentials".into()));
    }

    // A short suffix keyed off the current time keeps clone names unique across
    // repeated replicate calls (so a second batch doesn't collide with a first).
    let batch = chrono::Utc::now().timestamp() % 100000;
    let mut created: Vec<String> = Vec::new();
    for i in 1..=count {
        let clone_name = format!("{name}-rep-{batch}-{i}");
        let mut spec = source.spec.clone();
        // Force the execution gate to the requested launch state; strip parent
        // linkage so each clone is an independent, top-level run.
        spec.execution = Some(crate::kars::task::TaskExecution {
            launch: false,
            runtime: None,
        });
        spec.parent_ref = None;
        let mut task = KarsTask::new(&clone_name, spec);
        // Label the batch so the UI can group a reliability cohort together.
        task.metadata
            .labels
            .get_or_insert_with(Default::default)
            .insert("kars.azure.com/reliability-of".into(), name.clone());
        let annotations = task
            .metadata
            .annotations
            .get_or_insert_with(Default::default);
        annotations.insert("kars.azure.com/owner-sub".into(), principal.sub.clone());
        annotations.insert("kars.azure.com/owner-name".into(), principal.name.clone());
        let captured = api
            .create(&PostParams::default(), &task)
            .await
            .map_err(map_kube_err)?;
        cluster
            .finish_created_credentials(
                &crate::kars::credentials::Target {
                    kind: "KarsTask".into(),
                    namespace: ns.clone(),
                    name: captured.name_any(),
                    uid: captured
                        .uid()
                        .ok_or_else(|| AppError::Upstream("Replica CREATE omitted UID".into()))?,
                },
                req.launch,
            )
            .await
            .map_err(map_kube_err)?;
        created.push(clone_name);
    }

    Ok(Json(serde_json::json!({
        "replicated": name,
        "count": created.len(),
        "runs": created,
        "note": format!("{} identical runs created — pass^{} reliability will appear on the efficiency frontier once they complete and are reviewed.", created.len(), created.len() + 1),
    })))
}

/// Launch/un-launch request body.
#[derive(Debug, Deserialize)]
pub struct LaunchRequest {
    pub launch: bool,
}

/// `POST /api/namespaces/:ns/tasks/:name/launch` — flip the execution gate.
///
/// The §20 launch action: setting `launch: true` asks the controller to
/// materialize a governed sandbox; `false` tears it down. The BFF only patches
/// the spec — the controller does the materialization and reports status.
pub async fn launch_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<LaunchRequest>,
) -> AppResult<Json<TaskDetailDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_task(cluster, &ns, &name, &principal).await?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let patch = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "spec": { "execution": { "launch": req.launch } },
    });
    let patched = api
        .patch(
            &name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(&patch),
        )
        .await
        .map_err(map_kube_err)?;
    Ok(Json(to_detail(
        &patched,
        Vec::new(),
        Vec::new(),
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        None,
    )))
}

#[derive(Debug, Deserialize)]
pub struct IncreaseTaskBudgetRequest {
    pub daily_tokens: i64,
}

/// Request an owned Mission's token-budget increase. Bridge never widens the
/// trust envelope directly; the controller opens a typed human approval and is
/// the sole writer of the new ceiling after approval.
pub async fn increase_task_budget(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<IncreaseTaskBudgetRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let current = task
        .spec
        .envelope
        .budget
        .as_ref()
        .and_then(|budget| budget.tokens)
        .unwrap_or(0);
    if req.daily_tokens <= current {
        return Err(AppError::BadRequest(format!(
            "new daily token budget must be greater than the current {current}"
        )));
    }

    let tasks: Api<KarsTask> = cluster.tasks(&ns);
    let request_id = chrono::Utc::now().timestamp_micros().to_string();
    let patched = tasks
        .patch(
            &name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(&serde_json::json!({
                "metadata": {
                    "annotations": {
                        "kars.azure.com/requested-by": principal.name,
                        "kars.azure.com/requested-by-sub": principal.sub,
                        "kars.azure.com/budget-request-id": request_id
                    }
                },
                "spec": {
                    "requestedBudgetTokens": req.daily_tokens
                }
            })),
        )
        .await
        .map_err(map_kube_err)?;

    Ok(Json(serde_json::json!({
        "requested": true,
        "name": name,
        "budget_tokens": req.daily_tokens,
        "resource_version": patched.metadata.resource_version,
        "note": "A typed human approval is being opened. The controller widens the budget only after approval."
    })))
}
