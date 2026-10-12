// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsSkill` reconciler — validates a capability bundle, pins its version
//! digest, and marks it `Ready` (grantable) or `Degraded` (invalid). The
//! controller is the sole writer of `KarsSkill.status`. Skills are consumed by
//! the `KarsTeam` reconciler (merged into member blueprints when a role
//! acquires a skill); this reconciler only validates + versions them.

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
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

    let mut errors = skill.validation_errors();
    if errors.is_empty() {
        let policies: Api<crate::tool_policy::ToolPolicy> =
            Api::namespaced(ctx.client.clone(), &ns);
        if policies
            .get_opt(&skill.spec.bounding_policy)
            .await?
            .is_none_or(|policy| policy.metadata.deletion_timestamp.is_some())
        {
            errors
                .push("The bounding ToolPolicy is missing or terminating in this namespace".into());
        }
    }
    if errors.is_empty() && skill.spec.package {
        let packages: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);
        match packages.get_opt(&format!("karsskill-{name}")).await? {
            Some(package) => {
                if let Err(error) = skill.validate_package(&package) {
                    errors.push(error);
                }
            }
            None => errors.push("Waiting for the owned skill package ConfigMap".into()),
        }
    }
    let valid = errors.is_empty();
    let status = if valid {
        KarsSkillStatus {
            phase: Some(PHASE_READY.into()),
            observed_generation: skill.metadata.generation,
            version_digest: Some(skill.version_digest()),
            attestation_ref: skill.spec.attestation_ref.clone(),
            detail: Some(format!(
                "Skill v{} content validated; independent operator approval is required before use. Attestation references are unverified provenance claims.",
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
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(Action::requeue(if valid {
        REQUEUE_OK
    } else {
        REQUEUE_PENDING
    }))
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
    let packages: Api<ConfigMap> = Api::all(client.clone());
    let ctx = Arc::new(Ctx { client });
    Controller::new(skills, crate::watch_config::bounded())
        .owns(packages, crate::watch_config::bounded())
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
