// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::credential_source::{INTENT, PURPOSE, TARGET, WORKSPACE};
use k8s_openapi::{ByteString, apimachinery::pkg::apis::meta::v1::OwnerReference};
use kube::api::PostParams;
use std::collections::BTreeMap;

#[path = "targets.rs"]
mod targets;

fn annotation<'a>(metadata: &'a kube::api::ObjectMeta, key: &str) -> Option<&'a str> {
    metadata.annotations.as_ref()?.get(key).map(String::as_str)
}

pub fn input_name(kind: &str, name: &str) -> Result<String, String> {
    let kind = match kind {
        "Workspace" => "workspace",
        "KarsTeam" => "team",
        "KarsTask" => "task",
        "KarsSandbox" => "sandbox",
        _ => return Err("Unsupported credential source target kind".into()),
    };
    if kind == "workspace" {
        Ok(format!("{INPUT_PREFIX}workspace"))
    } else {
        Ok(format!("{INPUT_PREFIX}{kind}-{name}"))
    }
}

fn source_metadata(source: &Secret, grant: &KarsCredentialGrant) -> Result<SourceMetadata, String> {
    let (uid, rv) = identity(&source.metadata)?;
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let kind = annotation(&source.metadata, TARGET_KIND).ok_or("Source target kind missing")?;
    let name = annotation(&source.metadata, TARGET).ok_or("Source target name missing")?;
    if source.name_any() != input_name(kind, name)?
        || source.namespace().as_deref() != Some(namespace.as_str())
        || annotation(&source.metadata, PURPOSE) != Some(INPUT_PURPOSE)
        || annotation(&source.metadata, WORKSPACE) != Some(namespace.as_str())
        || annotation(&source.metadata, GRANT_UID) != grant.metadata.uid.as_deref()
        || annotation(&source.metadata, INTENT) != Some("explicit-reference-v2")
        || source.type_.as_deref() != Some("Opaque")
    {
        return Err("Source purpose, target, workspace, type or grant identity is invalid".into());
    }
    let bound = annotation(&source.metadata, TARGET_UID).is_some();
    let target = annotation(&source.metadata, TARGET_UID)
        .filter(|_| kind != "Workspace")
        .map(|uid| CredentialTarget {
            kind: kind.into(),
            namespace: namespace.clone(),
            name: name.into(),
            uid: uid.into(),
        });
    let keys = source
        .data
        .iter()
        .flatten()
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let allowed = permitted_agent_keys(grant)?;
    let valid = source.data.iter().flatten().all(|(key, value)| {
        allowed.contains(key) && !value.0.contains(&0) && std::str::from_utf8(&value.0).is_ok()
    }) && source
        .data
        .iter()
        .flatten()
        .map(|(_, value)| value.0.len())
        .sum::<usize>()
        <= 131_072;
    Ok(SourceMetadata {
        name: source.name_any(),
        uid: uid.into(),
        resource_version: rv.into(),
        keys,
        phase: if !valid {
            "Blocked"
        } else if bound {
            "Ready"
        } else {
            "Unbound"
        }
        .into(),
        reason: if valid {
            "SourceValidated"
        } else {
            "KeyGrantOrValueInvalid"
        }
        .into(),
        target,
    })
}

pub(super) async fn inventory(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<Vec<SourceMetadata>, String> {
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let api: Api<Secret> = Api::namespaced(client.clone(), &namespace);
    let metadata = api
        .list_metadata(&ListParams::default())
        .await
        .map_err(|e| api_error("Read credential source metadata", e))?;
    let mut sources = Vec::new();
    for item in metadata {
        if !item.name_any().starts_with(INPUT_PREFIX)
            || annotation(&item.metadata, GRANT_UID) != grant.metadata.uid.as_deref()
            || annotation(&item.metadata, PURPOSE) != Some(INPUT_PURPOSE)
        {
            continue;
        }
        let source = api
            .get(&item.name_any())
            .await
            .map_err(|e| api_error("Read enrolled credential source", e))?;
        if identity(&source.metadata)? != identity(&item.metadata)? {
            return Err("Source changed during inventory".into());
        }
        if let Ok(mut value) = source_metadata(&source, grant) {
            if value.target.is_none()
                && annotation(&source.metadata, TARGET_KIND) != Some("Workspace")
            {
                let kind = annotation(&source.metadata, TARGET_KIND)
                    .ok_or("Source target kind missing")?;
                let name =
                    annotation(&source.metadata, TARGET).ok_or("Source target name missing")?;
                let resource = kube::core::ApiResource::from_gvk(
                    &kube::core::GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind),
                );
                if let Some(target) = Api::<kube::core::DynamicObject>::namespaced_with(
                    client.clone(),
                    &namespace,
                    &resource,
                )
                .get_opt(name)
                .await
                .map_err(|e| api_error("Read explicitly bound source target", e))?
                {
                    let bindings = if kind == "KarsSandbox" {
                        &target.data["spec"]["credentialBindings"]
                    } else {
                        &target.data["spec"]["blueprint"]["credentialBindings"]
                    };
                    let uid = identity(&target.metadata)?.0;
                    if bindings["grant"]["uid"] == json!(grant.metadata.uid)
                        && bindings["sources"].as_array().is_some_and(|items| {
                            items.iter().any(|item| {
                                item["source"]["uid"] == value.uid && item["owner"]["uid"] == uid
                            })
                        })
                    {
                        value.target = Some(CredentialTarget {
                            kind: kind.into(),
                            namespace: namespace.clone(),
                            name: name.into(),
                            uid: uid.into(),
                        });
                    }
                }
            }
            if let Some(target) = &value.target {
                let owner = owner_ref(target);
                let valid = targets::read(client, target).await.is_ok()
                    && source
                        .metadata
                        .owner_references
                        .as_ref()
                        .is_none_or(|refs| refs.is_empty() || refs == &[owner.clone()]);
                if !valid {
                    value.phase = "Blocked".into();
                    value.reason = "TargetIdentityOrOwnershipChanged".into();
                } else if source
                    .metadata
                    .owner_references
                    .as_ref()
                    .is_none_or(Vec::is_empty)
                {
                    let bound=api.patch_metadata(&source.name_any(),&PatchParams::default(),&Patch::Merge(json!({
                        "metadata":{"uid":source.metadata.uid,"resourceVersion":source.metadata.resource_version,"ownerReferences":[owner],
                            "annotations":{TARGET_UID:target.uid}}
                    }))).await.map_err(|e|api_error("Bind observed source ownership",e))?;
                    value.resource_version = identity(&bound.metadata)?.1.into();
                    value.phase = "Ready".into();
                }
            }
            sources.push(value);
        }
    }
    sources.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sources)
}

fn owner_ref(target: &CredentialTarget) -> OwnerReference {
    OwnerReference {
        api_version: if target.kind == "Workspace" {
            "v1"
        } else {
            "kars.azure.com/v1alpha1"
        }
        .into(),
        kind: if target.kind == "Workspace" {
            "Namespace".into()
        } else {
            target.kind.clone()
        },
        name: target.name.clone(),
        uid: target.uid.clone(),
        controller: Some(true),
        block_owner_deletion: Some(false),
    }
}

async fn read_input(
    client: &Client,
    grant: &KarsCredentialGrant,
    target: &CredentialTarget,
    selection: &CredentialSelection,
) -> Result<Secret, String> {
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let owner = match selection.scope {
        CredentialScope::Workspace => CredentialTarget {
            kind: "Workspace".into(),
            namespace: namespace.clone(),
            name: namespace.clone(),
            uid: grant.spec.workspace_uid.clone(),
        },
        _ => selection
            .owner
            .clone()
            .ok_or("A non-workspace credential source must pin its actual target CREATE UID")?,
    };
    if selection.scope != CredentialScope::Workspace {
        targets::owner_allowed(client, target, &owner).await?;
    }
    let api: Api<Secret> = Api::namespaced(client.clone(), &namespace);
    let meta = api
        .get_metadata(&selection.source.name)
        .await
        .map_err(|e| api_error("Read selected source identity", e))?;
    if identity(&meta.metadata)?.0 != selection.source.uid {
        return Err("Selected credential source was replaced".into());
    }
    let mut source = api
        .get(&selection.source.name)
        .await
        .map_err(|e| api_error("Read selected agent credentials", e))?;
    if identity(&source.metadata)? != identity(&meta.metadata)? {
        return Err("Credential source changed during read".into());
    }
    let status = source_metadata(&source, grant)?;
    if status.phase == "Blocked"
        || selection.keys.iter().any(|key| {
            !permitted_agent_keys(grant)
                .unwrap_or_default()
                .contains(key)
        })
        || source.name_any() != input_name(&owner.kind, &owner.name)?
        || annotation(&source.metadata, TARGET_KIND) != Some(owner.kind.as_str())
        || annotation(&source.metadata, TARGET) != Some(owner.name.as_str())
        || annotation(&source.metadata, TARGET_UID).is_some_and(|uid| uid != owner.uid)
    {
        return Err("Credential source key grant or exact owner does not match".into());
    }
    let expected = owner_ref(&owner);
    if source
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|refs| !refs.is_empty() && refs != &[expected.clone()])
    {
        return Err("Credential source has a foreign owner; it is not adopted".into());
    }
    let import_key = "kars.azure.com/credential-import-revision";
    let migration = if annotation(&source.metadata, import_key).is_none() {
        Some(
            super::legacy::import_values(
                client,
                grant,
                &source.name_any(),
                if owner.kind == "Workspace" {
                    None
                } else {
                    Some(&owner)
                },
            )
            .await?,
        )
    } else {
        None
    };
    if annotation(&source.metadata, TARGET_UID).is_none()
        || source
            .metadata
            .owner_references
            .as_ref()
            .is_none_or(Vec::is_empty)
    {
        let (uid, rv) = identity(&source.metadata)?;
        let bound=api.patch_metadata(&source.name_any(),&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":uid,"resourceVersion":rv,"ownerReferences":[expected],"annotations":{TARGET_UID:owner.uid}}
        }))).await.map_err(|e|api_error("Bind source to captured target UID",e))?;
        source.metadata = bound.metadata;
    }
    if let Some((mut imported, revision)) = migration {
        imported.extend(source.data.clone().unwrap_or_default());
        let (uid, rv) = identity(&source.metadata)?;
        let written = api
            .patch_metadata(
                &source.name_any(),
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata":{"uid":uid,"resourceVersion":rv,"annotations":{import_key:revision}},
                    "data":imported,
                })),
            )
            .await
            .map_err(|e| api_error("Import only reviewed legacy credential keys", e))?;
        source.metadata = written.metadata;
        source.data = Some(imported);
    }
    Ok(source)
}

fn bundle_name(target: &CredentialTarget) -> String {
    format!(
        "{BUNDLE_PREFIX}{}-{}",
        target.kind.to_ascii_lowercase(),
        target.name
    )
}

pub(crate) async fn prepare(
    client: &Client,
    target: &CredentialTarget,
    bindings: &CredentialBindings,
) -> Result<Secret, String> {
    validate_bindings(bindings)?;
    let mut target_object = targets::read(client, target).await?;
    if target.kind == "KarsTask" {
        let task: crate::kars_task::KarsTask = serde_json::from_value(
            serde_json::to_value(&target_object).map_err(|_| "Task serialization failed")?,
        )
        .map_err(|_| "Credential target Task is malformed")?;
        if !crate::kars_task_reconciler::task_is_ready(&task) {
            return Err("Credential target Task authority is not current".into());
        }
    }
    let grant = current(client, &target.namespace, &bindings.grant).await?;
    let mut values = BTreeMap::<String, ByteString>::new();
    let mut states = Vec::new();
    for selection in &bindings.sources {
        let source = read_input(client, &grant, target, selection).await?;
        for key in &selection.keys {
            if let Some(value) = source.data.as_ref().and_then(|data| data.get(key)) {
                values.insert(key.clone(), value.clone());
            } else {
                values.remove(key);
            }
        }
        states.push(json!({"name":source.name_any(),"uid":source.metadata.uid,"resourceVersion":source.metadata.resource_version,
            "keys":selection.keys,"scope":selection.scope}));
    }
    let input_state = json!({"grantUid":grant.metadata.uid,"grantVersion":grant.metadata.resource_version,
        "target":target,"sources":states,"bindings":bindings});
    let serialized = serde_json::to_string(&input_state)
        .map_err(|_| "Credential binding metadata serialization failed")?;
    let api: Api<Secret> = Api::namespaced(client.clone(), &target.namespace);
    let name = bundle_name(target);
    let bundle_uid_key = "kars.azure.com/credential-bundle-uid";
    let mut bundle = match api
        .get_opt(&name)
        .await
        .map_err(|e| api_error("Read owned credential bundle", e))?
    {
        Some(source) => {
            if annotation(&source.metadata, PURPOSE) != Some(BUNDLE_PURPOSE)
                || annotation(&source.metadata, TARGET_UID) != Some(target.uid.as_str())
                || annotation(&source.metadata, GRANT_UID) != grant.metadata.uid.as_deref()
                || source.metadata.owner_references.as_deref()
                    != Some([owner_ref(target)].as_slice())
                || source.type_.as_deref() != Some("Opaque")
                || annotation(&target_object.metadata, bundle_uid_key)
                    != source.metadata.uid.as_deref()
            {
                return Err("Existing credential bundle is not owned by the exact target".into());
            }
            source
        }
        None => {
            let source:Secret=serde_json::from_value(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
                "metadata":{"name":name,"namespace":target.namespace,"ownerReferences":[owner_ref(target)],
                    "annotations":{PURPOSE:BUNDLE_PURPOSE,TARGET_KIND:target.kind,TARGET:target.name,
                        TARGET_UID:target.uid,WORKSPACE:target.namespace,GRANT_UID:grant.metadata.uid}}}))
                .map_err(|_|"Credential bundle metadata serialization failed")?;
            if annotation(&target_object.metadata, bundle_uid_key).is_some() {
                return Err("Previously bound credential bundle disappeared; explicit operator recovery is required".into());
            }
            let created = api
                .create(&PostParams::default(), &source)
                .await
                .map_err(|e| api_error("Create owned credential bundle anchor", e))?;
            let resource = kube::core::ApiResource::from_gvk(&kube::core::GroupVersionKind::gvk(
                "kars.azure.com",
                "v1alpha1",
                &target.kind,
            ));
            let targets: Api<kube::core::DynamicObject> =
                Api::namespaced_with(client.clone(), &target.namespace, &resource);
            target_object=targets.patch(&target.name,&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":target.uid,"resourceVersion":target_object.metadata.resource_version,
                    "annotations":{bundle_uid_key:created.metadata.uid}}
            }))).await.map_err(|e|api_error("Record actual credential bundle CREATE UID",e))?;
            created
        }
    };
    let live = targets::read(client, target).await?;
    if identity(&live.metadata)? != identity(&target_object.metadata)? {
        return Err("Credential target changed before bundle write".into());
    }
    let fresh = current(client, &target.namespace, &bindings.grant).await?;
    if identity(&fresh.metadata)? != identity(&grant.metadata)? {
        return Err("Credential grant changed before bundle write".into());
    }
    for state in states {
        let meta = api
            .get_metadata(state["name"].as_str().ok_or("Source name missing")?)
            .await
            .map_err(|e| api_error("Recheck credential source", e))?;
        if meta.metadata.uid.as_deref() != state["uid"].as_str()
            || meta.metadata.resource_version.as_deref() != state["resourceVersion"].as_str()
        {
            return Err("Credential source changed before bundle write".into());
        }
    }
    if bundle.data.as_ref() != Some(&values)
        || annotation(&bundle.metadata, INPUT_STATE) != Some(serialized.as_str())
    {
        let (uid, rv) = identity(&bundle.metadata)?;
        let mut data =
            serde_json::to_value(&values).map_err(|_| "Credential data serialization failed")?;
        for key in bundle.data.iter().flatten().map(|(key, _)| key) {
            if !values.contains_key(key) {
                data[key] = serde_json::Value::Null;
            }
        }
        let updated=api.patch_metadata(&name,&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":uid,"resourceVersion":rv,"annotations":{INPUT_STATE:serialized}},"data":data,
        }))).await.map_err(|e|api_error("Write UID-fenced credential bundle",e))?;
        bundle.metadata = updated.metadata;
        bundle.data = Some(values);
    }
    Ok(bundle)
}

pub(crate) async fn for_sandbox(
    client: &Client,
    sandbox: &crate::crd::KarsSandbox,
) -> Result<Secret, String> {
    let namespace = sandbox.namespace().ok_or("Sandbox workspace missing")?;
    let task_owner = sandbox.metadata.owner_references.as_ref().and_then(|refs| {
        refs.iter().find(|owner| {
            owner.kind == "KarsTask"
                && owner.api_version == "kars.azure.com/v1alpha1"
                && owner.controller == Some(true)
        })
    });
    if task_owner.is_none()
        && let Some(bindings) = &sandbox.spec.credential_bindings
    {
        if sandbox.spec.credentials_ref.is_some() {
            return Err("Direct v1 and governed v2 credentials cannot be combined".into());
        }
        return prepare(
            client,
            &CredentialTarget {
                kind: "KarsSandbox".into(),
                namespace,
                name: sandbox.name_any(),
                uid: identity(&sandbox.metadata)?.0.into(),
            },
            bindings,
        )
        .await;
    }
    let owner = task_owner.ok_or("A bundle-bound Sandbox must have its exact Task owner")?;
    let task = Api::<crate::kars_task::KarsTask>::namespaced(client.clone(), &namespace)
        .get(&owner.name)
        .await
        .map_err(|e| api_error("Read bundle Task owner", e))?;
    if task.uid().as_deref() != Some(owner.uid.as_str())
        || task.name_any() != sandbox.name_any()
        || !crate::kars_task_reconciler::task_is_ready(&task)
    {
        return Err("Bundle Task owner identity or authority changed".into());
    }
    let bindings = task
        .spec
        .blueprint
        .as_ref()
        .and_then(|b| b.credential_bindings.as_ref())
        .ok_or("Task credential grant was removed")?;
    let bundle = prepare(
        client,
        &CredentialTarget {
            kind: "KarsTask".into(),
            namespace,
            name: task.name_any(),
            uid: owner.uid.clone(),
        },
        bindings,
    )
    .await?;
    if let Some(declared) = &sandbox.spec.credential_bindings {
        if serde_json::to_value(declared).ok() != serde_json::to_value(bindings).ok() {
            return Err(
                "Sandbox credential declaration differs from its current Task authority".into(),
            );
        }
        return Ok(bundle);
    }
    if sandbox
        .spec
        .credentials_ref
        .as_ref()
        .is_none_or(|reference| {
            reference.name != bundle.name_any() || reference.uid != bundle.uid().unwrap_or_default()
        })
    {
        return Err("Sandbox bundle reference was replaced or is stale".into());
    }
    Ok(bundle)
}
