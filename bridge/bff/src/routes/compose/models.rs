use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ComposeRequest {
    pub objective: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeModel {
    pub provider: String,
    pub deployment: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeEgress {
    pub host: String,
    pub port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ComposeDelegationRole {
    pub name: String,
    pub objective: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ComposeDelegation {
    pub mode: String,
    pub roles: Vec<ComposeDelegationRole>,
    pub max_parallel: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeProposal {
    pub tier: i32,
    pub model: Option<ComposeModel>,
    /// Ordered routes that independently qualify the complete Mission package.
    pub model_fallbacks: Vec<ComposeModel>,
    /// Plain-language basis for the model choice, so the reviewer sees WHY this
    /// model was proposed — the learned efficiency frontier, the orchestrator's
    /// objective-driven pick, or the cluster default. Never fabricated.
    pub model_basis: Option<String>,
    pub runtime: String,
    pub instructions: String,
    pub tool_policy: Option<String>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub egress: Vec<ComposeEgress>,
    pub isolation: String,
    pub memory: Option<String>,
    pub budget_tokens: Option<i64>,
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
    pub delegation: ComposeDelegation,
}

#[derive(Debug, Serialize)]
pub struct ComposeResponse {
    /// Whether the orchestrator is configured + reachable on this deployment.
    pub available: bool,
    /// Why it isn't available (only set when `available` is false), so the UI
    /// can explain the manual-composer fallback honestly.
    pub reason: Option<String>,
    /// The composed, validated launch package — ready to review and edit.
    pub proposal: Option<ComposeProposal>,
    /// A short, plain-language rationale for the choices (for the reviewer).
    pub rationale: Option<String>,
    /// The model that composed the package (provenance).
    pub source: Option<String>,
}
