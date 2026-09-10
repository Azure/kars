// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    constant_time::constant_time_eq,
    crd::KarsSandbox,
    credential_grant::{KarsCredentialGrant, NAME},
    observation_privacy::{self as wire, Operation},
    reconciler::governed_services,
    service_observer::Binding,
};
use k8s_openapi::api::core::v1::{Namespace, Secret, ServiceAccount};
use kube::{Api, Client, ResourceExt};

const DENIED: &str = "Observation privacy authority unavailable";

fn live(meta: &kube::api::ObjectMeta) -> Result<(), String> {
    if meta.uid.as_deref().is_none_or(str::is_empty)
        || meta.resource_version.as_deref().is_none_or(str::is_empty)
        || meta.deletion_timestamp.is_some()
    {
        return Err(DENIED.into());
    }
    Ok(())
}

async fn registration_current(client: &Client, epoch: Option<&str>) -> Result<(), String> {
    use crate::sre_registration::{KarsSRERegistration, ROUTER_SA, RUNTIME_NAMESPACE};
    let current = Api::<KarsSRERegistration>::all(client.clone())
        .get_opt("canonical")
        .await
        .map_err(|_| DENIED)?;
    match (current, epoch) {
        (None, None) => Ok(()),
        (Some(reg), Some(epoch)) => {
            live(&reg.metadata)?;
            let status = reg.status.as_ref().ok_or(DENIED)?;
            if !reg.spec.enabled
                || status.phase != "Ready"
                || status.observed_generation != reg.metadata.generation.unwrap_or_default()
                || reg.epoch() != epoch
                || status.privacy_epoch.as_deref() != Some(epoch)
            {
                return Err(DENIED.into());
            }
            let account = Api::<ServiceAccount>::namespaced(client.clone(), RUNTIME_NAMESPACE)
                .get(ROUTER_SA)
                .await
                .map_err(|_| DENIED)?;
            live(&account.metadata)?;
            if account.metadata.uid != status.router_service_account_uid
                || status.router_service_account_uid.is_none()
            {
                return Err(DENIED.into());
            }
            Ok(())
        }
        (Some(reg), None)
            if !reg.spec.enabled
                && reg.metadata.deletion_timestamp.is_none()
                && reg.status.as_ref().is_some_and(|status| {
                    status.phase == "Retired"
                        && status.observed_generation == reg.metadata.generation.unwrap_or_default()
                        && status.legacy_secret_access_denied
                        && status.privacy_revision.as_deref() == Some(crate::sre_privacy::REVISION)
                }) =>
        {
            Ok(())
        }
        _ => Err(DENIED.into()),
    }
}

async fn snapshot(
    client: &Client,
    request: &wire::Request,
    bearer: &str,
) -> Result<Binding, String> {
    let mut diagnostic = wire::Readiness::new("rpc_grant_read");
    let target = &request.target;
    let grant = Api::<KarsCredentialGrant>::namespaced(client.clone(), &target.workspace)
        .get(NAME)
        .await
        .map_err(|_| DENIED)?;
    live(&grant.metadata)?;
    diagnostic.stage("rpc_grant_current");
    if grant.uid().as_deref() != Some(request.grant_uid.as_str())
        || grant.metadata.generation != Some(request.grant_generation)
        || !grant.spec.enabled
        || grant.spec.workspace_uid != target.workspace_uid
        || grant.status.as_ref().is_none_or(|status| {
            status.phase != "Ready"
                || status.observed_generation != request.grant_generation
                || !status.conditions.iter().any(|condition| {
                    condition.type_ == "WriterReady"
                        && condition.status == "True"
                        && condition.observed_generation == Some(request.grant_generation)
                })
        })
        || !grant.spec.observation_targets.iter().any(|selected| {
            selected.kind == "KarsSandbox"
                && selected.namespace == target.workspace
                && selected.name == target.name
                && selected.uid == target.uid
        })
    {
        return Err(DENIED.into());
    }
    diagnostic.stage("rpc_target_read");
    let sandbox = Api::<KarsSandbox>::namespaced(client.clone(), &target.workspace)
        .get(&target.name)
        .await
        .map_err(|_| DENIED)?;
    live(&sandbox.metadata)?;
    diagnostic.stage("rpc_target_current");
    let observed = sandbox
        .status
        .as_ref()
        .and_then(|status| status.service_observation.as_ref())
        .ok_or(DENIED)?;
    if sandbox.uid().as_deref() != Some(target.uid.as_str())
        || observed.capability != crate::service_observer::CAPABILITY
        || observed.version != request.credential_version
        || observed.grant.uid != request.grant_uid
        || observed.grant.name != NAME
        || observed.namespace_uid != target.namespace_uid
        || !(observed.phase == "Ready"
            || (request.operation == Operation::Scope && observed.phase == "Prepared"))
    {
        return Err(DENIED.into());
    }
    diagnostic.stage("rpc_namespaces");
    let workspace = Api::<Namespace>::all(client.clone())
        .get(&target.workspace)
        .await
        .map_err(|_| DENIED)?;
    let runtime_name = format!("kars-{}", target.name);
    let namespace = Api::<Namespace>::all(client.clone())
        .get(&runtime_name)
        .await
        .map_err(|_| DENIED)?;
    live(&workspace.metadata)?;
    live(&namespace.metadata)?;
    if workspace.uid().as_deref() != Some(target.workspace_uid.as_str())
        || namespace.uid().as_deref() != Some(target.namespace_uid.as_str())
    {
        return Err(DENIED.into());
    }
    diagnostic.stage("rpc_credential_read");
    let secret = Api::<Secret>::namespaced(client.clone(), &runtime_name)
        .get(crate::service_observer::SECRET)
        .await
        .map_err(|_| DENIED)?;
    diagnostic.stage("rpc_credential_current");
    governed_services::credentials::validate(
        &secret,
        &target.uid,
        &namespace,
        governed_services::credentials::OBSERVER,
    )?;
    if secret.type_.as_deref() != Some("Opaque")
        || secret.uid().as_deref() != Some(observed.secret.uid.as_str())
        || observed.secret.name != crate::service_observer::SECRET
        || request.credential_version
            != format!(
                "{}:{}",
                secret.uid().ok_or(DENIED)?,
                secret.resource_version().ok_or(DENIED)?
            )
    {
        return Err(DENIED.into());
    }
    diagnostic.stage("rpc_bearer");
    let data = secret.data.as_ref().ok_or(DENIED)?;
    if data.len() != 2
        || !constant_time_eq(
            &data
                .get(crate::service_observer::TOKEN_KEY)
                .ok_or(DENIED)?
                .0,
            bearer.as_bytes(),
        )
    {
        return Err(DENIED.into());
    }
    diagnostic.stage("rpc_binding");
    let binding: Binding =
        serde_json::from_slice(&data.get("config.json").ok_or(DENIED)?.0).map_err(|_| DENIED)?;
    if !binding.valid()
        || binding.expires_at <= chrono::Utc::now().timestamp()
        || binding.expires_at > chrono::Utc::now().timestamp() + wire::MAX_TOKEN_SECONDS
        || binding.workspace_uid != target.workspace_uid
        || binding.grant.namespace != target.workspace
        || binding.grant.name != NAME
        || binding.grant.uid != request.grant_uid
        || binding.grant.generation != request.grant_generation
        || binding.identity != request.identity
        || binding.privacy_epoch != request.epoch
        || binding.privacy_revision != crate::sre_privacy::REVISION
        || observed.privacy_revision != binding.privacy_revision
        || observed.privacy_epoch != binding.privacy_epoch
        || binding.recipients != request.recipients
        || binding.verifier.as_ref() != Some(&request.verifier)
        || !governed_services::credentials::current(&secret, binding.privacy_epoch.as_deref())
        || grant.spec.writers.len() != binding.recipients.len()
    {
        return Err(DENIED.into());
    }
    for recipient in &binding.recipients {
        diagnostic.stage("rpc_recipients");
        if !grant.spec.writers.iter().any(|writer| {
            writer.namespace == recipient.namespace
                && writer.name == recipient.name
                && writer.uid == recipient.uid
        }) {
            return Err(DENIED.into());
        }
        let ns = Api::<Namespace>::all(client.clone())
            .get(&recipient.namespace)
            .await
            .map_err(|_| DENIED)?;
        let sa = Api::<ServiceAccount>::namespaced(client.clone(), &recipient.namespace)
            .get(&recipient.name)
            .await
            .map_err(|_| DENIED)?;
        live(&ns.metadata)?;
        live(&sa.metadata)?;
        if ns.uid().as_deref() != Some(recipient.namespace_uid.as_str())
            || sa.uid().as_deref() != Some(recipient.uid.as_str())
        {
            return Err(DENIED.into());
        }
    }
    diagnostic.stage("rpc_writer_authority");
    crate::credential_grants::verify_observation_writers(client, &grant).await?;
    diagnostic.stage("rpc_service_identity");
    if governed_services::identity_read_only(client, &sandbox, &namespace).await?
        != request.identity
    {
        return Err(DENIED.into());
    }
    diagnostic.finish();
    Ok(binding)
}

pub(super) async fn verify(
    client: &Client,
    request: &wire::Request,
    bearer: &str,
    endpoint: &wire::Endpoint,
) -> Result<wire::Proof, String> {
    let mut diagnostic = wire::Readiness::new("rpc_request");
    if !request.valid(chrono::Utc::now().timestamp()) || request.verifier != *endpoint {
        return Err(DENIED.into());
    }
    diagnostic.stage("rpc_initial_snapshot");
    snapshot(client, request, bearer).await?;
    diagnostic.stage("rpc_initial_endpoint");
    super::discovery::validate(client, endpoint).await?;
    // This is the complete controller proof, including private alias inventory.
    // No caller is granted the native Secret permissions required to compute it.
    diagnostic.stage("rpc_runtime_privacy");
    let epoch =
        crate::sre_authority::privacy_epoch(client, &format!("kars-{}", request.target.name))
            .await?;
    if epoch != request.epoch {
        return Err(DENIED.into());
    }
    diagnostic.stage("rpc_controller_privacy");
    if crate::sre_authority::privacy_epoch(client, &endpoint.namespace).await? != epoch {
        return Err(DENIED.into());
    }
    diagnostic.stage("rpc_audience_denial");
    super::identity::access_denial(
        client,
        wire::audience_tls_reviews(
            &endpoint.namespace,
            &request.recipients,
            &format!("kars-{}", request.target.name),
        ),
    )
    .await?;
    diagnostic.stage("rpc_admission");
    super::identity::admission(client).await?;
    diagnostic.stage("rpc_final_snapshot");
    snapshot(client, request, bearer).await?;
    diagnostic.stage("rpc_final_endpoint");
    super::discovery::validate(client, endpoint).await?;
    diagnostic.stage("rpc_registration");
    registration_current(client, epoch.as_deref()).await?;
    diagnostic.finish();
    Ok(wire::Proof::allow(request, epoch))
}
