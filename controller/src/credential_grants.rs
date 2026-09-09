// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod admission;
mod control;
pub(crate) mod github;
mod legacy;
mod operator;
pub(crate) mod readiness;
pub(crate) use operator::decorate as decorate_observations;
pub(crate) use operator::mount as mount_observations;
pub(crate) async fn verify_observation_writers(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<(), String> {
    writers::verify(client, grant).await
}
pub(crate) mod observation_network;
mod observer_metadata;
mod observer_rbac;
mod observer_runtime;
mod rbac;
pub(crate) mod sources;
mod writers;

use crate::credential_grant::*;
use k8s_openapi::api::core::v1::{Namespace, Secret, ServiceAccount};
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams},
};
use serde_json::json;

pub(crate) fn api_error(stage: &str, error: kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!("{stage}: Kubernetes status {}", status.code),
        _ => format!("{stage}: Kubernetes transport or serialization failure"),
    }
}

pub(crate) fn identity(meta: &kube::api::ObjectMeta) -> Result<(&str, &str), String> {
    match (meta.uid.as_deref(), meta.resource_version.as_deref()) {
        (Some(uid), Some(rv))
            if !uid.is_empty() && !rv.is_empty() && meta.deletion_timestamp.is_none() =>
        {
            Ok((uid, rv))
        }
        _ => Err("Credential object identity is absent or terminating".into()),
    }
}

pub(crate) async fn verify(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    identity(&grant.metadata)?;
    let namespace = grant
        .namespace()
        .ok_or("Credential grant workspace missing")?;
    let current = Api::<KarsCredentialGrant>::namespaced(client.clone(), &namespace)
        .get(NAME)
        .await
        .map_err(|e| api_error("Recheck live credential grant", e))?;
    if current.metadata.uid != grant.metadata.uid
        || current.metadata.generation != grant.metadata.generation
        || current.metadata.deletion_timestamp.is_some()
    {
        return Err("Credential grant changed during reconciliation".into());
    }
    if grant.name_any() != NAME
        || !grant.spec.enabled
        || grant.spec.writers.len() > 16
        || grant.spec.integration_stores.len() > 32
        || grant.spec.github_connections.len() > 32
    {
        return Err("Credential grant is disabled or has invalid bounds".into());
    }
    permitted_agent_keys(grant)?;
    let namespace = grant
        .namespace()
        .ok_or("Credential grant workspace missing")?;
    let live = Api::<Namespace>::all(client.clone())
        .get(&namespace)
        .await
        .map_err(|e| api_error("Verify credential workspace", e))?;
    if identity(&live.metadata)?.0 != grant.spec.workspace_uid {
        return Err("Credential workspace was replaced".into());
    }
    let mut names = std::collections::BTreeSet::new();
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace);
    for store in &grant.spec.integration_stores {
        if !names.insert(&store.secret.name)
            || store.secret.uid.is_empty()
            || store.secret.name.starts_with(INPUT_PREFIX)
            || store.secret.name.starts_with(BUNDLE_PREFIX)
            || ![
                "providers",
                "foundry",
                "provider-default",
                "github-app",
                "github-connection",
                "teams",
                "controller-settings",
            ]
            .contains(&store.purpose.as_str())
        {
            return Err(
                "Integration stores must have unique explicitly enrolled identities and purposes"
                    .into(),
            );
        }
        let secret = secrets
            .get(&store.secret.name)
            .await
            .map_err(|e| api_error("Verify enrolled integration store", e))?;
        if identity(&secret.metadata)?.0 != store.secret.uid
            || secret.type_.as_deref() != Some("Opaque")
            || secret
                .data
                .iter()
                .flatten()
                .any(|(key, _)| !integration_keys(&store.purpose, &store.secret.name, key))
        {
            return Err(
                "Integration store identity, type, or key purpose differs from the operator grant"
                    .into(),
            );
        }
    }
    Ok(())
}

pub(crate) async fn current(
    client: &Client,
    namespace: &str,
    reference: &ObjectIdentity,
) -> Result<KarsCredentialGrant, String> {
    if reference.name != NAME || reference.uid.is_empty() {
        return Err("An exact workspace credential grant is required".into());
    }
    let grant = Api::<KarsCredentialGrant>::namespaced(client.clone(), namespace)
        .get(NAME)
        .await
        .map_err(|e| api_error("Read credential grant", e))?;
    verify(client, &grant).await?;
    if grant.uid().as_deref() != Some(reference.uid.as_str())
        || grant.status.as_ref().is_none_or(|status| {
            status.phase != "Ready"
                || status.observed_generation != grant.metadata.generation.unwrap_or_default()
        })
    {
        return Err("Credential grant is stale, unready, or replaced".into());
    }
    Ok(grant)
}

struct AuxiliaryStatus {
    integration: Result<String, String>,
    writer_error: Option<String>,
}

async fn publish(
    client: &Client,
    grant: &KarsCredentialGrant,
    phase: &str,
    reason: String,
    sources: Vec<SourceMetadata>,
    legacy_sources: Vec<LegacyImport>,
    auxiliary: AuxiliaryStatus,
) -> Result<(), String> {
    let AuxiliaryStatus {
        integration,
        writer_error,
    } = auxiliary;
    let mut conditions = grant
        .status
        .as_ref()
        .map(|s| s.conditions.clone())
        .unwrap_or_default();
    for (kind, active) in [
        ("Ready", phase == "Ready"),
        ("Progressing", phase == "Pending"),
        ("Degraded", phase == "Blocked"),
    ] {
        let condition = crate::status::conditions::preserve_transition_time(
            crate::status::conditions::find(&conditions, kind),
            kind,
            if active { "True" } else { "False" },
            phase,
            &reason,
            grant.metadata.generation,
        );
        crate::status::conditions::set(&mut conditions, condition);
    }
    let integration_condition = crate::status::conditions::preserve_transition_time(
        crate::status::conditions::find(&conditions, "IntegrationReady"),
        "IntegrationReady",
        if phase == "Ready" && integration.is_ok() {
            "True"
        } else {
            "False"
        },
        if integration.is_ok() {
            "Reconciled"
        } else {
            "IntegrationUnavailable"
        },
        integration
            .as_ref()
            .err()
            .map(String::as_str)
            .unwrap_or("Enrolled integration reconciliation completed"),
        grant.metadata.generation,
    );
    crate::status::conditions::set(&mut conditions, integration_condition);
    let writer_condition = crate::status::conditions::preserve_transition_time(
        crate::status::conditions::find(&conditions, "WriterReady"),
        "WriterReady",
        if writer_error.is_none() {
            "True"
        } else {
            "False"
        },
        if writer_error.is_none() {
            "Enrolled"
        } else {
            "WriterUnavailable"
        },
        writer_error
            .as_deref()
            .unwrap_or("Enrolled writer identities are current"),
        grant.metadata.generation,
    );
    crate::status::conditions::set(&mut conditions, writer_condition);
    let status = CredentialGrantStatus {
        observed_generation: grant.metadata.generation.unwrap_or_default(),
        phase: phase.into(),
        reason,
        sources,
        legacy_sources,
        conditions,
        integration_revision: integration.as_ref().ok().cloned(),
        integration_error: integration.err(),
    };
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let mut status_value =
        serde_json::to_value(&status).map_err(|_| "Grant status serialization failed")?;
    status_value["integrationError"] = json!(status.integration_error);
    status_value["integrationRevision"] = json!(status.integration_revision);
    Api::<KarsCredentialGrant>::namespaced(client.clone(),&namespace).patch_status(NAME,&PatchParams::default(),
        &Patch::Merge(json!({"metadata":{"uid":grant.metadata.uid,"resourceVersion":grant.metadata.resource_version},"status":status_value})))
        .await.map_err(|e|api_error("Publish credential authority",e))?;
    Ok(())
}

pub(crate) async fn reconcile(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    const FINALIZER: &str = "kars.azure.com/credential-authority";
    let namespace = grant.namespace().ok_or("Grant namespace missing")?;
    let api: Api<KarsCredentialGrant> = Api::namespaced(client.clone(), &namespace);
    if grant.metadata.deletion_timestamp.is_some() {
        github::revoke(client, grant).await?;
        operator::revoke(client, grant).await?;
        rbac::revoke(client, grant).await?;
        writers::release(client, grant).await?;
        let finalizers = grant
            .metadata
            .finalizers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry != FINALIZER)
            .collect::<Vec<_>>();
        api.patch(NAME,&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":grant.metadata.uid,"resourceVersion":grant.metadata.resource_version,"finalizers":finalizers}
        }))).await.map_err(|e|api_error("Finalize revoked credential authority",e))?;
        return Ok(());
    }
    let mut owned = grant.clone();
    if !grant
        .metadata
        .finalizers
        .as_ref()
        .is_some_and(|values| values.iter().any(|value| value == FINALIZER))
    {
        let mut finalizers = grant.metadata.finalizers.clone().unwrap_or_default();
        finalizers.push(FINALIZER.into());
        owned=api.patch(NAME,&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":grant.metadata.uid,"resourceVersion":grant.metadata.resource_version,"finalizers":finalizers}
        }))).await.map_err(|e|api_error("Protect credential authority revocation lifecycle",e))?;
    }
    let grant = &owned;
    let validation = async {
        verify(client, grant).await?;
        admission::verify(client).await?;
        let sources = sources::inventory(client, grant).await?;
        let legacy = legacy::inventory(client, grant).await?;
        Ok::<_, String>((sources, legacy))
    }
    .await;
    match validation {
        Ok((sources, legacy)) => {
            let (active, writer_error) = writers::authority(client, grant, &sources).await;
            let observations =
                if writer_error.is_some() && !grant.spec.observation_targets.is_empty() {
                    Err("Observation recipient authority is unavailable".into())
                } else if writer_error.is_some() {
                    operator::revoke(client, grant).await.map(|_| true)
                } else {
                    operator::reconcile(client, &active).await
                };
            let controls = control::reconcile(client, grant).await;
            let integration = match observations {
                Ok(true) => controls,
                Ok(false) => Err(controls
                    .err()
                    .map(|error| {
                        format!(
                            "Private observations awaiting verifier/consumer readiness; {error}"
                        )
                    })
                    .unwrap_or_else(|| {
                        "Private observations awaiting verifier/consumer readiness".into()
                    })),
                Err(error) => {
                    let revoked = operator::revoke(client, grant).await;
                    let mut detail = match revoked {
                        Ok(()) => format!("Private observations unavailable: {error}"),
                        Err(revoke) => format!(
                            "Private observations unavailable: {error}; revocation failed: {revoke}"
                        ),
                    };
                    if let Err(control) = controls {
                        detail.push_str(&format!("; integration control unavailable: {control}"));
                    }
                    Err(detail)
                }
            };
            publish(
                client,
                grant,
                "Ready",
                "Credential source and integration-store authority is current".into(),
                sources,
                legacy,
                AuxiliaryStatus {
                    integration,
                    writer_error,
                },
            )
            .await
        }
        Err(reason) => {
            let revoked = rbac::revoke(client, grant).await;
            let operators = operator::revoke(client, grant).await;
            let github = github::revoke(client, grant).await;
            if revoked.is_ok() && operators.is_ok() {
                writers::release(client, grant).await?;
            }
            let reason = revoked
                .err()
                .or_else(|| operators.err())
                .or_else(|| github.err())
                .map(|e| format!("{reason}; owned writer revocation failed: {e}"))
                .unwrap_or(reason);
            publish(
                client,
                grant,
                if grant.spec.enabled {
                    "Blocked"
                } else {
                    "Revoked"
                },
                reason.clone(),
                Vec::new(),
                Vec::new(),
                AuxiliaryStatus {
                    integration: Ok(String::new()),
                    writer_error: Some("Writer authority is revoked".into()),
                },
            )
            .await?;
            Err(reason)
        }
    }
}

pub async fn run(client: Client) {
    let grants: Api<KarsCredentialGrant> = Api::all(client.clone());
    loop {
        match grants.list(&ListParams::default()).await {
            Ok(list) => {
                for grant in list {
                    if let Err(error) = reconcile(&client, &grant).await {
                        tracing::warn!(namespace=?grant.namespace(),error=%error,"Credential grant is not ready");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(error=%api_error("Read credential grants",error),"Credential authority unavailable")
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    }
}
