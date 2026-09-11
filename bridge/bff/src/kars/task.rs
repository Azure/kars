// kars Bridge BFF — typed view of the `KarsTask` CRD.
//
// CONTRACT OWNERSHIP: the `KarsTask` schema is owned by core kars
// (`Azure/kars`, controller/src/kars_task.rs). This module is a *consumer*
// projection — the standard kube-rs pattern for a client that reads/writes a
// CRD it does not own. It deliberately mirrors only the fields the Bridge UI
// needs, with matching group/version/kind and camelCase serde so the wire
// shape is identical.
//
// One-way dependency (design note §21): Bridge depends on the kars contract,
// never the reverse. When core kars extracts its CRD types into a shared
// library crate, this projection should be replaced by a dependency on that
// crate — until then, the schema contract is the seam.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `KarsTask.spec` — the subset the Bridge reads/writes.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsTask",
    namespaced,
    status = "KarsTaskStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct KarsTaskSpec {
    /// Plain-language statement of the work to be performed.
    pub objective: String,
    /// The trust envelope that governs this task and bounds delegation.
    pub envelope: TaskEnvelope,
    /// Optional parent task (same namespace) this task is a delegated child of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_ref: Option<LocalObjectRef>,
    /// Execution gate — when launched, the controller materializes a sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<TaskExecution>,
    /// The concrete, editable run blueprint reviewed on the launch package —
    /// runtime/model/instructions/tools/MCP/egress/isolation/memory. The
    /// controller compiles it into the InferencePolicy + KarsSandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint: Option<TaskBlueprint>,
    /// Optional short label for listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Per-task retention override, in seconds — mirrors the core contract
    /// (controller/src/kars_task.rs::KarsTaskSpec.retention_ttl_seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_ttl_seconds: Option<i64>,
}

/// `KarsTask.spec.blueprint` — the editable composition. Mirrors the core
/// contract (controller/src/kars_task.rs::TaskBlueprint); every field maps to a
/// real field on the materialized InferencePolicy / KarsSandbox.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskBlueprint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TaskModel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_fallbacks: Vec<TaskModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_bindings: Option<crate::kars::credentials::CredentialBindings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_binding: Option<crate::kars::credential_contract::GitHubBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress: Vec<TaskEgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_write: Option<GitWriteConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_plan: Option<ExecutionPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPlan {
    pub schema: String,
    pub roles: Vec<ExecutionRole>,
    pub max_parallel: i32,
    pub synthesis: ExecutionSynthesis,
    #[serde(default)]
    pub deliverables: Vec<ExecutionDeliverable>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRole {
    pub name: String,
    pub objective: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub phases: Vec<ExecutionPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPhase {
    pub name: String,
    pub objective: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_tool_calls: Vec<ExecutionRequiredToolCall>,
    #[serde(default)]
    pub min_tool_calls: i32,
    pub max_tool_calls: i32,
    #[serde(default)]
    pub fresh_context: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRequiredToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSynthesis {
    pub objective: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub max_tool_calls: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionDeliverable {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitWriteConfig {
    pub connection_config_map_ref: LocalObjectRef,
    #[serde(default)]
    pub repos: Vec<String>,
}

/// A model route: provider tag + deployment name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskModel {
    pub provider: String,
    pub deployment: String,
}

/// A network destination the mission may reach (host + optional port).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskEgress {
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<i32>,
}

/// Execution settings — the launch gate between governed and executing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskExecution {
    #[serde(default)]
    pub launch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// The trust envelope carried by a `KarsTask`. Every field is a ceiling a
/// delegated child may narrow but never exceed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskEnvelope {
    /// Autonomy tier (1..5).
    pub tier: i32,
    /// Optional resource budget for the task subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<TaskBudget>,
    /// Same-namespace `ToolPolicy` reference bounding callable tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy_ref: Option<LocalObjectRef>,
    /// Same-namespace egress allow-list reference bounding destinations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_allowlist_ref: Option<LocalObjectRef>,
    /// Remaining delegation hops this task may still spawn (>= 0).
    #[serde(default)]
    pub delegation_depth: i32,
    /// Maximum autonomy tier any descendant may hold (1..5, <= tier).
    pub authority_ceiling: i32,
}

/// Optional resource budget for a task subtree.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub enum BudgetScope {
    GovernedInference,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<BudgetScope>,
    /// Maximum total tokens for the task subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    /// Maximum total spend in micro-USD (1e-6 USD).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_micros: Option<i64>,
}

/// Same-namespace object reference (name only).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocalObjectRef {
    pub name: String,
}

/// `KarsTask.status` — the subset the Bridge surfaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsTaskStatus {
    /// `Pending` | `Ready` | `Degraded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// The generation most recently reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// `sha256:` digest of the validated trust envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_digest: Option<String>,
    /// Ancestry (oldest-first) for a delegated task; empty for a root task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<String>,
    /// Standard K8s conditions; the BFF surfaces the `Ready` message so the UI
    /// can show *why* a task is Degraded (e.g. an amplification rejection).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<TaskCondition>,
    /// Execution phase: `Idle` | `Launching` | `Running` | `Degraded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_phase: Option<String>,
    /// Name of the materialized sandbox, when launched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_ref: Option<LocalObjectRef>,
    /// Human-readable execution detail (e.g. the kind/Foundry caveat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<TaskAssignmentStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignment_events: Vec<TaskAssignmentEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_sequence: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskAssignmentStatus {
    pub task_id: String,
    pub state: String,
    pub worker_did: Option<String>,
    pub stage: Option<String>,
    pub child_task_id: Option<String>,
    pub child_role: Option<String>,
    pub last_progress_at: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskAssignmentEvent {
    pub sequence: i64,
    pub event_id: String,
    pub task_id: String,
    pub event_type: String,
    pub state: String,
    pub at: String,
    pub worker_did: Option<String>,
    pub stage: Option<String>,
    pub child_task_id: Option<String>,
    pub child_role: Option<String>,
    pub outcome: Option<String>,
    pub message: Option<String>,
}

/// A subset of a K8s `Condition` — enough to surface the Ready reason/message.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskCondition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_write_matches_core_camelcase_contract() {
        let blueprint = TaskBlueprint {
            git_write: Some(GitWriteConfig {
                connection_config_map_ref: LocalObjectRef {
                    name: "kars-github-connection-0123456789abcdef".into(),
                },
                repos: vec!["owner/repo".into()],
            }),
            ..Default::default()
        };
        let value = serde_json::to_value(blueprint).expect("serializes");
        assert_eq!(
            value["gitWrite"]["connectionConfigMapRef"]["name"],
            "kars-github-connection-0123456789abcdef"
        );
        assert_eq!(value["gitWrite"]["repos"][0], "owner/repo");
    }

    #[test]
    fn execution_plan_retains_required_empty_arrays() {
        let plan = ExecutionPlan {
            schema: "kars.execution-plan/v1".into(),
            roles: vec![ExecutionRole {
                name: "worker".into(),
                objective: "Produce deterministic evidence for this request.".into(),
                depends_on: Vec::new(),
                phases: vec![ExecutionPhase {
                    name: "execute".into(),
                    objective: "Complete the bounded execution phase with evidence.".into(),
                    capabilities: Vec::new(),
                    required_tool_calls: Vec::new(),
                    min_tool_calls: 0,
                    max_tool_calls: 0,
                    fresh_context: true,
                }],
                budget_tokens: None,
            }],
            max_parallel: 1,
            synthesis: ExecutionSynthesis {
                objective: "Publish a concise evidence-backed result.".into(),
                capabilities: Vec::new(),
                max_tool_calls: 0,
            },
            deliverables: Vec::new(),
        };

        let value = serde_json::to_value(plan).expect("serializes");
        assert_eq!(value["roles"][0]["dependsOn"], serde_json::json!([]));
        assert_eq!(
            value["roles"][0]["phases"][0]["capabilities"],
            serde_json::json!([])
        );
        assert_eq!(value["synthesis"]["capabilities"], serde_json::json!([]));
        assert_eq!(value["deliverables"], serde_json::json!([]));
    }

    #[test]
    fn execution_plan_preserves_provider_neutral_web_search_capability() {
        let plan = ExecutionPlan {
            schema: "kars.execution-plan/v1".into(),
            roles: vec![ExecutionRole {
                name: "source-scout".into(),
                objective: "Discover exact URLs and fetch the evidence.".into(),
                depends_on: Vec::new(),
                phases: vec![ExecutionPhase {
                    name: "discover".into(),
                    objective: "Search and fetch the exact URLs.".into(),
                    capabilities: vec!["web-search".into(), "network".into()],
                    required_tool_calls: Vec::new(),
                    min_tool_calls: 1,
                    max_tool_calls: 4,
                    fresh_context: true,
                }],
                budget_tokens: None,
            }],
            max_parallel: 1,
            synthesis: ExecutionSynthesis {
                objective: "Return the verified answer.".into(),
                capabilities: Vec::new(),
                max_tool_calls: 0,
            },
            deliverables: Vec::new(),
        };

        let value = serde_json::to_value(plan).expect("serializes");
        assert_eq!(
            value["roles"][0]["phases"][0]["capabilities"],
            serde_json::json!(["web-search", "network"])
        );
    }
}
