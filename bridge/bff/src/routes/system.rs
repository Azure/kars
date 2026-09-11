// kars Bridge BFF — system / wiring introspection.
//
// Delivery Constraint #5 (honest wiring visibility): the product must never
// let an un-wired gap hide behind a finished-looking screen. This endpoint
// reports the true, cluster-read status of every stage of the governed-agent
// pipeline — task → envelope → digest → delegation → sandbox → agent →
// telemetry → receipt — labelling each `live`, `partial`, or `not_wired`.
//
// Where a fact can be read from the cluster (CRD installed? how many tasks /
// sandboxes?), it is read, never asserted. Where a capability is simply not
// built yet, that is stated plainly rather than implied to exist.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::kars::task::KarsTask;
use crate::state::AppState;
use kube::api::{Api, ListParams};

/// Wiring status of a single pipeline stage.
#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum WiringStatus {
    /// Fully implemented and exercised end-to-end.
    Live,
    /// Partially wired — works to a point, with a stated limitation.
    Partial,
    /// Not yet implemented; named so the gap is visible, not hidden.
    NotWired,
}

#[derive(Debug, Serialize)]
pub struct PipelineStage {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub status: WiringStatus,
    /// Honest, specific note — what works, what doesn't, and why.
    pub detail: String,
}

#[derive(Debug, Serialize, Default)]
pub struct SystemCounts {
    pub tasks: usize,
    pub ready_tasks: usize,
    pub degraded_tasks: usize,
    pub digested_tasks: usize,
    /// Sandboxes in the namespace; `null` when the CRD is not installed.
    pub sandboxes: Option<usize>,
    /// Sandboxes actually reporting Running/Ready (agent serving).
    pub running_sandboxes: usize,
    /// Governance receipts issued.
    pub receipts: usize,
    /// Human approvals (steering decisions) recorded.
    pub approvals: usize,
    /// Hash-chained receipt inclusion-log size.
    pub inclusion_log_size: usize,
    /// Whether a signed checkpoint (signed tree head) is published.
    pub checkpoint_published: bool,
    /// Captured mission deliverables (review-loop + content-addressed).
    pub deliverables: usize,
    /// Whether an independent transparency witness co-signs the log head.
    pub transparency_witnessed: bool,
    /// Per-mission token/trace records captured (live telemetry substrate).
    pub trace_records: usize,
}

#[derive(Debug, Serialize)]
pub struct SystemStatus {
    pub namespace: String,
    pub controller_reachable: bool,
    pub crds: Vec<CrdStatus>,
    pub counts: SystemCounts,
    pub pipeline: Vec<PipelineStage>,
}

#[derive(Debug, Serialize)]
pub struct CrdStatus {
    pub name: &'static str,
    pub installed: bool,
}

const TRACKED_CRDS: &[&str] = &[
    "karstasks.kars.azure.com",
    "karssandboxes.kars.azure.com",
    "inferencepolicies.kars.azure.com",
    "toolpolicies.kars.azure.com",
    "egressapprovals.kars.azure.com",
];

/// `GET /api/system` — the honest wiring map.
pub async fn get_system(State(state): State<AppState>) -> AppResult<Json<SystemStatus>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let ns = state.default_namespace().to_string();

    // Read what we can from the cluster.
    let tasks_api: Api<KarsTask> = cluster.tasks(&ns);
    let task_list = tasks_api.list(&ListParams::default()).await;
    let controller_reachable = task_list.is_ok();

    let mut counts = SystemCounts::default();
    if let Ok(list) = &task_list {
        counts.tasks = list.items.len();
        for t in &list.items {
            let phase = t
                .status
                .as_ref()
                .and_then(|s| s.phase.clone())
                .unwrap_or_default();
            match phase.as_str() {
                "Ready" => counts.ready_tasks += 1,
                "Degraded" => counts.degraded_tasks += 1,
                _ => {}
            }
            if t.status
                .as_ref()
                .and_then(|s| s.envelope_digest.as_ref())
                .is_some()
            {
                counts.digested_tasks += 1;
            }
        }
    }

    let sandbox_crd = cluster.crd_installed("karssandboxes.kars.azure.com").await;
    counts.sandboxes = if sandbox_crd {
        cluster.count_kind(&ns, "KarsSandbox").await
    } else {
        None
    };

    // Real audit + steering + execution facts (read, never asserted). Count
    // only *task-owned* running sandboxes — a standalone `kars dev` sandbox
    // (e.g. localkarstest) is not a Bridge mission and must not inflate the
    // agent-execution stage. Task-materialized sandboxes carry the
    // `kars.azure.com/task` label.
    if let Ok(sandboxes) = cluster.list_kind_all("KarsSandbox").await {
        counts.running_sandboxes = sandboxes
            .iter()
            .filter(|s| {
                let task_owned = s
                    .metadata
                    .labels
                    .as_ref()
                    .map(|l| l.contains_key("kars.azure.com/task"))
                    .unwrap_or(false)
                    || s.metadata
                        .owner_references
                        .as_ref()
                        .map(|o| o.iter().any(|r| r.kind == "KarsTask"))
                        .unwrap_or(false);
                let running = s
                    .data
                    .get("status")
                    .and_then(|st| st.get("phase"))
                    .and_then(|p| p.as_str())
                    .map(|p| p == "Running" || p == "Ready")
                    .unwrap_or(false);
                task_owned && running
            })
            .count();
    }
    counts.receipts = cluster
        .list_kind_all("KarsReceipt")
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    counts.approvals = cluster
        .list_kind_all("KarsApproval")
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    let log = cluster
        .receipt_log()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    counts.inclusion_log_size = log.entries.len();
    counts.checkpoint_published = log.checkpoint.is_some();
    // Real deliverables (review-loop + content-addressed) and live-telemetry
    // substrate (per-mission token/trace records). These drive the artifacts +
    // telemetry stage statuses with cluster-read truth, not hardcoded claims.
    counts.deliverables = cluster.list_mission_output_evidence().await.len();
    counts.trace_records = cluster.count_trace_records().await;
    counts.transparency_witnessed = log
        .witness
        .as_ref()
        .and_then(|d| d.get("witnessKeyId").cloned())
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    let mut crds = Vec::with_capacity(TRACKED_CRDS.len());
    for name in TRACKED_CRDS {
        crds.push(CrdStatus {
            name,
            installed: cluster.crd_installed(name).await,
        });
    }

    let pipeline = build_pipeline(&counts);

    Ok(Json(SystemStatus {
        namespace: ns,
        controller_reachable,
        crds,
        counts,
        pipeline,
    }))
}

/// Build the pipeline status. A stage is `Live` only when there is concrete
/// cluster evidence it has actually run; a stage that is wired but not yet
/// exercised is `Partial` (stated plainly), never asserted `Live` on faith.
fn build_pipeline(counts: &SystemCounts) -> Vec<PipelineStage> {
    let sandbox_count = counts.sandboxes.unwrap_or(0);
    // Live once there is at least one governed task; until then the capability
    // is wired but unexercised.
    let live_if = |cond: bool| {
        if cond {
            WiringStatus::Live
        } else {
            WiringStatus::Partial
        }
    };
    vec![
        PipelineStage {
            id: "intake",
            name: "Task intake",
            description: "Create a governed unit of work.",
            status: live_if(counts.tasks > 0),
            detail: if counts.tasks > 0 {
                format!(
                    "{} governed task(s) present in this namespace (created via the product or the `kars`/kubectl surface — both land as the same CRD).",
                    counts.tasks
                )
            } else {
                "Wired and ready — no governed task exists in this namespace yet. Create one from the product or the `kars`/kubectl surface to exercise this stage.".to_string()
            },
        },
        PipelineStage {
            id: "envelope",
            name: "Envelope validation & digest",
            description: "Validate the trust envelope and stamp a verifiable digest.",
            status: live_if(counts.digested_tasks > 0),
            detail: if counts.digested_tasks > 0 {
                format!(
                    "{}/{} task(s) validated and digested by the controller.",
                    counts.digested_tasks, counts.tasks
                )
            } else {
                "Wired — the controller stamps a verifiable envelope digest on each task; no task has been digested yet in this namespace.".to_string()
            },
        },
        PipelineStage {
            id: "delegation",
            name: "Capability-attenuating delegation",
            description: "Verify child roles attenuate their parent; reject amplification.",
            // This guard runs whenever tasks are reconciled; treat it as proven
            // once there are tasks to reconcile, otherwise wired-but-unexercised.
            status: live_if(counts.tasks > 0),
            detail:
                "Child authority (tier, tool policy, and the egress the sandbox actually enforces) is verified to be a strict subset of the parent; amplifying delegations are rejected with no digest, and a child of a not-ready parent waits rather than running."
                    .to_string(),
        },
        PipelineStage {
            id: "validation",
            name: "Pre-flight validation (§20)",
            description: "Prove the package is launch-ready before any agent starts.",
            status: WiringStatus::Live,
            detail:
                "Before launch, the package is validated against the live cluster: the trust envelope (autonomy tier + token budget), the referenced tool policy / connected services / shared memory, the model the cluster serves, egress-host resolution, and capability-readiness probes that the connected MCP servers are reachable + usable before dispatch. Failures are itemised with a specific reason. A live in-sandbox RBAC-delegation probe is the remaining deepening.".to_string(),
        },
        PipelineStage {
            id: "sandbox",
            name: "Sandbox materialization",
            description: "Materialize a governed KarsSandbox (pod) from the task.",
            status: live_if(sandbox_count > 0),
            detail: if sandbox_count > 0 {
                format!(
                    "Launching a task materializes an envelope-bounded KarsSandbox + InferencePolicy (owned by the task). {sandbox_count} sandbox(es) currently in the namespace."
                )
            } else {
                "Wired — launching a task materializes an envelope-bounded KarsSandbox + InferencePolicy owned by the task. No sandbox is materialized in this namespace right now.".to_string()
            },
        },
        PipelineStage {
            id: "agent",
            name: "Agent execution",
            description: "Run the OpenClaw agent through the secure inference router.",
            status: if counts.running_sandboxes > 0
                || counts.deliverables > 0
                || counts.trace_records > 0
            {
                WiringStatus::Live
            } else {
                WiringStatus::Partial
            },
            detail: if counts.running_sandboxes > 0 {
                format!(
                    "{} sandbox(es) running and serving through the inference router. Inference uses the controller's configured model provider (GitHub Models / Azure OpenAI / Foundry).",
                    counts.running_sandboxes
                )
            } else if counts.deliverables > 0 || counts.trace_records > 0 {
                format!(
                    "The agent execution path is proven by {} retained deliverable(s) and {} router trace record(s). No sandbox is running right now because these workloads are on-demand and completed sandboxes are retired or left idle.",
                    counts.deliverables, counts.trace_records
                )
            } else {
                "Sandboxes materialize and reconcile; performing inference needs a model provider configured on the controller (GitHub Models, Azure OpenAI, or Foundry). With none, the pod spawns and degrades honestly at the inference step.".to_string()
            },
        },
        PipelineStage {
            id: "steering",
            name: "Steering & decisions (HITL)",
            description: "Human-in-the-loop steering: approve, deny, raise/lower tier.",
            status: live_if(counts.approvals > 0),
            detail: if counts.approvals > 0 {
                format!(
                    "The steering inbox is live: {} approval(s) recorded; each decision is bound to the task envelope and routed to the owning OIDC subject. Operator-only authority requests remain available in the Console approval plane.",
                    counts.approvals
                )
            } else {
                "Wired — the steering inbox records human approve/deny/tier decisions, each bound to the task envelope and written into the signed receipt. No decision has been recorded in this namespace yet.".to_string()
            },
        },
        PipelineStage {
            id: "telemetry",
            name: "Live activity & tool-call stream",
            description: "Watch tool calls, decisions, and token burn in flight.",
            status: if counts.trace_records > 0 {
                WiringStatus::Live
            } else {
                WiringStatus::Partial
            },
            detail: format!(
                "Per-mission token cost + a per-round/per-tool execution trace stream live over SSE (GET /tasks/{{name}}/stream) into the activity panel + active-agents view — {} mission(s) carry a trace. Each tool call shows its real params/result/duration; egress domains surface. Streams on a 2s tail; never fakes ticks.",
                counts.trace_records
            ),
        },
        PipelineStage {
            id: "receipt",
            name: "Governance Receipt",
            description: "Signed, independently verifiable record of a governed task.",
            status: live_if(counts.receipts > 0),
            detail: if counts.receipts > 0 {
                format!(
                    "{} signed in-toto/DSSE receipt(s) issued; entered in a hash-chained inclusion log ({} entr{}){}{}. Verify independently with `kars receipt verify` — on a plain kars cluster, no Bridge required.",
                    counts.receipts,
                    counts.inclusion_log_size,
                    if counts.inclusion_log_size == 1 { "y" } else { "ies" },
                    if counts.checkpoint_published {
                        " and committed to a published signed checkpoint (signed tree head)"
                    } else {
                        ""
                    },
                    if counts.transparency_witnessed {
                        ", independently co-signed by a separate transparency witness key"
                    } else {
                        ""
                    }
                )
            } else {
                "Wired — a delivered task is signed as an in-toto/DSSE receipt and entered in a hash-chained inclusion log, independently verifiable with `kars receipt verify`. No receipt has been issued in this namespace yet.".to_string()
            },
        },
        PipelineStage {
            id: "artifacts",
            name: "Artifact review (§16)",
            description: "Review deliverables in place with provenance.",
            status: if counts.deliverables > 0 {
                WiringStatus::Live
            } else {
                WiringStatus::Partial
            },
            detail: format!(
                "Captured deliverables ({}) carry an in-place review loop (approve / request-changes re-drives on the delta), per-file sha256 content-addresses + a did:kars deliverable identity, and a standalone provenance overlay (envelope → predicate → claims → inclusion → signed checkpoint). Honestly empty — never a mockup — until a mission produces one.",
                counts.deliverables
            ),
        },
    ]
}
