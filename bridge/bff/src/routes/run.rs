// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Mission execution is a durable request for a Core dispatcher, never a BFF
// model call. Missing ACKs and ambiguous writes do not authorize fallback work.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::Serialize;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::KarsTask;
use crate::routes::ownership::require_owned_task;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub ok: bool,
    /// The agent's produced output (the deliverable text).
    pub output: Option<String>,
    /// Measured token usage from the bound agent completion record.
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    /// The model that actually served the run.
    pub model: Option<String>,
    /// When the run completed (RFC3339).
    pub finished_at: Option<String>,
    /// Honest error detail when ok=false.
    pub error: Option<String>,
}

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

/// `POST /api/namespaces/:ns/tasks/:name/run` — drive a real mission run.
pub async fn run_mission(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<RunResult>> {
    let cluster = require_cluster(&state)?;

    // The task must be launched with a Running sandbox to drive a run.
    let task: KarsTask = require_owned_task(cluster, &ns, &name, &principal).await?;

    // Aggregate inference-budget gate (cluster + workspace + user): a run consumes
    // inference tokens, so a strict/over-buffer budget at any tier blocks it. The
    // creator is read from the task's stamped annotation.
    let created_by = task
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("kars.azure.com/created-by").cloned())
        .unwrap_or_else(|| "unattributed".into());
    crate::routes::budgets::enforce_launch_budget(cluster, &ns, &created_by).await?;

    let sandbox = task
        .status
        .as_ref()
        .and_then(|s| s.sandbox_ref.as_ref())
        .map(|r| r.name.clone());
    let Some(sandbox) = sandbox else {
        return Ok(Json(RunResult {
            ok: false,
            output: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            model: None,
            finished_at: None,
            error: Some(
                "This mission isn't launched — launch it first so its agent sandbox is running."
                    .into(),
            ),
        }));
    };

    let Some(_) = cluster.running_pod_for_sandbox(&sandbox).await else {
        return Ok(Json(RunResult {
            ok: false,
            output: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            model: None,
            finished_at: None,
            error: Some(
                "The mission's agent sandbox isn't Running yet. Wait for it to come up, then run."
                    .into(),
            ),
        }));
    };

    let nonce = cluster.request_mesh_run(&task).await?;
    let outcome = cluster
        .await_mesh_run(&task, &nonce, std::time::Duration::from_secs(200))
        .await;
    use crate::kars::cluster::MeshRunOutcome;
    let result = match outcome {
        MeshRunOutcome::Completed(out) => {
            let output = out.get("output").cloned();
            let ok = out.get("status").is_some_and(|s| s == "ok")
                && output.as_deref().is_some_and(|s| !s.trim().is_empty());
            let parse_tok = |key: &str| out.get(key).and_then(|v| v.parse::<i64>().ok());
            RunResult {
                ok,
                output,
                prompt_tokens: parse_tok("promptTokens"),
                completion_tokens: parse_tok("completionTokens"),
                total_tokens: parse_tok("totalTokens"),
                model: out.get("model").filter(|s| !s.is_empty()).cloned(),
                finished_at: out.get("finishedAt").cloned(),
                error: if ok {
                    None
                } else {
                    Some(
                        out.get("error")
                            .filter(|s| !s.is_empty())
                            .cloned()
                            .unwrap_or_else(|| {
                                "The agent did not produce a successful deliverable.".into()
                            }),
                    )
                },
            }
        }
        MeshRunOutcome::InProgress | MeshRunOutcome::NeverProcessed => {
            tracing::info!(task = %name, run = %nonce, "completion not verified; preserving durable request");
            RunResult {
                ok: true,
                output: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                model: None,
                finished_at: None,
                error: None,
            }
        }
    };
    Ok(Json(result))
}
