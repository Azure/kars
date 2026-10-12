// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Leader-owned bootstrap independent of task arrival. Entra creation is journaled before Graph POST.

use super::{Config, Error, Result, annotation, api_error, identity, roots, same_snapshot};
use crate::{agent_id_provisioning::ProvisionerCache, agent_identity::dispatcher::{guid, tags},
    auth_config::{KarsAuthConfig, MeshAuthBackend}};
use k8s_openapi::{ByteString, api::{apps::v1::Deployment, core::v1::{Namespace, Secret}},
    apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference}};
use kube::{Api, Client, ResourceExt, api::{Patch, PatchParams, PostParams}};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

const NAME: &str = "kars-mission-dispatcher-entra";
const PIN: &str = "kars.azure.com/mission-entra-uid";
const RECOVERY: &str = "kars.azure.com/mission-entra-recovery-uid";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    version: String, state: String, namespace_uid: String, deployment_uid: String,
    root_uid: String, journal_uid: String, tenant_id: String, blueprint_id: String,
    app_id: String, object_id: String,
}

fn env<'a>(deployment: &'a Deployment, name: &str) -> Result<Option<&'a str>> {
    let pod = deployment.spec.as_ref().and_then(|s| s.template.spec.as_ref()).ok_or(Error::Invalid("missing dispatcher pod"))?;
    let container = &pod.containers[0];
    if container.env_from.as_ref().is_some_and(|v| !v.is_empty()) { return Err(Error::Invalid("dispatcher envFrom is not supported")); }
    let values: Vec<_> = container.env.iter().flatten().filter(|e| e.name == name).collect();
    match values.as_slice() {
        [] => Ok(None),
        [value] if value.value_from.is_none() => Ok(value.value.as_deref()),
        _ => Err(Error::Invalid("invalid dispatcher identity configuration")),
    }
}
fn owner(deployment: &Deployment) -> Result<OwnerReference> {
    Ok(OwnerReference { api_version: "apps/v1".into(), kind: "Deployment".into(), name: deployment.name_any(),
        uid: identity(&deployment.metadata)?.0.into(), controller: Some(true), block_owner_deletion: Some(true) })
}
fn decode(secret: &Secret, deployment: &Deployment, expected: &Binding) -> Result<Binding> {
    let uid = identity(&secret.metadata)?.0;
    if secret.name_any() != NAME || secret.namespace() != deployment.namespace()
        || secret.metadata.owner_references.as_deref() != Some(&[owner(deployment)?])
        || secret.type_.as_deref() != Some("Opaque") {
        return Err(Error::Invalid("dispatcher Entra journal custody mismatch"));
    }
    let bytes = secret.data.as_ref().filter(|d| d.len() == 1).and_then(|d| d.get("binding"))
        .ok_or(Error::Invalid("invalid dispatcher Entra journal"))?;
    let binding: Binding = serde_json::from_slice(&bytes.0).map_err(|_| Error::Invalid("invalid dispatcher Entra journal"))?;
    let mut anchor = binding.clone();
    anchor.state = expected.state.clone(); anchor.journal_uid.clear(); anchor.app_id.clear(); anchor.object_id.clear();
    if anchor != *expected || (!binding.journal_uid.is_empty() && binding.journal_uid != uid)
        || !["unissued", "issuing", "ready"].contains(&binding.state.as_str()) {
        return Err(Error::Invalid("dispatcher Entra binding anchors changed; explicit recovery required"));
    }
    if binding.state == "ready" {
        if secret.immutable != Some(true) || binding.journal_uid != uid || !guid(&binding.app_id)
            || !guid(&binding.object_id) || binding.app_id == binding.blueprint_id {
            return Err(Error::Invalid("dispatcher Entra binding is not immutable and complete"));
        }
    } else if secret.immutable == Some(true) || !binding.app_id.is_empty() || !binding.object_id.is_empty() {
        return Err(Error::Invalid("invalid pending dispatcher Entra journal"));
    }
    Ok(binding)
}
fn data(binding: &Binding) -> Result<BTreeMap<String, ByteString>> {
    Ok(BTreeMap::from([("binding".into(), ByteString(serde_json::to_vec(binding).map_err(|_| Error::Invalid("invalid dispatcher binding"))?))]))
}

async fn ensure(client: &Client, config: &Config, cache: &ProvisionerCache) -> Result<()> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &config.namespace);
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &config.namespace);
    let namespace = namespaces.get(&config.namespace).await.map_err(|e| api_error("read dispatcher namespace", e))?;
    if namespace.metadata.owner_references.as_ref().is_some_and(|v| !v.is_empty()) { return Err(Error::Invalid("dispatcher namespace is owned")); }
    let mut deployment = deployments.get(&config.deployment).await.map_err(|e| api_error("read dispatcher deployment", e))?;
    config.validate_dispatcher(&deployment)?;
    let root = roots::ensure(client, roots::Anchor::Dispatcher(&deployment), &namespace).await?;
    deployment.metadata = root.anchor.clone();
    match env(&deployment, "KARS_MISSION_AUTH_MODE")? {
        Some("development") => return Ok(()),
        Some("entra") => {},
        _ => return Err(Error::Invalid("dispatcher authentication must be explicit")),
    }
    let configs: Api<KarsAuthConfig> = Api::all(client.clone());
    let auth = configs.get("default").await.map_err(|e| api_error("read dispatcher auth configuration", e))?;
    identity(&auth.metadata)?;
    let spec = &auth.spec;
    if spec.mesh_auth_backend != MeshAuthBackend::EntraAgentIdentity || !guid(&spec.tenant.tenant_id)
        || !guid(&spec.agent_id.blueprint_client_id) || spec.controller.managed_identity_client_id.as_deref().is_none_or(|id| !guid(id))
        || spec.agent_id.sponsor_user_object_ids.is_empty() || spec.agent_id.sponsor_user_object_ids.iter().any(|id| !guid(id))
        || env(&deployment, "KARS_MISSION_TENANT_ID")? != Some(&spec.tenant.tenant_id)
        || env(&deployment, "KARS_MISSION_BLUEPRINT_ID")? != Some(&spec.agent_id.blueprint_client_id) {
        return Err(Error::Invalid("dispatcher Entra prerequisites or template pins do not match AuthConfig"));
    }
    let expected = Binding { version: "v1".into(), state: "unissued".into(), namespace_uid: identity(&namespace.metadata)?.0.into(),
        deployment_uid: identity(&deployment.metadata)?.0.into(), root_uid: root.uid.clone(), journal_uid: String::new(),
        tenant_id: spec.tenant.tenant_id.clone(), blueprint_id: spec.agent_id.blueprint_client_id.clone(), app_id: String::new(), object_id: String::new() };
    let existing = secrets.get_opt(NAME).await.map_err(|e| api_error("read dispatcher Entra journal", e))?;
    let created = existing.is_none();
    let mut secret = match existing {
        Some(value) => value,
        None => {
            if annotation(&deployment.metadata, PIN).is_some() { return Err(Error::Invalid("pinned dispatcher Entra journal missing; no replacement")); }
            roots::recheck(client, roots::Anchor::Dispatcher(&deployment), &namespace, &root.uid).await?;
            secrets.create(&PostParams::default(), &Secret { metadata: ObjectMeta { name: Some(NAME.into()), namespace: Some(config.namespace.clone()),
                owner_references: Some(vec![owner(&deployment)?]), ..Default::default() }, type_: Some("Opaque".into()), data: Some(data(&expected)?), ..Default::default() })
                .await.map_err(|e| api_error("create dispatcher Entra journal", e))?
        }
    };
    let mut binding = decode(&secret, &deployment, &expected)?;
    let uid = identity(&secret.metadata)?.0.to_owned();
    match annotation(&deployment.metadata, PIN) {
        Some(pin) if pin == uid => {},
        Some(_) => return Err(Error::Invalid("dispatcher Entra journal UID changed; no adoption")),
        None => {
            if !created && annotation(&namespace.metadata, RECOVERY) != Some(&uid) { return Err(Error::Invalid("unpinned dispatcher Entra journal; exact namespace recovery UID required")); }
            roots::recheck(client, roots::Anchor::Dispatcher(&deployment), &namespace, &root.uid).await?;
            deployment = deployments.patch(&config.deployment, &PatchParams::default(), &Patch::Merge(json!({"metadata":{
                "uid":deployment.metadata.uid, "resourceVersion":deployment.metadata.resource_version, "annotations":{PIN:uid}}})))
                .await.map_err(|e| api_error("pin dispatcher Entra journal", e))?;
        }
    }
    if binding.state == "ready" { return Ok(()); }
    let graph = cache.get_or_init(spec, &expected.namespace_uid).await;
    let tags = tags(&expected.namespace_uid, &expected.deployment_uid, &root.uid, &uid);
    let found = graph.find_dispatcher(&tags).await.map_err(Error::Invalid)?;
    let provisioned = match found {
        Some(value) => value,
        None => {
            if binding.state != "unissued" { return Err(Error::NotReady("dispatcher Graph create uncertain; lookup-only recovery awaiting unique owned identity")); }
            roots::recheck(client, roots::Anchor::Dispatcher(&deployment), &namespace, &root.uid).await?;
            same_snapshot(&auth.metadata, &configs.get_metadata("default").await.map_err(|e| api_error("recheck dispatcher AuthConfig", e))?.metadata)?;
            binding.state = "issuing".into(); binding.journal_uid = uid.clone();
            secret.data = Some(data(&binding)?);
            // ResourceVersion CAS is the sole authority to issue one remote CREATE, even across leader restarts.
            secret = secrets.replace(NAME, &PostParams::default(), &secret).await.map_err(|e| api_error("claim dispatcher Graph create", e))?;
            graph.create_dispatcher(&tags, &spec.agent_id.sponsor_user_object_ids).await.map_err(Error::Invalid)?
        }
    };
    roots::recheck(client, roots::Anchor::Dispatcher(&deployment), &namespace, &root.uid).await?;
    same_snapshot(&auth.metadata, &configs.get_metadata("default").await.map_err(|e| api_error("recheck dispatcher AuthConfig", e))?.metadata)?;
    binding.state = "ready".into(); binding.journal_uid = uid;
    binding.app_id = provisioned.app_id; binding.object_id = provisioned.id;
    secret.data = Some(data(&binding)?); secret.immutable = Some(true);
    secrets.replace(NAME, &PostParams::default(), &secret).await.map_err(|e| api_error("seal dispatcher Entra binding", e))?;
    Ok(())
}

pub(crate) async fn run(client: Client) -> Result<()> {
    let Some(config) = Config::from_env()? else { return std::future::pending().await; };
    let cache = ProvisionerCache::new();
    loop {
        if let Err(error) = ensure(&client, &config, &cache).await { tracing::warn!(%error, "dispatcher identity bootstrap pending"); }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Secret, Deployment, Binding) {
        let deployment = Deployment { metadata: ObjectMeta { name: Some("dispatcher".into()), namespace: Some("system".into()), uid: Some("dep".into()), resource_version: Some("1".into()), ..Default::default() }, ..Default::default() };
        let binding = Binding { version: "v1".into(), state: "unissued".into(), namespace_uid: "ns".into(), deployment_uid: "dep".into(), root_uid: "root".into(), journal_uid: String::new(), tenant_id: "tenant".into(), blueprint_id: "blueprint".into(), app_id: String::new(), object_id: String::new() };
        let secret = Secret { metadata: ObjectMeta { name: Some(NAME.into()), namespace: Some("system".into()), uid: Some("journal".into()), resource_version: Some("1".into()), owner_references: Some(vec![owner(&deployment).unwrap()]), ..Default::default() }, type_: Some("Opaque".into()), data: Some(data(&binding).unwrap()), ..Default::default() };
        (secret, deployment, binding)
    }
    #[test]
    fn dispatcher_journal_binds_custody_and_enforces_states() {
        let (mut secret, deployment, expected) = fixture();
        assert!(decode(&secret, &deployment, &expected).is_ok());
        for state in ["issuing", "ready", "unknown"] {
            let mut binding = expected.clone(); binding.state = state.into();
            secret.data = Some(data(&binding).unwrap());
            assert_eq!(decode(&secret, &deployment, &expected).is_ok(), state == "issuing");
        }
        secret.data = Some(data(&expected).unwrap()); secret.metadata.uid = Some("replacement".into());
        let mut binding = expected.clone(); binding.journal_uid = "journal".into();
        secret.data = Some(data(&binding).unwrap()); assert!(decode(&secret, &deployment, &expected).is_err());
        secret.metadata.uid = Some("journal".into()); binding.root_uid = "changed".into();
        secret.data = Some(data(&binding).unwrap()); assert!(decode(&secret, &deployment, &expected).is_err());
    }
}
