// kars Bridge BFF — Governance steering: the HITL approval inbox.
//
// These endpoints back the steering inbox — the fleet-wide list of human
// decisions a task fleet is waiting on — and the approve/deny action. The BFF
// only ever patches `spec.decision`; the controller is the sole writer of
// status and drives the terminal transition. The browser never touches the
// cluster directly.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::approval::{ApprovalDecision, KarsApproval};
use crate::state::AppState;
use kube::ResourceExt;
use kube::api::{Api, ListParams, Patch, PatchParams};

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

/// Browser-facing approval shape.
#[derive(Debug, Serialize)]
pub struct ApprovalDto {
    pub name: String,
    pub namespace: String,
    pub task: String,
    pub team: Option<String>,
    pub milestone: Option<String>,
    pub action_kind: String,
    pub summary: String,
    pub detail: Option<String>,
    pub requested_tier: Option<i32>,
    pub phase: String,
    pub decider: Option<String>,
    pub requested_at: Option<String>,
    pub decided_at: Option<String>,
    pub expires_at: Option<String>,
    pub bound_envelope_digest: Option<String>,
    pub run_nonce: Option<String>,
    pub resource_version: String,
    pub generation: i64,
    /// Whether a human can still act on this (only a Pending approval).
    pub actionable: bool,
}

fn to_dto(ns: &str, a: &KarsApproval) -> ApprovalDto {
    let status = a.status.clone().unwrap_or_default();
    let phase = status.phase.unwrap_or_else(|| "Pending".to_string());
    let actionable = phase == "Pending";
    let metadata_value = |key: &str| {
        a.annotations()
            .get(key)
            .cloned()
            .or_else(|| a.labels().get(key).cloned())
    };
    ApprovalDto {
        name: a.name_any(),
        namespace: ns.to_string(),
        task: a.spec.task_ref.name.clone(),
        team: metadata_value("kars.azure.com/team"),
        milestone: metadata_value("kars.azure.com/milestone"),
        action_kind: a.spec.action.kind.clone(),
        summary: a.spec.action.summary.clone(),
        detail: a.spec.action.detail.clone(),
        requested_tier: a.spec.action.requested_tier,
        phase,
        decider: status.decider,
        requested_at: status.requested_at,
        decided_at: status.decided_at,
        expires_at: status.expires_at,
        bound_envelope_digest: status.bound_envelope_digest,
        run_nonce: a.annotations().get("kars.azure.com/req-run").cloned(),
        resource_version: a.metadata.resource_version.clone().unwrap_or_default(),
        generation: a.metadata.generation.unwrap_or_default(),
        actionable,
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// When `true`, only undecided (Pending) approvals — the steering inbox.
    #[serde(default)]
    pub pending: bool,
    /// Operator/admin-only fleet view. Workspace callers omit this and receive
    /// only approvals owned by their immutable OIDC subject.
    #[serde(default)]
    pub scope_all: bool,
}

fn owner_subject(a: &KarsApproval) -> Option<&str> {
    a.annotations()
        .get("kars.azure.com/owner-sub")
        .map(String::as_str)
}

fn is_owner(a: &KarsApproval, principal: &Principal) -> bool {
    owner_subject(a).is_some_and(|subject| subject == principal.sub)
}

fn can_view_all(principal: &Principal) -> bool {
    principal
        .roles
        .iter()
        .any(|role| role == "operator" || role == "admin")
}

fn is_team_milestone_review(approval: &KarsApproval) -> bool {
    approval.spec.action.kind == "checkpoint"
        && (approval.annotations().contains_key("kars.azure.com/team")
            || approval.labels().contains_key("kars.azure.com/team"))
        && (approval
            .annotations()
            .contains_key("kars.azure.com/milestone")
            || approval.labels().contains_key("kars.azure.com/milestone"))
}

/// `GET /api/namespaces/:ns/approvals?pending=` — the fleet-wide steering
/// inbox. Pending-first, then most recently decided.
pub async fn list_approvals(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<ApprovalDto>>> {
    let cluster = require_cluster(&state)?;
    let api: Api<KarsApproval> = cluster.approvals(&ns);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(map_kube_err)?;
    if q.scope_all && !can_view_all(&principal) {
        return Err(AppError::Forbidden(
            "operator or admin role required for fleet approval scope".into(),
        ));
    }
    let mut dtos: Vec<ApprovalDto> = list
        .items
        .iter()
        .filter(|a| q.scope_all || is_owner(a, &principal))
        .map(|a| to_dto(&ns, a))
        .collect();
    if q.pending {
        dtos.retain(|d| d.phase == "Pending");
    }
    // Pending first (actionable), then the rest; stable by name within a group.
    dtos.sort_by(|a, b| {
        b.actionable
            .cmp(&a.actionable)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Json(dtos))
}

/// `GET /api/namespaces/:ns/tasks/:name/approvals` — approvals gating one task.
pub async fn list_task_approvals(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<Vec<ApprovalDto>>> {
    let cluster = require_cluster(&state)?;
    let api: Api<KarsApproval> = cluster.approvals(&ns);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(map_kube_err)?;
    let mut dtos: Vec<ApprovalDto> = list
        .items
        .iter()
        .filter(|a| a.spec.task_ref.name == name && is_owner(a, &principal))
        .map(|a| to_dto(&ns, a))
        .collect();
    dtos.sort_by(|a, b| {
        b.actionable
            .cmp(&a.actionable)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Json(dtos))
}

/// Decision request from the UI.
#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    /// `approve` or `deny`.
    pub verdict: String,
    pub reason: Option<String>,
    /// Optimistic-concurrency token for the exact approval the human reviewed.
    pub resource_version: String,
    /// Envelope digest shown to the reviewer; stale/moved envelopes cannot be
    /// approved by replaying an old browser tab.
    pub bound_envelope_digest: Option<String>,
}

/// `POST /api/namespaces/:ns/approvals/:name/decision` — record a human
/// decision by patching `spec.decision`. The controller drives the transition.
pub async fn decide_approval(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<DecisionRequest>,
) -> AppResult<Json<ApprovalDto>> {
    if req.verdict != "approve" && req.verdict != "deny" {
        return Err(AppError::Rejected(format!(
            "verdict must be 'approve' or 'deny', got '{}'",
            req.verdict
        )));
    }

    let cluster = require_cluster(&state)?;
    let api: Api<KarsApproval> = cluster.approvals(&ns);
    let current = api.get(&name).await.map_err(map_kube_err)?;
    if req.verdict == "deny"
        && is_team_milestone_review(&current)
        && req
            .reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Err(AppError::Rejected(
            "requesting changes for a Team milestone requires written feedback".into(),
        ));
    }
    let can_expand_authority = principal
        .roles
        .iter()
        .any(|role| role == "operator" || role == "admin");
    let owned_by_principal = is_owner(&current, &principal);
    let owner_may_decide = owned_by_principal
        && matches!(
            current.spec.action.kind.as_str(),
            "clarification" | "egress"
        );
    if !owner_may_decide && !can_expand_authority {
        return Err(AppError::Forbidden(
            "operator or admin role required for this approval decision".into(),
        ));
    }
    if req.verdict == "approve"
        && current.spec.action.kind != "clarification"
        && current
            .spec
            .requested_by
            .as_ref()
            .is_some_and(|actor| actor.subject == principal.sub)
    {
        return Err(AppError::Forbidden(
            "requester cannot approve their own authority expansion".into(),
        ));
    }
    let phase = current
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("Pending");
    if phase != "Pending" || current.spec.decision.is_some() {
        return Err(AppError::Conflict(format!(
            "approval is already terminal ({phase})"
        )));
    }
    let current_rv = current
        .metadata
        .resource_version
        .clone()
        .unwrap_or_default();
    if req.resource_version != current_rv {
        return Err(AppError::Conflict(
            "approval changed since it was displayed; reload before deciding".into(),
        ));
    }
    let current_digest = current
        .status
        .as_ref()
        .and_then(|s| s.bound_envelope_digest.clone());
    if req.bound_envelope_digest != current_digest {
        return Err(AppError::Conflict(
            "the governed envelope changed since review; reload before deciding".into(),
        ));
    }

    let decision = ApprovalDecision {
        verdict: req.verdict,
        decider: principal.name,
        decider_subject: Some(principal.sub),
        decider_roles: principal.roles,
        reason: req.reason.filter(|r| !r.trim().is_empty()),
    };
    let patch = json_patch::Patch(vec![
        json_patch::PatchOperation::Test(json_patch::TestOperation {
            path: json_patch::jsonptr::PointerBuf::from_tokens(["metadata", "resourceVersion"]),
            value: serde_json::Value::String(current_rv),
        }),
        json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: json_patch::jsonptr::PointerBuf::from_tokens(["spec", "decision"]),
            value: serde_json::to_value(decision).map_err(|e| AppError::Internal(e.into()))?,
        }),
    ]);
    let patched = api
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Json::<KarsApproval>(patch),
        )
        .await
        .map_err(|e| {
            if matches!(e, kube::Error::Api(ref ae) if ae.code == 409 || ae.code == 422) {
                AppError::Conflict("approval was decided concurrently; reload".into())
            } else {
                map_kube_err(e)
            }
        })?;
    Ok(Json(to_dto(&ns, &patched)))
}

#[cfg(test)]
mod tests {
    use super::{is_owner, is_team_milestone_review, owner_subject, to_dto};
    use crate::auth::Principal;
    use crate::kars::approval::{ApprovalAction, KarsApproval, KarsApprovalSpec};
    use crate::kars::task::LocalObjectRef;

    fn approval(owner: Option<&str>) -> KarsApproval {
        let mut value = KarsApproval::new(
            "ask",
            KarsApprovalSpec {
                task_ref: LocalObjectRef {
                    name: "task".into(),
                },
                action: ApprovalAction {
                    kind: "clarification".into(),
                    summary: "Which environment?".into(),
                    detail: None,
                    requested_tier: None,
                },
                requested_by: None,
                ttl: None,
                decision: None,
            },
        );
        if let Some(owner) = owner {
            value
                .metadata
                .annotations
                .get_or_insert_with(Default::default)
                .insert("kars.azure.com/owner-sub".into(), owner.into());
        }
        value
    }

    #[test]
    fn approval_visibility_uses_immutable_subject() {
        let principal = Principal {
            sub: "subject-a".into(),
            name: "same-name".into(),
            roles: vec!["user".into()],
        };
        let owned = approval(Some("subject-a"));
        let other = approval(Some("subject-b"));
        assert_eq!(owner_subject(&owned), Some("subject-a"));
        assert!(is_owner(&owned, &principal));
        assert!(!is_owner(&other, &principal));
        assert!(!is_owner(&approval(None), &principal));
    }

    #[test]
    fn checkpoint_approval_projects_team_and_milestone_identity() {
        let approval: KarsApproval = serde_json::from_value(serde_json::json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsApproval",
            "metadata": {
                "name": "checkpoint-abc",
                "namespace": "kars-system",
                "resourceVersion": "7",
                "labels": {
                    "kars.azure.com/team": "engineering",
                    "kars.azure.com/milestone": "pr-34"
                }
            },
            "spec": {
                "taskRef": {"name": "engineering-principal"},
                "action": {
                    "kind": "checkpoint",
                    "summary": "Review PR #34"
                }
            },
            "status": {
                "phase": "Pending",
                "boundEnvelopeDigest": "sha256:abc"
            }
        }))
        .expect("approval");

        let dto = to_dto("kars-system", &approval);

        assert_eq!(dto.team.as_deref(), Some("engineering"));
        assert_eq!(dto.milestone.as_deref(), Some("pr-34"));
        assert!(dto.actionable);
        assert!(is_team_milestone_review(&approval));
    }
}
