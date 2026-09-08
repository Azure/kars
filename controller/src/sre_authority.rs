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
mod retirement_tests;
#[cfg(test)]
mod tests;

use crate::sre_registration::{KarsSRERegistration, NAME, RegistrationStatus};
use crate::status::conditions;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
};

pub(crate) use live::{api_error, check_secret_denial, privacy_epoch};

fn registration_conditions(
    prior: &[Condition],
    generation: Option<i64>,
    phase: &str,
    detail: Option<&str>,
) -> Vec<Condition> {
    let (reason, message) = match phase {
        "Ready" => ("AuthorityReady", "Reviewed SRE authority is ready"),
        "Retired" => ("AuthorityRetired", "Private SRE authority has been retired"),
        "Provisioning" => ("Provisioning", "Private SRE identity is being provisioned"),
        "Migrating" => (
            "Migrating",
            "Reviewed SRE grants and consumers are migrating",
        ),
        _ => ("AuthorityBlocked", "SRE authority cannot be established"),
    };
    let mut result = prior.to_vec();
    for (kind, active) in [
        (conditions::TYPE_READY, phase == "Ready"),
        (
            conditions::TYPE_PROGRESSING,
            matches!(phase, "Migrating" | "Provisioning"),
        ),
        (
            conditions::TYPE_DEGRADED,
            !matches!(phase, "Ready" | "Retired" | "Migrating" | "Provisioning"),
        ),
    ] {
        conditions::set(
            &mut result,
            conditions::preserve_transition_time(
                conditions::find(prior, kind),
                kind,
                if active {
                    conditions::status::TRUE
                } else {
                    conditions::status::FALSE
                },
                reason,
                detail.unwrap_or(message),
                generation,
            ),
        );
    }
    result
}

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
        conditions: registration_conditions(
            reg.status
                .as_ref()
                .map(|status| status.conditions.as_slice())
                .unwrap_or_default(),
            reg.metadata.generation,
            phase,
            detail.as_deref(),
        ),
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

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn registration_phases_publish_truthful_standard_conditions() {
        for (phase, expected) in [
            ("Ready", ["True", "False", "False"]),
            ("Retired", ["False", "False", "False"]),
            ("Migrating", ["False", "True", "False"]),
            ("Provisioning", ["False", "True", "False"]),
            ("Blocked", ["False", "False", "True"]),
            ("UnknownPhase", ["False", "False", "True"]),
        ] {
            let result = registration_conditions(&[], Some(3), phase, None);
            assert_eq!(result.len(), 3);
            for (kind, expected) in ["Ready", "Progressing", "Degraded"]
                .into_iter()
                .zip(expected)
            {
                let condition = conditions::find(&result, kind).unwrap();
                assert_eq!(condition.status, expected, "{phase}/{kind}");
                assert_eq!(condition.observed_generation, Some(3));
                assert!(!condition.reason.is_empty());
                assert!(!condition.message.is_empty());
            }
        }
    }

    #[test]
    fn repeated_status_preserves_transition_time_and_unrelated_conditions() {
        let mut prior = registration_conditions(&[], Some(1), "Migrating", None);
        conditions::set(
            &mut prior,
            conditions::new_condition("CustomEvidence", "True", "Observed", "preserve", Some(1)),
        );
        let next = registration_conditions(&prior, Some(2), "Migrating", Some("Still draining"));
        assert_eq!(next.len(), 4);
        assert_eq!(
            conditions::find(&next, "CustomEvidence"),
            conditions::find(&prior, "CustomEvidence")
        );
        for kind in ["Ready", "Progressing", "Degraded"] {
            let before = conditions::find(&prior, kind).unwrap();
            let after = conditions::find(&next, kind).unwrap();
            assert_eq!(before.last_transition_time, after.last_transition_time);
            assert_eq!(after.observed_generation, Some(2));
            assert_eq!(after.message, "Still draining");
        }
    }
}
