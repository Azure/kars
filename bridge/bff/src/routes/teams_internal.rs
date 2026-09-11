// kars Bridge BFF — Internal Teams gateway endpoints.
//
// Authenticated by X-Teams-Internal-Secret. The gateway sends the Entra subject;
// the BFF resolves the principal from its own server-side role map
// (BRIDGE_TEAMS_ENTRA_ROLE_MAP). Roles from the request body are IGNORED.
//
// Decision endpoint applies identical authorization + stale-protection as the
// browser path (approvals.rs). Command endpoint delegates to shared team helpers.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::approval::{ApprovalDecision, KarsApproval};
use crate::state::AppState;
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams};

const INTERNAL_SECRET_HEADER: &str = "x-teams-internal-secret";

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

fn verify_internal_auth(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    let expected = state.teams_internal_secret().ok_or(AppError::Forbidden(
        "teams integration not configured".into(),
    ))?;
    let provided = headers
        .get(INTERNAL_SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided.is_empty() || provided != expected {
        return Err(AppError::Forbidden("invalid internal teams secret".into()));
    }
    Ok(())
}

/// Resolve a principal from the BFF's own Entra role map.
/// The gateway sends only the Entra subject; roles come from server-side config.
fn resolve_teams_principal(
    state: &AppState,
    entra_subject: &str,
    _entra_name: &str,
) -> AppResult<Principal> {
    if entra_subject.is_empty() {
        return Err(AppError::BadRequest("entra_subject is required".into()));
    }
    let role_map = state.teams_entra_role_map();
    let entry = role_map.iter().find(|(sub, _, _, _)| sub == entra_subject);
    match entry {
        Some((_, bridge_subject, roles, display_name)) => Ok(Principal {
            sub: bridge_subject.clone(),
            name: display_name.clone(),
            roles: roles.clone(),
        }),
        None => Err(AppError::Forbidden(format!(
            "Entra subject {entra_subject} is not in the BFF role map"
        ))),
    }
}

// ─── Decision endpoint ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TeamsDecisionRequest {
    pub approval_name: String,
    pub approval_namespace: String,
    pub verdict: String,
    pub reason: Option<String>,
    pub resource_version: String,
    pub bound_envelope_digest: Option<String>,
    pub entra_subject: String,
    pub entra_name: String,
}

#[derive(Debug, Serialize)]
pub struct TeamsDecisionResponse {
    pub phase: String,
}

fn owner_subject(a: &KarsApproval) -> Option<&str> {
    a.annotations()
        .get("kars.azure.com/owner-sub")
        .map(String::as_str)
}

fn is_owner(a: &KarsApproval, principal: &Principal) -> bool {
    owner_subject(a).is_some_and(|subject| subject == principal.sub)
}

fn can_expand_authority(principal: &Principal) -> bool {
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

pub async fn teams_decision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TeamsDecisionRequest>,
) -> AppResult<(StatusCode, Json<TeamsDecisionResponse>)> {
    verify_internal_auth(&state, &headers)?;

    if !matches!(req.verdict.as_str(), "approve" | "request-changes" | "deny") {
        return Err(AppError::BadRequest(format!(
            "verdict must be 'approve', 'request-changes', or 'deny', got '{}'",
            req.verdict
        )));
    }

    // Resolve principal from BFF's own role map — never trust gateway's role claim
    let principal = resolve_teams_principal(&state, &req.entra_subject, &req.entra_name)?;

    let cluster = require_cluster(&state)?;
    let api: Api<KarsApproval> = cluster.approvals(&req.approval_namespace);
    let current = api
        .get(&req.approval_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;

    // ── Authorization (identical to browser approvals.rs) ──────────────────────
    if req.verdict == "request-changes"
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

    let owned_by_principal = is_owner(&current, &principal);
    let owner_may_decide = owned_by_principal
        && matches!(
            current.spec.action.kind.as_str(),
            "clarification" | "egress"
        );
    if !owner_may_decide && !can_expand_authority(&principal) {
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

    // ── Stale-protection ──────────────────────────────────────────────────────
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
            "approval changed since the card was sent; stale resourceVersion".into(),
        ));
    }
    let current_digest = current
        .status
        .as_ref()
        .and_then(|s| s.bound_envelope_digest.clone());
    if req.bound_envelope_digest != current_digest {
        return Err(AppError::Conflict(
            "the governed envelope changed since the card was sent".into(),
        ));
    }

    // ── Apply decision ────────────────────────────────────────────────────────
    let decision = ApprovalDecision {
        verdict: if req.verdict == "request-changes" {
            "deny".to_string()
        } else {
            req.verdict.clone()
        },
        decider: principal.name.clone(),
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
            path: json_patch::jsonptr::PointerBuf::from_tokens([
                "metadata",
                "annotations",
                "kars.azure.com/review-decision-kind",
            ]),
            value: serde_json::Value::String(req.verdict),
        }),
        json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: json_patch::jsonptr::PointerBuf::from_tokens(["spec", "decision"]),
            value: serde_json::to_value(decision).map_err(|e| AppError::Internal(e.into()))?,
        }),
    ]);
    let patched = api
        .patch(
            &req.approval_name,
            &PatchParams::default(),
            &Patch::Json::<KarsApproval>(patch),
        )
        .await
        .map_err(|e| {
            if matches!(e, kube::Error::Api(ref ae) if ae.code == 409 || ae.code == 422) {
                AppError::Conflict("approval was decided concurrently; stale".into())
            } else {
                AppError::Upstream(e.to_string())
            }
        })?;

    let result_phase = patched
        .status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Pending".to_string());

    Ok((
        StatusCode::OK,
        Json(TeamsDecisionResponse {
            phase: result_phase,
        }),
    ))
}

// ─── Team command endpoint ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TeamsCommandRequest {
    pub team_name: String,
    pub namespace: String,
    pub command: String,
    pub args: String,
    pub entra_subject: String,
    pub entra_name: String,
}

#[derive(Debug, Serialize)]
pub struct TeamsCommandResponse {
    pub message: String,
}

pub async fn teams_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TeamsCommandRequest>,
) -> AppResult<(StatusCode, Json<TeamsCommandResponse>)> {
    verify_internal_auth(&state, &headers)?;

    let principal = resolve_teams_principal(&state, &req.entra_subject, &req.entra_name)?;
    if !principal
        .roles
        .iter()
        .any(|r| r == "user" || r == "operator" || r == "admin")
    {
        return Err(AppError::Forbidden(
            "user, operator, or admin role required".into(),
        ));
    }

    let cluster = require_cluster(&state)?;
    let ns = &req.namespace;
    let team_name = &req.team_name;

    // Enforce team ownership — same as browser handlers
    crate::routes::teams::require_owned_team(cluster, ns, team_name, &principal).await?;

    match req.command.as_str() {
        "status" => {
            let team = cluster
                .teams(ns)
                .get_opt(team_name)
                .await
                .map_err(|e| AppError::Upstream(e.to_string()))?
                .ok_or(AppError::NotFound)?;
            let phase = team
                .status
                .as_ref()
                .and_then(|s| s.phase.clone())
                .unwrap_or_else(|| "Unknown".to_string());
            let health = team
                .status
                .as_ref()
                .and_then(|s| s.health.clone())
                .unwrap_or_else(|| "–".to_string());
            Ok((
                StatusCode::OK,
                Json(TeamsCommandResponse {
                    message: format!(
                        "**{}** — Phase: `{}` | Health: `{}`",
                        team.name_any(),
                        phase,
                        health
                    ),
                }),
            ))
        }
        "list-tasks" => {
            let raw = cluster.read_team_tasks(team_name).await;
            let tasks: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
            if tasks.is_empty() {
                return Ok((
                    StatusCode::OK,
                    Json(TeamsCommandResponse {
                        message: format!("No tasks in team **{team_name}** backlog."),
                    }),
                ));
            }
            let mut lines = vec![format!("**{team_name}** backlog ({} tasks):", tasks.len())];
            for task in tasks.iter().take(10) {
                let id = task.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                lines.push(format!("• `{id}` — {title} [{status}]"));
            }
            if tasks.len() > 10 {
                lines.push(format!("  …and {} more", tasks.len() - 10));
            }
            Ok((
                StatusCode::OK,
                Json(TeamsCommandResponse {
                    message: lines.join("\n"),
                }),
            ))
        }
        "add-task" => {
            if req.args.trim().is_empty() {
                return Err(AppError::BadRequest("add-task requires a title".into()));
            }
            let title = req.args.trim().to_string();
            let task_id = format!("t-{}", chrono::Utc::now().timestamp_micros());
            let new_task = serde_json::json!({
                "id": task_id,
                "title": title,
                "description": "",
                "depends_on": [],
                "acceptance_criteria": [],
                "review_required": false,
                "status": "pending",
                "created_at": chrono::Utc::now().to_rfc3339(),
            });
            cluster
                .update_configmap_data(
                    &format!("kars-team-tasks-{team_name}"),
                    &[("kars.azure.com/team-tasks", team_name.as_str())],
                    |data| {
                        let mut tasks: Vec<serde_json::Value> = data
                            .get("tasks.json")
                            .and_then(|raw| serde_json::from_str(raw).ok())
                            .unwrap_or_default();
                        tasks.push(new_task.clone());
                        data.insert(
                            "tasks.json".into(),
                            serde_json::to_string(&tasks).unwrap_or_else(|_| "[]".into()),
                        );
                    },
                )
                .await
                .map_err(|e| AppError::Upstream(e.to_string()))?;
            Ok((
                StatusCode::OK,
                Json(TeamsCommandResponse {
                    message: format!("✅ Task `{task_id}` added: {title}"),
                }),
            ))
        }
        "run" => {
            crate::routes::teams::request_team_run(cluster, ns, team_name, &principal).await?;
            Ok((
                StatusCode::OK,
                Json(TeamsCommandResponse {
                    message: format!("▶️ Run requested for team **{team_name}**."),
                }),
            ))
        }
        "halt" => {
            let mut parts = req.args.trim().splitn(2, char::is_whitespace);
            let run = parts
                .next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    AppError::BadRequest("halt requires: /halt <run-name> [reason]".into())
                })?;
            let reason = parts
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            crate::routes::teams::request_team_run_halt(
                cluster, ns, team_name, run, reason, &principal,
            )
            .await?;
            Ok((
                StatusCode::OK,
                Json(TeamsCommandResponse {
                    message: format!("⏸️ Run `{run}` halted and team **{team_name}** paused."),
                }),
            ))
        }
        "bind" => {
            // Validate team ownership — if ownership check passes, the team exists
            // and is owned by this principal. Return success so the gateway can store
            // the binding.
            Ok((
                StatusCode::OK,
                Json(TeamsCommandResponse {
                    message: format!("✅ Ownership verified for team **{team_name}**."),
                }),
            ))
        }
        _ => Err(AppError::BadRequest(format!(
            "unknown command '{}': use bind, add-task, list-tasks, status, run, halt",
            req.command
        ))),
    }
}
