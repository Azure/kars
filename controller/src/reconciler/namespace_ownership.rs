// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Namespace claim v1. Namespace annotations are cluster-admin/controller
//! authority, not tenant-supplied labels. See docs/how-to/namespace-ownership.md.
//! Never read Secrets to establish ownership.

use k8s_openapi::{
    api::{apps::v1::Deployment, core::v1::Namespace},
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, ListParams, Patch, PatchParams, PostParams, Preconditions},
    runtime::reflector::ObjectRef,
};
use serde_json::{Value, json};
use std::sync::LazyLock;
use tokio::sync::{Mutex, MutexGuard};

use crate::{crd::KarsSandbox, field_managers::CLAWSANDBOX};

pub const FINALIZER: &str = "kars.azure.com/namespace-cleanup";
pub const VERSION: &str = "kars.azure.com/namespace-claim-version";
pub const SOURCE_NAMESPACE: &str = "kars.azure.com/sandbox-namespace";
pub const SOURCE_NAME: &str = "kars.azure.com/sandbox-name";
pub const SOURCE_UID: &str = "kars.azure.com/sandbox-uid";
pub const NAMESPACE_UID: &str = "kars.azure.com/namespace-uid";
pub const PRESTAGE: &str = "kars.azure.com/namespace-prestage";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Kubernetes namespace ownership API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("NamespaceOwnershipConflict: {0}; see docs/how-to/namespace-ownership.md")]
    Conflict(String),
    #[error("Namespace ownership serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

fn conflict(message: &str) -> Error {
    Error::Conflict(message.into())
}

// Same-named CRs have different controller queue keys. Serialize their entire
// reconcile (including cleanup), not only the claim operation. Bounded stripes
// avoid an unbounded map of tenant-controlled names.
static LOCKS: LazyLock<Vec<Mutex<()>>> =
    LazyLock::new(|| (0..64).map(|_| Mutex::new(())).collect());

pub async fn lock(name: &str) -> MutexGuard<'static, ()> {
    let stripe = name.bytes().fold(0usize, |sum, byte| sum + byte as usize) % LOCKS.len();
    LOCKS[stripe].lock().await
}

/// Watch routing is only a hint; ensure() always re-reads the authoritative CR
/// and namespace. Neither labels nor watch payloads authorize a write.
pub fn to_sandbox_ref(namespace: Namespace) -> Option<ObjectRef<KarsSandbox>> {
    let meta = &namespace.metadata;
    let name = annotation(meta, SOURCE_NAME)?;
    let source_namespace = annotation(meta, SOURCE_NAMESPACE)?;
    if annotation(meta, VERSION) != Some("v1")
        || source_namespace.is_empty()
        || name.is_empty()
        || namespace.name_any() != format!("kars-{name}")
    {
        return None;
    }
    Some(ObjectRef::new(name).within(source_namespace))
}

fn annotation<'a>(meta: &'a ObjectMeta, key: &str) -> Option<&'a str> {
    meta.annotations.as_ref()?.get(key).map(String::as_str)
}

fn identity(meta: &ObjectMeta) -> Result<(&str, &str), Error> {
    match (meta.uid.as_deref(), meta.resource_version.as_deref()) {
        (Some(uid), Some(rv)) if !uid.is_empty() && !rv.is_empty() => Ok((uid, rv)),
        _ => Err(conflict(
            "object is missing its API-server UID/resourceVersion",
        )),
    }
}

fn owner_annotations(sandbox: &KarsSandbox) -> Value {
    json!({
        VERSION: "v1",
        SOURCE_NAMESPACE: sandbox.namespace(),
        SOURCE_NAME: sandbox.name_any(),
        SOURCE_UID: sandbox.metadata.uid,
    })
}

/// Only metadata is written when adopting; existing labels, workloads, Secrets,
/// and pod templates are never modified by this module.
fn claim_patch(namespace: &Namespace, sandbox: &KarsSandbox) -> Result<Value, Error> {
    let (uid, rv) = identity(&namespace.metadata)?;
    let mut annotations = owner_annotations(sandbox);
    annotations[PRESTAGE] = Value::Null;
    Ok(json!({"metadata": {
        "uid": uid, "resourceVersion": rv, "annotations": annotations,
    }}))
}

fn has_foreign_owner(meta: &ObjectMeta) -> bool {
    meta.owner_references
        .as_ref()
        .is_some_and(|refs| !refs.is_empty())
}

/// `true` means already claimed; `false` means legacy evidence is needed.
pub fn claimed(namespace: &Namespace, sandbox: &KarsSandbox) -> Result<bool, Error> {
    let (ns_uid, _) = identity(&namespace.metadata)?;
    identity(&sandbox.metadata)?;
    if namespace.name_any() != format!("kars-{}", sandbox.name_any())
        || has_foreign_owner(&namespace.metadata)
    {
        return Err(conflict("namespace name or ownerReferences do not match"));
    }
    if let Some(bound_uid) = annotation(&sandbox.metadata, NAMESPACE_UID)
        && bound_uid != ns_uid
    {
        return Err(conflict(
            "namespace was replaced; recorded namespace UID differs",
        ));
    }
    let meta = &namespace.metadata;
    let has_claim = [VERSION, SOURCE_NAMESPACE, SOURCE_NAME, SOURCE_UID, PRESTAGE]
        .iter()
        .any(|key| annotation(meta, key).is_some());
    if !has_claim {
        return Ok(false);
    }
    if annotation(meta, VERSION) != Some("v1")
        || annotation(meta, SOURCE_NAMESPACE) != sandbox.metadata.namespace.as_deref()
        || annotation(meta, SOURCE_NAME) != sandbox.metadata.name.as_deref()
    {
        return Err(conflict(
            "namespace is reserved for a different sandbox/workspace",
        ));
    }
    if annotation(meta, SOURCE_UID) != sandbox.metadata.uid.as_deref()
        || annotation(meta, PRESTAGE).is_some()
    {
        return Err(conflict("namespace is not bound to this Sandbox UID"));
    }
    Ok(true)
}

fn prestaged(namespace: &Namespace, sandbox: &KarsSandbox) -> bool {
    let meta = &namespace.metadata;
    namespace.name_any() == format!("kars-{}", sandbox.name_any())
        && annotation(meta, VERSION) == Some("v1")
        && annotation(meta, SOURCE_NAMESPACE) == sandbox.metadata.namespace.as_deref()
        && annotation(meta, SOURCE_NAME) == sandbox.metadata.name.as_deref()
        && annotation(meta, SOURCE_UID).is_none()
        && annotation(meta, PRESTAGE) == Some("bind-next-sandbox")
        && annotation(&sandbox.metadata, NAMESPACE_UID) == meta.uid.as_deref()
        && !has_foreign_owner(meta)
        && meta
            .creation_timestamp
            .as_ref()
            .zip(sandbox.metadata.creation_timestamp.as_ref())
            .is_some_and(|(ns_time, sandbox_time)| ns_time <= sandbox_time)
}

fn applied_field(meta: &ObjectMeta, path: &[&str]) -> bool {
    meta.managed_fields.as_ref().is_some_and(|fields| {
        fields.iter().any(|entry| {
            entry.manager.as_deref() == Some(CLAWSANDBOX)
                && entry.operation.as_deref() == Some("Apply")
                && entry.subresource.as_deref().is_none_or(str::is_empty)
                && entry.fields_v1.as_ref().is_some_and(|fields| {
                    let mut value = &fields.0;
                    for segment in path {
                        let Some(next) = value.get(*segment) else {
                            return false;
                        };
                        value = next;
                    }
                    true
                })
        })
    })
}

/// Legacy proof is conjunctive: current CR status + controller-managed namespace
/// and Deployment + Deployment creation strictly after this CR incarnation.
/// Equal timestamps are ambiguous (Kubernetes timestamps have second precision).
/// Namespace creation may precede the CR: the legacy CLI prestages credentials.
pub fn legacy_proof(namespace: &Namespace, deployment: &Deployment, sandbox: &KarsSandbox) -> bool {
    let name = sandbox.name_any();
    let ns_name = format!("kars-{name}");
    let label = |meta: &ObjectMeta, key: &str, value: &str| {
        meta.labels
            .as_ref()
            .and_then(|labels| labels.get(key))
            .is_some_and(|v| v == value)
    };
    sandbox
        .status
        .as_ref()
        .and_then(|status| status.namespace.as_deref())
        == Some(&ns_name)
        && sandbox
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|fs| fs.iter().any(|f| f == FINALIZER))
        && label(&namespace.metadata, "kars.azure.com/sandbox", &name)
        && label(&namespace.metadata, "kars.azure.com/role", "sandbox")
        && applied_field(
            &namespace.metadata,
            &["f:metadata", "f:labels", "f:kars.azure.com/sandbox"],
        )
        && deployment.name_any() == name
        && deployment.namespace().as_deref() == Some(&ns_name)
        && !has_foreign_owner(&deployment.metadata)
        && deployment.metadata.deletion_timestamp.is_none()
        && label(&deployment.metadata, "kars.azure.com/sandbox", &name)
        && deployment
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("kars.azure.com/parent-namespace"))
            .is_none_or(|parent| Some(parent.as_str()) == sandbox.metadata.namespace.as_deref())
        && applied_field(&deployment.metadata, &["f:spec"])
        && deployment
            .metadata
            .creation_timestamp
            .as_ref()
            .zip(sandbox.metadata.creation_timestamp.as_ref())
            .is_some_and(|(deployment_time, sandbox_time)| deployment_time > sandbox_time)
        && deployment.spec.as_ref().is_some_and(|spec| {
            spec.selector
                .match_labels
                .as_ref()
                .and_then(|labels| labels.get("kars.azure.com/sandbox"))
                == Some(&name)
                && spec
                    .template
                    .metadata
                    .as_ref()
                    .is_some_and(|meta| label(meta, "kars.azure.com/sandbox", &name))
        })
}

async fn unique_sandbox(client: &Client, sandbox: &KarsSandbox) -> Result<(), Error> {
    let api: Api<KarsSandbox> = Api::all(client.clone());
    let list = api
        .list(&ListParams::default().fields(&format!("metadata.name={}", sandbox.name_any())))
        .await?;
    if list.items.len() != 1
        || list.items[0].metadata.uid != sandbox.metadata.uid
        || list.items[0].metadata.namespace != sandbox.metadata.namespace
    {
        return Err(conflict(
            "sandbox name is not unique across workspaces; no namespace was claimed",
        ));
    }
    Ok(())
}

async fn live_sandbox(client: &Client, sandbox: &KarsSandbox) -> Result<KarsSandbox, Error> {
    identity(&sandbox.metadata)?;
    let ns = sandbox
        .namespace()
        .filter(|ns| !ns.is_empty())
        .ok_or_else(|| conflict("Sandbox source namespace is missing"))?;
    let api: Api<KarsSandbox> = Api::namespaced(client.clone(), &ns);
    let live = api.get(&sandbox.name_any()).await?;
    if live.metadata.uid != sandbox.metadata.uid {
        return Err(conflict("Sandbox was recreated during reconciliation"));
    }
    Ok(live)
}

async fn patch_sandbox(
    client: &Client,
    sandbox: &KarsSandbox,
    mut metadata: Value,
) -> Result<KarsSandbox, Error> {
    let (uid, rv) = identity(&sandbox.metadata)?;
    metadata["uid"] = json!(uid);
    metadata["resourceVersion"] = json!(rv);
    let api: Api<KarsSandbox> = Api::namespaced(client.clone(), &sandbox.namespace().unwrap());
    Ok(api
        .patch(
            &sandbox.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata": metadata})),
        )
        .await?)
}

fn new_namespace(sandbox: &KarsSandbox) -> Result<Namespace, Error> {
    Ok(serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Namespace",
        "metadata": {
            "name": format!("kars-{}", sandbox.name_any()),
            "annotations": owner_annotations(sandbox),
            "labels": {
                "app.kubernetes.io/name": "kars",
                "app.kubernetes.io/component": "sandbox",
                "kars.azure.com/sandbox": sandbox.name_any(),
                "kars.azure.com/role": "sandbox",
                "kars.azure.com/isolated": "strict",
                "pod-security.kubernetes.io/enforce": "privileged",
                "pod-security.kubernetes.io/audit": "baseline",
                "pod-security.kubernetes.io/warn": "baseline"
            }
        }
    }))?)
}

/// Establish authority before *any* target-namespace operations. HTTP conflicts
/// propagate to the controller's retry queue; every retry re-reads all evidence.
pub async fn ensure(
    client: &Client,
    observed: &KarsSandbox,
) -> Result<(KarsSandbox, Option<Namespace>), Error> {
    let mut sandbox = live_sandbox(client, observed).await?;
    let api: Api<Namespace> = Api::all(client.clone());
    let name = format!("kars-{}", sandbox.name_any());
    let mut namespace = api.get_opt(&name).await?;
    if namespace.is_none() {
        unique_sandbox(client, &sandbox).await?;
        if sandbox.metadata.deletion_timestamp.is_some() {
            return Ok((sandbox, None));
        }
        if annotation(&sandbox.metadata, NAMESPACE_UID).is_some() {
            return Err(conflict(
                "bound namespace is missing; explicit operator recovery is required",
            ));
        }
    }
    if let Some(ns) = &namespace {
        if ns.metadata.deletion_timestamp.is_some() && sandbox.metadata.deletion_timestamp.is_none()
        {
            return Err(conflict(
                "namespace is terminating; workload reconciliation is stopped",
            ));
        }
        if !prestaged(ns, &sandbox) && !claimed(ns, &sandbox)? {
            unique_sandbox(client, &sandbox).await?;
            let deployments: Api<Deployment> = Api::namespaced(client.clone(), &name);
            let deployment = deployments.get_opt(&sandbox.name_any()).await?;
            if !deployment
                .as_ref()
                .is_some_and(|d| legacy_proof(ns, d, &sandbox))
            {
                return Err(conflict(
                    "legacy namespace ownership is unproven; preserve it and run upgrade preflight",
                ));
            }
        } else if prestaged(ns, &sandbox) {
            unique_sandbox(client, &sandbox).await?;
        }
    }
    if sandbox.metadata.deletion_timestamp.is_none()
        && !sandbox
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|fs| fs.iter().any(|f| f == FINALIZER))
    {
        let mut finalizers = sandbox.metadata.finalizers.clone().unwrap_or_default();
        finalizers.push(FINALIZER.into());
        sandbox = patch_sandbox(client, &sandbox, json!({"finalizers": finalizers})).await?;
    }
    if namespace.is_none() {
        match api
            .create(&PostParams::default(), &new_namespace(&sandbox)?)
            .await
        {
            Ok(created) => namespace = Some(created),
            Err(kube::Error::Api(error)) if error.code == 409 => {
                // A concurrent creator won. Never turn this into an apply/adopt.
                let existing = api.get(&name).await?;
                if !claimed(&existing, &sandbox)? {
                    return Err(conflict("namespace creation raced an unclaimed namespace"));
                }
                namespace = Some(existing);
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut namespace = namespace.unwrap();
    if prestaged(&namespace, &sandbox) || !claimed(&namespace, &sandbox)? {
        namespace = api
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(claim_patch(&namespace, &sandbox)?),
            )
            .await?;
    }
    // Re-read the CR before persisting the backlink. If its UID or deletion
    // state changed, do not proceed into any workload operations.
    let live = live_sandbox(client, &sandbox).await?;
    if live.metadata.deletion_timestamp != sandbox.metadata.deletion_timestamp {
        return Err(conflict("Sandbox deletion started during namespace claim"));
    }
    sandbox = live;
    if annotation(&sandbox.metadata, NAMESPACE_UID)
        .is_some_and(|uid| Some(uid) != namespace.metadata.uid.as_deref())
    {
        return Err(conflict("Sandbox namespace binding changed during claim"));
    }
    if annotation(&sandbox.metadata, NAMESPACE_UID) != namespace.metadata.uid.as_deref() {
        sandbox = patch_sandbox(
            client,
            &sandbox,
            json!({"annotations": {NAMESPACE_UID: namespace.metadata.uid}}),
        )
        .await?;
    }
    recheck(client, &sandbox, &namespace).await?;
    Ok((sandbox, Some(namespace)))
}

pub async fn recheck(
    client: &Client,
    sandbox: &KarsSandbox,
    expected: &Namespace,
) -> Result<Namespace, Error> {
    let api: Api<Namespace> = Api::all(client.clone());
    let live = api.get(&expected.name_any()).await?;
    if live.metadata.uid != expected.metadata.uid || !claimed(&live, sandbox)? {
        return Err(conflict(
            "namespace ownership changed during reconciliation",
        ));
    }
    if live.metadata.deletion_timestamp.is_some() && sandbox.metadata.deletion_timestamp.is_none() {
        return Err(conflict("namespace deletion started during reconciliation"));
    }
    Ok(live)
}

/// Read-only gate for other controllers consuming sandbox-local resources.
/// A missing namespace is not authority to read or create anything there.
/// Legacy namespaces wait for the Sandbox reconciler to establish their claim.
pub async fn verify_target(
    client: &Client,
    source_namespace: &str,
    name: &str,
) -> Result<bool, Error> {
    let valid_label = |value: &str| {
        !value.is_empty()
            && value.len() <= 63
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
    };
    if !valid_label(source_namespace) || !valid_label(name) {
        return Err(conflict(
            "target name/workspace is not a Kubernetes DNS label",
        ));
    }
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let Some(namespace) = namespaces.get_opt(&format!("kars-{name}")).await? else {
        return Ok(false);
    };
    let sandboxes: Api<KarsSandbox> = Api::namespaced(client.clone(), source_namespace);
    let sandbox = sandboxes
        .get_opt(name)
        .await?
        .ok_or_else(|| conflict("target namespace has no Sandbox owner in this workspace"))?;
    if sandbox.name_any() != name || sandbox.namespace().as_deref() != Some(source_namespace) {
        return Err(conflict(
            "target Sandbox identity differs from the requested workspace/name",
        ));
    }
    if !claimed(&namespace, &sandbox)? {
        return Err(conflict(
            "target namespace ownership has not been established",
        ));
    }
    Ok(true)
}

/// Accepted deletion is sufficient; namespace claims prevent reuse while it
/// terminates. Waiting for disappearance would deadlock a CR inside its own
/// runtime namespace, whose deletion itself waits for this CR's finalizer.
pub async fn delete(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: Option<&Namespace>,
) -> Result<(), Error> {
    let Some(namespace) = namespace else {
        return Ok(());
    };
    let live = match recheck(client, sandbox, namespace).await {
        Err(Error::Kube(kube::Error::Api(error))) if error.code == 404 => return Ok(()),
        result => result?,
    };
    let (uid, rv) = identity(&live.metadata)?;
    let api: Api<Namespace> = Api::all(client.clone());
    if live.metadata.deletion_timestamp.is_none() {
        match api
            .delete(
                &live.name_any(),
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: Some(uid.into()),
                        resource_version: Some(rv.into()),
                    }),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(_) => {}
            Err(kube::Error::Api(error)) if error.code == 404 => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub async fn remove_finalizer(client: &Client, sandbox: &KarsSandbox) -> Result<(), Error> {
    let sandbox = live_sandbox(client, sandbox).await?;
    let finalizers: Vec<_> = sandbox
        .metadata
        .finalizers
        .iter()
        .flatten()
        .filter(|f| f.as_str() != FINALIZER)
        .cloned()
        .collect();
    patch_sandbox(client, &sandbox, json!({"finalizers": finalizers})).await?;
    Ok(())
}

pub async fn report_conflict(
    client: &Client,
    sandbox: &KarsSandbox,
    message: &str,
) -> Result<(), Error> {
    let sandbox = live_sandbox(client, sandbox).await?;
    let (uid, rv) = identity(&sandbox.metadata)?;
    let mut patch =
        crate::status::build_degraded_status_patch(&sandbox, "NamespaceOwnershipConflict", message);
    let current_status = serde_json::to_value(&sandbox.status)?;
    if patch["status"].as_object().is_some_and(|fields| {
        fields
            .iter()
            .all(|(key, value)| current_status.get(key) == Some(value))
    }) {
        return Ok(());
    }
    patch["metadata"] = json!({"uid": uid, "resourceVersion": rv});
    let api: Api<KarsSandbox> = Api::namespaced(client.clone(), &sandbox.namespace().unwrap());
    api.patch_status(
        &sandbox.name_any(),
        &PatchParams::default(),
        &Patch::Merge(patch),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "namespace_ownership_tests.rs"]
mod tests;
