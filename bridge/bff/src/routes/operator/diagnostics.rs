// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::error::AppResult;
use crate::state::AppState;

use super::require_cluster;

// ─── Diagnostics: live "what's actually broken right now" scan ────────────────
// The Troubleshooting page's real job: not a wiring/roadmap checklist, but the
// concrete problems an operator must act on — pods that won't start, containers
// crash-looping or stuck pulling an image, sandboxes the controller marked
// Degraded/Failed, and agents that came up but never went Ready. Every issue is
// read from live pod/CRD status and carries a plain remedy hint.

#[derive(Debug, Serialize)]
pub struct DiagnosticIssue {
    /// "critical" (blocks the workload) or "warning" (degraded but running).
    pub severity: String,
    /// Short machine-ish kind, e.g. "ImagePullBackOff", "CrashLoopBackOff",
    /// "PodPending", "NotReady", "SandboxDegraded", "HighRestarts".
    pub kind: String,
    /// The affected object, `namespace/name`.
    pub subject: String,
    /// The raw reason/phase from the cluster.
    pub reason: String,
    /// Human detail (container message / status message) when available.
    pub detail: Option<String>,
    /// A concrete next step for the operator.
    pub remedy: String,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsDto {
    pub issues: Vec<DiagnosticIssue>,
    pub scanned_pods: usize,
    pub scanned_sandboxes: usize,
    /// True when the scan found nothing wrong — the honest "all clear".
    pub healthy: bool,
}

/// `GET /api/operator/diagnostics` — the live problem scan behind Troubleshooting.
pub async fn get_diagnostics(State(state): State<AppState>) -> AppResult<Json<DiagnosticsDto>> {
    let cluster = require_cluster(&state)?;
    let mut issues: Vec<DiagnosticIssue> = Vec::new();

    // ── Pods: the ground truth for "won't start / not healthy". ──────────────
    let pods = cluster.all_pods().await?;
    let scanned_pods = pods.len();
    for p in &pods {
        let ns = p.metadata.namespace.as_deref().unwrap_or("").to_string();
        let name = p.metadata.name.as_deref().unwrap_or("").to_string();
        let subject = format!("{ns}/{name}");
        let status = p.status.as_ref();
        let phase = status.and_then(|s| s.phase.as_deref()).unwrap_or("");
        let age_secs = status
            .and_then(|s| s.start_time.as_ref())
            .or(p.metadata.creation_timestamp.as_ref())
            .map(|t| (chrono::Utc::now() - t.0).num_seconds().max(0))
            .unwrap_or(0);

        // Container-level waiting reasons (image pull, crashloop, config error).
        let mut container_flagged = false;
        if let Some(pod_status) = status {
            for c in pod_status
                .init_container_statuses
                .iter()
                .flatten()
                .chain(pod_status.container_statuses.iter().flatten())
            {
                if let Some(w) = c.state.as_ref().and_then(|st| st.waiting.as_ref()) {
                    let reason = w.reason.clone().unwrap_or_default();
                    let bad = matches!(
                        reason.as_str(),
                        "ImagePullBackOff"
                            | "ErrImagePull"
                            | "CrashLoopBackOff"
                            | "CreateContainerConfigError"
                            | "CreateContainerError"
                            | "InvalidImageName"
                            | "RunContainerError"
                    );
                    if bad {
                        container_flagged = true;
                        let remedy = match reason.as_str() {
                            "ImagePullBackOff" | "ErrImagePull" | "InvalidImageName" => {
                                "Image can't be pulled — check the image tag exists in the registry and the node has pull access."
                            }
                            "CrashLoopBackOff" | "RunContainerError" => {
                                "Container keeps exiting — check its logs (kubectl logs) for the crash cause."
                            }
                            _ => {
                                "Container config is invalid — check the ConfigMap/Secret mounts and env for this container."
                            }
                        };
                        issues.push(DiagnosticIssue {
                            severity: "critical".into(),
                            kind: reason.clone(),
                            subject: format!("{subject} · {}", c.name),
                            reason,
                            detail: w.message.clone(),
                            remedy: remedy.into(),
                        });
                    }
                }
                // A container restarting many times is a warning even if currently up.
                if c.restart_count >= 5 {
                    issues.push(DiagnosticIssue {
                        severity: "warning".into(),
                        kind: "HighRestarts".into(),
                        subject: format!("{subject} · {}", c.name),
                        reason: format!("{} restarts", c.restart_count),
                        detail: None,
                        remedy:
                            "Container is unstable — inspect its logs for the recurring failure."
                                .into(),
                    });
                }
            }
        }

        // Pod stuck Pending (unschedulable / image / volume) for > 60s.
        if phase == "Pending" && age_secs > 60 && !container_flagged {
            let msg = status
                .and_then(|s| s.conditions.as_ref())
                .and_then(|c| c.iter().find(|cond| cond.status == "False"))
                .and_then(|c| c.message.clone());
            issues.push(DiagnosticIssue {
                severity: "critical".into(),
                kind: "PodPending".into(),
                subject: subject.clone(),
                reason: "Pending".into(),
                detail: msg,
                remedy: "Pod can't be scheduled — check node capacity, taints, or unbound volumes (kubectl describe pod)."
                    .into(),
            });
        }

        // Running but not all containers Ready for > 120s (probes failing).
        if phase == "Running" && age_secs > 120 && !container_flagged {
            let cs = status.and_then(|s| s.container_statuses.as_ref());
            let total = p
                .spec
                .as_ref()
                .map(|s| s.containers.len())
                .unwrap_or_else(|| cs.map_or(0, Vec::len));
            let ready = cs.map_or(0, |cs| cs.iter().filter(|s| s.ready).count());
            if total > 0 && ready < total {
                issues.push(DiagnosticIssue {
                        severity: "warning".into(),
                        kind: "NotReady".into(),
                        subject: subject.clone(),
                        reason: format!("{ready}/{total} containers ready"),
                        detail: None,
                        remedy: "A container is up but failing its readiness probe — check the probe and the container's logs."
                            .into(),
                    });
            }
        }
    }

    // ── Sandboxes the controller itself flagged Degraded/Failed. ─────────────
    let sandboxes = cluster.list_kind_all("KarsSandbox").await?;
    let scanned_sandboxes = sandboxes.len();
    for sb in &sandboxes {
        let phase = sb
            .data
            .get("status")
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .unwrap_or("");
        if matches!(phase, "Degraded" | "Failed") {
            let name = sb.metadata.name.as_deref().unwrap_or("").to_string();
            let msg = sb
                .data
                .get("status")
                .and_then(|s| s.get("message"))
                .and_then(|m| m.as_str())
                .map(String::from);
            issues.push(DiagnosticIssue {
                severity: if phase == "Failed" { "critical" } else { "warning" }.into(),
                kind: "SandboxDegraded".into(),
                subject: format!("kars-system/{name}"),
                reason: phase.to_string(),
                detail: msg,
                remedy: "The controller couldn't fully reconcile this sandbox — check the controller logs and the sandbox's referenced policies/secrets."
                    .into(),
            });
        }
        // Run-level stall detection (audit f24): the pod-level scan is blind to a
        // run whose sandbox is "Running" but whose run has FAILED/timed out. A
        // mission-output recorded with status=error is a definitive run failure
        // the operator must see even though the pod looks healthy.
        if phase == "Running" {
            let name = sb.metadata.name.as_deref().unwrap_or("").to_string();
            if let Some(out) = cluster.read_mission_output(&name).await
                && matches!(
                    out.get("status").map(String::as_str),
                    Some("error" | "failed" | "rejected")
                )
            {
                let detail = out
                    .get("output")
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .or_else(|| out.get("error").cloned());
                issues.push(DiagnosticIssue {
                        severity: "warning".into(),
                        kind: "RunFailed".into(),
                        subject: format!("kars-system/{name}"),
                        reason: "run reported an error while the sandbox is still Running".into(),
                        detail,
                        remedy: "The agent's run did not complete (often a slow/absent agent or a chat-gateway harness that never executed the loop). Check the mission's Run tab, or re-run with the OpenClaw harness for autonomous missions."
                            .into(),
                    });
            }
        }
    }

    // Critical first, then warnings; stable within a severity.
    issues.sort_by(|a, b| {
        let rank = |s: &str| if s == "critical" { 0 } else { 1 };
        rank(&a.severity).cmp(&rank(&b.severity))
    });

    let healthy = issues.is_empty();
    Ok(Json(DiagnosticsDto {
        issues,
        scanned_pods,
        scanned_sandboxes,
        healthy,
    }))
}

// ─── Orchestrator health + the compose failover path ─────────────────────────
// The Bridge composer ("intent → package") runs its own inference. It prefers a
// DIRECT endpoint (BRIDGE_ORCHESTRATOR_* — scales for many teams) and otherwise
// routes through the standing `bridge-orchestrator` sandbox's router. This
// surfaces which path is live, the orchestrator sandbox's health, and — when the
// sandbox path is under strain — recommends configuring the direct endpoint
// (the "switch to inference-based orchestration under load" lever).

#[derive(Debug, Serialize)]
pub struct OrchestratorDto {
    /// Active compose inference path: "direct" (endpoint configured) or
    /// "sandbox" (routing through the orchestrator sandbox router), or "none".
    pub mode: String,
    /// Whether a direct BRIDGE_ORCHESTRATOR endpoint triple is configured.
    pub direct_configured: bool,
    /// Whether the standing orchestrator sandbox exists.
    pub sandbox_present: bool,
    /// The orchestrator sandbox phase (Running/Degraded/…), when present.
    pub sandbox_phase: Option<String>,
    /// Ready/total containers of the orchestrator pod, restarts, waiting reason.
    pub sandbox_ready: Option<String>,
    pub sandbox_restarts: Option<i32>,
    pub sandbox_waiting_reason: Option<String>,
    pub sandbox_node: Option<String>,
    pub sandbox_uptime_seconds: Option<i64>,
    /// How many Running sandbox routers the composer can fall back through.
    pub router_candidates: usize,
    /// True when the operator should configure the direct endpoint (sandbox path
    /// is the only option and it's unhealthy or capacity is thin).
    pub recommend_direct: bool,
    /// Plain-language recommendation.
    pub note: String,
}

/// `GET /api/operator/orchestrator` — orchestrator health + compose failover path.
pub async fn get_orchestrator(State(state): State<AppState>) -> AppResult<Json<OrchestratorDto>> {
    let cluster = require_cluster(&state)?;

    let direct_configured = [
        "BRIDGE_ORCHESTRATOR_ENDPOINT",
        "BRIDGE_ORCHESTRATOR_TOKEN",
        "BRIDGE_ORCHESTRATOR_MODEL",
    ]
    .iter()
    .all(|k| {
        std::env::var(k)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    });

    // Orchestrator sandbox presence + health.
    let sandboxes = cluster.list_kind_all("KarsSandbox").await?;
    let orch = sandboxes.iter().find(|sb| {
        sb.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/orchestrator"))
            .map(String::as_str)
            == Some("true")
    });
    let sandbox_present = orch.is_some();
    let sandbox_phase = orch.and_then(|o| {
        o.data
            .get("status")
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .map(String::from)
    });
    let health = if let Some(o) = orch {
        let name = o.metadata.name.clone().unwrap_or_default();
        cluster.sandbox_pod_health(&name).await?
    } else {
        None
    };
    let router_candidates = cluster.running_sandbox_candidates().await?.len();
    Ok(Json(orchestrator_observation(
        direct_configured,
        sandbox_present,
        sandbox_phase,
        health,
        router_candidates,
    )))
}

fn orchestrator_observation(
    direct_configured: bool,
    sandbox_present: bool,
    sandbox_phase: Option<String>,
    health: Option<crate::kars::cluster::PodHealth>,
    router_candidates: usize,
) -> OrchestratorDto {
    let (sandbox_ready, sandbox_restarts, sandbox_waiting_reason) = match &health {
        Some(h) => (
            Some(format!("{}/{}", h.ready_containers, h.total_containers)),
            Some(h.restarts),
            h.waiting_reason.clone(),
        ),
        None => (None, None, None),
    };

    let sandbox_healthy = sandbox_phase.as_deref() == Some("Running")
        && health
            .as_ref()
            .map(|h| h.ready_containers == h.total_containers && h.total_containers > 0)
            .unwrap_or(false);

    let mode = if direct_configured {
        "direct"
    } else if router_candidates > 0 {
        "sandbox"
    } else {
        "none"
    }
    .to_string();

    let recommend_direct = !direct_configured && router_candidates == 0;

    let note = if direct_configured {
        "Direct inference endpoint configured. Reachability, model credentials and successful composition are not verified by this health read."
            .to_string()
    } else if recommend_direct {
        "No ready sandbox router candidate or direct endpoint is configured. Check pod scheduling and router readiness, or configure BRIDGE_ORCHESTRATOR_{ENDPOINT,TOKEN,MODEL}.".to_string()
    } else if !sandbox_present {
        "The standing orchestrator is absent, but other ready sandbox routers are candidates for composition. Model credentials and successful composition remain unverified.".to_string()
    } else if sandbox_healthy {
        "The orchestrator pod reports all containers ready. Router candidates are available, but pod readiness does not verify model credentials, successful composition or tool execution.".to_string()
    } else {
        "The orchestrator pod is not fully ready. A ready sandbox router candidate is still available, but successful composition remains unverified.".to_string()
    };

    OrchestratorDto {
        mode,
        direct_configured,
        sandbox_present,
        sandbox_phase,
        sandbox_ready,
        sandbox_restarts,
        sandbox_waiting_reason,
        sandbox_node: health.as_ref().and_then(|h| h.node.clone()),
        sandbox_uptime_seconds: health.as_ref().and_then(|h| h.uptime_seconds),
        router_candidates,
        recommend_direct,
        note,
    }
}

// ─── Integrations: kars-SRE agent + Headlamp plugin ──────────────────────────
// kars ships a real Headlamp plugin (tools/headlamp-plugin — /kars/sre and
// /kars/* views) and a real SRE agent (deploy/helm/kars/templates/sre.yaml,
// gated on sre.enabled; `kars sre install`). This surfaces whether each is
// active, deep-links into the existing plugin views, and gives the exact
// activation for what isn't enabled — rather than pretending to integrate.

#[derive(Debug, Serialize)]
pub struct IntegrationsDto {
    /// kars-SRE agent.
    pub sre_present: bool,
    pub sre_phase: Option<String>,
    pub sre_ready: Option<String>,
    /// The `kars sre install` activation command when SRE isn't enabled.
    pub sre_activate_cmd: String,
    /// Headlamp dashboard + kars plugin.
    pub headlamp_deployed: bool,
    pub headlamp_url: Option<String>,
    /// Deep-link paths into the kars Headlamp plugin (appended to headlamp_url).
    pub headlamp_paths: Vec<HeadlampLink>,
    /// How to activate the existing dashboard and its browser-reachable link.
    pub headlamp_install_hint: String,
}

#[derive(Debug, Serialize)]
pub struct HeadlampLink {
    pub label: String,
    pub path: String,
}

/// `GET /api/operator/integrations` — kars-SRE + Headlamp status & deep-links.
pub async fn get_integrations(State(state): State<AppState>) -> AppResult<Json<IntegrationsDto>> {
    let cluster = require_cluster(&state)?;

    // SRE agent: the `sre` KarsSandbox (deploy/helm/kars/templates/sre.yaml).
    let sandboxes = cluster.list_kind_all("KarsSandbox").await?;
    let sre = sandboxes
        .iter()
        .find(|sb| sb.metadata.name.as_deref() == Some("sre"));
    let sre_present = sre.is_some();
    let sre_phase = sre.and_then(|o| {
        o.data
            .get("status")
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .map(String::from)
    });
    let sre_ready = if sre_present {
        cluster
            .sandbox_pod_health("sre")
            .await?
            .map(|h| format!("{}/{}", h.ready_containers, h.total_containers))
    } else {
        None
    };

    // Headlamp: the `headlamp` Deployment in the `headlamp` namespace.
    let headlamp_deployed = cluster.deployment_exists("headlamp", "headlamp").await?;

    Ok(Json(IntegrationsDto {
        sre_present,
        sre_phase,
        sre_ready,
        sre_activate_cmd: "kars sre install   # helm upgrade --reuse-values --set sre.enabled=true".into(),
        headlamp_deployed,
        headlamp_url: std::env::var("BRIDGE_HEADLAMP_URL").ok().filter(|u| !u.trim().is_empty()),
        headlamp_paths: vec![
            HeadlampLink { label: "SRE console".into(), path: "/kars/sre".into() },
            HeadlampLink { label: "Sandboxes".into(), path: "/kars/karssandboxes".into() },
            HeadlampLink { label: "Agent mesh".into(), path: "/kars/mesh".into() },
        ],
        headlamp_install_hint: "Core Helm includes Headlamp with the packaged Kars plugin (headlamp.enabled). Set Bridge headlamp.url to its browser-reachable URL and sign in with your Kubernetes credential. For external Headlamp, disable Core headlamp.enabled and manage the plugin through that installation; do not copy into an immutable pod.".into(),
    }))
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
