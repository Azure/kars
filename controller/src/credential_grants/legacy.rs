// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only discovery followed by explicitly reviewed legacy import.

use super::*;
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

async fn inspect(
    client: &Client,
    namespace: &str,
    name: &str,
    source_name: String,
    target: Option<CredentialTarget>,
    selected: bool,
) -> Result<Option<LegacyImport>, String> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let Some(meta) = api
        .get_metadata_opt(name)
        .await
        .map_err(|e| api_error("Inspect legacy credential identity", e))?
    else {
        return Ok(None);
    };
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let Some(ns) = namespaces
        .get_opt(namespace)
        .await
        .map_err(|e| api_error("Inspect legacy credential namespace", e))?
    else {
        return if selected {
            Err("Selected legacy credential namespace disappeared".into())
        } else {
            Ok(None)
        };
    };
    if ns.metadata.deletion_timestamp.is_some() || meta.metadata.deletion_timestamp.is_some() {
        return if selected {
            Err("Selected legacy credential namespace or store is terminating".into())
        } else {
            Ok(None)
        };
    }
    let namespace_uid = identity(&ns.metadata)?.0.to_string();
    let secret = api
        .get(name)
        .await
        .map_err(|e| api_error("Inspect legacy credential key names", e))?;
    if identity(&secret.metadata)? != identity(&meta.metadata)?
        || secret.type_.as_deref() != Some("Opaque")
    {
        return Err("Legacy credential store changed or is not Opaque".into());
    }
    Ok(Some(LegacyImport {
        source_name,
        namespace: namespace.into(),
        namespace_uid,
        secret: ObjectIdentity {
            name: name.into(),
            uid: identity(&secret.metadata)?.0.into(),
        },
        resource_version: identity(&secret.metadata)?.1.into(),
        keys: secret
            .data
            .iter()
            .flatten()
            .map(|(key, _)| key.clone())
            .collect(),
        target,
    }))
}

pub(super) async fn inventory(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<Vec<LegacyImport>, String> {
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let mut sources = Vec::new();
    let mut workspaces = BTreeSet::from([namespace.clone(), "kars-system".into()]);
    for reviewed in &grant.spec.legacy_imports {
        workspaces.insert(reviewed.namespace.clone());
    }
    for workspace in &workspaces {
        if let Some(store) = inspect(
            client,
            workspace,
            "kars-workspace-channels",
            format!("{INPUT_PREFIX}workspace"),
            None,
            false,
        )
        .await?
        {
            sources.push(store);
        }
    }
    for kind in ["KarsSandbox", "KarsTask", "KarsTeam"] {
        let resource =
            ApiResource::from_gvk(&GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind));
        let targets = Api::<DynamicObject>::namespaced_with(client.clone(), &namespace, &resource)
            .list(&ListParams::default())
            .await
            .map_err(|e| api_error("Inspect legacy credential targets", e))?;
        for target in targets {
            if target.metadata.deletion_timestamp.is_some() {
                continue;
            }
            let target = CredentialTarget {
                kind: kind.into(),
                namespace: namespace.clone(),
                name: target.name_any(),
                uid: identity(&target.metadata)?.0.into(),
            };
            let source_name = super::sources::input_name(kind, &target.name)?;
            if kind == "KarsTeam" {
                for workspace in &workspaces {
                    if let Some(store) = inspect(
                        client,
                        workspace,
                        &format!("kars-team-channel-{}", target.name),
                        source_name.clone(),
                        Some(target.clone()),
                        false,
                    )
                    .await?
                    {
                        sources.push(store);
                    }
                }
            }
            if let Some(store) = inspect(
                client,
                &format!("kars-{}", target.name),
                &format!("{}-credentials", target.name),
                source_name,
                Some(target),
                false,
            )
            .await?
            {
                sources.push(store);
            }
        }
    }
    Ok(sources)
}

pub(super) async fn import_values(
    client: &Client,
    grant: &KarsCredentialGrant,
    source_name: &str,
    target: Option<&CredentialTarget>,
) -> Result<(BTreeMap<String, k8s_openapi::ByteString>, String), String> {
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let mut workspaces = BTreeSet::from([namespace.clone(), "kars-system".into()]);
    workspaces.extend(
        grant
            .spec
            .legacy_imports
            .iter()
            .map(|entry| entry.namespace.clone()),
    );
    let mut discovered = Vec::new();
    if let Some(target) = target {
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "kars.azure.com",
            "v1alpha1",
            &target.kind,
        ));
        let live = Api::<DynamicObject>::namespaced_with(client.clone(), &namespace, &resource)
            .get(&target.name)
            .await
            .map_err(|e| api_error("Verify selected legacy credential owner", e))?;
        if identity(&live.metadata)?.0 != target.uid || target.namespace != namespace {
            return Err("Selected legacy credential owner changed".into());
        }
        if target.kind == "KarsTeam" {
            for workspace in &workspaces {
                if let Some(store) = inspect(
                    client,
                    workspace,
                    &format!("kars-team-channel-{}", target.name),
                    source_name.into(),
                    Some(target.clone()),
                    true,
                )
                .await?
                {
                    discovered.push(store);
                }
            }
        }
        if let Some(store) = inspect(
            client,
            &format!("kars-{}", target.name),
            &format!("{}-credentials", target.name),
            source_name.into(),
            Some(target.clone()),
            true,
        )
        .await?
        {
            discovered.push(store);
        }
    } else {
        for workspace in &workspaces {
            if let Some(store) = inspect(
                client,
                workspace,
                "kars-workspace-channels",
                source_name.into(),
                None,
                true,
            )
            .await?
            {
                discovered.push(store);
            }
        }
    }
    for reviewed in grant
        .spec
        .legacy_imports
        .iter()
        .filter(|entry| entry.source_name == source_name && entry.target.as_ref() == target)
    {
        if !discovered.iter().any(|entry| {
            entry.namespace == reviewed.namespace
                && entry.secret == reviewed.secret
                && entry.namespace_uid == reviewed.namespace_uid
        }) {
            return Err(
                "Selected reviewed legacy credentials disappeared or changed identity".into(),
            );
        }
    }
    let candidates = discovered
        .iter()
        .filter(|entry| entry.source_name == source_name && entry.target.as_ref() == target)
        .collect::<Vec<_>>();
    let mut values = BTreeMap::new();
    let mut revisions = Vec::new();
    let allowed = permitted_agent_keys(grant)?;
    for candidate in candidates {
        if !grant.spec.legacy_imports.iter().any(|review| {
            review.source_name == candidate.source_name
                && review.namespace == candidate.namespace
                && review.namespace_uid == candidate.namespace_uid
                && review.secret == candidate.secret
                && review.resource_version == candidate.resource_version
                && review.target == candidate.target
                && review.keys == candidate.keys
        }) {
            return Err("Legacy credentials require explicit operator UID/resourceVersion/key-name review before source migration".into());
        }
        if let Some(target) = target
            && candidate.namespace == format!("kars-{}", target.name)
        {
            let ns = Api::<Namespace>::all(client.clone())
                .get(&candidate.namespace)
                .await
                .map_err(|e| api_error("Recheck legacy runtime namespace", e))?;
            if identity(&ns.metadata)?.0 != candidate.namespace_uid {
                return Err("Legacy runtime namespace was replaced".into());
            }
            let annotations = ns.metadata.annotations.as_ref();
            if annotations.is_some_and(|a| a.contains_key("kars.azure.com/namespace-claim-version"))
            {
                let sandbox =
                    Api::<crate::crd::KarsSandbox>::namespaced(client.clone(), &target.namespace)
                        .get(&target.name)
                        .await
                        .map_err(|e| api_error("Verify legacy credential Sandbox owner", e))?;
                if !crate::reconciler::namespace_ownership::claimed(&ns, &sandbox)
                    .map_err(|_| "Legacy namespace claim is invalid")?
                    || target.kind == "KarsTeam"
                    || (target.kind == "KarsSandbox"
                        && sandbox.uid().as_deref() != Some(target.uid.as_str()))
                    || (target.kind == "KarsTask"
                        && sandbox
                            .metadata
                            .owner_references
                            .as_ref()
                            .is_none_or(|owners| {
                                !owners.iter().any(|o| {
                                    o.kind == "KarsTask"
                                        && o.uid == target.uid
                                        && o.controller == Some(true)
                                })
                            }))
                {
                    return Err("Legacy credential namespace belongs to another target; no import was authorized".into());
                }
            } else if annotations.is_some_and(|a| {
                a.keys()
                    .any(|key| key.starts_with("kars.azure.com/sandbox-"))
            }) {
                return Err(
                    "Partial legacy namespace ownership must be resolved before migration".into(),
                );
            }
        }
        let secret = Api::<Secret>::namespaced(client.clone(), &candidate.namespace)
            .get(&candidate.secret.name)
            .await
            .map_err(|e| api_error("Read reviewed legacy credentials", e))?;
        if identity(&secret.metadata)?
            != (
                candidate.secret.uid.as_str(),
                candidate.resource_version.as_str(),
            )
        {
            return Err("Legacy credentials changed after migration preflight".into());
        }
        for (key, value) in secret.data.unwrap_or_default() {
            if key == "TEAMS_ENABLED" && target.is_none() {
                continue;
            }
            if !allowed.contains(&key)
                || value.0.contains(&0)
                || std::str::from_utf8(&value.0).is_err()
            {
                return Err("Legacy credentials contain unapproved or reserved keys; values remain unchanged".into());
            }
            if values.insert(key, value).is_some() {
                return Err(
                    "Multiple legacy stores overlap; operator must resolve the ambiguous migration"
                        .into(),
                );
            }
        }
        revisions.push(format!(
            "{}:{}:{}",
            candidate.namespace, candidate.secret.uid, candidate.resource_version
        ));
    }
    Ok((values, revisions.join(",")))
}
