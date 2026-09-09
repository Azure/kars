// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native RBAC subjects are name-bound. Hold enrolled object names until every
//! owned read Role is gone, including deletion of the add-on namespace.

use super::*;
use k8s_openapi::api::authentication::v1::SelfSubjectReview;
use kube::api::PostParams;

mod guards;
mod permissions;
#[cfg(test)]
mod tests;

const PREFIX: &str = "kars.azure.com/credential-reader-";

fn key(grant: &KarsCredentialGrant) -> Result<String, String> {
    Ok(format!(
        "{PREFIX}{}",
        grant.uid().ok_or("Grant UID missing")?
    ))
}

fn protected(meta: &kube::api::ObjectMeta, key: &str, namespace_uid: &str) -> bool {
    meta.finalizers
        .as_ref()
        .is_some_and(|v| v.iter().any(|v| v == key))
        && meta
            .annotations
            .as_ref()
            .and_then(|a| a.get(key))
            .is_some_and(|v| !v.is_empty())
        && meta
            .labels
            .as_ref()
            .and_then(|a| a.get(key))
            .map(String::as_str)
            == Some(namespace_uid)
}

fn namespace_held(namespace: &Namespace) -> bool {
    namespace
        .spec
        .as_ref()
        .and_then(|spec| spec.finalizers.as_ref())
        .is_some_and(|finalizers| finalizers.iter().any(|entry| entry == "kubernetes"))
}

pub(super) async fn controller_uid(client: &Client) -> Result<String, String> {
    Ok(controller_subject(client).await?.1)
}

async fn controller_subject(client: &Client) -> Result<(String, String), String> {
    if matches!(
        std::env::var("LEADER_ELECTION_ENABLED")
            .unwrap_or_else(|_| "true".into())
            .to_ascii_lowercase()
            .as_str(),
        "false" | "0" | "no" | "off"
    ) {
        return Err("Governed writer authority requires the controller leadership barrier".into());
    }
    let caller = Api::<SelfSubjectReview>::all(client.clone())
        .create(&PostParams::default(), &SelfSubjectReview::default())
        .await
        .map_err(|e| api_error("Verify credential guard controller identity", e))?;
    let caller = serde_json::to_value(caller).map_err(|_| "Controller identity is invalid")?;
    let user = &caller["status"]["userInfo"];
    if !user["username"].as_str().is_some_and(|name| {
        name.starts_with("system:serviceaccount:") && name.ends_with(":kars-controller")
    }) {
        return Err("Credential guard requires the installed controller ServiceAccount".into());
    }
    let uid = user["uid"]
        .as_str()
        .filter(|uid| !uid.is_empty())
        .ok_or("Controller UID missing")?;
    Ok((
        user["username"]
            .as_str()
            .ok_or("Controller username missing")?
            .into(),
        uid.into(),
    ))
}

pub(super) async fn verify(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    let key = key(grant)?;
    let controller = if grant.spec.writers.is_empty() {
        None
    } else {
        Some(controller_uid(client).await?)
    };
    for writer in &grant.spec.writers {
        let ns = Api::<Namespace>::all(client.clone())
            .get(&writer.namespace)
            .await
            .map_err(|e| api_error("Recheck guarded writer namespace", e))?;
        let account = Api::<ServiceAccount>::namespaced(client.clone(), &writer.namespace)
            .get(&writer.name)
            .await
            .map_err(|e| api_error("Recheck guarded writer identity", e))?;
        let uid = identity(&ns.metadata)?.0;
        if identity(&account.metadata)?.0 != writer.uid
            || !namespace_held(&ns)
            || !protected(&account.metadata, &key, uid)
            || !protected(&ns.metadata, &key, uid)
            || account
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(&key))
                != controller.as_ref()
            || ns.metadata.annotations.as_ref().and_then(|a| a.get(&key)) != controller.as_ref()
        {
            return Err("Writer identity lacks an enforced name-continuity guard".into());
        }
    }
    Ok(())
}

pub(super) async fn reconcile(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<KarsCredentialGrant, String> {
    let key = key(grant)?;
    let mut active = grant.clone();
    active.spec.writers.clear();
    let mut unguarded = Vec::new();
    for writer in &grant.spec.writers {
        let ns = Api::<Namespace>::all(client.clone())
            .get_opt(&writer.namespace)
            .await
            .map_err(|e| api_error("Inspect enrolled writer namespace", e))?;
        let account = Api::<ServiceAccount>::namespaced(client.clone(), &writer.namespace)
            .get_opt(&writer.name)
            .await
            .map_err(|e| api_error("Inspect enrolled writer identity", e))?;
        let (Some(ns), Some(account)) = (ns, account) else {
            continue;
        };
        let Ok((namespace_uid, _)) = identity(&ns.metadata) else {
            continue;
        };
        if identity(&account.metadata).map(|(uid, _)| uid) != Ok(writer.uid.as_str()) {
            continue;
        }
        if !protected(&account.metadata, &key, namespace_uid)
            || !protected(&ns.metadata, &key, namespace_uid)
        {
            unguarded.push((ns, account));
        }
        active.spec.writers.push(writer.clone());
    }
    let stale = guards::stale(client, &active, &key).await?;
    if !unguarded.is_empty() || stale || guards::stale_readers(client, &active).await? {
        // DELETE success alone is not proof: finalizers may retain the Role.
        // release() performs uncached absence checks before releasing any name.
        super::rbac::revoke(client, grant).await?;
        super::observer_rbac::revoke(client, grant).await?;
        guards::release_stale(client, &active, &key).await?;
    }
    if !unguarded.is_empty() {
        let controller = controller_uid(client).await?;
        for (namespace, account) in unguarded {
            guards::protect(client, &namespace, &account, &key, &controller).await?;
        }
    }
    verify(client, &active).await?;
    permissions::verify(client, &active).await?;
    Ok(active)
}

pub(super) async fn release(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    let mut retired = grant.clone();
    retired.spec.writers.clear();
    guards::release_stale(client, &retired, &key(grant)?).await
}

pub(super) async fn authority(
    client: &Client,
    grant: &KarsCredentialGrant,
    sources: &[SourceMetadata],
) -> (KarsCredentialGrant, Option<String>) {
    let result = async {
        let active = reconcile(client, grant).await?;
        if !active.spec.writers.is_empty() {
            super::rbac::apply(client, &active, sources).await?;
        }
        Ok::<_, String>(active)
    }
    .await;
    match result {
        Ok(active) => {
            let unavailable = (active.spec.writers.len() != grant.spec.writers.len() || active.spec.writers.is_empty())
                .then(|| "Writer identity is absent, terminating or replaced; valid source delivery is retained".into());
            (active, unavailable)
        }
        Err(mut error) => {
            for revoked in [
                super::rbac::revoke(client, grant).await,
                super::operator::revoke(client, grant).await,
            ] {
                if let Err(revoke) = revoked {
                    error.push_str(&format!("; read authority revocation pending: {revoke}"));
                }
            }
            if let Err(held) = release(client, grant).await {
                error.push_str(&format!("; enrolled name holds retained: {held}"));
            }
            let mut active = grant.clone();
            active.spec.writers.clear();
            (active, Some(error))
        }
    }
}
