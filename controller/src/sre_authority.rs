// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Registrar-authorized migration and private SRE Kubernetes identity.

mod admission;
mod bindings;
mod credential_guard;
mod credentials;
mod live;
mod migration;
pub(crate) mod pod;
#[cfg(test)]
mod privacy_tests;
#[cfg(test)]
mod tests;

use crate::sre_registration::{KarsSRERegistration, NAME, RegistrationStatus};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
};

pub(crate) use live::{api_error, check_secret_denial, privacy_epoch};

async fn status(
    client: &Client,
    reg: &KarsSRERegistration,
    phase: &str,
    detail: Option<String>,
    service_account_uid: Option<String>,
) -> Result<(), String> {
    let api: Api<KarsSRERegistration> = Api::all(client.clone());
    let status = RegistrationStatus {
        phase: phase.into(),
        observed_generation: reg.metadata.generation.unwrap_or_default(),
        privacy_epoch: (phase == "Ready").then(|| reg.epoch()),
        router_service_account_uid: if phase == "Retired" {
            None
        } else {
            service_account_uid.or_else(|| {
                reg.status
                    .as_ref()
                    .and_then(|status| status.router_service_account_uid.clone())
            })
        },
        legacy_secret_access_denied: matches!(phase, "Ready" | "Retired"),
        privacy_revision: matches!(phase, "Ready" | "Retired")
            .then(|| crate::sre_privacy::REVISION.into()),
        detail,
    };
    let mut status_value =
        serde_json::to_value(&status).map_err(|_| "SRE authority status serialization failed")?;
    status_value["privacyEpoch"] = serde_json::json!(status.privacy_epoch);
    status_value["privacyRevision"] = serde_json::json!(status.privacy_revision);
    status_value["routerServiceAccountUid"] = serde_json::json!(status.router_service_account_uid);
    api.patch_status(
        NAME,
        &PatchParams::default(),
        &Patch::Merge(serde_json::json!({
            "metadata":{"uid":reg.metadata.uid,"resourceVersion":reg.metadata.resource_version},
            "status":status_value,
        })),
    )
    .await
    .map_err(|error| api_error("Publish SRE authority status", error))?;
    Ok(())
}

pub async fn reconcile(client: &Client, reg: &KarsSRERegistration) -> Result<(), String> {
    reg.validate()?;
    let result = reconcile_inner(client, reg).await;
    if let Err(error) = &result {
        let waiting = migration::is_waiting(error);
        let revocation = if waiting {
            Ok(())
        } else {
            bindings::revoke_private(client, reg).await
        };
        let detail = match revocation {
            Ok(()) => error.clone(),
            Err(revocation) => {
                format!("{error}; owned private authority revocation failed: {revocation}")
            }
        };
        let detail = if !waiting
            && reg
                .status
                .as_ref()
                .is_some_and(|status| status.router_service_account_uid.is_some())
        {
            match credentials::retire(client, reg).await {
                Ok(()) => detail,
                Err(error) => {
                    format!("{detail}; owned private credential retirement failed: {error}")
                }
            }
        } else {
            detail
        };
        status(
            client,
            reg,
            if waiting { "Migrating" } else { "Blocked" },
            Some(detail),
            None,
        )
        .await?;
    }
    result
}

async fn reconcile_inner(client: &Client, reg: &KarsSRERegistration) -> Result<(), String> {
    reg.validate()?;
    if !reg.spec.enabled
        && reg.status.as_ref().is_some_and(|status| {
            status.phase == "Retired"
                && status.observed_generation == reg.metadata.generation.unwrap_or_default()
                && status.privacy_revision.as_deref() == Some(crate::sre_privacy::REVISION)
        })
    {
        return check_secret_denial(client, &reg.spec.runtime_namespace.name).await;
    }
    if !reg.spec.enabled {
        migration::stop_registered_consumer_for_retirement(client, reg).await?;
        bindings::revoke_private(client, reg).await?;
        credentials::retire(client, reg).await?;
        check_secret_denial(client, &reg.spec.runtime_namespace.name).await?;
        return status(client, reg, "Retired", None, None).await;
    }
    let authority = live::verify(client, reg).await?;
    admission::verify(client).await?;
    credential_guard::scan(client, reg).await?;
    // Validate the full review set before retiring even the first grant.
    let reviewed = bindings::review(client, reg).await?;
    migration::validate_consumer(client, reg).await?;
    bindings::validate_private_targets(client, reg).await?;
    credentials::review_targets(client, reg).await?;
    bindings::retire_legacy(client, reg, &reviewed).await?;
    check_secret_denial(client, &reg.spec.runtime_namespace.name).await?;
    migration::stop_legacy_consumer(client, reg).await?;
    let service_account = credentials::ensure_service_account(client, reg, &authority).await?;
    let refreshed = if reg
        .status
        .as_ref()
        .and_then(|status| status.router_service_account_uid.as_ref())
        != service_account.metadata.uid.as_ref()
    {
        status(client, reg, "Provisioning", None, service_account.uid()).await?;
        Api::<KarsSRERegistration>::all(client.clone())
            .get(NAME)
            .await
            .map_err(|error| api_error("Refresh provisioning registration", error))?
    } else {
        reg.clone()
    };
    if refreshed.metadata.uid != reg.metadata.uid
        || refreshed.metadata.generation != reg.metadata.generation
    {
        return Err("Registration changed while provisioning private identity".into());
    }
    let reg = &refreshed;
    bindings::grant_private(client, reg, &service_account).await?;
    // No private material is minted until the old principal is actually denied.
    check_secret_denial(client, &reg.spec.runtime_namespace.name).await?;
    credential_guard::scan(client, reg).await?;
    live::verify(client, reg).await?;
    credentials::ensure(client, reg, &authority, &service_account).await?;
    migration::rotate_owned_control_credentials(client, reg).await?;
    status(client, reg, "Ready", None, service_account.uid()).await
}

pub async fn run(client: Client) {
    let api: Api<KarsSRERegistration> = Api::all(client.clone());
    loop {
        match api.get_opt(NAME).await {
            Ok(Some(reg)) => {
                if let Err(error) = reconcile(&client, &reg).await {
                    tracing::warn!(registration = NAME, error = %error, "SRE authority is not ready");
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(error = %api_error("Read SRE registration", error), "SRE authority unavailable")
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    }
}
