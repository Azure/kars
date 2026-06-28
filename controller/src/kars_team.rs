// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsTeam` — the **standing team / org** primitive (design note §11, the
//! durability axis).
//!
//! A `KarsTask` is a *task force*: spun for one unit of work, dissolves on
//! delivery. A `KarsTeam` is the other shape enterprises actually organise
//! around — a **standing org with a persistent mandate** that:
//!
//! - holds a **charter** (a standing mandate in plain language),
//! - has a **roster** of member roles, each holding a strict *subset* of the
//!   team's authority (the org chart **is** the security topology, §12),
//! - runs on a **cadence** — its standing-operation loop periodically mints
//!   task-force `KarsTask`s from the charter (autonomous monitoring: "watch the
//!   repo / reconcile the ledger / keep the docs current" — §20),
//! - accrues a **knowledge commons** (shared, provenance-tracked memory, §14),
//! - **hibernates** when idle and resumes on its cadence, budget-capped.
//!
//! The team is domain-blind: a finance close team, a docs-review team, an SRE
//! team, or the eng team maintaining kars are all the *same* primitive — the
//! domain lives in the charter, the roster, and the commons, never the platform.
//!
//! **Architecture (additive, cohesive).** A `KarsTeam` does **not** re-implement
//! sandbox materialization. Its reconciler authors **`KarsTask` CRs** — a
//! principal task holding the full charter envelope, member tasks holding
//! attenuated sub-envelopes (parented to the principal so the existing
//! capability-attenuation + org-chart machinery applies unchanged), and, on each
//! cadence tick, a fresh task-force task derived from the charter. Everything
//! downstream (envelope attenuation, sandbox materialization, the mesh task
//! loop, receipts, metering) is reused as-is. Bridge *consumes* `KarsTeam`;
//! core never depends on Bridge.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kars_task::{TaskBlueprint, TaskEnvelope};
use crate::mcp_server::LocalObjectRef;

/// `KarsTeam.spec` — a standing org with a persistent mandate + trust envelope.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsTeam",
    namespaced,
    status = "KarsTeamStatus",
    shortname = "cteam",
    printcolumn = r#"{"name":"Tier","type":"integer","jsonPath":".spec.envelope.tier"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Members","type":"integer","jsonPath":".status.memberCount"}"#,
    printcolumn = r#"{"name":"Generated","type":"integer","jsonPath":".status.generatedTaskCount"}"#,
    printcolumn = r#"{"name":"LastRun","type":"string","jsonPath":".status.lastRunAt"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsTeamSpec {
    /// The **charter** — the team's standing mandate in plain language. This is
    /// the durable instruction that *generates* the team's work: each cadence
    /// tick mints a task-force `KarsTask` whose objective is derived from this
    /// charter. E.g. *"Keep the kars repo healthy: triage new issues, run tests
    /// on open PRs, and draft fixes for failing checks."*
    pub charter: String,

    /// The team's full trust envelope — the ceiling of authority any member or
    /// generated task may hold. Reuses the `KarsTask` envelope so attenuation,
    /// digesting, and the org-as-topology lattice apply unchanged.
    pub envelope: TaskEnvelope,

    /// The roster of member roles. Each role holds a strict *subset* of the
    /// team envelope (capability-attenuating delegation, §12). Materialized as
    /// member `KarsTask`s parented to the principal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roster: Vec<TeamRole>,

    /// The standing-operation cadence — how often the charter loop mints a
    /// task-force task (autonomous monitoring). Absent ⇒ the team is a passive
    /// org (members exist, but no autonomous tick).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cadence: Option<TeamCadence>,

    /// The default run blueprint for the principal + generated task-force tasks
    /// (harness/model/instructions/tools/egress/isolation). Member roles may
    /// override their own blueprint via `TeamRole.blueprint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint: Option<TaskBlueprint>,

    /// The human owner this team reports to (the apex of the org chart, §12).
    /// Surfaced verbatim; digests + escalations route here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporting_to: Option<String>,

    /// Name of the team's **knowledge commons** (shared, provenance-tracked
    /// memory, §14). Defaults to the team name when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_commons: Option<String>,

    /// When `true` the team **hibernates**: members stay governed-but-idle and
    /// the charter loop does not tick (idle-scaled, budget-preserving, §11).
    #[serde(default)]
    pub paused: bool,

    /// Optional short label surfaced in CLI / UI listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// A member role in the team roster — a named seat in the org chart holding an
/// attenuated subset of the team's authority.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamRole {
    /// The role name (e.g. `bugfix-engineer`, `compliance-screener`). Becomes
    /// the materialized member `KarsTask` name suffix.
    pub name: String,

    /// The role's standing instructions (its system prompt), in addition to the
    /// charter. Drives the member sandbox's `instructions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// The role's attenuated trust envelope — a strict subset of the team
    /// envelope. When unset the member inherits a safe attenuation of the team
    /// envelope (one tier below the team, no further delegation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<TaskEnvelope>,

    /// Optional per-role run blueprint override (model/tools/egress). Falls back
    /// to the team blueprint when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint: Option<TaskBlueprint>,
}

/// The team's standing-operation cadence.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamCadence {
    /// Tick interval in **minutes**. On each tick the charter loop mints one
    /// task-force `KarsTask`. Kept as a simple interval so the standing loop is
    /// honest and reproducible on a plain (kind) cluster. Must be `>= 1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_minutes: Option<u32>,
}

/// `KarsTeam.status` — the controller is the sole writer.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsTeamStatus {
    /// Lifecycle phase: `Forming` (validating + materializing), `Active`
    /// (running, cadence ticking), `Hibernating` (paused/idle), `Degraded`
    /// (envelope invalid — no authority to operate), `Retired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    /// `sha256:` digest of the validated team envelope (reuses the task digest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_digest: Option<String>,

    /// The materialized **principal** `KarsTask` (the org apex + authority root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_ref: Option<LocalObjectRef>,

    /// The materialized **member** `KarsTask`s (the roster as cluster state).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_refs: Vec<LocalObjectRef>,

    /// Number of members materialized (printcolumn convenience).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_count: Option<i64>,

    /// How many task-force tasks the charter loop has generated so far.
    #[serde(default)]
    pub generated_task_count: i64,

    /// The most recent task-force task the charter loop minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_generated_task: Option<String>,

    /// When the charter loop last ticked (RFC3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,

    /// When the charter loop is next due to tick (RFC3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,

    /// Human-readable detail surfaced verbatim in the product.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl KarsTeam {
    /// The team's knowledge-commons name (explicit or defaulted to the team).
    /// Consumed by the BFF + the knowledge-commons write path.
    #[allow(dead_code)]
    pub fn commons_name(&self) -> String {
        self.spec
            .knowledge_commons
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                self.metadata
                    .name
                    .clone()
                    .unwrap_or_else(|| "team".to_string())
            })
    }

    /// Validation errors for the team envelope + roster (empty ⇒ valid). Mirrors
    /// the `KarsTask` envelope rules and adds the roster-attenuation check: every
    /// member envelope must be a strict subset of the team envelope.
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errs = Vec::new();
        let env = &self.spec.envelope;
        if env.tier < crate::kars_task::TIER_MIN || env.tier > crate::kars_task::TIER_MAX {
            errs.push(format!(
                "envelope.tier {} out of range [{}..{}]",
                env.tier,
                crate::kars_task::TIER_MIN,
                crate::kars_task::TIER_MAX
            ));
        }
        if env.authority_ceiling > env.tier {
            errs.push(format!(
                "envelope.authorityCeiling {} exceeds tier {}",
                env.authority_ceiling, env.tier
            ));
        }
        if env.delegation_depth < 0 {
            errs.push("envelope.delegationDepth must be >= 0".to_string());
        }
        if self.spec.charter.trim().is_empty() {
            errs.push("charter must not be empty".to_string());
        }
        for role in &self.spec.roster {
            if let Some(role_env) = &role.envelope {
                for v in role_env.attenuation_violations(&self.spec.envelope) {
                    errs.push(format!("roster role '{}': {}", role.name, v));
                }
            }
        }
        if let Some(c) = &self.spec.cadence
            && let Some(m) = c.every_minutes
            && m < 1
        {
            errs.push("cadence.everyMinutes must be >= 1".to_string());
        }
        errs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_task::{TaskBudget, TaskEnvelope};

    fn team_envelope() -> TaskEnvelope {
        TaskEnvelope {
            tier: 4,
            budget: Some(TaskBudget {
                tokens: Some(1_000_000),
                usd_micros: None,
            }),
            tool_policy_ref: None,
            egress_allowlist_ref: None,
            delegation_depth: 2,
            authority_ceiling: 3,
        }
    }

    fn sample_team(roster: Vec<TeamRole>) -> KarsTeam {
        let mut t = KarsTeam::new(
            "eng",
            KarsTeamSpec {
                charter: "Keep the repo healthy".into(),
                envelope: team_envelope(),
                roster,
                cadence: Some(TeamCadence {
                    every_minutes: Some(60),
                }),
                blueprint: None,
                reporting_to: Some("alice@corp".into()),
                knowledge_commons: None,
                paused: false,
                display_name: None,
            },
        );
        t.metadata.namespace = Some("kars-system".into());
        t
    }

    #[test]
    fn valid_team_has_no_errors() {
        let t = sample_team(vec![TeamRole {
            name: "bugfix".into(),
            system_prompt: None,
            // a strict attenuation of the team envelope
            envelope: Some(TaskEnvelope {
                tier: 3,
                budget: Some(TaskBudget {
                    tokens: Some(100_000),
                    usd_micros: None,
                }),
                tool_policy_ref: None,
                egress_allowlist_ref: None,
                delegation_depth: 1,
                authority_ceiling: 2,
            }),
            blueprint: None,
        }]);
        assert!(t.validation_errors().is_empty(), "{:?}", t.validation_errors());
    }

    #[test]
    fn member_exceeding_team_is_rejected() {
        let t = sample_team(vec![TeamRole {
            name: "over".into(),
            system_prompt: None,
            // tier 5 > team tier 4 — must be rejected
            envelope: Some(TaskEnvelope {
                tier: 5,
                budget: None,
                tool_policy_ref: None,
                egress_allowlist_ref: None,
                delegation_depth: 1,
                authority_ceiling: 5,
            }),
            blueprint: None,
        }]);
        let errs = t.validation_errors();
        assert!(errs.iter().any(|e| e.contains("over")), "{errs:?}");
    }

    #[test]
    fn empty_charter_is_rejected() {
        let mut t = sample_team(vec![]);
        t.spec.charter = "  ".into();
        assert!(t.validation_errors().iter().any(|e| e.contains("charter")));
    }

    #[test]
    fn commons_name_defaults_to_team() {
        let t = sample_team(vec![]);
        assert_eq!(t.commons_name(), "eng");
    }
}
