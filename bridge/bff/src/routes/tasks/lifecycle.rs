// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
/// promotion (§12). Requests an exact tier; the controller opens a human
/// `KarsApproval` and widens only an unconsumed Task after approval. Requests
/// on immutable runs use metadata to preserve their admission generation.
/// The BFF never widens an envelope directly.
pub async fn promote_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(body): Json<PromoteMissionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    if !(1..=5).contains(&body.tier) {
        return Err(AppError::BadRequest("tier must be in 1..5".into()));
    }
    let api: Api<KarsTask> = cluster.tasks(&ns);
    if body.tier <= task.spec.envelope.tier {
        return Err(AppError::BadRequest(
            "requested tier must exceed the current tier".into(),
        ));
    }
    let patch = promotion_patch(&task, &principal, body.tier);
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
        "note": "An exact-tier human review was requested. The controller opens the approval after validating current authority. Approval can update an unconsumed Task only; launched or controller-owned Missions require promotion of their source package and a fresh authorized run."
    })))
}

fn promotion_patch(task: &KarsTask, principal: &Principal, tier: i32) -> serde_json::Value {
    let immutable_run = task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch)
        || task
            .metadata
            .owner_references
            .iter()
            .flatten()
            .any(|owner| owner.controller == Some(true))
        || task.status.as_ref().is_some_and(|status| {
            status.sandbox_ref.is_some()
                || status
                    .execution_phase
                    .as_deref()
                    .is_some_and(|phase| phase != "Idle")
        })
        || [
            "kars.azure.com/run-requested",
            "kars.azure.com/reviewed-state",
            "kars.azure.com/reviewed-root",
            "kars.azure.com/qualification-run",
            "kars.azure.com/halted",
        ]
        .iter()
        .any(|key| task.annotations().contains_key(*key));
    let mut patch = serde_json::json!({
        "metadata": { "uid": task.uid(), "resourceVersion": task.resource_version(),
            "annotations": {
                "kars.azure.com/promotion-requested-by-sub": principal.sub,
                "kars.azure.com/promotion-requested-by": principal.name,
            } },
        "spec": { "requestedTier": tier },
    });
    if immutable_run {
        // Admission binds Task generation. Even a request-only spec edit would
        // invalidate an active run, so retain it as metadata for the same consumer.
        patch.as_object_mut().unwrap().remove("spec");
        patch["metadata"]["annotations"]["kars.azure.com/requested-tier"] = tier.to_string().into();
    } else {
        patch["metadata"]["annotations"]["kars.azure.com/requested-tier"] = serde_json::Value::Null;
    }
    patch
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
/// reconcile then deletes UID-owned sandboxes + InferencePolicies throughout
/// the Task tree, so descendants cannot continue delegated work) and stamps
/// a governed decision annotation (`kars.azure.com/halted` = operator/reason/at)
/// so the halt itself is a durable, attestable execution gate that scheduling
/// cannot reverse. A new run is required to resume work. The deliverable, trace,
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
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("operator emergency-stop");
    let at = chrono::Utc::now().to_rfc3339();
    // Un-launch and preserve the operator decision atomically; only the
    // controller propagates that gate and tears down UID-owned execution.
    let patch = halt_patch(&task, &principal, &at, reason);
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
        "note": "The controller is stopping this Mission and its UID-owned descendants. Task records, deliverables, and accounting are retained. This governed halt prevents automatic relaunch.",
    })))
}

fn halt_patch(task: &KarsTask, principal: &Principal, at: &str, reason: &str) -> serde_json::Value {
    let decision = task
        .annotations()
        .get("kars.azure.com/halted")
        .cloned()
        .unwrap_or_else(|| {
            format!(
                "halted by {} ({}) at {at}: {reason}",
                principal.name, principal.sub
            )
        });
    serde_json::json!({
        "metadata": { "uid": task.uid(), "resourceVersion": task.resource_version(),
            "annotations": { "kars.azure.com/halted": decision } },
        "spec": { "execution": { "launch": false } },
    })
}

#[cfg(test)]
mod halt_tests {
    use super::*;

    #[test]
    fn promotion_request_is_fenced_attributed_and_never_changes_consumed_run_generation() {
        let mut task = KarsTask::new(
            "mission",
            serde_json::from_value(serde_json::json!({
                "objective":"report", "envelope":{"tier":1,"authorityCeiling":1}
            }))
            .unwrap(),
        );
        task.metadata.uid = Some("task-uid".into());
        task.metadata.resource_version = Some("10".into());
        let principal = Principal {
            sub: "customer".into(),
            name: "Customer".into(),
            roles: vec![],
        };
        let idle = promotion_patch(&task, &principal, 3);
        assert_eq!(idle["metadata"]["uid"], "task-uid");
        assert_eq!(idle["metadata"]["resourceVersion"], "10");
        assert_eq!(
            idle["metadata"]["annotations"]["kars.azure.com/promotion-requested-by-sub"],
            "customer"
        );
        assert_eq!(idle["spec"], serde_json::json!({"requestedTier":3}));
        task.annotations_mut()
            .insert("kars.azure.com/run-requested".into(), "consumed-run".into());
        let consumed = promotion_patch(&task, &principal, 3);
        assert!(consumed.get("spec").is_none());
        assert!(consumed.get("status").is_none());
        assert_eq!(
            consumed["metadata"]["annotations"]["kars.azure.com/requested-tier"],
            "3"
        );
    }

    #[test]
    fn halt_patch_is_fenced_attributed_and_preserves_the_original_decision() {
        let mut task = KarsTask::new("mission", serde_json::from_value(serde_json::json!({
            "objective": "Retain Mission records", "envelope": { "tier": 2, "authorityCeiling": 2 }
        })).unwrap());
        task.metadata.uid = Some("mission-uid".into());
        task.metadata.resource_version = Some("42".into());
        let principal = Principal {
            sub: "operator-sub".into(),
            name: "Operator".into(),
            roles: vec![],
        };
        let patch = halt_patch(
            &task,
            &principal,
            "2026-10-17T16:30:00Z",
            "capacity recovery",
        );
        assert_eq!(patch["metadata"]["uid"], "mission-uid");
        assert_eq!(patch["metadata"]["resourceVersion"], "42");
        assert_eq!(patch["spec"]["execution"]["launch"], false);
        assert_eq!(
            patch["metadata"]["annotations"]["kars.azure.com/halted"],
            "halted by Operator (operator-sub) at 2026-10-17T16:30:00Z: capacity recovery"
        );
        assert!(patch.get("status").is_none());
        task.metadata.annotations = Some(std::collections::BTreeMap::from([(
            "kars.azure.com/halted".into(),
            "original stop decision".into(),
        )]));
        assert_eq!(
            halt_patch(&task, &principal, "later", "retry")["metadata"]["annotations"]["kars.azure.com/halted"],
            "original stop decision"
        );
    }
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
    crate::routes::qualification_runs::forbid_inherited_run(&source)?;
    let mut spec = source.spec.clone();
    spec.execution = Some(crate::kars::task::TaskExecution {
        launch: false,
        runtime: None,
    });
    spec.parent_ref = None;
    spec.requested_tier = None;
    // Independent runs inherit the package, never the source's run authority.
    let mut template = KarsTask::new(&name, spec);
    template
        .metadata
        .labels
        .get_or_insert_with(Default::default)
        .insert("kars.azure.com/reliability-of".into(), name.clone());
    let annotations = template
        .metadata
        .annotations
        .get_or_insert_with(Default::default);
    annotations.insert("kars.azure.com/owner-sub".into(), principal.sub.clone());
    annotations.insert("kars.azure.com/owner-name".into(), principal.name.clone());
    if let Some(marker) = source
        .annotations()
        .get("kars.azure.com/mission-decomposition")
    {
        annotations.insert(
            "kars.azure.com/mission-decomposition".into(),
            marker.clone(),
        );
    }
    crate::kars::execution_plans::validate(&template, req.launch).map_err(AppError::BadRequest)?;
    if req.launch {
        crate::routes::validate::require_budget_contracts(cluster, &source).await?;
    }
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
        let mut task = template.clone();
        task.metadata.name = Some(clone_name.clone());
        let captured = api
            .create(&PostParams::default(), &task)
            .await
            .map_err(map_kube_err)?;
        crate::kars::execution_plans::ensure_preserved(&task, &captured)
            .map_err(AppError::Upstream)?;
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
    let mut task = require_owned_task(cluster, &ns, &name, &principal).await?;
    if req.launch {
        if task.annotations().contains_key("kars.azure.com/halted") {
            return Err(AppError::Conflict("This Mission was halted. Create a new run instead of reactivating its stopped tree.".into()));
        }
        let qualification_run =
            crate::routes::qualification_runs::authorize_run(&task, &principal)?;
        if qualification_run {
            crate::routes::validate::require_task_checks(cluster, &task, true).await?;
        }
        crate::kars::execution_plans::validate(&task, true).map_err(AppError::BadRequest)?;
        crate::routes::validate::require_budget_contracts(cluster, &task).await?;
        if task.spec.blueprint.as_ref().is_some_and(|blueprint| {
            blueprint.github_binding.is_some() && blueprint.credential_bindings.is_none()
        }) {
            let uid = task
                .uid()
                .ok_or_else(|| AppError::Conflict("Draft UID is missing".into()))?;
            let version = task
                .resource_version()
                .ok_or_else(|| AppError::Conflict("Draft resourceVersion is missing".into()))?;
            let prepared = cluster
                .resume_keyless_draft_credentials(&ns, &name, &uid, &version)
                .await
                .map_err(map_kube_err)?;
            task = serde_json::from_value(
                serde_json::to_value(prepared)
                    .map_err(|error| AppError::Upstream(error.to_string()))?,
            )
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        }
    }
    let fence = crate::kars::execution_plans::activation_fence(&task, req.launch)
        .map_err(AppError::Conflict)?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let patch = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "metadata": fence,
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
