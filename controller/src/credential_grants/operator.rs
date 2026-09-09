// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private read-only observation issuance, distinct from agent/admin credentials.

use super::*;
use crate::{crd::KarsSandbox, reconciler::governed_services, service_observer};
use k8s_openapi::api::apps::v1::Deployment;

pub(super) async fn reconcile(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    let workspace = grant.namespace().ok_or("Observation workspace missing")?;
    let sandboxes: Api<KarsSandbox> = Api::namespaced(client.clone(), &workspace);
    for target in &grant.spec.observation_targets {
        if target.kind != "KarsSandbox" || target.namespace != workspace || target.uid.is_empty() {
            return Err(
                "Observation authority requires an explicit same-workspace Sandbox UID".into(),
            );
        }
        let sandbox = sandboxes
            .get(&target.name)
            .await
            .map_err(|e| api_error("Read observation target", e))?;
        if sandbox.uid().as_deref() != Some(target.uid.as_str())
            || sandbox.metadata.deletion_timestamp.is_some()
        {
            return Err("Observation target was replaced or is terminating".into());
        }
        let namespace = Api::<Namespace>::all(client.clone())
            .get(&format!("kars-{}", target.name))
            .await
            .map_err(|e| api_error("Read observation runtime namespace", e))?;
        crate::reconciler::namespace_ownership::recheck(client, &sandbox, &namespace)
            .await
            .map_err(|_| "Observation target namespace ownership changed")?;
        super::observation_network::verify(client, grant, &sandbox, &namespace).await?;
        match crate::sre_authority::privacy_readiness(client, &namespace.name_any()).await {
            Ok(crate::sre_authority::PrivacyReadiness::Pending) => {
                publish(client, &sandbox, None).await?;
                continue;
            }
            Err(error) => {
                publish(client, &sandbox, None).await?;
                retire(client, &sandbox, &namespace).await?;
                return Err(error);
            }
            Ok(crate::sre_authority::PrivacyReadiness::Qualified(_)) => {}
        }
        let epoch = crate::sre_authority::privacy_epoch(client, &namespace.name_any()).await?;
        if epoch.is_some() {
            return Err(service_observer::ACTIVE_PRIVACY_UNAVAILABLE.into());
        }
        let identity = governed_services::identity(client, &sandbox, &namespace).await?;
        let server_name = format!(
            "observer-{}.kars.internal",
            sandbox.uid().ok_or("Sandbox UID missing")?
        );
        let existing_tls = governed_services::credentials::existing_configuration(
            client,
            &sandbox,
            &namespace,
            governed_services::credentials::OBSERVER_TLS,
        )
        .await?;
        let tls = if let Some(existing) = existing_tls.filter(|value| {
            value["identity"] == identity
                && value["serverName"] == server_name
                && value["expiresAt"]
                    .as_i64()
                    .is_some_and(|expiry| expiry > chrono::Utc::now().timestamp() + 172800)
        }) {
            existing
        } else {
            let issued = crate::providers::sre_tls::issue_for(vec![server_name.clone()])?;
            json!({"identity":identity,"serverName":server_name,"caPem":issued.ca,
                "certificatePem":issued.certificate,"privateKeyPem":issued.private_key,"expiresAt":issued.expires_at})
        };
        let tls_configuration =
            serde_json::to_string(&tls).map_err(|_| "Observation TLS serialization failed")?;
        governed_services::credentials::ensure_for(
            client,
            &sandbox,
            &namespace,
            governed_services::credentials::OBSERVER_TLS,
            Some(&tls_configuration),
        )
        .await?;
        let mut recipients = Vec::new();
        for writer in &grant.spec.writers {
            let ns = Api::<Namespace>::all(client.clone())
                .get(&writer.namespace)
                .await
                .map_err(|e| api_error("Read observation recipient namespace", e))?;
            let sa = Api::<ServiceAccount>::namespaced(client.clone(), &writer.namespace)
                .get(&writer.name)
                .await
                .map_err(|e| api_error("Read observation recipient identity", e))?;
            if identity_of(&sa.metadata)?.0 != writer.uid {
                return Err("Observation recipient ServiceAccount UID changed".into());
            }
            recipients.push(service_observer::Recipient {
                namespace: writer.namespace.clone(),
                namespace_uid: identity_of(&ns.metadata)?.0.into(),
                name: writer.name.clone(),
                uid: writer.uid.clone(),
            });
        }
        let binding = service_observer::Binding {
            capability: service_observer::CAPABILITY.into(),
            identity,
            grant: service_observer::Grant {
                namespace: workspace.clone(),
                name: NAME.into(),
                uid: grant.uid().ok_or("Observation grant UID missing")?,
                generation: grant.metadata.generation.unwrap_or_default(),
            },
            recipients,
            privacy_revision: crate::sre_privacy::REVISION.into(),
            privacy_epoch: epoch.clone(),
            server_name,
            ca_pem: tls["caPem"]
                .as_str()
                .ok_or("Observation CA missing")?
                .into(),
        };
        if !binding.valid() {
            return Err("Observation binding is invalid".into());
        }
        let configuration = serde_json::to_string(&binding)
            .map_err(|_| "Observation binding serialization failed")?;
        let credential = governed_services::credentials::ensure_for(
            client,
            &sandbox,
            &namespace,
            governed_services::credentials::OBSERVER,
            Some(&configuration),
        )
        .await?;
        if credential.epoch != epoch {
            return Err("Observation privacy changed during issuance".into());
        }
        super::observer_metadata::ensure(client, grant, &sandbox, &namespace, &binding.recipients)
            .await?;
        let deployed = governed_services::credentials::review_consumer(
            client,
            &namespace.name_any(),
            &sandbox.name_any(),
        )
        .await?;
        let current = deployed.as_ref().is_some_and(|deployment| {
            deployment
                .spec
                .as_ref()
                .and_then(|spec| spec.template.metadata.as_ref())
                .and_then(|meta| meta.annotations.as_ref())
                .and_then(|annotations| {
                    annotations.get(governed_services::credentials::OBSERVER.version_annotation)
                })
                == Some(&credential.version)
        }) && credential
            .consumers_current(client, &namespace.name_any(), &sandbox.name_any())
            .await?;
        let secret_uid = credential
            .version
            .split(':')
            .next()
            .ok_or("Observation version missing")?
            .to_string();
        publish(
            client,
            &sandbox,
            Some(ObservationStatus {
                capability: service_observer::CAPABILITY.into(),
                phase: if current { "Ready" } else { "Prepared" }.into(),
                reason: if current {
                    "Qualified"
                } else {
                    "AwaitingCredentialRollout"
                }
                .into(),
                version: credential.version,
                grant: ObjectIdentity {
                    name: NAME.into(),
                    uid: grant.uid().ok_or("Grant UID missing")?,
                },
                secret: ObjectIdentity {
                    name: service_observer::SECRET.into(),
                    uid: secret_uid,
                },
                namespace_uid: namespace.uid().ok_or("Namespace UID missing")?,
                privacy_revision: crate::sre_privacy::REVISION.into(),
                privacy_epoch: epoch,
                deployment_uid: deployed.as_ref().and_then(ResourceExt::uid),
            }),
        )
        .await?;
    }
    for sandbox in sandboxes
        .list(&ListParams::default())
        .await
        .map_err(|e| api_error("Read retired observation targets", e))?
    {
        let ours = sandbox
            .status
            .as_ref()
            .and_then(|s| s.service_observation.as_ref())
            .is_some_and(|status| Some(status.grant.uid.as_str()) == grant.metadata.uid.as_deref());
        let selected = grant.spec.observation_targets.iter().any(|target| {
            target.name == sandbox.name_any()
                && Some(target.uid.as_str()) == sandbox.metadata.uid.as_deref()
        });
        if ours && !selected {
            publish(client, &sandbox, None).await?;
            let namespace = Api::<Namespace>::all(client.clone())
                .get(&format!("kars-{}", sandbox.name_any()))
                .await
                .map_err(|e| api_error("Read retired observation namespace", e))?;
            retire(client, &sandbox, &namespace).await?;
        }
    }
    super::observer_rbac::reconcile(client, grant).await?;
    super::observer_metadata::revoke_stale(client, grant).await
}

fn identity_of(meta: &kube::api::ObjectMeta) -> Result<(&str, &str), String> {
    super::identity(meta)
}

async fn publish(
    client: &Client,
    sandbox: &KarsSandbox,
    status: Option<ObservationStatus>,
) -> Result<(), String> {
    let namespace = sandbox.namespace().ok_or("Observation workspace missing")?;
    let api: Api<KarsSandbox> = Api::namespaced(client.clone(), &namespace);
    let current = api
        .get(&sandbox.name_any())
        .await
        .map_err(|e| api_error("Refresh observation status target", e))?;
    if current.uid() != sandbox.uid() {
        return Err("Observation status target was replaced".into());
    }
    if serde_json::to_value(
        current
            .status
            .as_ref()
            .and_then(|s| s.service_observation.as_ref()),
    )
    .ok()
        == serde_json::to_value(&status).ok()
    {
        return Ok(());
    }
    api.patch_status(&sandbox.name_any(),&PatchParams::default(),&Patch::Merge(json!({
        "metadata":{"uid":current.metadata.uid,"resourceVersion":current.metadata.resource_version},
        "status":{(service_observer::STATUS_FIELD):status}
    }))).await.map_err(|e|api_error("Publish private observation capability",e))?;
    Ok(())
}

pub(super) async fn revoke(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    super::observer_rbac::revoke(client, grant).await?;
    super::observer_metadata::revoke(client, grant).await?;
    let workspace = grant.namespace().ok_or("Observation workspace missing")?;
    for sandbox in Api::<KarsSandbox>::namespaced(client.clone(), &workspace)
        .list(&ListParams::default())
        .await
        .map_err(|e| api_error("Read observation consumers for revocation", e))?
    {
        if sandbox
            .status
            .as_ref()
            .and_then(|s| s.service_observation.as_ref())
            .is_some_and(|status| Some(status.grant.uid.as_str()) == grant.metadata.uid.as_deref())
        {
            publish(client, &sandbox, None).await?;
            let namespace = Api::<Namespace>::all(client.clone())
                .get(&format!("kars-{}", sandbox.name_any()))
                .await
                .map_err(|e| api_error("Read observation namespace for revocation", e))?;
            retire(client, &sandbox, &namespace).await?;
        }
    }
    Ok(())
}

async fn retire(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), String> {
    for purpose in [
        governed_services::credentials::OBSERVER,
        governed_services::credentials::OBSERVER_TLS,
    ] {
        governed_services::credentials::retire_for(client, sandbox, namespace, purpose).await?;
    }
    Ok(())
}

pub(crate) fn mount(pod: &mut serde_json::Value, sandbox: &KarsSandbox) -> Option<String> {
    let status = sandbox.status.as_ref()?.service_observation.as_ref()?;
    if !["Ready", "Prepared"].contains(&status.phase.as_str())
        || status.capability != service_observer::CAPABILITY
    {
        return None;
    }
    pod["volumes"].as_array_mut()?.push(json!({"name":"service-observations","secret":{
        "secretName":service_observer::SECRET,"items":[{"key":service_observer::TOKEN_KEY,"path":service_observer::TOKEN_KEY},
            {"key":"config.json","path":"config.json"}]}}));
    pod["volumes"].as_array_mut()?.push(json!({"name":"service-observation-identity","secret":{
        "secretName":service_observer::TLS_SECRET,"items":[{"key":"config.json","path":"config.json"}]}}));
    for container in pod["containers"].as_array_mut()? {
        if container["name"] == "inference-router" {
            container["volumeMounts"]
                .as_array_mut()?
                .push(json!({"name":"service-observations",
                "mountPath":service_observer::DIRECTORY,"readOnly":true}));
            container["env"]
                .as_array_mut()?
                .push(json!({"name":service_observer::VERSION_ENV,"value":status.version}));
            container["volumeMounts"]
                .as_array_mut()?
                .push(json!({"name":"service-observation-identity",
                "mountPath":service_observer::TLS_DIRECTORY,"readOnly":true}));
        }
    }
    Some(status.version.clone())
}

pub(crate) fn decorate(deployment: &mut Deployment, sandbox: &KarsSandbox) {
    if let Some(status) = sandbox
        .status
        .as_ref()
        .and_then(|status| status.service_observation.as_ref())
        && ["Ready", "Prepared"].contains(&status.phase.as_str())
    {
        deployment
            .spec
            .as_mut()
            .expect("Deployment spec")
            .template
            .metadata
            .get_or_insert_default()
            .annotations
            .get_or_insert_default()
            .insert(
                governed_services::credentials::OBSERVER
                    .version_annotation
                    .into(),
                status.version.clone(),
            );
    }
}
