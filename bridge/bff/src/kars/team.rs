// kars Bridge BFF — typed view of the `KarsTeam` CRD.
//
// CONTRACT OWNERSHIP: the `KarsTeam` schema is owned by core kars
// (`Azure/kars`, controller/src/kars_team.rs). This module is a *consumer*
// projection — the standard kube-rs pattern for a client that reads a CRD it
// does not own. It mirrors only the fields the Bridge Teams surface needs, with
// matching group/version/kind and camelCase serde so the wire shape is
// identical.
//
// A KarsTeam is the durability-axis primitive (design note §11): a standing
// org with a charter, a roster (org chart), and a cadence loop that mints
// task-force KarsTasks autonomously. Bridge renders it; it never authors the
// member/principal tasks itself (the controller does).

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kars::task::{LocalObjectRef, TaskBlueprint, TaskEnvelope};

/// `KarsTeam.spec` — the subset the Bridge reads.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsTeam",
    namespaced,
    status = "KarsTeamStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct KarsTeamSpec {
    /// The standing mandate that generates the team's work.
    pub charter: String,
    /// The team's full trust envelope — the authority ceiling for every member
    /// and generated task.
    pub envelope: TaskEnvelope,
    /// Member roles — each a seat in the org chart holding an attenuated subset
    /// of the team envelope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roster: Vec<TeamRole>,
    /// Standing-operation cadence — how often the charter loop mints a
    /// task-force task. Absent ⇒ a passive org (no autonomous tick).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cadence: Option<TeamCadence>,
    /// Default run blueprint for the principal + generated tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint: Option<TaskBlueprint>,
    /// The human owner the team reports to (apex of the org chart).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporting_to: Option<String>,
    /// Name of the team's knowledge commons (shared memory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_commons: Option<String>,
    /// Runtime retention policy for assignments executed by this standing team.
    /// Missing on older teams means the compatibility `ephemeral` behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_mode: Option<String>,
    /// Idle window before a resource-optimized runtime is suspended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm_idle_seconds: Option<i64>,
    /// When `true` the team hibernates: members idle, charter loop paused.
    #[serde(default)]
    pub paused: bool,
    /// Optional short label for listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Profile this team is instantiated from (§17).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<LocalObjectRef>,
    /// A requested higher autonomy tier (§12) — drives a governed promotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_tier: Option<i32>,
}

/// A member role in the team roster.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamRole {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<TaskEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint: Option<TaskBlueprint>,
    /// Skills (KarsSkill names) this role acquires (§13).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

/// The team's standing-operation cadence.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamCadence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_minutes: Option<u32>,
}

/// `KarsTeam.status` — the controller is the sole writer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsTeamStatus {
    /// `Forming` | `Active` | `Hibernating` | `Degraded` | `Retired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_digest: Option<String>,
    /// The materialized principal task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_ref: Option<LocalObjectRef>,
    /// The materialized member tasks (org chart).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_refs: Vec<LocalObjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_count: Option<i64>,
    /// How many task-force tasks the charter loop has minted (autonomy proof).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_task_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_generated_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_succeeded: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_spent_total: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commons_entry_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    /// Effective lifecycle mode, echoed by the controller for older teams too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_mode: Option<String>,
    /// `Working` | `Warm` | `Hibernating` | `Idle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_assignment_nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_assignment_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_deadline_at: Option<String>,
}
