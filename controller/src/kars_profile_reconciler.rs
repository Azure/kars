// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsProfile` reconciler — validates a team template, pins its digest, and
//! marks it `Ready` (instantiable) or `Degraded`. The controller is the sole
//! writer of `KarsProfile.status`. Instantiation (a `KarsTeam` adopting a
//! profile via `spec.profileRef`) is performed by the `KarsTeam` reconciler.

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

use crate::kars_profile::{KarsProfile, KarsProfileStatus};
use crate::status::phase::{PHASE_DEGRADED, PHASE_READY};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_PROFILE;
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

async fn reconcile(profile: Arc<KarsProfile>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = profile.name_any();
    let ns = profile.namespace().unwrap_or_else(|| "default".into());
    let api: Api<KarsProfile> = Api::namespaced(ctx.client.clone(), &ns);

    let errors = profile.validation_errors();
    let status = if errors.is_empty() {
        KarsProfileStatus {
            phase: Some(PHASE_READY.into()),
            observed_generation: profile.metadata.generation,
            template_digest: Some(profile.template_digest()),
            role_count: Some(profile.spec.roles.len() as i64),
            detail: Some(format!(
                "Profile '{}' validated and instantiable ({} role(s)).",
                profile.spec.domain,
                profile.spec.roles.len()
            )),
            conditions: None,
        }
    } else {
        KarsProfileStatus {
            phase: Some(PHASE_DEGRADED.into()),
            observed_generation: profile.metadata.generation,
            template_digest: None,
            role_count: None,
            detail: Some(format!("invalid profile: {}", errors.join("; "))),
            conditions: None,
        }
    };

    let patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsProfile",
        "status": status,
    });
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(Action::requeue(REQUEUE_OK))
}

fn error_policy(_p: Arc<KarsProfile>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("KarsProfile", error.class());
    Action::requeue(REQUEUE_PENDING)
}

pub async fn run(client: Client) -> Result<()> {
    let profiles: Api<KarsProfile> = Api::all(client.clone());
    match profiles.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("KarsProfile CRD found — starting reconciler"),
        Err(e) => {
            tracing::warn!("KarsProfile CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx { client });
    Controller::new(profiles, crate::watch_config::bounded())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("KarsProfile", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("KarsProfile reconciled {:?}", o),
                Err(e) => tracing::warn!("KarsProfile reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}
