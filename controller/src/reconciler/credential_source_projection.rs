// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use kube::api::{DeleteParams, Preconditions};

fn owner(ns: &Namespace) -> Result<OwnerReference, Error> {
    Ok(OwnerReference {
        api_version: "v1".into(),
        kind: "Namespace".into(),
        name: ns.name_any(),
        uid: identity(&ns.metadata)?.0.into(),
        controller: Some(true),
        block_owner_deletion: Some(false),
    })
}

fn validate_owner(meta: &ObjectMeta, sandbox: &KarsSandbox, ns: &Namespace) -> Result<(), Error> {
    identity(meta)?;
    if meta.name.as_deref() != Some(projection_name(&sandbox.name_any()).as_str())
        || meta.namespace.as_deref() != Some(ns.name_any().as_str())
        || meta.deletion_timestamp.is_some()
        || annotation(meta, PURPOSE) != Some(PROJECTION_PURPOSE)
        || annotation(meta, TARGET) != sandbox.metadata.name.as_deref()
        || annotation(meta, WORKSPACE) != sandbox.metadata.namespace.as_deref()
        || annotation(meta, SANDBOX_UID) != sandbox.metadata.uid.as_deref()
        || annotation(meta, NAMESPACE_UID) != ns.metadata.uid.as_deref()
        || meta.owner_references.as_deref() != Some([owner(ns)?].as_slice())
    {
        return Err(Error::Invalid(
            "credential projection is not owned by this Sandbox/namespace",
        ));
    }
    Ok(())
}

pub(super) fn validate(
    meta: &ObjectMeta,
    sandbox: &KarsSandbox,
    ns: &Namespace,
) -> Result<(), Error> {
    validate_owner(meta, sandbox, ns)?;
    if annotation(meta, PROJECTION_UID) != Some(identity(meta)?.0) {
        return Err(Error::Invalid(
            "projection incarnation is not sealed to its UID",
        ));
    }
    Ok(())
}

async fn seal(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    meta: &ObjectMeta,
) -> Result<PartialObjectMeta<Secret>, Error> {
    namespace_current(client, sandbox, ns).await?;
    sandbox_current(client, sandbox).await?;
    let (uid, rv) = identity(meta)?;
    let api: Api<Secret> = Api::namespaced(client.clone(), &ns.name_any());
    let sealed = api.patch_metadata(&projection_name(&sandbox.name_any()), &PatchParams::default(), &Patch::Merge(json!({
        "metadata": {"uid": uid, "resourceVersion": rv, "annotations": {PROJECTION_UID: uid}}
    }))).await.map_err(|e| api_error("seal projection anchor UID", e))?;
    validate(&sealed.metadata, sandbox, ns)?;
    Ok(sealed)
}

pub(super) async fn anchor(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    source: &Secret,
) -> Result<PartialObjectMeta<Secret>, Error> {
    let api: Api<Secret> = Api::namespaced(client.clone(), &ns.name_any());
    let name = projection_name(&sandbox.name_any());
    if let Some(existing) = metadata(&api, &name).await? {
        validate_owner(&existing.metadata, sandbox, ns)?;
        if annotation(&existing.metadata, PROJECTION_UID).is_some() {
            validate(&existing.metadata, sandbox, ns)?;
            return Ok(existing);
        }
        let anchor = api
            .get(&name)
            .await
            .map_err(|e| api_error("inspect unfinished projection anchor", e))?;
        let authored = anchor
            .metadata
            .managed_fields
            .as_ref()
            .is_some_and(|fields| {
                fields.iter().any(|field| {
                    field.manager.as_deref() == Some(crate::field_managers::CLAWSANDBOX)
                })
            });
        if identity(&anchor.metadata)? != identity(&existing.metadata)?
            || anchor.type_.as_deref() != Some("Opaque")
            || anchor.data.as_ref().is_some_and(|data| !data.is_empty())
            || !authored
        {
            return Err(Error::Invalid(
                "unsealed projection is not an empty controller anchor",
            ));
        }
        return seal(client, sandbox, ns, &anchor.metadata).await;
    }
    namespace_current(client, sandbox, ns).await?;
    sandbox_current(client, sandbox).await?;
    let anchor: Secret = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
        "metadata": {
            "name": name, "namespace": ns.name_any(),
            "annotations": {
                PURPOSE: PROJECTION_PURPOSE, TARGET: sandbox.name_any(),
                WORKSPACE: sandbox.namespace(), SANDBOX_UID: sandbox.metadata.uid,
                NAMESPACE_UID: ns.metadata.uid, SOURCE_UID: source.metadata.uid
            },
            "ownerReferences": [owner(ns)?]
        }
    }))
    .map_err(|_| Error::Invalid("projection metadata serialization failed"))?;
    let created = api
        .create(
            &PostParams {
                field_manager: Some(crate::field_managers::CLAWSANDBOX.into()),
                ..Default::default()
            },
            &anchor,
        )
        .await
        .map_err(|e| api_error("create projection anchor", e))?;
    validate_owner(&created.metadata, sandbox, ns)?;
    seal(client, sandbox, ns, &created.metadata).await
}

pub(super) async fn revoke(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    remove: bool,
) -> Result<(), Error> {
    let api: Api<Secret> = Api::namespaced(client.clone(), &ns.name_any());
    let name = projection_name(&sandbox.name_any());
    let Some(meta) = metadata(&api, &name).await? else {
        return Ok(());
    };
    if validate(&meta.metadata, sandbox, ns).is_err() {
        // Never clear/delete an unrelated object, even when its reserved name
        // collides with a previously used projection.
        return Ok(());
    }
    namespace_current(client, sandbox, ns).await?;
    let existing = api
        .get(&name)
        .await
        .map_err(|e| api_error("read owned projection for revocation", e))?;
    if identity(&existing.metadata)? != identity(&meta.metadata)?
        || existing.type_.as_deref() != Some("Opaque")
    {
        return Err(Error::Invalid(
            "projection identity/type changed before revocation",
        ));
    }
    let (uid, rv) = identity(&existing.metadata)?;
    if remove {
        match api
            .delete(
                &name,
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
            Err(kube::Error::Api(status)) if status.code == 404 => {}
            Err(error) => return Err(api_error("remove owned projection", error)),
        }
    } else if existing.data.as_ref().is_some_and(|data| !data.is_empty()) {
        // JSON merge null clears all keys; {} alone would preserve old keys.
        api.patch_metadata(
            &name,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {"uid": uid, "resourceVersion": rv}, "data": null
            })),
        )
        .await
        .map_err(|e| api_error("revoke owned projection", e))?;
    }
    Ok(())
}

pub(super) async fn detach(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
) -> Result<(), Error> {
    let api: Api<Secret> = Api::namespaced(client.clone(), &ns.name_any());
    let owned = metadata(&api, &projection_name(&sandbox.name_any()))
        .await?
        .is_some_and(|value| validate(&value.metadata, sandbox, ns).is_ok());
    workloads::pause(client, sandbox, ns, !owned).await?;
    if owned {
        revoke(client, sandbox, ns, true).await?;
    }
    Ok(())
}
