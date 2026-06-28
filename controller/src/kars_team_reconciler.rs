// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsTeam` reconciler — the standing-team lifecycle (design note §11).
//!
//! A team is *long-lived governance over short-lived work*. This reconciler:
//!
//! 1. **Validates** the team envelope + roster (every member attenuates the
//!    team — the org chart **is** the security topology, §12). Invalid ⇒
//!    `Degraded`, no authority to operate, no tasks authored.
//! 2. **Materializes the org** as `KarsTask`s: a **principal** task holding the
//!    full charter envelope, and a **member** task per roster role holding an
//!    attenuated sub-envelope, parented to the principal. The existing
//!    `KarsTask` machinery (attenuation enforcement, sandbox materialization,
//!    the mesh agent loop, receipts, metering) is reused unchanged — the team
//!    reconciler never re-implements any of it.
//! 3. **Runs the charter loop** (autonomous monitoring): on each cadence tick it
//!    mints a fresh task-force `KarsTask` from the charter mandate and launches
//!    it. This is the standing-operation heartbeat — the team periodically does
//!    what its charter says (watch the repo, reconcile the ledger, …) without a
//!    human re-asking. Honest + reproducible on a plain (kind) cluster.
//! 4. **Hibernates** when `spec.paused` — members stay governed-but-idle, the
//!    loop stops ticking.
//!
//! Everything is additive: no existing reconciler changes; a cluster with no
//! `KarsTeam` objects behaves exactly as before. Bridge *consumes* teams via the
//! CRDs; core never depends on Bridge.

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams},
    runtime::Controller,
    runtime::controller::Action,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use crate::kars_task::{
    KarsTask, KarsTaskSpec, TaskBlueprint, TaskEnvelope, TaskExecution,
};
use crate::kars_team::{KarsTeam, KarsTeamStatus, TeamRole};
use crate::mcp_server::LocalObjectRef;
use crate::status::phase::{PHASE_ACTIVE, PHASE_DEGRADED, PHASE_HIBERNATING};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_TEAM;
const FINALIZER: &str = "kars.azure.com/karsteam-cleanup";
const REQUEUE_OK: Duration = Duration::from_secs(60);
const REQUEUE_PENDING: Duration = Duration::from_secs(10);

/// Annotation linking a generated task-force task back to its team.
const ANNOT_TEAM: &str = "kars.azure.com/team";
/// Annotation marking a task's role within a team (`principal` | `member` | `taskforce`).
const ANNOT_TEAM_ROLE: &str = "kars.azure.com/team-role";
/// Annotation the mesh task-delivery loop watches to drive an autonomous run.
const ANNOT_RUN_REQUESTED: &str = "kars.azure.com/run-requested";
/// Cap on concurrently-executing standing-operation runs per team, so the
/// charter loop never floods the cluster faster than runs complete + retire.
const MAX_CONCURRENT_RUNS: usize = 2;

#[derive(thiserror::Error, Debug)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

impl ReconcileError {
    fn class(&self) -> &'static str {
        match self {
            ReconcileError::Kube(_) => "kube_api",
            ReconcileError::SerdeJson(_) => "serde",
        }
    }
}

struct Ctx {
    client: Client,
}

async fn reconcile(team: Arc<KarsTeam>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = team.name_any();
    let ns = team.namespace().unwrap_or_else(|| "default".into());
    let teams: Api<KarsTeam> = Api::namespaced(ctx.client.clone(), &ns);
    let tasks: Api<KarsTask> = Api::namespaced(ctx.client.clone(), &ns);

    // Deletion: drop the finalizer. The materialized KarsTasks are owned via
    // ownerReferences, so the API server garbage-collects them — nothing else
    // to clean up.
    if team.metadata.deletion_timestamp.is_some() {
        if has_finalizer(&team) {
            let patch = json!({ "metadata": { "finalizers": drop_finalizer(&team) } });
            teams
                .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                .await?;
        }
        return Ok(Action::await_change());
    }

    if !has_finalizer(&team) {
        let mut finalizers = team.metadata.finalizers.clone().unwrap_or_default();
        finalizers.push(FINALIZER.to_string());
        let patch = json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsTeam",
            "metadata": { "name": name, "finalizers": finalizers },
        });
        teams
            .patch(&name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(patch))
            .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    // 1. Validate the team envelope + roster attenuation.
    let errors = team.validation_errors();
    if !errors.is_empty() {
        let detail = format!("invalid team: {}", errors.join("; "));
        write_status(
            &teams,
            &name,
            KarsTeamStatus {
                phase: Some(PHASE_DEGRADED.into()),
                observed_generation: team.metadata.generation,
                envelope_digest: None,
                detail: Some(detail),
                ..Default::default()
            },
        )
        .await?;
        return Ok(Action::requeue(REQUEUE_OK));
    }

    // Hibernation: paused teams keep their members governed-but-idle and the
    // charter loop does not tick. We still keep the principal/members present.
    let paused = team.spec.paused;

    // Ensure the team's knowledge commons exists (shared, provenance-tracked
    // memory, §14). Owned by the team so it is GC'd on deletion.
    let commons = team.commons_name();
    crate::team_commons::ensure_commons(&ctx.client, &commons, owner_ref(&team))
        .await
        .ok();

    // Write path: harvest any completed standing-operation run whose deliverable
    // is not yet in the commons, then retire its (now-finished) sandbox so runs
    // never pile up. This is what makes the team *accumulate* knowledge across
    // ticks — each run deposits what it learned, with provenance, into the
    // shared store. Returns the count of runs still executing.
    let active_runs = harvest_and_retire_runs(&ctx.client, &tasks, &team, &commons).await;

    // 2. Materialize the org: principal + members as KarsTasks.
    let principal_name = format!("{name}-principal");
    materialize_principal(&tasks, &team, &principal_name).await?;

    let mut member_refs: Vec<LocalObjectRef> = Vec::new();
    for role in &team.spec.roster {
        let member_name = format!("{name}-{}", sanitize(&role.name));
        materialize_member(&tasks, &team, &principal_name, role, &member_name).await?;
        member_refs.push(LocalObjectRef { name: member_name });
    }

    // 3. Charter loop — mint a task-force task when the cadence is due.
    let prior = team.status.clone().unwrap_or_default();
    let mut generated = prior.generated_task_count;
    let mut last_generated = prior.last_generated_task.clone();
    let mut last_run_at = prior.last_run_at.clone();

    let every = team
        .spec
        .cadence
        .as_ref()
        .and_then(|c| c.every_minutes)
        .filter(|m| *m >= 1);

    let now = Utc::now();
    let mut next_run_at = None;
    if let Some(every_min) = every {
        let due = match prior.last_run_at.as_deref().and_then(parse_rfc3339) {
            Some(prev) => now >= prev + chrono::Duration::minutes(every_min as i64),
            None => true, // never run → due immediately
        };
        // Backpressure: only mint when the cluster isn't already saturated with
        // in-flight runs from this team. Skipping a tick keeps the standing
        // operation honest without flooding — the next reconcile re-checks.
        if !paused && due && active_runs < MAX_CONCURRENT_RUNS {
            let tf_name = format!("{name}-run-{}", now.format("%Y%m%d%H%M%S"));
            // Read path: inject the team's accumulated knowledge so the run
            // builds on prior ticks instead of starting cold.
            let prior = crate::team_commons::prior_knowledge(&ctx.client, &commons).await;
            mint_taskforce(&tasks, &team, &principal_name, &tf_name, &prior).await?;
            generated += 1;
            last_generated = Some(tf_name);
            last_run_at = Some(now.to_rfc3339());
            next_run_at = Some((now + chrono::Duration::minutes(every_min as i64)).to_rfc3339());
        } else if let Some(prev) = prior.last_run_at.as_deref().and_then(parse_rfc3339) {
            next_run_at = Some((prev + chrono::Duration::minutes(every_min as i64)).to_rfc3339());
        }
    }

    let phase = if paused { PHASE_HIBERNATING } else { PHASE_ACTIVE };
    let member_count = member_refs.len() as i64;
    let detail = if paused {
        "Team hibernating — members governed-but-idle; charter loop paused.".to_string()
    } else if every.is_some() {
        format!(
            "Standing operation active — {} task-force task(s) generated from the charter.",
            generated
        )
    } else {
        "Team active — no cadence set; members run on demand.".to_string()
    };

    write_status(
        &teams,
        &name,
        KarsTeamStatus {
            phase: Some(phase.into()),
            observed_generation: team.metadata.generation,
            envelope_digest: Some(team.spec.envelope.digest()),
            principal_ref: Some(LocalObjectRef { name: principal_name }),
            member_refs,
            member_count: Some(member_count),
            generated_task_count: generated,
            last_generated_task: last_generated,
            last_run_at,
            next_run_at,
            detail: Some(detail),
            ..Default::default()
        },
    )
    .await?;

    // Requeue cadence: short while a tick is pending, otherwise the standing
    // poll interval. We always requeue so the charter loop keeps ticking.
    let requeue = if every.is_some() && !paused {
        // Re-check at most once a minute so a due tick fires promptly.
        Duration::from_secs(30)
    } else {
        REQUEUE_OK
    };
    Ok(Action::requeue(requeue))
}

/// Build the shared owner-reference so materialized tasks are GC'd with the team.
fn owner_ref(team: &KarsTeam) -> serde_json::Value {
    json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTeam",
        "name": team.name_any(),
        "uid": team.metadata.uid.clone().unwrap_or_default(),
        "controller": true,
        "blockOwnerDeletion": true,
    })
}

/// Materialize (SSA, idempotent) the **principal** task — the org apex holding
/// the team's full charter envelope. Governed-but-idle by default; the charter
/// loop is what produces *running* work, so the principal itself is a stable
/// authority root, not a running agent (no launch).
async fn materialize_principal(
    tasks: &Api<KarsTask>,
    team: &KarsTeam,
    principal_name: &str,
) -> Result<(), ReconcileError> {
    let spec = KarsTaskSpec {
        objective: format!("[principal] {}", team.spec.charter),
        envelope: team.spec.envelope.clone(),
        parent_ref: None,
        execution: None,
        blueprint: team.spec.blueprint.clone(),
        display_name: Some(format!(
            "{} — principal",
            team.spec.display_name.clone().unwrap_or_else(|| team.name_any())
        )),
    };
    apply_task(tasks, team, principal_name, spec, "principal").await
}

/// Materialize (SSA, idempotent) a **member** task — a roster seat holding an
/// attenuated subset of the team envelope, parented to the principal so the
/// existing attenuation + lineage machinery enforces the org topology.
async fn materialize_member(
    tasks: &Api<KarsTask>,
    team: &KarsTeam,
    principal_name: &str,
    role: &TeamRole,
    member_name: &str,
) -> Result<(), ReconcileError> {
    let envelope = role
        .envelope
        .clone()
        .unwrap_or_else(|| default_member_envelope(&team.spec.envelope));
    let blueprint = member_blueprint(team, role);
    let spec = KarsTaskSpec {
        objective: role
            .system_prompt
            .clone()
            .unwrap_or_else(|| format!("[{}] {}", role.name, team.spec.charter)),
        envelope,
        parent_ref: Some(LocalObjectRef { name: principal_name.to_string() }),
        execution: None,
        blueprint,
        display_name: Some(format!(
            "{} — {}",
            team.spec.display_name.clone().unwrap_or_else(|| team.name_any()),
            role.name
        )),
    };
    apply_task(tasks, team, member_name, spec, "member").await
}

/// Mint + launch a **task-force** task from the charter — the standing-operation
/// tick. Parented to the principal (attenuated under the charter) and launched
/// so the existing mesh agent loop runs it autonomously.
async fn mint_taskforce(
    tasks: &Api<KarsTask>,
    team: &KarsTeam,
    principal_name: &str,
    tf_name: &str,
    prior_knowledge: &str,
) -> Result<(), ReconcileError> {
    // The task-force runs under an attenuation of the team envelope (one tier
    // below, no further delegation) so a generated run can never hold more
    // authority than the charter.
    let envelope = default_member_envelope(&team.spec.envelope);
    let spec = KarsTaskSpec {
        objective: format!(
            "Standing-operation run for team '{}'. Charter: {}{}",
            team.name_any(),
            team.spec.charter,
            prior_knowledge
        ),
        envelope,
        parent_ref: Some(LocalObjectRef { name: principal_name.to_string() }),
        execution: Some(TaskExecution { launch: true, runtime: None }),
        blueprint: team.spec.blueprint.clone(),
        display_name: Some(format!(
            "{} — standing run",
            team.spec.display_name.clone().unwrap_or_else(|| team.name_any())
        )),
    };
    apply_task(tasks, team, tf_name, spec, "taskforce").await
}

/// Write path for the knowledge commons + run lifecycle: scan the team's
/// standing-operation run tasks and, for any whose deliverable has landed,
/// harvest the output into a provenance-tracked commons entry (idempotent — a
/// run contributes at most one entry) and then **retire** the run by un-launching
/// it, which tears down the now-finished sandbox so runs never pile up. Returns
/// the count of runs still executing (deliverable not yet landed), used as
/// backpressure for the charter loop. Best-effort: a transient read failure just
/// defers the work to the next reconcile, never failing the team.
async fn harvest_and_retire_runs(
    client: &Client,
    tasks: &Api<KarsTask>,
    team: &KarsTeam,
    commons: &str,
) -> usize {
    let team_name = team.name_any();
    let lp = ListParams::default().labels(&format!("kars.azure.com/team={team_name}"));
    let Ok(list) = tasks.list(&lp).await else {
        return 0;
    };
    let ns = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let cms: Api<k8s_openapi::api::core::v1::ConfigMap> = Api::namespaced(client.clone(), &ns);

    let mut active = 0usize;
    for task in &list.items {
        // Only standing-operation runs deposit knowledge (members/principal are
        // standing authority, not run deliverables).
        let is_run = task
            .annotations()
            .get(ANNOT_TEAM_ROLE)
            .is_some_and(|r| r == "taskforce");
        if !is_run {
            continue;
        }
        let run = task.name_any();
        let launched = task
            .spec
            .execution
            .as_ref()
            .map(|e| e.launch)
            .unwrap_or(false);

        let output_cm = format!("kars-mission-output-{run}");
        let landed = cms.get_opt(&output_cm).await.ok().flatten();
        let Some(cm) = landed else {
            // Deliverable not landed yet — still executing while launched.
            if launched {
                active += 1;
            }
            continue;
        };
        let data = cm.data.unwrap_or_default();
        let ok = data.get("status").map(String::as_str) == Some("ok");
        // A *substantive* deliverable did real inference work — harness-neutral
        // signal: tokens were spent or artifacts were produced. This keeps the
        // commons free of empty/error runs (e.g. a model that rejected the
        // request) that would otherwise pollute the team's prior knowledge.
        let did_work = data
            .get("totalTokens")
            .and_then(|t| t.parse::<i64>().ok())
            .is_some_and(|t| t > 0)
            || data
                .get("artifactCount")
                .and_then(|c| c.parse::<i64>().ok())
                .is_some_and(|c| c > 0);
        if did_work
            && let Some(output) = data.get("output").filter(|s| ok && !s.trim().is_empty())
        {
            // Title the entry by the team's mandate (clean), not the verbose
            // run objective (which carries the injected prior-knowledge preamble).
            let title = team
                .spec
                .charter
                .lines()
                .next()
                .unwrap_or(&team.spec.charter)
                .to_string();
            let _ = crate::team_commons::record_entry(
                client, commons, &run, &title, &run, &run, output,
            )
            .await;
        }
        // Deliverable has landed (ok or error) — retire the sandbox so the run
        // doesn't keep consuming a pod. The task record + output ConfigMap
        // remain for history; the knowledge lives on in the commons.
        if launched {
            let retire = json!({ "spec": { "execution": { "launch": false } } });
            let _ = tasks.patch(&run, &PatchParams::default(), &Patch::Merge(retire)).await;
        }
    }
    active
}

/// SSA-apply a KarsTask owned by the team, tagged with team annotations. For a
/// `taskforce` run, also stamps the run-request annotation the mesh delivery
/// loop watches, so the standing-operation run executes autonomously.
async fn apply_task(
    tasks: &Api<KarsTask>,
    team: &KarsTeam,
    task_name: &str,
    spec: KarsTaskSpec,
    role: &str,
) -> Result<(), ReconcileError> {
    let mut annotations = serde_json::Map::new();
    annotations.insert(ANNOT_TEAM.into(), json!(team.name_any()));
    annotations.insert(ANNOT_TEAM_ROLE.into(), json!(role));
    if role == "taskforce" {
        // Stable nonce = run name, so the run is dispatched once and not
        // re-triggered on subsequent reconciles.
        annotations.insert(ANNOT_RUN_REQUESTED.into(), json!(task_name));
    }
    let obj = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "metadata": {
            "name": task_name,
            "ownerReferences": [owner_ref(team)],
            "annotations": annotations,
            "labels": { "kars.azure.com/team": team.name_any() },
        },
        "spec": spec,
    });
    tasks
        .patch(
            task_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(obj),
        )
        .await?;
    Ok(())
}

/// A safe attenuation of the team envelope for a member/task-force with no
/// explicit envelope: one tier below the team (floored at 1), ceiling matched,
/// one fewer delegation hop, same budget/policy refs.
fn default_member_envelope(team_env: &TaskEnvelope) -> TaskEnvelope {
    let tier = (team_env.tier - 1).max(crate::kars_task::TIER_MIN);
    let ceiling = team_env.authority_ceiling.min(tier);
    TaskEnvelope {
        tier,
        budget: team_env.budget.clone(),
        tool_policy_ref: team_env.tool_policy_ref.clone(),
        egress_allowlist_ref: team_env.egress_allowlist_ref.clone(),
        delegation_depth: (team_env.delegation_depth - 1).max(0),
        authority_ceiling: ceiling.max(crate::kars_task::TIER_MIN),
    }
}

/// Resolve a member's blueprint: role override merged over the team default, so
/// a role can specialise (its own prompt/tools) while inheriting team defaults.
fn member_blueprint(team: &KarsTeam, role: &TeamRole) -> Option<TaskBlueprint> {
    match (&team.spec.blueprint, &role.blueprint) {
        (_, Some(rb)) => Some(rb.clone()),
        (Some(tb), None) => Some(tb.clone()),
        (None, None) => None,
    }
}

async fn write_status(
    teams: &Api<KarsTeam>,
    name: &str,
    status: KarsTeamStatus,
) -> Result<(), ReconcileError> {
    let patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTeam",
        "status": status,
    });
    teams
        .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(patch))
        .await?;
    Ok(())
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

/// Sanitize a role name into a K8s-safe name suffix.
fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() { "role".to_string() } else { trimmed }
}

fn has_finalizer(team: &KarsTeam) -> bool {
    team.metadata
        .finalizers
        .as_ref()
        .is_some_and(|f| f.iter().any(|s| s == FINALIZER))
}

fn drop_finalizer(team: &KarsTeam) -> Vec<String> {
    team.metadata
        .finalizers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s != FINALIZER)
        .collect()
}

fn error_policy(_team: Arc<KarsTeam>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("KarsTeam", error.class());
    Action::requeue(REQUEUE_PENDING)
}

pub async fn run(client: Client) -> Result<()> {
    let teams: Api<KarsTeam> = Api::all(client.clone());
    match teams.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("KarsTeam CRD found — starting reconciler"),
        Err(e) => {
            tracing::warn!("KarsTeam CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx { client });
    Controller::new(teams, crate::watch_config::bounded())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("KarsTeam", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("KarsTeam reconciled {:?}", o),
                Err(e) => tracing::warn!("KarsTeam reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_task::{TaskBudget, TaskEnvelope};

    fn team_env() -> TaskEnvelope {
        TaskEnvelope {
            tier: 4,
            budget: Some(TaskBudget { tokens: Some(1_000_000), usd_micros: None }),
            tool_policy_ref: Some(LocalObjectRef { name: "kars-default".into() }),
            egress_allowlist_ref: None,
            delegation_depth: 2,
            authority_ceiling: 3,
        }
    }

    #[test]
    fn default_member_envelope_attenuates_team() {
        let team = team_env();
        let m = default_member_envelope(&team);
        // strictly attenuated on every axis the lattice checks
        assert!(m.tier <= team.tier);
        assert!(m.authority_ceiling <= team.authority_ceiling);
        assert!(m.delegation_depth <= team.delegation_depth);
        // and it is a valid subset (no violations against the team)
        assert!(
            m.attenuation_violations(&team).is_empty(),
            "{:?}",
            m.attenuation_violations(&team)
        );
    }

    #[test]
    fn default_member_envelope_floors_tier_at_one() {
        let mut team = team_env();
        team.tier = 1;
        team.authority_ceiling = 1;
        let m = default_member_envelope(&team);
        assert_eq!(m.tier, 1);
        assert_eq!(m.authority_ceiling, 1);
        assert!(m.attenuation_violations(&team).is_empty());
    }

    #[test]
    fn sanitize_makes_safe_names() {
        assert_eq!(sanitize("Bugfix Engineer"), "bugfix-engineer");
        assert_eq!(sanitize("docs/quality"), "docs-quality");
        assert_eq!(sanitize("  "), "role");
    }

    #[test]
    fn parse_rfc3339_roundtrips() {
        let now = Utc::now();
        let s = now.to_rfc3339();
        let back = parse_rfc3339(&s).unwrap();
        assert!((back - now).num_seconds().abs() < 2);
    }
}
