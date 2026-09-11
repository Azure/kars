// kars Bridge BFF — the kars-sre self-remediation approval surface.
//
// Backs the operator "SRE Actions" console page: the kars-sre agent
// diagnoses a workload incident and proposes ONE typed remediation
// (`KarsSREAction`, Pending). An operator approves or rejects; the BFF only
// ever patches `spec.approval` — the controller is the sole executor (it
// mints a narrowly-scoped one-shot token, applies the action, tears the
// binding down, and records the outcome in `status`).

use axum::Json;
use axum::extract::{Path, State};
use kube::api::{ListParams, Patch, PatchParams};
use kube::{Resource, ResourceExt};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::kars::sre_action::{KarsSREAction, SreApprovalSpec};
use crate::state::AppState;

fn map_kube_err(e: kube::Error) -> AppError {
    if let kube::Error::Api(resp) = &e
        && (400..500).contains(&resp.code)
    {
        return AppError::Rejected(resp.message.clone());
    }
    AppError::Upstream(e.to_string())
}

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

/// Browser-facing SRE action proposal shape.
#[derive(Debug, Serialize)]
pub struct SreActionDto {
    pub name: String,
    pub namespace: String,
    pub action_type: String,
    pub target_namespace: Option<String>,
    pub target_name: Option<String>,
    pub params: serde_json::Value,
    pub rationale: Option<String>,
    pub diagnosis: Option<String>,
    pub approval_state: String,
    pub approval_note: Option<String>,
    pub phase: String,
    pub applied_at: Option<String>,
    pub ttl_minutes: Option<u32>,
    pub created_at: Option<String>,
    /// Whether a human can still act on this (only a Pending proposal).
    pub actionable: bool,
}

fn to_dto(a: &KarsSREAction) -> SreActionDto {
    let status = a.status.clone().unwrap_or_default();
    let phase = status.phase.unwrap_or_else(|| "Proposed".to_string());
    let approval_state = a.spec.approval.state.clone();
    let actionable = approval_state == "Pending";
    let target_namespace = a
        .spec
        .action
        .params
        .get("namespace")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let target_name = a
        .spec
        .action
        .params
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    SreActionDto {
        name: a.name_any(),
        namespace: a.namespace().unwrap_or_default(),
        action_type: a.spec.action.kind.clone(),
        target_namespace,
        target_name,
        params: serde_json::to_value(&a.spec.action.params).unwrap_or_default(),
        rationale: a.spec.rationale.clone(),
        diagnosis: a.spec.diagnosis.clone(),
        approval_state,
        approval_note: a.spec.approval.note.clone(),
        phase,
        applied_at: status.applied_at,
        ttl_minutes: a.spec.ttl_minutes,
        created_at: a
            .meta()
            .creation_timestamp
            .as_ref()
            .map(|t| t.0.to_rfc3339()),
        actionable,
    }
}

/// `GET /api/operator/sre-actions` — every SRE remediation proposal,
/// cluster-wide, pending-first then most-recently-created.
pub async fn list_sre_actions(State(state): State<AppState>) -> AppResult<Json<Vec<SreActionDto>>> {
    let cluster = require_cluster(&state)?;
    let api = cluster.sre_actions_all();
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(map_kube_err)?;
    let mut dtos: Vec<SreActionDto> = list.items.iter().map(to_dto).collect();
    dtos.sort_by(|a, b| {
        b.actionable
            .cmp(&a.actionable)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    Ok(Json(dtos))
}

/// Decision request from the operator console.
#[derive(Debug, Deserialize)]
pub struct SreDecisionRequest {
    /// `approve` or `reject`.
    pub verdict: String,
    pub note: Option<String>,
}

/// `POST /api/operator/sre-actions/:ns/:name/decision` — record the
/// operator's approve/reject decision by patching `spec.approval`. The
/// controller is the sole executor of the remediation itself.
pub async fn decide_sre_action(
    State(state): State<AppState>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<SreDecisionRequest>,
) -> AppResult<Json<SreActionDto>> {
    let state_value = match req.verdict.as_str() {
        "approve" => "Approved",
        "reject" => "Rejected",
        other => {
            return Err(AppError::Rejected(format!(
                "verdict must be 'approve' or 'reject', got '{other}'"
            )));
        }
    };
    let cluster = require_cluster(&state)?;
    let api = cluster.sre_actions(&ns);

    let approval = SreApprovalSpec {
        state: state_value.to_string(),
        note: req.note.filter(|n| !n.trim().is_empty()),
    };
    let patch = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSREAction",
        "spec": { "approval": approval },
    });
    let patched = api
        .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(map_kube_err)?;
    Ok(Json(to_dto(&patched)))
}
