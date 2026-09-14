// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Repair only an anchor CAS interrupted in this invocation after exclusive CREATE.
//! No receipt is reconstructed from a GET, a name, or owner-shaped annotations.
//! Recovery never writes Secret values and always requires a fresh prepare.
//! Sandbox status/suspension churn and narrowly verified materialized-Task
//! execution status are tolerated; grant/source revisions are never rebased.
//! Lost CREATE acknowledgements, changed authority and exhausted conflicts remain
//! explicit errors; existing objects are never adopted, deleted or cleared here.

use super::*;
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
use serde_json::Value;

#[path = "bundle_task.rs"]
mod task;

#[derive(Clone, Copy)]
pub(super) struct TaskConsumer<'a> {
    pub sandbox: &'a crate::crd::KarsSandbox,
    pub task: &'a crate::kars_task::KarsTask,
}

pub(super) async fn verify_task_caller(
    client: &Client,
    consumer: TaskConsumer<'_>,
    target_object: &DynamicObject,
    target: &CredentialTarget,
    bindings: &CredentialBindings,
) -> Result<(), String> {
    task::verify_caller(client, consumer, target_object, target, bindings).await
}

pub(super) const UID_ANNOTATION: &str = "kars.azure.com/credential-bundle-uid";
pub(super) const RECOVERY_PATCHES: usize = 2;
pub(super) const RECONCILE_REQUIRED: &str =
    "Credential bundle anchor recovered; fresh reconciliation is required before credential values";

pub(super) struct Creation<'a> {
    pub target: &'a CredentialTarget,
    pub original: &'a DynamicObject,
    pub bindings: &'a CredentialBindings,
    pub grant: &'a KarsCredentialGrant,
    pub states: &'a [Value],
    pub created: &'a Secret,
    pub task_consumer: Option<TaskConsumer<'a>>,
}

pub(super) fn verify_owned(
    secret: &Secret,
    target: &CredentialTarget,
    grant: &KarsCredentialGrant,
) -> Result<(), String> {
    identity(&secret.metadata)?;
    if secret.name_any() != bundle_name(target)
        || secret.namespace().as_deref() != Some(target.namespace.as_str())
        || secret.type_.as_deref() != Some("Opaque")
        || secret.metadata.owner_references.as_deref() != Some([owner_ref(target)].as_slice())
        || annotation(&secret.metadata, PURPOSE) != Some(BUNDLE_PURPOSE)
        || annotation(&secret.metadata, TARGET_KIND) != Some(target.kind.as_str())
        || annotation(&secret.metadata, TARGET) != Some(target.name.as_str())
        || annotation(&secret.metadata, TARGET_UID) != Some(target.uid.as_str())
        || annotation(&secret.metadata, WORKSPACE) != Some(target.namespace.as_str())
        || annotation(&secret.metadata, GRANT_UID) != grant.metadata.uid.as_deref()
    {
        return Err("Existing credential bundle is not owned by the exact target".into());
    }
    Ok(())
}

fn empty_created(secret: &Secret, creation: &Creation<'_>) -> Result<(), String> {
    verify_owned(secret, creation.target, creation.grant)?;
    if identity(&secret.metadata)? != identity(&creation.created.metadata)?
        || secret.data.as_ref().is_some_and(|data| !data.is_empty())
        || secret
            .string_data
            .as_ref()
            .is_some_and(|data| !data.is_empty())
        || secret.immutable == Some(true)
        || annotation(&secret.metadata, INPUT_STATE).is_some()
    {
        return Err(
            "Credential bundle CREATE identity or empty state changed; operator recovery is required"
                .into(),
        );
    }
    Ok(())
}

fn authority_view(
    object: &DynamicObject,
    target: &CredentialTarget,
    allow_suspension_change: bool,
) -> Result<(kube::api::ObjectMeta, Value), String> {
    if identity(&object.metadata)?.0 != target.uid
        || object.name_any() != target.name
        || object.namespace().as_deref() != Some(target.namespace.as_str())
        || object.types.as_ref().is_none_or(|types| {
            types.kind != target.kind || types.api_version != "kars.azure.com/v1alpha1"
        })
    {
        return Err("Credential target identity changed during anchor recording".into());
    }
    let mut metadata = object.metadata.clone();
    metadata.resource_version = None;
    metadata.generation = None;
    metadata.managed_fields = None;
    if let Some(annotations) = metadata.annotations.as_mut() {
        annotations.remove(UID_ANNOTATION);
        if annotations.is_empty() {
            metadata.annotations = None;
        }
    }
    let mut data = object.data.clone();
    let fields = data
        .as_object_mut()
        .ok_or("Credential target data is malformed")?;
    fields.remove("status");
    let spec = fields
        .get_mut("spec")
        .and_then(Value::as_object_mut)
        .ok_or("Credential target spec is malformed")?;
    if allow_suspension_change {
        if target.kind != "KarsSandbox"
            || spec
                .get("suspended")
                .is_some_and(|value| !value.is_boolean())
        {
            return Err("Credential target suspension is not a supported metadata recovery".into());
        }
        spec.remove("suspended");
    }
    Ok((metadata, data))
}

fn response(
    prior: &DynamicObject,
    updated: DynamicObject,
    creation: &Creation<'_>,
) -> Result<DynamicObject, String> {
    if creation.task_consumer.is_some() {
        task::verify_transition(prior, &updated, creation.target, creation.bindings)?;
    }
    if identity(&updated.metadata)?.1 == identity(&prior.metadata)?.1
        || annotation(&updated.metadata, UID_ANNOTATION) != creation.created.metadata.uid.as_deref()
        || authority_view(prior, creation.target, false)?
            != authority_view(&updated, creation.target, false)?
    {
        return Err(
            "Credential bundle anchor response changed authority or omitted its transition".into(),
        );
    }
    Ok(updated)
}

async fn patch_anchor(
    api: &Api<DynamicObject>,
    object: &DynamicObject,
    creation: &Creation<'_>,
) -> Result<DynamicObject, kube::Error> {
    api.patch(
        &creation.target.name,
        &PatchParams::default(),
        &Patch::Merge(json!({
            "metadata":{"uid":creation.target.uid,"resourceVersion":object.metadata.resource_version,
                "annotations":{UID_ANNOTATION:creation.created.metadata.uid}},
        })),
    )
    .await
}

async fn recovery_snapshot(
    client: &Client,
    creation: &Creation<'_>,
) -> Result<DynamicObject, String> {
    let target = creation.target;
    let live = targets::read(client, target).await?;
    if let Some(consumer) = creation.task_consumer {
        task::verify_transition(creation.original, &live, target, creation.bindings)?;
        verify_task_caller(client, consumer, &live, target, creation.bindings).await?;
    } else {
        if authority_view(creation.original, target, true)? != authority_view(&live, target, true)?
        {
            return Err("Credential target authority changed during anchor recovery".into());
        }
        verify_bindings(&live, creation.bindings)?;
    }
    let grant = current(client, &target.namespace, &creation.bindings.grant).await?;
    if identity(&grant.metadata)? != identity(&creation.grant.metadata)?
        || grant.metadata.generation != creation.grant.metadata.generation
        || serde_json::to_value(&grant.spec).map_err(|_| "Grant serialization failed")?
            != serde_json::to_value(&creation.grant.spec)
                .map_err(|_| "Grant serialization failed")?
    {
        return Err("Credential grant changed during anchor recovery".into());
    }
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &target.namespace);
    for state in creation.states {
        let name = state["name"].as_str().ok_or("Source name missing")?;
        let source = secrets
            .get_metadata(name)
            .await
            .map_err(|e| api_error("Recheck source before anchor recovery", e))?;
        let (uid, rv) = identity(&source.metadata)?;
        if Some(uid) != state["uid"].as_str()
            || Some(rv) != state["resourceVersion"].as_str()
            || source.name_any() != name
            || source.namespace().as_deref() != Some(target.namespace.as_str())
        {
            return Err("Credential source changed during anchor recovery".into());
        }
    }
    let bundle = secrets
        .get(&bundle_name(target))
        .await
        .map_err(|e| api_error("Recheck actual empty bundle CREATE", e))?;
    empty_created(&bundle, creation)?;
    Ok(live)
}

pub(super) fn verify_bindings(
    target: &DynamicObject,
    expected: &CredentialBindings,
) -> Result<(), String> {
    let declared: CredentialBindings =
        serde_json::from_value(target.data["spec"]["credentialBindings"].clone())
            .map_err(|_| "Credential target bindings are missing or malformed")?;
    if &declared != expected
        || target.data["spec"]
            .get("credentialsRef")
            .is_some_and(|value| !value.is_null())
    {
        return Err("Credential target bindings differ from the requested authority".into());
    }
    Ok(())
}

pub(super) async fn record_created(
    client: &Client,
    creation: Creation<'_>,
) -> Result<DynamicObject, String> {
    empty_created(creation.created, &creation)?;
    let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
        "kars.azure.com",
        "v1alpha1",
        &creation.target.kind,
    ));
    let api = Api::namespaced_with(client.clone(), &creation.target.namespace, &resource);
    match patch_anchor(&api, creation.original, &creation).await {
        Ok(updated) => return response(creation.original, updated, &creation),
        Err(kube::Error::Api(error))
            if error.code == 409
                && ((creation.target.kind == "KarsTask" && creation.task_consumer.is_some())
                    || (creation.target.kind == "KarsSandbox"
                        && creation
                            .original
                            .metadata
                            .owner_references
                            .as_ref()
                            .is_none_or(|owners| {
                                !owners.iter().any(|owner| {
                                    owner.kind == "KarsTask" && owner.controller == Some(true)
                                })
                            }))) =>
        {
            tracing::warn!(
                target_kind = %creation.target.kind,
                namespace = %creation.target.namespace,
                target = %creation.target.name,
                status_code = 409,
                "CredentialBundleExclusiveCreateAnchorCasConflict"
            );
        }
        Err(error) => {
            return Err(api_error(
                "Record actual credential bundle CREATE UID",
                error,
            ));
        }
    }
    for _ in 0..RECOVERY_PATCHES {
        let live = recovery_snapshot(client, &creation).await?;
        if let Some(uid) = annotation(&live.metadata, UID_ANNOTATION) {
            return Err(if Some(uid) == creation.created.metadata.uid.as_deref() {
                RECONCILE_REQUIRED.into()
            } else {
                "Credential bundle anchor changed to another UID; operator recovery is required"
                    .into()
            });
        }
        match patch_anchor(&api, &live, &creation).await {
            Ok(updated) => {
                response(&live, updated, &creation)?;
                return Err(RECONCILE_REQUIRED.into());
            }
            Err(kube::Error::Api(error)) if error.code == 409 => {}
            Err(error) => {
                return Err(api_error(
                    "Recover credential bundle metadata anchor",
                    error,
                ));
            }
        }
    }
    Err("Credential bundle anchor recovery exhausted metadata CAS retries; operator recovery is required".into())
}
