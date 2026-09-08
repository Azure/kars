// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standing Team orchestration. Authority generation, capability resolution,
//! ownership-checked lifecycle, promotion and durable harvesting live in
//! separate modules. Teams remain additive; Bridge is an optional consumer.

mod capabilities;
#[cfg(test)]
mod persistence_tests;
mod promotion;
mod runs;
pub(crate) mod specs;
#[cfg(test)]
mod state_tests;
mod tasks;
#[cfg(test)]
mod tests;

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams},
    runtime::{Controller, controller::Action},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

use crate::kars_task::KarsTask;
use crate::kars_team::{KarsTeam, KarsTeamStatus};
use crate::mcp_server::LocalObjectRef;
use crate::status::phase::{PHASE_ACTIVE, PHASE_DEGRADED, PHASE_HIBERNATING};

const FINALIZER: &str = "kars.azure.com/karsteam-cleanup";
const REQUEUE_OK: Duration = Duration::from_secs(60);
const REQUEUE_PENDING: Duration = Duration::from_secs(10);
const ANNOT_TEAM: &str = "kars.azure.com/team";
const ANNOT_TEAM_ROLE: &str = "kars.azure.com/team-role";
const ANNOT_RUN_REQUESTED: &str = "kars.azure.com/run-requested";
const MAX_CONCURRENT_RUNS: usize = 2;

#[derive(thiserror::Error, Debug)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("Team persistence failed: {0:#}")]
    Persistence(#[from] anyhow::Error),
}

impl ReconcileError {
    fn class(&self) -> &'static str {
        match self {
            Self::Kube(_) => "kube_api",
            Self::SerdeJson(_) => "serde",
            Self::Invalid(_) => "invalid_authority",
            Self::Persistence(_) => "persistence",
        }
    }
}

struct Ctx {
    client: Client,
}

fn namespace(team: &KarsTeam) -> Result<&str, ReconcileError> {
    team.metadata
        .namespace
        .as_deref()
        .filter(|namespace| !namespace.is_empty())
        .ok_or_else(|| ReconcileError::Invalid("Team has no namespace".into()))
}

fn resource_version(team: &KarsTeam) -> Result<&str, ReconcileError> {
    team.metadata
        .resource_version
        .as_deref()
        .filter(|version| !version.is_empty())
        .ok_or_else(|| ReconcileError::Invalid("Team has no resourceVersion".into()))
}

async fn reconcile(team: Arc<KarsTeam>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let ns = namespace(&team)?;
    let teams = Api::<KarsTeam>::namespaced(ctx.client.clone(), ns);
    let tasks_api = Api::<KarsTask>::namespaced(ctx.client.clone(), ns);
    let finalizers = team.metadata.finalizers.clone().unwrap_or_default();
    if team.metadata.deletion_timestamp.is_some() {
        if finalizers.iter().any(|value| value == FINALIZER) {
            tasks::revoke_all(&tasks_api, &team).await?;
            let remaining: Vec<_> = finalizers
                .into_iter()
                .filter(|value| value != FINALIZER)
                .collect();
            teams.patch(&team.name_any(), &PatchParams::default(), &Patch::Merge(json!({
                "metadata": { "resourceVersion": resource_version(&team)?, "finalizers": remaining }
            }))).await?;
        }
        return Ok(Action::await_change());
    }
    if !finalizers.iter().any(|value| value == FINALIZER) {
        let mut next = finalizers;
        next.push(FINALIZER.into());
        teams
            .patch(
                &team.name_any(),
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata": { "resourceVersion": resource_version(&team)?, "finalizers": next }
                })),
            )
            .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let effective = match capabilities::effective_team(&ctx.client, &team).await {
        Ok(effective) => effective,
        Err(error) => return fail_closed(&teams, &tasks_api, &team, error).await,
    };
    let errors = effective.validation_errors();
    if !errors.is_empty() {
        return fail_closed(
            &teams,
            &tasks_api,
            &team,
            ReconcileError::Invalid(format!("invalid team: {}", errors.join("; "))),
        )
        .await;
    }
    if let Err(error) = capabilities::capability_readiness(&ctx.client, &effective).await {
        return fail_closed(&teams, &tasks_api, &team, error).await;
    }
    let outcome = reconcile_valid(&teams, &tasks_api, &effective, &ctx.client).await;
    if let Err(error) = &outcome {
        write_degraded(&teams, &team, &error.to_string()).await?;
    }
    outcome
}

async fn fail_closed(
    teams: &Api<KarsTeam>,
    tasks_api: &Api<KarsTask>,
    team: &KarsTeam,
    error: ReconcileError,
) -> Result<Action, ReconcileError> {
    // Revoke even when status writes are unavailable; report Degraded even
    // when a retirement failed and needs another pass.
    let revoked = tasks::revoke_all(tasks_api, team).await;
    write_degraded(teams, team, &error.to_string()).await?;
    revoked?;
    Err(error)
}

async fn write_degraded(
    teams: &Api<KarsTeam>,
    team: &KarsTeam,
    detail: &str,
) -> Result<(), ReconcileError> {
    let mut status = team.status.clone().unwrap_or_default();
    status.phase = Some(PHASE_DEGRADED.into());
    status.observed_generation = team.metadata.generation;
    status.envelope_digest = None;
    status.detail = Some(detail.into());
    write_status(teams, team, status).await
}

async fn reconcile_valid(
    teams: &Api<KarsTeam>,
    tasks_api: &Api<KarsTask>,
    team: &KarsTeam,
    client: &Client,
) -> Result<Action, ReconcileError> {
    // Revoke old task-force authority and removed seats before creating anything.
    tasks::reconcile_revocations(tasks_api, team).await?;
    crate::team_commons::ensure_commons(client, team).await?;
    let stats = runs::harvest_and_retire_runs(client, tasks_api, team).await?;
    let principal_name = specs::principal_name(team);
    tasks::apply_task(
        tasks_api,
        team,
        &principal_name,
        specs::principal_spec(team),
        "principal",
    )
    .await?;
    if promotion::process_promotion(client, team, &principal_name).await? {
        // Promotion changed spec/generation; never publish an old-envelope status.
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    let mut members = Vec::new();
    for role in &team.spec.roster {
        let name = specs::member_name(team, role);
        tasks::apply_task(
            tasks_api,
            team,
            &name,
            specs::member_spec(team, role),
            "member",
        )
        .await?;
        members.push(LocalObjectRef { name });
    }

    let prior = team.status.clone().unwrap_or_default();
    let now = Utc::now();
    let every = team
        .spec
        .cadence
        .as_ref()
        .and_then(|cadence| cadence.every_minutes);
    let finite = specs::has_positive_budget(&team.spec.envelope);
    let unsupported = specs::unsupported_budget(&team.spec.envelope);
    let budget_error = if finite && !unsupported {
        crate::inference_budget::team::ready(client, team, &principal_name)
            .await
            .err()
    } else {
        None
    };
    let bounded_plan = unsupported || budget_error.is_some();
    let cadence_blocked = bounded_plan && every.is_some() && !team.spec.paused;
    let mut generated = prior.generated_task_count;
    let mut last_generated = prior.last_generated_task.clone();
    let mut last_run_at = prior.last_run_at.clone();
    let mut next_run_at = None;
    if let Some(minutes) = every {
        let interval = chrono::Duration::minutes(i64::from(minutes));
        let previous = parse_optional_time(prior.last_run_at.as_deref(), "lastRunAt")?;
        let due = previous.is_none_or(|previous| now >= previous + interval);
        if !team.spec.paused && !bounded_plan && due && stats.active < MAX_CONCURRENT_RUNS {
            let name = runs::cadence_name(team)?;
            let allowance = specs::run_knowledge_budget(team).map_err(ReconcileError::Invalid)?;
            let knowledge = crate::team_commons::prior_knowledge(client, team, allowance).await?;
            let task = tasks::apply_task(
                tasks_api,
                team,
                &name,
                specs::run_spec(team, &knowledge).map_err(ReconcileError::Invalid)?,
                "taskforce",
            )
            .await?;
            let created_timestamp = task
                .metadata
                .creation_timestamp
                .as_ref()
                .map(|time| time.0.to_string());
            let created =
                parse_optional_time(created_timestamp.as_deref(), "task creationTimestamp")?
                    .unwrap_or(now);
            generated = generated.saturating_add(1);
            last_generated = Some(name);
            last_run_at = Some(created.to_rfc3339());
            next_run_at = Some((created + interval).to_rfc3339());
        } else {
            next_run_at = previous.map(|previous| (previous + interval).to_rfc3339());
        }
    }
    let last_success_at = stats
        .last_success_at
        .clone()
        .or(prior.last_success_at.clone());
    let overdue = matches!(
        (every, next_run_at.as_deref().and_then(parse_rfc3339)),
        (Some(minutes), Some(next)) if now > next + chrono::Duration::minutes(2 * i64::from(minutes))
    );
    let health = if team.spec.paused {
        "Hibernating"
    } else if cadence_blocked {
        PHASE_DEGRADED
    } else if generated == 0 {
        "Watching"
    } else if overdue {
        "Stalled"
    } else if stats.succeeded > 0 || last_success_at.is_some() {
        "Healthy"
    } else if stats.barren > 0 {
        "Unproductive"
    } else {
        "Watching"
    };
    let entries = crate::team_commons::entry_count(client, team).await?;
    let mut last_digest_at = prior.last_digest_at.clone();
    if let Some(minutes) = team
        .spec
        .cadence
        .as_ref()
        .and_then(|cadence| cadence.digest_every_minutes)
    {
        let previous = parse_optional_time(prior.last_digest_at.as_deref(), "lastDigestAt")?;
        let due = previous.map_or(generated > 0, |previous| {
            now >= previous + chrono::Duration::minutes(i64::from(minutes))
        });
        if !team.spec.paused && due {
            let summary = format!(
                "{health}: {generated} run(s) generated, {} delivered, {} tokens spent, {entries} knowledge entries.",
                stats.succeeded, stats.tokens_total,
            );
            crate::team_digest::publish(
                client,
                team,
                team.spec.reporting_to.as_deref(),
                health,
                &summary,
                generated,
                stats.succeeded,
                stats.tokens_total,
                entries,
            )
            .await?;
            last_digest_at = Some(now.to_rfc3339());
        }
    }
    let detail = if team.spec.paused {
        "Team hibernating — members and runs governed-but-idle; charter loop paused.".into()
    } else if let Some(error) = &budget_error {
        format!(
            "Governed inference admissions paused: {error}. No new cadence or launch is admitted; existing Task UIDs and funded work are retained."
        )
    } else if unsupported {
        "UnsupportedLaunchBudget: Team and member plans are governed-but-idle. Finite total/subtree token or monetary budgets require durable enforcement; cadence and bounded execution are unavailable.".into()
    } else if finite {
        format!(
            "Governed inference account bound to Team UID for its lifetime. Standing operation {health}; compute, tool, storage and invoice costs are excluded."
        )
    } else {
        format!(
            "Standing operation {health}: {generated} run(s), {} delivered, {entries} knowledge entries.",
            stats.succeeded
        )
    };
    write_status(
        teams,
        team,
        KarsTeamStatus {
            phase: Some(
                if team.spec.paused {
                    PHASE_HIBERNATING
                } else if cadence_blocked {
                    PHASE_DEGRADED
                } else {
                    PHASE_ACTIVE
                }
                .into(),
            ),
            observed_generation: team.metadata.generation,
            envelope_digest: Some(team.spec.envelope.digest()),
            principal_ref: Some(LocalObjectRef {
                name: principal_name,
            }),
            member_count: Some(members.len() as i64),
            member_refs: members,
            generated_task_count: generated,
            last_generated_task: last_generated,
            last_run_at,
            next_run_at,
            detail: Some(detail),
            health: Some(health.into()),
            runs_succeeded: Some(stats.succeeded),
            tokens_spent_total: Some(stats.tokens_total),
            commons_entry_count: Some(entries),
            last_success_at,
            last_digest_at,
            inference_budget_account: prior.inference_budget_account.clone(),
            ..Default::default()
        },
    )
    .await?;
    Ok(Action::requeue(if every.is_some() && !team.spec.paused {
        Duration::from_secs(30)
    } else {
        REQUEUE_OK
    }))
}

async fn write_status(
    teams: &Api<KarsTeam>,
    team: &KarsTeam,
    status: KarsTeamStatus,
) -> Result<(), ReconcileError> {
    let mut value = serde_json::to_value(status)?;
    // Merge-patch must clear removed optional authority facts, not retain a stale digest.
    if let Some(previous) = &team.status {
        for (key, _) in serde_json::to_value(previous)?
            .as_object()
            .into_iter()
            .flatten()
        {
            value
                .as_object_mut()
                .expect("status is an object")
                .entry(key.clone())
                .or_insert(serde_json::Value::Null);
        }
    }
    teams
        .patch_status(
            &team.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": { "resourceVersion": resource_version(team)? },
                "status": value,
            })),
        )
        .await?;
    Ok(())
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

fn parse_optional_time(
    value: Option<&str>,
    field: &str,
) -> Result<Option<DateTime<Utc>>, ReconcileError> {
    value
        .map(|value| {
            parse_rfc3339(value)
                .ok_or_else(|| ReconcileError::Invalid(format!("malformed {field}")))
        })
        .transpose()
}

fn error_policy(_team: Arc<KarsTeam>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("KarsTeam", error.class());
    Action::requeue(REQUEUE_PENDING)
}

pub async fn run(client: Client) -> Result<()> {
    let teams: Api<KarsTeam> = Api::all(client.clone());
    match teams.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("KarsTeam CRD found — starting reconciler"),
        Err(kube::Error::Api(error)) if error.code == 404 => {
            tracing::warn!("KarsTeam CRD not installed — reconciler disabled");
            std::future::pending::<()>().await;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    Controller::new(teams, crate::watch_config::bounded())
        .run(
            |team, ctx| async move {
                crate::metrics::observe_reconcile("KarsTeam", reconcile(team, ctx)).await
            },
            error_policy,
            Arc::new(Ctx { client }),
        )
        .for_each(|result| async move {
            match result {
                Ok(object) => tracing::debug!("KarsTeam reconciled {object:?}"),
                Err(error) => tracing::warn!("KarsTeam reconcile failed: {error:?}"),
            }
        })
        .await;
    Ok(())
}
