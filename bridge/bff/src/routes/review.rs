// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
    /// The distinct run requested by feedback, not the run being reviewed.
    pub requested_revision_nonce: Option<String>,
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

pub(super) fn review_matches_output(
    output: &BTreeMap<String, String>,
    review: &BTreeMap<String, String>,
) -> bool {
    ["taskUid", "assignmentNonce"].into_iter().all(|key| {
        output
            .get(key)
            .is_some_and(|value| !value.is_empty() && review.get(key) == Some(value))
    })
}

fn parse_history(data: &BTreeMap<String, String>) -> Vec<ReviewEntry> {
    data.get("history")
        .and_then(|s| serde_json::from_str::<Vec<ReviewEntry>>(s).ok())
        .unwrap_or_default()
}

fn review_state(
    task: &crate::kars::task::KarsTask,
    data: &BTreeMap<String, String>,
    output: Option<&BTreeMap<String, String>>,
) -> ReviewStateDto {
    let bound = task
        .metadata
        .uid
        .as_deref()
        .filter(|s| !s.is_empty())
        .is_some_and(|uid| data.get("taskUid").map(String::as_str) == Some(uid));
    let empty = BTreeMap::new();
    let data = if bound { data } else { &empty };
    let mut history = parse_history(data);
    history.sort_by(|a, b| b.decided_at.cmp(&a.decided_at));
    let assignment_nonce = data.get("assignmentNonce").cloned();
    let requested_revision_nonce = data.get("requestedRevisionNonce").cloned();
    let output_nonce = output
        .filter(|out| out.get("taskUid") == data.get("taskUid"))
        .and_then(|out| out.get("assignmentNonce"));
    let redrive_pending = data.get("redrivePending").is_some_and(|v| v == "true")
        && requested_revision_nonce
            .as_ref()
            .is_none_or(|nonce| Some(nonce) != output_nonce);
    let status = if assignment_nonce
        .as_ref()
        .is_some_and(|nonce| Some(nonce) == output_nonce)
    {
        data.get("status").cloned().unwrap_or_else(|| "none".into())
    } else if redrive_pending {
        "changes_requested".into()
    } else {
        "none".into()
    };
    ReviewStateDto {
        status,
        revision: data
            .get("revision")
            .and_then(|r| r.parse().ok())
            .unwrap_or(0),
        history,
        redrive_pending,
        assignment_nonce,
        requested_revision_nonce,
    }
}

/// `GET /api/namespaces/:ns/tasks/:name/review` — the deliverable's review state.
pub async fn get_review(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<ReviewStateDto>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task_or_output(cluster, &ns, &name, &principal)
        .await?
        .ok_or(AppError::NotFound)?;
    let data = cluster
        .review_snapshot(&ns, &name)
        .await?
        .and_then(|cm| cm.data)
        .unwrap_or_default();
    let output = cluster.read_mission_output(&name).await;
    // Derive pending from exact committed evidence. A GET never rewrites the
    // review journal or races a concurrent reviewer with forced SSA.
    Ok(Json(review_state(&task, &data, output.as_ref())))
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
    let output = cluster.read_mission_output(&name).await.ok_or_else(|| {
        AppError::Conflict(
            "a committed current deliverable is required; reload before reviewing".into(),
        )
    })?;
    let reviewed_assignment = body.assignment_nonce.clone();
    let uid = task
        .metadata
        .uid
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Conflict("mission UID is missing".into()))?;
    let annotations = task.metadata.annotations.clone().unwrap_or_default();
    if ns != "kars-system"
        || reviewed_assignment.is_empty()
        || task.metadata.deletion_timestamp.is_some()
        || output.get("taskUid").map(String::as_str) != Some(uid)
        || output.get("assignmentNonce") != Some(&reviewed_assignment)
        || annotations.get("kars.azure.com/run-requested") != Some(&reviewed_assignment)
        || annotations.get("kars.azure.com/run-completed") != Some(&reviewed_assignment)
    {
        return Err(AppError::Conflict(
            "the deliverable changed since it was displayed; reload before reviewing".into(),
        ));
    }

    // Read prior review state, distinguishing a genuine cluster-read error from
    // "no review yet". Silently defaulting on error would erase the entire
    // review history on the next write (a transient blip = permanent data loss).
    let prior = cluster.review_snapshot(&ns, &name).await?;
    let data = prior
        .as_ref()
        .and_then(|cm| cm.data.clone())
        .unwrap_or_default();
    if prior.is_some() && data.get("taskUid").map(String::as_str) != Some(uid) {
        return Err(AppError::Conflict(
            "the review record belongs to an unbound or different mission; it cannot be reused"
                .into(),
        ));
    }
    let mut history: Vec<ReviewEntry> = match data.get("history") {
        Some(history) => {
            serde_json::from_str(history).map_err(|e| AppError::Upstream(e.to_string()))?
        }
        None => Vec::new(),
    };
    let revision = match data.get("revision") {
        None => 0,
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|revision| *revision >= 0)
            .ok_or_else(|| AppError::Upstream("invalid stored review revision".into()))?,
    };
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

    let (status, new_revision, requested_revision_nonce) = if decision == "approve" {
        ("approved".to_string(), revision, None)
    } else {
        // Compose the revision objective from the original + the previous
        // deliverable + the reviewer's requested changes, then re-drive the
        // producing task on the delta. Including the prior deliverable lets the
        // agent actually REVISE (keep what was right, change what was flagged)
        // rather than blindly re-run.
        let comment = body.comment.clone().unwrap_or_default();
        let prior_output = output
            .get("output")
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
        let new_revision = revision
            .checked_add(1)
            .ok_or_else(|| AppError::Conflict("review revision limit reached".into()))?;
        let nonce = cluster
            .redrive_with_revision(&task, &reviewed_assignment, &revised)
            .await?;
        ("changes_requested".to_string(), new_revision, Some(nonce))
    };

    history.push(entry);
    history.sort_by(|a, b| b.decided_at.cmp(&a.decided_at));
    let redrive_pending = requested_revision_nonce.is_some();
    let mut next = data;
    next.insert("taskUid".into(), uid.into());
    next.insert("status".into(), status.clone());
    next.insert("revision".into(), new_revision.to_string());
    next.insert("originalObjective".into(), original);
    next.insert("redrivePending".into(), redrive_pending.to_string());
    next.insert("assignmentNonce".into(), reviewed_assignment.clone());
    next.insert(
        "history".into(),
        serde_json::to_string(&history).map_err(|e| AppError::Upstream(e.to_string()))?,
    );
    if let Some(nonce) = &requested_revision_nonce {
        next.insert("requestedRevisionNonce".into(), nonce.clone());
    } else {
        next.remove("requestedRevisionNonce");
    }
    // The Task revision may already be committed if this independent journal
    // write fails. Propagate the error; never replay or undo the run request.
    cluster.write_review(&task, prior.as_ref(), next).await?;

    Ok(Json(ReviewStateDto {
        status,
        revision: new_revision,
        history,
        redrive_pending,
        assignment_nonce: Some(reviewed_assignment),
        requested_revision_nonce,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_review_attribution_requires_nonempty_exact_uid_and_run() {
        let output = BTreeMap::from([
            ("taskUid".into(), "task-uid".into()),
            ("assignmentNonce".into(), "run-1".into()),
        ]);
        assert!(review_matches_output(&output, &output));
        for key in ["taskUid", "assignmentNonce"] {
            for value in [None, Some(""), Some("different")] {
                let mut review = output.clone();
                if let Some(value) = value {
                    review.insert(key.into(), value.into());
                } else {
                    review.remove(key);
                }
                assert!(!review_matches_output(&output, &review));
                assert!(!review_matches_output(&review, &output));
                if value.is_none_or(str::is_empty) {
                    assert!(!review_matches_output(&review, &review));
                }
            }
        }
    }
}
