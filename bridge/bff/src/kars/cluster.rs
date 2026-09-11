// kars Bridge BFF — cluster access.
//
// The BFF is the only process that holds a cluster client. The browser never
// receives kube credentials. For local dev the client is built from the
// ambient kubeconfig; in-cluster it uses the mounted ServiceAccount.

use kube::Client;

use super::credentials;

mod configuration;
mod connections;
mod engineering_sources;
mod local_inference;
mod mission_records;
mod mission_runs;
mod orchestrator;
mod providers;
mod resources;
mod sandboxes;

#[derive(Debug, Clone)]
pub struct MissionOutputRecord {
    pub task_name: String,
    pub evidence_key: String,
    pub data: std::collections::BTreeMap<String, String>,
}

/// A handle to the kars cluster, scoped per request to a namespace.
#[derive(Clone)]
pub struct Cluster {
    pub(super) client: Client,
}

/// Outcome of awaiting a mesh-driven run (`Cluster::await_mesh_run`).
pub enum MeshRunOutcome {
    /// The controller stamped `run-completed` and the deliverable is written.
    Completed(std::collections::BTreeMap<String, String>),
    /// The mesh peer acknowledged (`run-ack`) and is actively delivering, but
    /// hasn't finished within the wait — the caller must NOT single-turn (that
    /// would race the controller's deliverable write); the result lands async.
    InProgress,
    /// No `run-ack` appeared — the mesh peer never picked this up (no lease
    /// holder / relay down), so a single-turn fallback is safe.
    NeverProcessed,
}

/// A running agent's mesh identity, discovered from the AGT registry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentIdentity {
    /// The agent's decentralized mesh identifier, e.g. `did:mesh:<hash>`.
    pub did: String,
    /// Capabilities the agent advertises (includes the sandbox name + e.g.
    /// `task-execution`, `kars-agent`).
    pub capabilities: Vec<String>,
    /// Last time the registry saw the agent (RFC3339), when reported.
    pub last_seen: Option<String>,
    /// The agent's mesh reputation score, when reported.
    pub reputation_score: Option<f64>,
}

/// Honest, status-derived health of an agent's running pod (no metrics-server /
/// CPU-mem dependency). Answers "is this agent healthy right now".
#[derive(Debug, Clone, serde::Serialize)]
pub struct PodHealth {
    /// Ready containers vs total (e.g. 2/2). Fewer-than-total means degraded.
    pub ready_containers: i32,
    pub total_containers: i32,
    /// Cumulative container restarts — a crash-loop signal.
    pub restarts: i32,
    /// Seconds since the pod started (uptime).
    pub uptime_seconds: Option<i64>,
    /// The node the pod is scheduled on.
    pub node: Option<String>,
    /// A container's waiting reason (e.g. CrashLoopBackOff, ImagePullBackOff)
    /// when one isn't running — the honest unhealthy signal.
    pub waiting_reason: Option<String>,
}

/// Per-container state for the run-failure troubleshooter.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContainerState {
    pub name: String,
    pub ready: bool,
    pub restarts: i32,
    /// running / waiting / terminated / unknown.
    pub state: String,
    /// The waiting or terminated reason (ImagePullBackOff, OOMKilled, …).
    pub reason: Option<String>,
}

/// One `ModelDeployment.status.conditions[]` entry, browser-facing.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DeployCondition {
    #[serde(rename = "type")]
    pub cond_type: String,
    pub status: String,
    pub reason: String,
    pub message: String,
}

/// Live state of one KAITO pod backing a local model deployment.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DeployPodState {
    pub name: String,
    pub phase: String,
    pub ready: bool,
    pub running: bool,
    pub waiting_reason: Option<String>,
    pub waiting_message: Option<String>,
}

/// One real Kubernetes Event — an entry in the live activity feed.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DeployActivity {
    pub time: Option<String>,
    pub reason: String,
    pub message: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub count: i32,
}

/// The full live status the deploy tracker renders.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LocalDeployLiveStatus {
    pub name: String,
    pub found: bool,
    pub phase: Option<String>,
    pub message: Option<String>,
    pub percent: u8,
    pub ready: bool,
    pub failed: bool,
    pub failure_reason: Option<String>,
    pub failure_message: Option<String>,
    pub replicas_desired: u32,
    pub replicas_ready: u32,
    pub conditions: Vec<DeployCondition>,
    pub pods: Vec<DeployPodState>,
    pub activities: Vec<DeployActivity>,
}

/// Namespace the Bridge creates its own `ModelDeployment` objects in — never
/// the operator's `default` or the AI Runway/KAITO system namespaces, so
/// listing is naturally scoped to what the Bridge itself manages.
pub const LOCAL_INFERENCE_NAMESPACE: &str = "kars-local-inference";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GpuNodeSummary {
    pub gpu_node_count: u32,
    pub gpu_products: Vec<String>,
}

impl Cluster {
    #[cfg(test)]
    pub(crate) fn for_test_client(client: Client) -> Self {
        Self { client }
    }

    /// Build a cluster handle from the ambient configuration (kubeconfig
    /// locally, in-cluster ServiceAccount in production). Returns `None`-style
    /// errors as `anyhow` so the readiness probe can report honest status.
    pub async fn connect() -> anyhow::Result<Self> {
        let client = Client::try_default().await?;
        Ok(Self { client })
    }
}

#[cfg(test)]
mod provider_tests;
