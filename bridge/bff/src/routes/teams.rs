// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — Teams API DTOs + handlers.
//
// A Team (KarsTeam) is a standing org with a charter and a cadence loop that
// mints task-force KarsTasks autonomously (design note §11). These endpoints
// project the typed CRD into stable, browser-facing JSON so the Teams surface
// never depends on raw Kubernetes envelopes. Read-only: the controller is the
// sole writer of team membership + generated tasks.

mod backlog;
mod channels;
mod commons;
mod lifecycle;
mod mutations;
mod queries;
mod validation;

pub use backlog::*;
pub use channels::*;
pub use commons::*;
pub use lifecycle::*;
pub use mutations::*;
pub use queries::*;

use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::team::KarsTeam;
use kube::ResourceExt;

/// One seat in the team roster (the org chart).
#[derive(Debug, Serialize)]
pub struct TeamRoleDto {
    pub name: String,
    pub system_prompt: Option<String>,
    pub tier: Option<i32>,
    /// The materialized member task name, when reconciled.
    pub member_task: Option<String>,
    /// Skills (KarsSkill names) this role acquires (§13).
    pub skills: Vec<String>,
    /// Per-member harness (runtime kind) — the differentiator: members can each
    /// run a different harness. `None` means inherit the team default.
    pub runtime: Option<String>,
    /// Per-member model deployment — `None` means inherit the team default.
    pub model: Option<String>,
}

/// Browser-facing team summary for the Teams index.
#[derive(Debug, Serialize)]
pub struct TeamSummaryDto {
    pub name: String,
    pub display_name: Option<String>,
    pub charter: String,
    pub phase: String,
    pub reporting_to: Option<String>,
    pub tier: i32,
    pub member_count: i64,
    pub generated_task_count: i64,
    pub every_minutes: Option<u32>,
    pub lifecycle_mode: String,
    pub warm_idle_seconds: Option<i64>,
    pub runtime_state: Option<String>,
    pub current_assignment_task: Option<String>,
    pub idle_deadline_at: Option<String>,
    pub paused: bool,
    pub created_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub next_run_at: Option<String>,
    pub health: Option<String>,
    pub detail: Option<String>,
    /// Delivered standing-run count — shown alongside generated so a card
    /// surfaces actual yield, not just scheduling activity.
    pub runs_succeeded: i64,
    pub retained_delivered: i64,
    pub retained_no_action: i64,
    pub retained_failed: i64,
}

/// Browser-facing team detail (charter, org chart, watching status, history).
#[derive(Debug, Serialize)]
pub struct TeamDetailDto {
    pub name: String,
    pub display_name: Option<String>,
    pub charter: String,
    pub phase: String,
    pub reporting_to: Option<String>,
    pub knowledge_commons: Option<String>,
    pub tier: i32,
    pub authority_ceiling: i32,
    pub delegation_depth: i32,
    pub paused: bool,
    pub every_minutes: Option<u32>,
    pub lifecycle_mode: String,
    pub warm_idle_seconds: Option<i64>,
    pub runtime_state: Option<String>,
    pub current_assignment_nonce: Option<String>,
    pub current_assignment_task: Option<String>,
    pub idle_deadline_at: Option<String>,
    pub envelope_digest: Option<String>,
    pub principal_task: Option<String>,
    pub roster: Vec<TeamRoleDto>,
    pub member_count: i64,
    pub generated_task_count: i64,
    pub last_generated_task: Option<String>,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub detail: Option<String>,
    pub health: Option<String>,
    pub runs_succeeded: i64,
    pub tokens_spent_total: i64,
    pub commons_entry_count: i64,
    pub last_success_at: Option<String>,
    pub created_at: Option<String>,
    pub last_activity_at: Option<String>,
    /// The task-force tasks the charter loop has minted, newest first.
    pub generated_tasks: Vec<String>,
    /// Customer-facing outcomes for retained runs, newest first. A cadence tick
    /// that correctly finds no work is a resolved outcome, not a failed delivery.
    pub recent_outcomes: Vec<TeamOutcomeDto>,
    pub recent_outcome_summary: TeamOutcomeSummaryDto,
    /// What the team can reach/use — surfaced for management at a glance.
    /// `*_default` flags mark values inherited from the cluster (no explicit
    /// blueprint override) so the UI can label them honestly rather than
    /// implying the operator chose them.
    pub tool_policy: Option<String>,
    pub tool_policy_default: bool,
    pub mcp_servers: Vec<String>,
    pub git_write_repos: Vec<String>,
    pub egress: Vec<String>,
    pub egress_mode: Option<String>,
    /// Domains the team's agents have ACTUALLY reached, aggregated live from the
    /// learn-mode observation buffer of its currently-running run sandboxes. This
    /// is the concrete "what has it touched so far" — distinct from the declared
    /// `egress` allowlist. Empty when no run is active (the buffer is per-run).
    pub learned_egress: Vec<String>,
    /// Network reachability posture when no explicit egress is declared — the
    /// kernel-level default-deny stance every sandbox runs under.
    pub network_posture: String,
    pub model: Option<String>,
    pub model_fallbacks: Vec<String>,
    pub model_default: bool,
    pub memory: Option<String>,
    /// The harness every run this team mints executes on (blueprint override,
    /// else the sandbox default OpenClaw). `runtime_default` marks the inherited
    /// case so the UI can badge it honestly.
    pub runtime: Option<String>,
    pub runtime_default: bool,
    pub isolation: Option<String>,
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
    /// The team's assigned task backlog (pending/active/done), newest last.
    pub tasks: Vec<TeamTaskDto>,
    /// Communication channels enabled on this team's envelope (telegram/slack/…).
    pub channels: Vec<String>,
}

fn phase_of(team: &KarsTeam) -> String {
    team.status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Forming".to_string())
}

fn is_team_owner(team: &KarsTeam, principal: &Principal) -> bool {
    team.annotations()
        .get("kars.azure.com/owner-sub")
        .is_some_and(|subject| subject == &principal.sub)
}

pub(crate) async fn require_owned_team(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    name: &str,
    principal: &Principal,
) -> AppResult<KarsTeam> {
    let team = cluster
        .teams(ns)
        .get_opt(name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .ok_or(AppError::NotFound)?;
    if !is_team_owner(&team, principal) {
        return Err(AppError::NotFound);
    }
    Ok(team)
}

fn effective_lifecycle_mode(team: &KarsTeam) -> String {
    team.status
        .as_ref()
        .and_then(|status| status.lifecycle_mode.clone())
        .or_else(|| team.spec.lifecycle_mode.clone())
        .unwrap_or_else(|| "ephemeral".into())
}

fn runtime_state(team: &KarsTeam) -> Option<String> {
    team.status
        .as_ref()
        .and_then(|status| status.runtime_state.clone())
}

#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
    pub charter: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub tier: Option<i32>,
    pub authority_ceiling: Option<i32>,
    pub delegation_depth: Option<i32>,
    pub reporting_to: Option<String>,
    pub knowledge_commons: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    pub cadence_minutes: Option<u32>,
    /// Runtime retention policy. Older clients omit this and retain ephemeral
    /// behavior; the Bridge composer recommends resource-optimized operation.
    #[serde(default)]
    pub lifecycle_mode: Option<String>,
    /// Idle window before a resource-optimized runtime is suspended.
    #[serde(default)]
    pub warm_idle_seconds: Option<i64>,
    /// Governance policy that bounds every run this team mints. When omitted the
    /// Bridge assigns the cluster default (`kars-default`) so the team's
    /// sandboxes are governed AND functional — an un-governed sandbox hangs
    /// because the agent's AGT engine fails closed on an empty policy set.
    pub tool_policy: Option<String>,
    /// The harness every run this team mints executes on (OpenClaw / Hermes /
    /// BYO). Written onto the team's run blueprint; the controller inherits it
    /// for each minted run. A bootstrap-only (non-autonomous) harness is
    /// corrected to OpenClaw. Absent => the sandbox default (OpenClaw).
    #[serde(default)]
    pub runtime: Option<String>,
    /// Model route for the team principal and minted runs, encoded as
    /// `provider::deployment` (for example
    /// `github-copilot::claude-opus-4.8`).
    #[serde(default)]
    pub model: Option<String>,
    /// Ordered provider::deployment routes used only after the primary route
    /// fails. Every entry must independently qualify the complete Team plan.
    #[serde(default)]
    pub model_fallbacks: Vec<String>,
    /// Network destinations inherited by every run.
    #[serde(default)]
    pub egress: Vec<CreateEgress>,
    /// `learning` for discovery or `strict` for allowlist enforcement.
    #[serde(default)]
    pub egress_mode: Option<String>,
    /// Connected MCP servers every run this team mints receives. Each name must
    /// resolve to an installed McpServer; preflight validates readiness before
    /// launch and the controller inherits the list onto every task-force run.
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// When false/omitted the team is created PAUSED (hibernating) so nothing
    /// runs until the operator explicitly launches it ("Run now" / resume) —
    /// the launch is a real human approval, not an automatic kickoff. Set true
    /// to opt into launching immediately on create.
    #[serde(default)]
    pub launch: Option<bool>,
    #[serde(default)]
    pub roles: Vec<CreateRole>,
    #[serde(default)]
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
    /// Repos every run may open PRs against, selected from the authenticated
    /// principal's connection. The server derives the typed connection reference.
    #[serde(default)]
    pub git_write_repos: Option<Vec<String>>,
    /// The identity creating this team (stamped `kars.azure.com/created-by`) for
    /// per-user budget attribution. Absent => "unattributed".
    #[serde(default)]
    pub created_by: Option<String>,
    /// Retention override, in seconds, for every task-force RUN this team
    /// mints — auto-delete a run's record this long after its deliverable
    /// lands. Does NOT apply to the standing principal/roster, which are
    /// never auto-deleted. `0` disables retention for this team's runs even
    /// if a cluster-wide default is set. Absent inherits the cluster default.
    #[serde(default)]
    pub run_retention_ttl_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEgress {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRole {
    pub name: String,
    pub system_prompt: Option<String>,
    pub runtime: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
}
struct TeamModelRoutes<'a> {
    namespace: &'a str,
    runtime: Option<&'a str>,
    model: Option<&'a str>,
    model_fallbacks: &'a [String],
    roles: &'a [CreateRole],
    execution_plan: &'a crate::routes::tasks::ExecutionPlanDto,
    mcp_servers: &'a [String],
    memory: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTeamRequest {
    pub charter: Option<String>,
    pub paused: Option<bool>,
    pub cadence_minutes: Option<u32>,
    pub reporting_to: Option<String>,
    /// Change how the standing runtime is retained between assignments.
    #[serde(default)]
    pub lifecycle_mode: Option<String>,
    /// Change the resource-optimized warm idle window.
    #[serde(default)]
    pub warm_idle_seconds: Option<i64>,
    /// When present, replaces the team's roster — editing the org post-create
    /// (add/remove roles, change per-member prompt/harness/model/skills). The
    /// controller reconciles member tasks to match.
    pub roles: Option<Vec<CreateRole>>,
    /// Change the harness every run this team mints executes on (OpenClaw /
    /// Hermes / BYO). A non-autonomous adapter is corrected to OpenClaw.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Change the principal/default model inherited by future runs.
    #[serde(default)]
    pub model: Option<String>,
    /// Replace the ordered fallback routes inherited by future runs.
    #[serde(default)]
    pub model_fallbacks: Option<Vec<String>>,
    /// Replace or clear the shared memory binding inherited by future runs.
    #[serde(default)]
    pub memory: Option<String>,
    /// Replace the connected MCP servers inherited by future team runs.
    /// `Some([])` explicitly clears the list; `None` leaves it unchanged.
    #[serde(default)]
    pub mcp_servers: Option<Vec<String>>,
    /// Replace the keyless GitHub write scope inherited by future team runs.
    /// `Some([])` revokes PR-write access; `None` leaves it unchanged.
    #[serde(default)]
    pub git_write_repos: Option<Vec<String>>,
    /// Replace the declared egress destinations for future runs.
    #[serde(default)]
    pub egress: Option<Vec<CreateEgress>>,
    /// Change future runs between learning and strict egress modes.
    #[serde(default)]
    pub egress_mode: Option<String>,
    /// Change the retention override for this team's future task-force runs.
    /// `0` disables retention; absent leaves the current setting unchanged.
    #[serde(default)]
    pub run_retention_ttl_seconds: Option<i64>,
    /// Replace the typed execution plan while preserving the team roster. The
    /// server validates structure, exact role-name parity, and route qualification.
    #[serde(default)]
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_team_visibility_requires_exact_owner_even_for_operators() {
        let team: KarsTeam = serde_json::from_value(serde_json::json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsTeam",
            "metadata": {
                "name": "private-team",
                "annotations": {"kars.azure.com/owner-sub": "subject-a"}
            },
            "spec": {
                "charter": "Maintain private work.",
                "envelope": {"tier": 2, "authorityCeiling": 1},
                "paused": true
            }
        }))
        .expect("team");
        let owner = Principal {
            sub: "subject-a".into(),
            name: "owner".into(),
            roles: vec!["user".into()],
        };
        let other_operator = Principal {
            sub: "subject-b".into(),
            name: "operator".into(),
            roles: vec!["operator".into()],
        };

        assert!(is_team_owner(&team, &owner));
        assert!(!is_team_owner(&team, &other_operator));
    }

    #[test]
    fn team_create_and_update_accept_mcp_servers() {
        let create: CreateTeamRequest = serde_json::from_value(serde_json::json!({
            "name": "review-board",
            "charter": "Review the release",
            "model": "github-copilot::claude-opus-4.8",
            "model_fallbacks": [
                "local-inference::gpt-oss-120b",
                "foundry::gpt-5.4-pro"
            ],
            "mcp_servers": ["playwright", "everything"],
            "memory": "foundry-default",
            "lifecycle_mode": "resourceOptimized",
            "warm_idle_seconds": 900,
            "egress_mode": "strict",
            "egress": [{"host":"docs.example.com","port":443}]
        }))
        .expect("create request");
        assert_eq!(create.mcp_servers, vec!["playwright", "everything"]);
        assert_eq!(create.memory.as_deref(), Some("foundry-default"));
        assert_eq!(
            create.model.as_deref(),
            Some("github-copilot::claude-opus-4.8")
        );
        assert_eq!(
            create.model_fallbacks,
            vec!["local-inference::gpt-oss-120b", "foundry::gpt-5.4-pro"]
        );
        assert_eq!(create.egress_mode.as_deref(), Some("strict"));
        assert_eq!(create.egress[0].host, "docs.example.com");
        assert_eq!(create.egress[0].port, Some(443));
        assert_eq!(create.lifecycle_mode.as_deref(), Some("resourceOptimized"));
        assert_eq!(create.warm_idle_seconds, Some(900));

        let update: UpdateTeamRequest = serde_json::from_value(serde_json::json!({
            "mcp_servers": [],
            "model": "local-inference::gpt-oss-120b",
            "model_fallbacks": ["github-copilot::gpt-5.6-sol"],
            "lifecycle_mode": "persistent",
            "warm_idle_seconds": 1800,
            "egress_mode": "learning",
            "egress": []
        }))
        .expect("update request");
        assert_eq!(update.mcp_servers, Some(Vec::new()));
        assert_eq!(
            update.model.as_deref(),
            Some("local-inference::gpt-oss-120b")
        );
        assert_eq!(
            update.model_fallbacks,
            Some(vec!["github-copilot::gpt-5.6-sol".into()])
        );
        assert_eq!(update.egress_mode.as_deref(), Some("learning"));
        assert_eq!(update.egress.map(|entries| entries.len()), Some(0));
        assert_eq!(update.lifecycle_mode.as_deref(), Some("persistent"));
        assert_eq!(update.warm_idle_seconds, Some(1800));
    }
}
