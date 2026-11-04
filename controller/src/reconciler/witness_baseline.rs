// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional fleet witness access. Namespace claims, not Bridge or Helm controller
//! installation metadata, establish the Core sandbox's ownership. Fleet authority
//! covers the kars-system workspace (matching the addon's discovery Role), not
//! unrelated tenant workspaces. Selected-scope Helm grants use a different name.
//! No Secret reads, Bridge dependencies, or observer installation occur here.

use std::sync::Arc;

use k8s_openapi::{
    api::{
        core::v1::{ConfigMap, Namespace, ServiceAccount},
        rbac::v1::{Role, RoleBinding},
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, PostParams, Preconditions},
    runtime::reflector::ObjectRef,
};
use serde::Deserialize;
use serde_json::json;

use super::namespace_ownership as ownership;
use crate::crd::KarsSandbox;

pub(super) const SETTINGS: &str = "kars-datapath-witness-settings";
pub(super) const WORKSPACE: &str = "kars-system";
pub(super) const OBSERVER_NAMESPACE: &str = "kars-witness-gadget";
pub(super) const OBSERVER: &str = "aggregator";
const GRANT: &str = "kars-datapath-witness-baseline";
const MANAGER: &str = "kars-witness-baseline-controller";
const OWNER: &str = "kars.azure.com/witness-baseline-owner";
const OBSERVER_UID: &str = "kars.azure.com/witness-observer-uid";
const SETTINGS_UID: &str = "kars.azure.com/witness-settings-uid";
const ROLE_UID: &str = "kars.azure.com/witness-role-uid";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    schema_version: u32,
    enabled: bool,
    scope: String,
    sandboxes: Vec<String>,
    release_revision: u64,
    config_digest: String,
}

#[derive(Debug, PartialEq, Eq)]
struct Intent {
    settings_uid: String,
    settings_version: String,
    observer_uid: String,
    observer_version: String,
}

fn annotation<'a>(meta: &'a ObjectMeta, key: &str) -> Option<&'a str> {
    meta.annotations.as_ref()?.get(key).map(String::as_str)
}

fn identity(meta: &ObjectMeta) -> Result<(&str, &str), String> {
    match (meta.uid.as_deref(), meta.resource_version.as_deref()) {
        (Some(uid), Some(rv)) if !uid.is_empty() && !rv.is_empty() => Ok((uid, rv)),
        _ => Err("witness object has no API-server UID/resourceVersion".into()),
    }
}

fn helm_owner(meta: &ObjectMeta, name: &str, namespace: &str) -> Result<String, String> {
    identity(meta)?;
    let release = annotation(meta, "meta.helm.sh/release-name");
    if meta.name.as_deref() != Some(name)
        || meta.namespace.as_deref() != Some(namespace)
        || meta.deletion_timestamp.is_some()
        || meta
            .owner_references
            .as_ref()
            .is_some_and(|refs| !refs.is_empty())
        || meta
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/witness-addon"))
            .map(String::as_str)
            != Some("true")
        || meta
            .labels
            .as_ref()
            .and_then(|l| l.get("app.kubernetes.io/managed-by"))
            .map(String::as_str)
            != Some("Helm")
        || annotation(meta, "meta.helm.sh/release-namespace") != Some(WORKSPACE)
        || !matches!(release, Some("kars" | "kars-datapath-witness"))
    {
        return Err(
            "witness settings or observer is not owned by the expected Helm release".into(),
        );
    }
    Ok(release.unwrap().into())
}

fn enabled(settings: &ConfigMap) -> Result<bool, String> {
    helm_owner(&settings.metadata, SETTINGS, WORKSPACE)?;
    let settings: Settings = serde_json::from_str(
        settings
            .data
            .as_ref()
            .and_then(|d| d.get("settings.json"))
            .ok_or("witness settings.json is absent")?,
    )
    .map_err(|_| "witness settings.json is malformed")?;
    if settings.schema_version != 1
        || settings.release_revision == 0
        || settings.config_digest.len() != 64
        || !settings
            .config_digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || !matches!(settings.scope.as_str(), "all" | "selected")
        || (settings.scope == "all" && !settings.sandboxes.is_empty())
    {
        return Err("witness settings schema or all-scope selection is invalid".into());
    }
    Ok(settings.enabled && settings.scope == "all")
}

fn intent(
    settings: Option<&ConfigMap>,
    observer: Option<&ServiceAccount>,
) -> Result<Option<Intent>, String> {
    let Some(settings) = settings else {
        return Ok(None);
    };
    if !enabled(settings)? {
        return Ok(None);
    }
    let Some(observer) = observer else {
        return Ok(None);
    };
    if helm_owner(&settings.metadata, SETTINGS, WORKSPACE)?
        != helm_owner(&observer.metadata, OBSERVER, OBSERVER_NAMESPACE)?
    {
        return Err("witness settings and observer belong to different Helm releases".into());
    }
    let (settings_uid, settings_version) = identity(&settings.metadata)?;
    let (observer_uid, observer_version) = identity(&observer.metadata)?;
    Ok(Some(Intent {
        settings_uid: settings_uid.into(),
        settings_version: settings_version.into(),
        observer_uid: observer_uid.into(),
        observer_version: observer_version.into(),
    }))
}

async fn read_intent(client: &Client) -> Result<Option<Intent>, String> {
    let settings = Api::<ConfigMap>::namespaced(client.clone(), WORKSPACE)
        .get_opt(SETTINGS)
        .await
        .map_err(|e| format!("read witness settings: {e}"))?;
    // Core never needs the observer namespace or its ServiceAccount when off.
    if settings.as_ref().map(enabled).transpose()? != Some(true) {
        return Ok(None);
    }
    let observer = Api::<ServiceAccount>::namespaced(client.clone(), OBSERVER_NAMESPACE)
        .get_opt(OBSERVER)
        .await
        .map_err(|e| format!("read witness observer: {e}"))?;
    intent(settings.as_ref(), observer.as_ref())
}

fn metadata(sandbox: &KarsSandbox, namespace: &Namespace, intent: &Intent) -> ObjectMeta {
    serde_json::from_value(json!({
        "name": GRANT, "namespace": namespace.name_any(),
        "labels": {OWNER: MANAGER},
        "annotations": {
            ownership::SOURCE_NAMESPACE: sandbox.namespace(),
            ownership::SOURCE_NAME: sandbox.name_any(),
            ownership::SOURCE_UID: sandbox.uid(),
            ownership::NAMESPACE_UID: namespace.uid(),
            OBSERVER_UID: intent.observer_uid,
            SETTINGS_UID: intent.settings_uid,
        },
        "ownerReferences": [{"apiVersion":"v1", "kind":"Namespace", "name":namespace.name_any(),
            "uid": namespace.uid(), "controller": true, "blockOwnerDeletion": false}]
    }))
    .expect("fixed witness grant metadata")
}

fn owned(meta: &ObjectMeta, sandbox: &KarsSandbox, namespace: &Namespace) -> bool {
    identity(meta).is_ok()
        && identity(&sandbox.metadata).is_ok()
        && identity(&namespace.metadata).is_ok()
        && meta.name.as_deref() == Some(GRANT)
        && meta.namespace == namespace.metadata.name
        && meta
            .labels
            .as_ref()
            .and_then(|l| l.get(OWNER))
            .map(String::as_str)
            == Some(MANAGER)
        && annotation(meta, ownership::SOURCE_NAMESPACE) == sandbox.metadata.namespace.as_deref()
        && annotation(meta, ownership::SOURCE_NAME) == sandbox.metadata.name.as_deref()
        && annotation(meta, ownership::SOURCE_UID) == sandbox.metadata.uid.as_deref()
        && annotation(meta, ownership::NAMESPACE_UID) == namespace.metadata.uid.as_deref()
        && meta.owner_references.as_ref().is_some_and(|refs| {
            refs.len() == 1 && {
                let owner = &refs[0];
                owner.api_version == "v1"
                    && owner.kind == "Namespace"
                    && Some(&owner.name) == namespace.metadata.name.as_ref()
                    && Some(&owner.uid) == namespace.metadata.uid.as_ref()
                    && owner.controller == Some(true)
                    && owner.block_owner_deletion == Some(false)
            }
        })
        && meta.managed_fields.as_ref().is_some_and(|fields| {
            fields.iter().any(|field| {
                field.manager.as_deref() == Some(MANAGER)
                    && field.operation.as_deref() == Some("Update")
                    && field.subresource.as_deref().is_none_or(str::is_empty)
            })
        })
}

fn desired_role(sandbox: &KarsSandbox, namespace: &Namespace, intent: &Intent) -> Role {
    Role {
        metadata: metadata(sandbox, namespace, intent),
        rules: Some(vec![k8s_openapi::api::rbac::v1::PolicyRule {
            api_groups: Some(vec![String::new()]),
            resources: Some(vec!["configmaps".into()]),
            resource_names: Some(vec![format!(
                "karssandbox-{}-egress-allowlist",
                sandbox.name_any()
            )]),
            verbs: vec!["get".into()],
            ..Default::default()
        }]),
    }
}

fn desired_binding(
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    intent: &Intent,
    role: &Role,
) -> Result<RoleBinding, String> {
    let mut meta = metadata(sandbox, namespace, intent);
    meta.annotations
        .as_mut()
        .unwrap()
        .insert(ROLE_UID.into(), identity(&role.metadata)?.0.into());
    Ok(RoleBinding {
        metadata: meta,
        role_ref: k8s_openapi::api::rbac::v1::RoleRef {
            api_group: "rbac.authorization.k8s.io".into(),
            kind: "Role".into(),
            name: GRANT.into(),
        },
        subjects: Some(vec![k8s_openapi::api::rbac::v1::Subject {
            kind: "ServiceAccount".into(),
            name: OBSERVER.into(),
            namespace: Some(OBSERVER_NAMESPACE.into()),
            ..Default::default()
        }]),
    })
}

fn current(meta: &ObjectMeta, intent: &Intent) -> bool {
    meta.deletion_timestamp.is_none()
        && annotation(meta, OBSERVER_UID) == Some(&intent.observer_uid)
        && annotation(meta, SETTINGS_UID) == Some(&intent.settings_uid)
}

fn delete_params(meta: &ObjectMeta) -> Result<DeleteParams, String> {
    let (uid, rv) = identity(meta)?;
    Ok(DeleteParams {
        preconditions: Some(Preconditions {
            uid: Some(uid.into()),
            resource_version: Some(rv.into()),
        }),
        ..Default::default()
    })
}

async fn revoke(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    role: Option<&Role>,
    binding: Option<&RoleBinding>,
) -> Result<(), String> {
    ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    // Independently remove each owned half even when its counterpart is foreign
    // or deletion fails. Never delete/adopt a foreign grant by name alone.
    if let Some(binding) = binding.filter(|b| owned(&b.metadata, sandbox, namespace)) {
        if let Err(error) = Api::<RoleBinding>::namespaced(client.clone(), &namespace.name_any())
            .delete(GRANT, &delete_params(&binding.metadata)?)
            .await
            && !matches!(&error, kube::Error::Api(e) if e.code == 404)
        {
            errors.push(format!("revoke witness binding: {error}"));
        }
    }
    if let Some(role) = role.filter(|r| owned(&r.metadata, sandbox, namespace)) {
        if let Err(error) = Api::<Role>::namespaced(client.clone(), &namespace.name_any())
            .delete(GRANT, &delete_params(&role.metadata)?)
            .await
            && !matches!(&error, kube::Error::Api(e) if e.code == 404)
        {
            errors.push(format!("revoke witness role: {error}"));
        }
    }
    if role.is_some_and(|r| !owned(&r.metadata, sandbox, namespace))
        || binding.is_some_and(|b| !owned(&b.metadata, sandbox, namespace))
    {
        errors.push("foreign witness grant preserved; observer access cannot be reconciled".into());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn verify_target(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), String> {
    ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|e| e.to_string())?;
    let live = Api::<KarsSandbox>::namespaced(
        client.clone(),
        &sandbox.namespace().ok_or("sandbox workspace absent")?,
    )
    .get(&sandbox.name_any())
    .await
    .map_err(|e| format!("recheck witness sandbox: {e}"))?;
    if live.uid() != sandbox.uid() || live.metadata.deletion_timestamp.is_some() {
        return Err("witness sandbox identity changed or is terminating".into());
    }
    Ok(())
}

/// Errors are diagnostic and must not prevent normal Core workload lifecycle.
/// Namespace deletion garbage-collects the grants through their Namespace owner.
pub(super) async fn reconcile(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), String> {
    if !ownership::claimed(namespace, sandbox).map_err(|e| e.to_string())? {
        return Err("witness baseline requires a claimed sandbox namespace".into());
    }
    let ns = namespace.name_any();
    let roles = Api::<Role>::namespaced(client.clone(), &ns);
    let bindings = Api::<RoleBinding>::namespaced(client.clone(), &ns);
    let role = roles.get_opt(GRANT).await;
    let binding = bindings.get_opt(GRANT).await;
    if role.is_err() || binding.is_err() {
        revoke(
            client,
            sandbox,
            namespace,
            role.as_ref().ok().and_then(Option::as_ref),
            binding.as_ref().ok().and_then(Option::as_ref),
        )
        .await?;
        return Err(format!(
            "read witness grants failed; readable owned grants revoked: role={:?}, binding={:?}",
            role.err(),
            binding.err(),
        ));
    }
    let role = role.map_err(|e| e.to_string())?;
    let binding = binding.map_err(|e| e.to_string())?;
    let read = if sandbox.metadata.deletion_timestamp.is_some()
        || sandbox.namespace().as_deref() != Some(WORKSPACE)
    {
        Ok(None)
    } else {
        read_intent(client).await
    };
    let desired = match read {
        Ok(Some(intent)) => intent,
        result => {
            revoke(client, sandbox, namespace, role.as_ref(), binding.as_ref()).await?;
            return result.map(|_| ());
        }
    };
    let expected_role = desired_role(sandbox, namespace, &desired);
    let invalid_role = role.as_ref().is_some_and(|old| {
        !owned(&old.metadata, sandbox, namespace)
            || !current(&old.metadata, &desired)
            || old.rules != expected_role.rules
    });
    let invalid_binding = binding.as_ref().is_some_and(|old| {
        !owned(&old.metadata, sandbox, namespace)
            || !current(&old.metadata, &desired)
            || role
                .as_ref()
                .and_then(|role| desired_binding(sandbox, namespace, &desired, role).ok())
                .is_none_or(|expected| {
                    old.role_ref != expected.role_ref
                        || old.subjects != expected.subjects
                        || annotation(&old.metadata, ROLE_UID)
                            != annotation(&expected.metadata, ROLE_UID)
                })
    });
    if invalid_role || invalid_binding {
        revoke(client, sandbox, namespace, role.as_ref(), binding.as_ref()).await?;
        return Err("witness grant identity/rules conflict; owned grants revoked, foreign resources preserved".into());
    }
    let params = PostParams {
        field_manager: Some(MANAGER.into()),
        ..Default::default()
    };
    verify_target(client, sandbox, namespace).await?;
    let role = match role {
        Some(role) => role,
        None => roles
            .create(&params, &expected_role)
            .await
            .map_err(|e| format!("create witness role without adoption: {e}"))?,
    };
    // Revalidate live authorization immediately before publishing the binding.
    // RBAC subjects cannot pin a ServiceAccount UID; the exact-name SA watch
    // revokes a replaced identity and these annotations prevent silent reuse.
    let live = read_intent(client).await;
    if live.as_ref().ok().and_then(Option::as_ref) != Some(&desired) {
        revoke(client, sandbox, namespace, Some(&role), binding.as_ref()).await?;
        return Err("witness authorization changed before binding; owned grants revoked".into());
    }
    let target = verify_target(client, sandbox, namespace).await;
    let live_role = roles.get_opt(GRANT).await;
    if target.is_err()
        || !live_role
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|live| {
                live.uid() == role.uid()
                    && owned(&live.metadata, sandbox, namespace)
                    && current(&live.metadata, &desired)
                    && live.rules == expected_role.rules
                    && live.metadata.resource_version == role.metadata.resource_version
            })
    {
        revoke(client, sandbox, namespace, Some(&role), binding.as_ref()).await?;
        return Err("witness target or role changed before binding; owned grants revoked".into());
    }
    if binding.is_none() {
        let expected = desired_binding(sandbox, namespace, &desired, &role)?;
        if let Err(error) = bindings.create(&params, &expected).await {
            revoke(client, sandbox, namespace, Some(&role), None).await?;
            return Err(format!("create witness binding without adoption: {error}"));
        }
    }
    Ok(())
}

pub(super) fn affected_sandboxes(
    meta: &ObjectMeta,
    name: &str,
    namespace: &str,
    sandboxes: &[Arc<KarsSandbox>],
) -> Vec<ObjectRef<KarsSandbox>> {
    // Do not filter ownership here: removal/corruption of ownership must revoke.
    if meta.name.as_deref() != Some(name) || meta.namespace.as_deref() != Some(namespace) {
        return Vec::new();
    }
    sandboxes
        .iter()
        .filter(|sandbox| sandbox.namespace().as_deref() == Some(WORKSPACE))
        .map(|sandbox| ObjectRef::from_obj(sandbox.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests;
