// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    crd::KarsSandbox,
    sre_registration::{KarsSRERegistration, NAME},
};
use k8s_openapi::api::{
    apps::v1::Deployment, authorization::v1::SubjectAccessReview, core::v1::Namespace,
};
use kube::{Api, Client, api::PostParams};

pub(super) struct Authority {
    pub sandbox: KarsSandbox,
    pub namespace: Namespace,
}

pub(crate) fn api_error(stage: &str, error: kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!("{stage}: Kubernetes status {}", status.code),
        _ => format!("{stage}: Kubernetes transport/serialization failure"),
    }
}

pub(super) async fn verify(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<Authority, String> {
    reg.validate()?;
    let registrations: Api<KarsSRERegistration> = Api::all(client.clone());
    let current = registrations
        .get(NAME)
        .await
        .map_err(|e| api_error("Read live SRE registration", e))?;
    if current.metadata.uid != reg.metadata.uid
        || current.metadata.generation != reg.metadata.generation
        || current.metadata.deletion_timestamp.is_some()
    {
        return Err("SRE registration changed during reconciliation".into());
    }
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let control_ns = namespaces
        .get(&reg.spec.controller.namespace.name)
        .await
        .map_err(|e| api_error("Read registered controller namespace", e))?;
    if control_ns.metadata.uid.as_deref() != Some(&reg.spec.controller.namespace.uid)
        || control_ns.metadata.deletion_timestamp.is_some()
    {
        return Err("Registered controller namespace was replaced or is terminating".into());
    }
    let deployments: Api<Deployment> =
        Api::namespaced(client.clone(), &reg.spec.controller.namespace.name);
    let controller = deployments
        .get(&reg.spec.controller.deployment.name)
        .await
        .map_err(|e| api_error("Read registered controller Deployment", e))?;
    if controller.metadata.uid.as_deref() != Some(&reg.spec.controller.deployment.uid)
        || controller.metadata.deletion_timestamp.is_some()
        || controller
            .metadata
            .annotations
            .as_ref()
            .is_some_and(|annotations| {
                annotations
                    .get("meta.helm.sh/release-name")
                    .is_some_and(|name| name != &reg.spec.controller.release)
                    || annotations
                        .get("meta.helm.sh/release-namespace")
                        .is_some_and(|name| name != &reg.spec.controller.namespace.name)
            })
    {
        return Err("Registered controller/release identity does not match".into());
    }
    let sandboxes: Api<KarsSandbox> = Api::namespaced(client.clone(), &reg.spec.sandbox.namespace);
    let sandbox = sandboxes
        .get(&reg.spec.sandbox.name)
        .await
        .map_err(|e| api_error("Read registered SRE Sandbox", e))?;
    let namespace = namespaces
        .get(&reg.spec.runtime_namespace.name)
        .await
        .map_err(|e| api_error("Read registered SRE runtime namespace", e))?;
    if sandbox.metadata.uid.as_deref() != Some(&reg.spec.sandbox.uid)
        || sandbox.metadata.deletion_timestamp.is_some()
        || sandbox
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("kars.azure.com/role"))
            .map(String::as_str)
            != Some("sre")
        || namespace.metadata.uid.as_deref() != Some(&reg.spec.runtime_namespace.uid)
        || namespace.metadata.deletion_timestamp.is_some()
        || sandbox
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get("kars.azure.com/namespace-uid"))
            != Some(&reg.spec.runtime_namespace.uid)
        || !crate::reconciler::namespace_ownership::claimed(&namespace, &sandbox)
            .map_err(|_| "Registered SRE namespace claim is invalid")?
    {
        return Err(
            "Registered SRE source/runtime UID or claim does not match; foreign occupant preserved"
                .into(),
        );
    }
    Ok(Authority { sandbox, namespace })
}

pub(crate) async fn check_secret_denial(client: &Client, namespace: &str) -> Result<(), String> {
    let reviews: Api<SubjectAccessReview> = Api::all(client.clone());
    for request in crate::sre_privacy::secret_access_reviews(namespace) {
        let review: SubjectAccessReview = serde_json::from_value(request)
            .map_err(|_| "SRE Secret authorization request is invalid")?;
        let checked = reviews
            .create(&PostParams::default(), &review)
            .await
            .map_err(|e| api_error("Verify legacy SRE Secret denial", e))?;
        let response = serde_json::to_value(checked)
            .map_err(|_| "SRE Secret authorization response is invalid")?;
        crate::sre_privacy::require_denial(&response)?;
    }
    Ok(())
}

/// Later private-credential issuers must call this immediately before issuance.
/// An absent registration is safe only when the old SRE subject has no access.
pub(crate) async fn privacy_epoch(
    client: &Client,
    target_namespace: &str,
) -> Result<Option<String>, String> {
    check_secret_denial(client, target_namespace).await?;
    let registrations: Api<KarsSRERegistration> = Api::all(client.clone());
    let Some(reg) = registrations
        .get_opt(NAME)
        .await
        .map_err(|e| api_error("Read SRE privacy epoch", e))?
    else {
        return Ok(None);
    };
    if !reg.spec.enabled
        && reg.status.as_ref().is_some_and(|status| {
            status.phase == "Retired"
                && status.observed_generation == reg.metadata.generation.unwrap_or_default()
        })
    {
        return Ok(None);
    }
    verify(client, &reg).await?;
    super::admission::verify(client).await?;
    super::credential_guard::scan(client, &reg).await?;
    let status = reg
        .status
        .as_ref()
        .ok_or("SRE authority has not been reconciled")?;
    if status.phase != "Ready"
        || status.observed_generation != reg.metadata.generation.unwrap_or_default()
        || !status.legacy_secret_access_denied
        || status.privacy_revision.as_deref() != Some(crate::sre_privacy::REVISION)
        || status.privacy_epoch.as_deref() != Some(reg.epoch().as_str())
    {
        return Err("SRE privacy migration is not Ready".into());
    }
    Ok(status.privacy_epoch.clone())
}
