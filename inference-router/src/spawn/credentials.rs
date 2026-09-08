// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Existing handoff credential propagation, authorized only by namespace claim
//! v1. Keep these keys aligned with controller/reconciler/namespace_ownership.rs
//! and cli/src/lib/namespace-ownership.ts. Labels/prestages never grant access.

use k8s_openapi::{
    api::core::v1::{Namespace, Secret},
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    Api, Client,
    api::{DynamicObject, Patch, PatchParams},
};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};

const VERSION: &str = "kars.azure.com/namespace-claim-version";
const SOURCE_NAMESPACE: &str = "kars.azure.com/sandbox-namespace";
const SOURCE_NAME: &str = "kars.azure.com/sandbox-name";
const SOURCE_UID: &str = "kars.azure.com/sandbox-uid";
const NAMESPACE_UID: &str = "kars.azure.com/namespace-uid";
const PRESTAGE: &str = "kars.azure.com/namespace-prestage";

const CREDENTIAL_ENV_VARS: &[&str] = &[
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_ALLOW_FROM",
    "SLACK_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "WHATSAPP_ENABLED",
    "BRAVE_API_KEY",
    "TAVILY_API_KEY",
    "EXA_API_KEY",
    "FIRECRAWL_API_KEY",
    "PERPLEXITY_API_KEY",
];

pub(super) struct Target {
    workspace: String,
    name: String,
    sandbox_uid: String,
    namespace_uid: Option<String>,
}

impl Target {
    pub(super) fn from_created(
        created: &DynamicObject,
        workspace: &str,
        name: &str,
    ) -> Result<Self, String> {
        let meta = &created.metadata;
        let actual_workspace = meta
            .namespace
            .as_deref()
            .ok_or("Created Sandbox has no workspace")?;
        let actual_name = meta.name.as_deref().ok_or("Created Sandbox has no name")?;
        if actual_workspace != workspace
            || actual_name != name
            || !valid_label(workspace)
            || !valid_label(name)
            || meta.deletion_timestamp.is_some()
        {
            return Err(
                "Created Sandbox has an invalid or changed workspace/name/lifecycle".into(),
            );
        }
        let sandbox_uid = meta
            .uid
            .as_ref()
            .filter(|uid| !uid.is_empty())
            .ok_or("Created Sandbox has no API-server UID")?
            .clone();
        let namespace_uid = annotation(meta, NAMESPACE_UID).map(str::to_string);
        if namespace_uid.as_deref() == Some("") {
            return Err("Created Sandbox has an empty namespace UID backlink".into());
        }
        Ok(Self {
            workspace: actual_workspace.into(),
            name: actual_name.into(),
            sandbox_uid,
            namespace_uid,
        })
    }

    fn namespace(&self) -> String {
        format!("kars-{}", self.name)
    }
}

fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}

fn annotation<'a>(meta: &'a ObjectMeta, name: &str) -> Option<&'a str> {
    meta.annotations.as_ref()?.get(name).map(String::as_str)
}

fn identity(meta: &ObjectMeta) -> Result<(&str, &str), String> {
    match (meta.uid.as_deref(), meta.resource_version.as_deref()) {
        (Some(uid), Some(version)) if !uid.is_empty() && !version.is_empty() => Ok((uid, version)),
        _ => Err("Ownership evidence omitted its API-server UID/resourceVersion".into()),
    }
}

fn api_error(stage: &str, error: kube::Error) -> String {
    // Admission errors may echo stringData. Never log API bodies or values.
    match error {
        kube::Error::Api(status) => format!("{stage}: Kubernetes API status {}", status.code),
        _ => format!("{stage}: Kubernetes client/transport failure"),
    }
}

fn validate_sandbox(target: &Target, live: &ObjectMeta) -> Result<(), String> {
    identity(live)?;
    if live.namespace.as_deref() != Some(target.workspace.as_str())
        || live.name.as_deref() != Some(target.name.as_str())
        || live.uid.as_deref() != Some(target.sandbox_uid.as_str())
        || live.deletion_timestamp.is_some()
    {
        return Err("Created Sandbox was replaced, moved, or is terminating".into());
    }
    if let Some(expected) = target.namespace_uid.as_deref()
        && annotation(live, NAMESPACE_UID) != Some(expected)
    {
        return Err("Created Sandbox namespace UID backlink changed".into());
    }
    Ok(())
}

fn claim_ready(
    target: &Target,
    sandbox: &ObjectMeta,
    namespace: &Namespace,
    pinned_namespace_uid: &mut Option<String>,
) -> Result<bool, String> {
    let meta = &namespace.metadata;
    let (uid, _) = identity(meta)?;
    if meta.name.as_deref() != Some(target.namespace().as_str())
        || meta.deletion_timestamp.is_some()
        || namespace
            .status
            .as_ref()
            .and_then(|status| status.phase.as_deref())
            == Some("Terminating")
        || meta
            .owner_references
            .as_ref()
            .is_some_and(|owners| !owners.is_empty())
    {
        return Err("Target namespace name/ownership/lifecycle is invalid".into());
    }
    if pinned_namespace_uid
        .as_deref()
        .is_some_and(|expected| expected != uid)
    {
        return Err("Target namespace was recreated during credential propagation".into());
    }
    *pinned_namespace_uid = Some(uid.to_string());
    let mut complete = true;
    for (key, expected) in [
        (VERSION, "v1"),
        (SOURCE_NAMESPACE, target.workspace.as_str()),
        (SOURCE_NAME, target.name.as_str()),
        (SOURCE_UID, target.sandbox_uid.as_str()),
    ] {
        match annotation(meta, key) {
            Some(value) if value == expected => {}
            Some(_) => {
                return Err(
                    "Target namespace claim belongs to another Sandbox UID/workspace".into(),
                );
            }
            None => complete = false,
        }
    }
    if let Some(prestage) = annotation(meta, PRESTAGE) {
        if prestage != "bind-next-sandbox" {
            return Err("Target namespace has an invalid prestage marker".into());
        }
        complete = false;
    }
    match annotation(sandbox, NAMESPACE_UID) {
        Some(backlink) if backlink == uid => {}
        Some(_) => {
            return Err("Sandbox namespace UID backlink does not match the live namespace".into());
        }
        None => complete = false,
    }
    Ok(complete)
}

async fn probe(
    client: &Client,
    target: &Target,
    namespace_uid: &mut Option<String>,
) -> Result<bool, String> {
    let sandboxes: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        &target.workspace,
        &super::kars_sandbox_api_resource(),
    );
    let live = sandboxes
        .get_metadata(&target.name)
        .await
        .map_err(|error| api_error("Read created Sandbox", error))?;
    validate_sandbox(target, &live.metadata)?;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    match namespaces
        .get_opt(&target.namespace())
        .await
        .map_err(|error| api_error("Read target namespace", error))?
    {
        Some(namespace) => claim_ready(target, &live.metadata, &namespace, namespace_uid),
        None if namespace_uid.is_none() && annotation(&live.metadata, NAMESPACE_UID).is_none() => {
            Ok(false)
        }
        None => Err("Previously observed/bound target namespace is missing".into()),
    }
}

#[derive(Clone, Copy)]
struct Wait {
    attempts: usize,
    delay: Duration,
}

async fn write_credentials(
    client: &Client,
    target: &Target,
    credentials: &BTreeMap<String, String>,
    predecessor: &str,
    wait: Wait,
) -> Result<(), String> {
    if credentials.is_empty() {
        return Ok(());
    }
    let mut namespace_uid = target.namespace_uid.clone();
    let mut ready = false;
    for attempt in 0..wait.attempts {
        if probe(client, target, &mut namespace_uid).await? {
            ready = true;
            break;
        }
        if attempt + 1 < wait.attempts {
            tokio::time::sleep(wait.delay).await;
        }
    }
    if !ready {
        return Err(
            "Namespace claim/backlink did not converge; no credentials were written".into(),
        );
    }
    let namespace = target.namespace();
    let secret_name = format!("{}-credentials", target.name);
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace);

    // Reserve only metadata, with a different manager from the historical
    // credential writer so SSA cannot prune its existing data. No values cross
    // the namespace boundary until its incarnation is rechecked below.
    let anchor = secrets
        .patch_metadata(
            &secret_name,
            &PatchParams::apply("kars-handoff-credential-anchor"),
            &Patch::Apply(json!({
                "apiVersion": "v1", "kind": "Secret",
                "metadata": { "name": secret_name, "namespace": namespace },
            })),
        )
        .await
        .map_err(|error| api_error("Reserve credential metadata", error))?;
    let (secret_uid, secret_version) = identity(&anchor.metadata)?;
    if anchor.metadata.name.as_deref() != Some(secret_name.as_str())
        || anchor.metadata.namespace.as_deref() != Some(namespace.as_str())
        || anchor.metadata.deletion_timestamp.is_some()
    {
        return Err("Credential metadata target is invalid or terminating".into());
    }
    if !probe(client, target, &mut namespace_uid).await? {
        return Err("Namespace claim/backlink disappeared before the credential write".into());
    }

    // A merge patch cannot create an absent Secret. UID/resourceVersion fence
    // the existing object, so deleting/recreating the namespace or Secret after
    // revalidation cannot redirect credential values into a new incarnation.
    // Metadata-only responses prevent reading existing credential values.
    secrets
        .patch_metadata(
            &secret_name,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {
                    "uid": secret_uid, "resourceVersion": secret_version,
                    "labels": {
                        "kars.azure.com/managed-by": "handoff",
                        "kars.azure.com/predecessor": predecessor,
                    },
                },
                "type": "Opaque", "stringData": credentials,
            })),
        )
        .await
        .map_err(|error| api_error("Write handoff credentials", error))?;
    Ok(())
}

pub(super) async fn propagate(client: &Client, target: &Target) -> Result<(), String> {
    let credentials: BTreeMap<String, String> = CREDENTIAL_ENV_VARS
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| ((*name).into(), value))
        })
        .collect();
    if credentials.is_empty() {
        tracing::info!(child = %target.name, "No channel/plugin credentials to propagate");
        return Ok(());
    }
    write_credentials(
        client,
        target,
        &credentials,
        &std::env::var("SANDBOX_NAME").unwrap_or_default(),
        Wait {
            attempts: 15,
            delay: Duration::from_secs(2),
        },
    )
    .await?;
    tracing::info!(child = %target.name, workspace = %target.workspace,
        credential_count = credentials.len(), "Propagated credentials to the claimed handoff namespace");
    Ok(())
}

#[cfg(test)]
mod test_server;
#[cfg(test)]
mod tests;
