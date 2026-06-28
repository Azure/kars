// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsSkill` reconciler — validates a capability bundle, pins its version
//! digest, and marks it `Ready` (grantable) or `Degraded` (invalid). The
//! controller is the sole writer of `KarsSkill.status`. Skills are consumed by
//! the `KarsTeam` reconciler (merged into member blueprints when a role
//! acquires a skill); this reconciler only validates + versions them.

use anyhow::Result;
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

use crate::kars_skill::{KarsSkill, KarsSkillStatus};
use crate::status::phase::{PHASE_DEGRADED, PHASE_READY};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_SKILL;
const REQUEUE_OK: Duration = Duration::from_secs(300);
const REQUEUE_PENDING: Duration = Duration::from_secs(10);

#[derive(thiserror::Error, Debug)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
}

impl ReconcileError {
    fn class(&self) -> &'static str {
        match self {
            ReconcileError::Kube(_) => "kube_api",
        }
    }
}

struct Ctx {
    client: Client,
}

async fn reconcile(skill: Arc<KarsSkill>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = skill.name_any();
    let ns = skill.namespace().unwrap_or_else(|| "default".into());
    let api: Api<KarsSkill> = Api::namespaced(ctx.client.clone(), &ns);

    let errors = skill.validation_errors();
    let status = if errors.is_empty() {
        KarsSkillStatus {
            phase: Some(PHASE_READY.into()),
            observed_generation: skill.metadata.generation,
            version_digest: Some(skill.version_digest()),
            attestation_ref: skill.spec.attestation_ref.clone(),
            detail: Some(format!(
                "Skill v{} validated and grantable.",
                skill.spec.version
            )),
            conditions: None,
        }
    } else {
        KarsSkillStatus {
            phase: Some(PHASE_DEGRADED.into()),
            observed_generation: skill.metadata.generation,
            version_digest: None,
            attestation_ref: None,
            detail: Some(format!("invalid skill: {}", errors.join("; "))),
            conditions: None,
        }
    };

    let patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSkill",
        "status": status,
    });
    api.patch_status(&name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(patch))
        .await?;
    Ok(Action::requeue(REQUEUE_OK))
}

fn error_policy(_skill: Arc<KarsSkill>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("KarsSkill", error.class());
    Action::requeue(REQUEUE_PENDING)
}

pub async fn run(client: Client) -> Result<()> {
    let skills: Api<KarsSkill> = Api::all(client.clone());
    match skills.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("KarsSkill CRD found — starting reconciler"),
        Err(e) => {
            tracing::warn!("KarsSkill CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx { client });
    Controller::new(skills, crate::watch_config::bounded())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("KarsSkill", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("KarsSkill reconciled {:?}", o),
                Err(e) => tracing::warn!("KarsSkill reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}
