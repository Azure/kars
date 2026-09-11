// kars Bridge BFF — artifact review loop (design note §16).
//
// A deliverable isn't done until a human accepts it. This surface turns the
// passive artifact view into a governed review loop: a reviewer approves or
// requests changes, and **request-changes re-drives the producing task on the
// delta** — the agent re-runs against the original objective plus the reviewer's
// feedback, producing a new revision. Every review is recorded with provenance
// (reviewer, decision, comment, time) and the revision lineage is preserved.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::{require_owned_task, require_owned_task_or_output};
use crate::routes::tasks::require_cluster;
use crate::state::AppState;

/// One recorded review decision (provenance-tracked).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReviewEntry {
    pub decision: String,
    pub comment: Option<String>,
    pub reviewer: String,
    pub decided_at: String,
    /// The revision this review applied to (0 = the first deliverable).
    pub revision: i64,
    /// Whether the reviewer identity came from the verified Bridge principal.
    #[serde(default)]
    pub attested: bool,
    /// Exact execution evidence this decision reviewed.
    #[serde(default)]
    pub assignment_nonce: Option<String>,
}

/// Browser-facing review state for a task's deliverable.
#[derive(Debug, Serialize)]
pub struct ReviewStateDto {
    /// `none` | `approved` | `changes_requested`.
    pub status: String,
    /// The current revision number (incremented on each request-changes).
    pub revision: i64,
    /// The full review history, newest first (revision lineage).
    pub history: Vec<ReviewEntry>,
    /// True when a re-drive is in flight (a revision was requested but the new
    /// run hasn't landed yet).
    pub redrive_pending: bool,
    /// Exact execution evidence represented by the current review status.
    pub assignment_nonce: Option<String>,
}

/// Request body for a review decision.
#[derive(Debug, Deserialize)]
pub struct ReviewRequest {
    /// `approve` | `request_changes`.
    pub decision: String,
    pub comment: Option<String>,
    /// Exact execution the reviewer saw.
    pub assignment_nonce: String,
}

fn parse_history(data: &BTreeMap<String, String>) -> Vec<ReviewEntry> {
    data.get("history")
        .and_then(|s| serde_json::from_str::<Vec<ReviewEntry>>(s).ok())
        .unwrap_or_default()
}

/// `GET /api/namespaces/:ns/tasks/:name/review` — the deliverable's review state.
pub async fn get_review(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<ReviewStateDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_task_or_output(cluster, &ns, &name, &principal).await?;
    let data = cluster.read_review(&name).await.unwrap_or_default();
    let mut history = parse_history(&data);
    history.sort_by(|a, b| b.decided_at.cmp(&a.decided_at));
    let status = data
        .get("status")
        .cloned()
        .unwrap_or_else(|| "none".to_string());
    let revision = data
        .get("revision")
        .and_then(|r| r.parse::<i64>().ok())
        .unwrap_or(0);
    let mut redrive_pending = data
        .get("redrivePending")
        .map(|v| v == "true")
        .unwrap_or(false);
    let assignment_nonce = data.get("assignmentNonce").cloned();

    // A pending re-drive clears once a fresh deliverable has landed (the output
    // finishedAt is newer than the most recent request-changes decision).
    if redrive_pending {
        let last_request = history
            .iter()
            .find(|e| e.decision == "request_changes")
            .map(|e| e.decided_at.clone());
        let finished_at = cluster
            .configmap_data(&format!("kars-mission-output-{name}"))
            .await
            .and_then(|d| d.get("finishedAt").cloned());
        if let (Some(req), Some(fin)) = (last_request, finished_at)
            && fin > req
        {
            redrive_pending = false;
            let mut updated = data.clone();
            updated.insert("redrivePending".into(), "false".into());
            // Best-effort auto-clear: persist the cleared flag, but don't fail
            // the read if it doesn't stick (the next GET simply re-clears). Log
            // it rather than silently discarding, so a persistent write failure
            // is visible instead of looping invisibly.
            if let Err(e) = cluster.write_review(&name, updated).await {
                tracing::warn!(task = %name, "failed to persist auto-cleared redrivePending: {e}");
            }
        }
    }
    Ok(Json(ReviewStateDto {
        status,
        revision,
        history,
        redrive_pending,
        assignment_nonce,
    }))
}

/// `POST /api/namespaces/:ns/tasks/:name/review` — record a review decision.
/// `request_changes` re-drives the producing task on the reviewer's delta.
pub async fn post_review(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(body): Json<ReviewRequest>,
) -> AppResult<Json<ReviewStateDto>> {
    let cluster = require_cluster(&state)?;
    let decision = body.decision.trim();
    if decision != "approve" && decision != "request_changes" {
        return Err(AppError::BadRequest(
            "decision must be 'approve' or 'request_changes'".into(),
        ));
    }
    if decision == "request_changes"
        && body
            .comment
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Err(AppError::BadRequest(
            "request_changes requires a comment describing what to change".into(),
        ));
    }

    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let reviewed_assignment = cluster
        .configmap_data(&format!("kars-mission-output-{name}"))
        .await
        .and_then(|output| output.get("assignmentNonce").cloned())
        .unwrap_or_else(|| name.clone());
    if body.assignment_nonce != reviewed_assignment {
        return Err(AppError::Conflict(
            "the deliverable changed since it was displayed; reload before reviewing".into(),
        ));
    }

    // Read prior review state, distinguishing a genuine cluster-read error from
    // "no review yet". Silently defaulting on error would erase the entire
    // review history on the next write (a transient blip = permanent data loss).
    let data = cluster
        .configmap_data_result(&format!("kars-mission-review-{name}"))
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .unwrap_or_default();
    let mut history = parse_history(&data);
    let revision = data
        .get("revision")
        .and_then(|r| r.parse::<i64>().ok())
        .unwrap_or(0);
    // Preserve the original objective once, so revisions compose from it rather
    // than compounding directives across rounds.
    let original = data
        .get("originalObjective")
        .cloned()
        .unwrap_or_else(|| task.spec.objective.clone());

    let now = chrono::Utc::now().to_rfc3339();
    let reviewer = principal.name;
    let entry = ReviewEntry {
        decision: decision.to_string(),
        comment: body.comment.clone(),
        reviewer: reviewer.clone(),
        decided_at: now.clone(),
        revision,
        attested: true,
        assignment_nonce: Some(reviewed_assignment.clone()),
    };

    let (status, new_revision, redrive_pending) = if decision == "approve" {
        ("approved".to_string(), revision, false)
    } else {
        // Compose the revision objective from the original + the previous
        // deliverable + the reviewer's requested changes, then re-drive the
        // producing task on the delta. Including the prior deliverable lets the
        // agent actually REVISE (keep what was right, change what was flagged)
        // rather than blindly re-run.
        let comment = body.comment.clone().unwrap_or_default();
        let prior_output = cluster
            .configmap_data(&format!("kars-mission-output-{name}"))
            .await
            .and_then(|d| d.get("output").cloned())
            .map(|o| o.chars().take(4000).collect::<String>())
            .unwrap_or_default();
        let prior_block = if prior_output.trim().is_empty() {
            String::new()
        } else {
            format!(
                "\n\nYour previous deliverable (revision {revision}) was:\n---\n{prior_output}\n---"
            )
        };
        let revised = format!(
            "{original}{prior_block}\n\nA reviewer reviewed it and requested changes:\n{comment}\n\n\
             Produce a revised deliverable that addresses this feedback. Keep what was already \
             correct; change only what the feedback asks for.",
        );
        cluster
            .redrive_with_revision(&ns, &name, &revised)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
        ("changes_requested".to_string(), revision + 1, true)
    };

    // Persist under optimistic concurrency: append THIS decision to whatever
    // history is currently stored (re-read inside the CAS), so two concurrent
    // reviews can't silently drop each other's decision — the whole point of an
    // audit trail. status/revision reflect this request; originalObjective is
    // preserved-once.
    let cm_name = format!("kars-mission-review-{name}");
    let entry_for_write = entry.clone();
    cluster
        .update_configmap_data(
            &cm_name,
            &[("kars.azure.com/mission-review", name.as_str())],
            |d| {
                let mut hist = parse_history(d);
                hist.push(entry_for_write.clone());
                d.insert(
                    "history".into(),
                    serde_json::to_string(&hist).unwrap_or_else(|_| "[]".into()),
                );
                d.insert("status".into(), status.clone());
                d.insert("revision".into(), new_revision.to_string());
                d.entry("originalObjective".into())
                    .or_insert_with(|| original.clone());
                d.insert("redrivePending".into(), redrive_pending.to_string());
                d.insert("assignmentNonce".into(), reviewed_assignment.clone());
            },
        )
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;

    // Response: reflect this request's decision on top of the pre-read history
    // (the durable record is the CAS-written one above).
    history.push(entry);
    history.sort_by(|a, b| b.decided_at.cmp(&a.decided_at));
    Ok(Json(ReviewStateDto {
        status,
        revision: new_revision,
        history,
        redrive_pending,
        assignment_nonce: Some(reviewed_assignment),
    }))
}
