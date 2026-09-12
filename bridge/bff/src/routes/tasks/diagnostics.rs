use axum::Json;
use axum::extract::{Extension, Path, State};

use crate::auth::Principal;
use crate::error::AppResult;
use crate::routes::ownership::require_owned_task_or_output;
use crate::state::AppState;

use super::require_cluster;

#[derive(serde::Serialize)]
pub struct TroubleshootDto {
    /// Whether a sandbox pod was found for this run at all.
    pub pod_found: bool,
    /// Ready containers vs total (e.g. "2/2") when a pod exists.
    pub pod_summary: Option<String>,
    /// Per-container state (name, ready, restarts, running/waiting/terminated).
    pub containers: Vec<crate::kars::cluster::ContainerState>,
    /// The tail of the agent container's REAL logs — the ground-truth evidence.
    pub agent_log_tail: Vec<String>,
    /// The specific log/status lines that matched a known failure signature —
    /// the smoking gun, highlighted for the reader.
    pub evidence: Vec<String>,
    /// Plain-language cause + remedy, derived from the REAL evidence above.
    pub cause: String,
    pub remedy: String,
    /// True when the harness itself is the problem (a chat-gateway on a one-shot
    /// mission) — the UI steers the re-compose to OpenClaw.
    pub harness_issue: bool,
    /// The recorded run status/reason, for cross-reference.
    pub result_status: Option<String>,
    pub result_reason: Option<String>,
}

/// `GET /api/namespaces/:ns/tasks/:name/troubleshoot` — actually troubleshoot a
/// run by reading the sandbox pod's real container states + agent logs and
/// diagnosing from that ground truth (not by pattern-matching a status string).
pub async fn troubleshoot_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<TroubleshootDto>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task_or_output(cluster, &ns, &name, &principal).await?;
    // Resolve the sandbox: for a mission the sandbox is named after the task.
    // Fall back to the task's recorded sandbox reference when present.
    let sandbox = task
        .and_then(|t| t.status.and_then(|s| s.sandbox_ref).map(|r| r.name))
        .unwrap_or_else(|| name.clone());

    let logs = cluster
        .read_sandbox_logs(&sandbox, "agent", 120)
        .await
        .unwrap_or_default();
    let containers = cluster.sandbox_container_states(&sandbox).await;
    let health = cluster.sandbox_pod_health(&sandbox).await;
    let output = cluster.read_mission_output(&name).await;
    let result_status = output.as_ref().and_then(|d| d.get("status").cloned());
    let result_reason = output.as_ref().and_then(|d| d.get("output").cloned());

    let log_lines: Vec<String> = logs.lines().map(|s| s.to_string()).collect();
    let (cause, remedy, harness_issue, evidence) =
        diagnose_run_failure(&log_lines, &containers, result_reason.as_deref());

    // Keep the last ~30 log lines for the "raw evidence" view.
    let agent_log_tail: Vec<String> = log_lines.iter().rev().take(30).rev().cloned().collect();

    Ok(Json(TroubleshootDto {
        pod_found: !containers.is_empty() || health.is_some(),
        pod_summary: health
            .as_ref()
            .map(|h| format!("{}/{}", h.ready_containers, h.total_containers)),
        containers,
        agent_log_tail,
        evidence,
        cause,
        remedy,
        harness_issue,
        result_status,
        result_reason,
    }))
}

/// Diagnose a run failure from REAL evidence: the agent log lines, the container
/// states, and the recorded reason. Returns (cause, remedy, harness_issue,
/// evidence-lines). Signatures are ordered most-specific first.
pub(super) fn diagnose_run_failure(
    log_lines: &[String],
    containers: &[crate::kars::cluster::ContainerState],
    reason: Option<&str>,
) -> (String, String, bool, Vec<String>) {
    let find = |needles: &[&str]| -> Vec<String> {
        log_lines
            .iter()
            .filter(|l| {
                let low = l.to_lowercase();
                needles.iter().any(|n| low.contains(&n.to_lowercase()))
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    // 1. Container-level infrastructure failures (authoritative).
    for c in containers {
        if let Some(r) = c.reason.as_deref() {
            let rl = r.to_lowercase();
            if rl.contains("imagepull") || rl.contains("errimage") {
                return (
                    format!("The “{}” container can't pull its image ({r}).", c.name),
                    "This is an infrastructure issue — the image tag is missing or the registry is unreachable. An operator should check the image reference and ACR/registry access.".into(),
                    false,
                    vec![format!("container {} is {} ({r})", c.name, c.state)],
                );
            }
            if rl.contains("crashloop") {
                return (
                    format!("The “{}” container is crash-looping (restarted {} times).", c.name, c.restarts),
                    "The container starts and immediately exits. Check the agent logs below for the panic/exit reason; often a bad config, missing secret, or an incompatible image.".into(),
                    false,
                    find(&["error", "panic", "fatal", "exited", "traceback"]),
                );
            }
            if rl.contains("oomkill") {
                return (
                    format!("The “{}” container was OOM-killed (out of memory).", c.name),
                    "The run exceeded the sandbox memory limit. Reduce the working set or raise the sandbox resources.".into(),
                    false,
                    vec![format!("container {} terminated: OOMKilled", c.name)],
                );
            }
        }
    }

    // 2. Hermes chat-gateway idle — the exact evidence from the entrypoint.
    let hermes = find(&[
        "no channels",
        "idle daemon mode",
        "no messaging platforms enabled",
        "gateway in idle",
    ]);
    if !hermes.is_empty() {
        return (
            "The agent is running on the Hermes chat-gateway harness, which started in IDLE DAEMON MODE because no messaging channels are configured. It is waiting for inbound messages (Telegram/Slack/…) and never executes a one-shot autonomous mission — so the run produced nothing and timed out.".into(),
            "Re-compose this mission on the OpenClaw harness (built for autonomous missions). Hermes only fits work that is DRIVEN by a chat channel.".into(),
            true,
            hermes,
        );
    }

    // 3. Content safety / auth / rate limit from logs.
    let safety = find(&[
        "content safety",
        "jailbreak",
        "blocked by policy",
        "content_filter",
    ]);
    if !safety.is_empty() {
        return (
            "A content-safety policy blocked the run.".into(),
            "Adjust the objective to avoid the flagged content, or ask an operator about the content-safety floor.".into(),
            false,
            safety,
        );
    }
    let auth = find(&[
        "401 unauthorized",
        "403 forbidden",
        "authentication failed",
        "invalid api key",
    ]);
    if !auth.is_empty() {
        return (
            "The agent's model calls were rejected by the provider (authentication/authorization).".into(),
            "An operator should check the router's provider credentials / workload-identity role for this model.".into(),
            false,
            auth,
        );
    }
    let rate = find(&["429", "rate limit", "too many requests", "quota"]);
    if !rate.is_empty() {
        return (
            "The model provider rate-limited or quota-limited the run.".into(),
            "Re-run after a short wait, or an operator can raise the model deployment's quota."
                .into(),
            false,
            rate,
        );
    }
    let schema = find(&[
        "stream_options.include_usage",
        "unknown parameter: 'stream_options",
        "stream_options: extra inputs",
    ]);
    if !schema.is_empty() {
        return (
            "The selected model rejected the translated inference request before it could reason or call tools.".into(),
            "This is a model/router compatibility issue, not an egress or prompt problem. Deploy the corrected inference router, then re-run the same mission; selecting another catalogue model is only a temporary workaround.".into(),
            false,
            schema,
        );
    }

    // 4. Fall back to the recorded reason.
    let rl = reason.unwrap_or("").to_lowercase();
    if rl.contains("did not come online")
        || rl.contains("not yet discoverable")
        || rl.contains("mesh registry")
    {
        return (
            "The agent never registered on the encrypted mesh within the startup window, so the controller timed the run out.".into(),
            "Re-run it — a fresh sandbox often comes up cleanly. If it repeats, check the agent logs below and the sandbox events.".into(),
            false,
            find(&["mesh", "relay", "register", "keepalive"]),
        );
    }
    if rl.contains("no progress heartbeat") || rl.contains("timed out") || rl.contains("timeout") {
        return (
            "The agent started but stopped making progress, so the controller timed the run out."
                .into(),
            "Re-run it; if it stalls again, narrow the objective or raise the token/time budget."
                .into(),
            false,
            find(&["error", "timeout", "stalled"]),
        );
    }

    (
        "The run ended without producing a deliverable. See the agent's own logs below for the specifics.".into(),
        "Re-run it, or re-compose with a different harness/model. If the logs show a repeating error, address that first.".into(),
        false,
        find(&["error", "panic", "fatal", "exception"]),
    )
}
