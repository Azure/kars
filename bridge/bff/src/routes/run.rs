// kars Bridge BFF — mission run (drive a real governed model run, capture it).
//
// This makes a "mission" actually DO something instead of a sandbox sitting
// idle. When a task is launched and its sandbox is Running, this drives a REAL
// model call through the sandbox's secure inference router and captures the
// real assistant output as a durable deliverable + the real token usage as
// telemetry, persisted to a ConfigMap so the result survives.
//
// HARNESS-NEUTRAL BY CONSTRUCTION: the per-pod inference router (:8443) is the
// identical seam EVERY runtime adapter (OpenClaw, Hermes, MAF, …) routes its
// model calls through (design note §2). We drive *that*, not a runtime-specific
// gateway — so this run path is agnostic across harnesses, not OpenClaw-bound.
//
// HONESTY BOUNDARY — what this is and isn't. This endpoint drives a real,
// governed run (content-safety + budget enforced, no key in the agent) and
// captures real output + real tokens. The PRIMARY path is the full autonomous
// agent LOOP (tools + sub-agent delegation), driven HARNESS-AGNOSTICALLY over
// the AGT mesh: `request_mesh_run` stamps the run-requested annotation, a mesh
// peer (the controller's `mesh_peer::task_delivery`) discovers the agent and
// delivers the objective into its native loop, and we await the deliverable.
// The SINGLE-TURN router path below is only a FALLBACK, used when the mesh
// round-trip is unavailable (the controller isn't the mesh-peer leader, the
// relay is down, or the request times out); it is one model turn (no tools /
// sub-agents) and is marked `source: single_turn` so the UI can say so.
// Driving the OpenClaw gateway directly was deliberately rejected as
// harness-specific.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::Serialize;
use serde_json::json;

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
    /// Real token usage from the router response.
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    /// The model that actually served the run.
    pub model: Option<String>,
    /// When the run completed (RFC3339).
    pub finished_at: String,
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
    let now = chrono::Utc::now().to_rfc3339();

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
            finished_at: now,
            error: Some(
                "This mission isn't launched — launch it first so its agent sandbox is running."
                    .into(),
            ),
        }));
    };

    let Some(pod) = cluster.running_pod_for_sandbox(&sandbox).await else {
        return Ok(Json(RunResult {
            ok: false,
            output: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            model: None,
            finished_at: now,
            error: Some(
                "The mission's agent sandbox isn't Running yet. Wait for it to come up, then run."
                    .into(),
            ),
        }));
    };

    // Build the run prompt from the task's real objective + composed
    // instructions (the system prompt the controller materialized).
    let objective = task.spec.objective.clone();
    let instructions = task
        .spec
        .blueprint
        .as_ref()
        .and_then(|b| b.instructions.clone());
    let model = match task
        .spec
        .blueprint
        .as_ref()
        .and_then(|b| b.model.as_ref().map(|m| m.deployment.clone()))
    {
        Some(m) => m,
        // No model pinned on the blueprint → use the cluster's ACTUAL configured
        // default (read from the controller), never a hardcoded guess. This is
        // the same value missions inherit, so the run telemetry reports the real
        // model. Falls back to the controller's stock default only if the
        // cluster is unreadable.
        None => cluster
            .controller_models()
            .await
            .0
            .unwrap_or_else(|| "gpt-4o-mini".to_string()),
    };

    let mut messages = Vec::new();
    if let Some(sys) = instructions {
        messages.push(json!({ "role": "system", "content": sys }));
    }
    messages.push(json!({ "role": "user", "content": objective }));

    let body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": 800,
    });

    let sandbox_ns = format!("kars-{sandbox}");

    // ── Mesh-driven agent run (primary path) ──────────────────────────────
    // Drive the agent's *native loop* (tools + sub-agent delegation) by asking
    // the core controller — a live mesh peer — to deliver the objective over
    // the mesh and capture the reply. This is the harness-neutral path: the
    // Bridge stamps a declarative run-request annotation and reads the durable
    // deliverable the controller writes back. The full agent loop runs inside
    // the sandbox, governed by the AGT `task:execute` policy. Falls back to a
    // single-turn router run below if the controller can't complete the mesh
    // round-trip in time (e.g. it isn't the mesh leader, or the agent isn't
    // discoverable yet).
    if let Ok(nonce) = cluster.request_mesh_run(&ns, &name).await {
        use crate::kars::cluster::MeshRunOutcome;
        match cluster
            .await_mesh_run(&ns, &name, &nonce, std::time::Duration::from_secs(200))
            .await
        {
            MeshRunOutcome::Completed(out) => {
                let output = out.get("output").cloned();
                let status_ok = out.get("status").map(|s| s == "ok").unwrap_or(false);
                let finished_at = out
                    .get("finishedAt")
                    .cloned()
                    .unwrap_or_else(|| now.clone());
                let served_model = out.get("model").cloned().unwrap_or_else(|| model.clone());
                // The controller records real token usage on the mesh deliverable —
                // parse it instead of reporting null, so the efficiency scorecard and
                // receipt telemetry reflect the PRIMARY execution path, not zeros.
                let parse_tok = |k: &str| out.get(k).and_then(|v| v.parse::<i64>().ok());
                // The mesh deliverable is the agent loop's real reply. `status: ok`
                // means the controller got a `task_response`; the body may still be
                // an agent-side error (surfaced verbatim, never masked).
                return Ok(Json(RunResult {
                    ok: status_ok && output.as_deref().map(|s| !s.is_empty()).unwrap_or(false),
                    output,
                    prompt_tokens: parse_tok("promptTokens"),
                    completion_tokens: parse_tok("completionTokens"),
                    total_tokens: parse_tok("totalTokens"),
                    model: Some(served_model),
                    finished_at,
                    error: None,
                }));
            }
            MeshRunOutcome::InProgress => {
                // The mesh peer acknowledged and the agent loop is STILL running
                // (it can run for many minutes). Do NOT single-turn — the
                // controller will write the real deliverable when it finishes,
                // and the UI's live refresh + Activity stream surface it landing.
                // Racing it with a single-turn write here is the double-write bug.
                tracing::info!(task = %name, "mesh run in progress past the sync window — returning in-progress (no single-turn)");
                return Ok(Json(RunResult {
                    ok: true,
                    output: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    model: Some(model.clone()),
                    finished_at: now,
                    error: None,
                }));
            }
            MeshRunOutcome::NeverProcessed => {
                // The request may be waiting behind authority cleanup, admission,
                // or agent registration. Preserve the nonce and report pending;
                // clearing it here silently cancels healthy gated work and races a
                // late controller claim. A one-shot mission must stay on its real
                // harness path rather than degrade into an unrelated single turn.
                tracing::info!(
                    task = %name,
                    "mesh run not acknowledged inside the sync window — preserving pending request"
                );
                return Ok(Json(RunResult {
                    ok: true,
                    output: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    model: Some(model.clone()),
                    finished_at: now,
                    error: None,
                }));
            }
        }
    }

    // ── Single-turn router run (fallback) ─────────────────────────────────
    // Drive a real, governed model run through the sandbox's inference router
    // (:8443) via the pods/proxy subresource. The router is the trusted in-pod
    // path and requires no caller auth, so pods/proxy can reach it. This is a
    // single model turn (no agent tools/delegation) — used only when the mesh
    // path above is unavailable, so the button always produces a real result.
    let raw = match cluster.router_chat(&sandbox_ns, &pod, &body).await {
        Ok(t) => t,
        Err(e) => {
            return Ok(Json(run_failed(
                now,
                model,
                format!("inference failed: {e}"),
            )));
        }
    };

    // Parse the OpenAI-shape response from the router.
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or(json!({}));
    let output = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    let usage = parsed.get("usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64());
    let completion_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64());
    let total_tokens = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_i64());
    let served_model = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
        .unwrap_or(model);

    let ok = output.is_some();
    if !ok {
        // The router answered but not with a completion (e.g. a safety block or
        // an upstream error) — surface the raw payload honestly, truncated.
        let detail = raw.chars().take(400).collect::<String>();
        return Ok(Json(RunResult {
            ok: false,
            output: None,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            model: Some(served_model),
            finished_at: now,
            error: Some(format!("The router returned no completion. Raw: {detail}")),
        }));
    }

    // Persist the deliverable + telemetry as a durable artifact record.
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("output".into(), output.clone().unwrap_or_default());
    data.insert("objective".into(), task.spec.objective.clone());
    data.insert("model".into(), served_model.clone());
    data.insert("finishedAt".into(), now.clone());
    // Mark this as a SINGLE-TURN completion so the UI can be honest that it is
    // one model turn (no tools / sub-agents) — the mesh agent loop was
    // unavailable. The mesh path leaves this unset (⇒ full loop, the default).
    data.insert("source".into(), "single_turn".into());
    if let Some(t) = total_tokens {
        data.insert("totalTokens".into(), t.to_string());
    }
    if let Some(t) = prompt_tokens {
        data.insert("promptTokens".into(), t.to_string());
    }
    if let Some(t) = completion_tokens {
        data.insert("completionTokens".into(), t.to_string());
    }
    // Persist the deliverable + telemetry as a durable artifact record. If this
    // fails the deliverable would vanish on the next page load, so report the
    // failure honestly instead of returning a green "ok" for work that wasn't
    // saved — the user can re-run rather than believe a phantom result landed.
    if let Err(e) = cluster.write_mission_output(&name, data).await {
        tracing::warn!(task = %name, "failed to persist mission output: {e}");
        return Ok(Json(RunResult {
            ok: false,
            output,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            model: Some(served_model),
            finished_at: now,
            error: Some(format!(
                "The model produced a result but the Bridge could not save it ({e}). Please re-run."
            )),
        }));
    }

    Ok(Json(RunResult {
        ok: true,
        output,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        model: Some(served_model),
        finished_at: now,
        error: None,
    }))
}

/// Build a failed `RunResult` with an honest error message.
fn run_failed(finished_at: String, model: String, error: String) -> RunResult {
    RunResult {
        ok: false,
        output: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        model: Some(model),
        finished_at,
        error: Some(error),
    }
}
