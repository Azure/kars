// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exclusive, opt-in agent credentials. Values only enter a UID/RV-fenced
//! projection after source, Sandbox, namespace, and target identity checks.

use crate::{crd::KarsSandbox, credential_source::*};
use k8s_openapi::{
    ByteString,
    api::{
        apps::v1::{Deployment, DeploymentStrategy},
        core::v1::{Namespace, Secret},
    },
    apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference},
};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams, PostParams},
    core::PartialObjectMeta,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[path = "credential_source_projection.rs"]
mod projection;
#[path = "credential_source_workloads.rs"]
mod workloads;

pub(super) fn refresh_interval(sandbox: &KarsSandbox) -> std::time::Duration {
    let governed = sandbox.spec.credentials_ref.is_some()
        || sandbox.spec.credential_bindings.is_some()
        || sandbox.spec.github_binding.is_some();
    std::time::Duration::from_secs(if governed { 30 } else { 300 })
}

pub(crate) async fn pause_owned(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), Error> {
    namespace_current(client, sandbox, namespace).await?;
    workloads::pause(client, sandbox, namespace, false).await
}

pub(crate) async fn quiescent_owned(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<bool, Error> {
    workloads::quiescent(client, sandbox, namespace).await
}

pub(crate) fn validate_owned_deployment(
    deployment: &Deployment,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), Error> {
    workloads::owned(&deployment.metadata, sandbox, namespace)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("CredentialSourceUnavailable: {0}")]
    Invalid(&'static str),
    #[error("CredentialSourceUnavailable: {stage} failed (Kubernetes status {code:?})")]
    Api {
        stage: &'static str,
        code: Option<u16>,
    },
}

fn api_error(stage: &'static str, error: kube::Error) -> Error {
    Error::Api {
        stage,
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

fn identity(meta: &ObjectMeta) -> Result<(&str, &str), Error> {
    match (meta.uid.as_deref(), meta.resource_version.as_deref()) {
        (Some(uid), Some(rv)) if !uid.is_empty() && !rv.is_empty() => Ok((uid, rv)),
        _ => Err(Error::Invalid("API object omitted UID/resourceVersion")),
    }
}

fn annotation<'a>(meta: &'a ObjectMeta, key: &str) -> Option<&'a str> {
    meta.annotations.as_ref()?.get(key).map(String::as_str)
}

async fn metadata(
    api: &Api<Secret>,
    name: &str,
) -> Result<Option<PartialObjectMeta<Secret>>, Error> {
    match api.get_metadata(name).await {
        Ok(value) => Ok(Some(value)),
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(None),
        Err(error) => Err(api_error("read Secret metadata", error)),
    }
}

async fn namespace_current(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
) -> Result<(), Error> {
    super::namespace_ownership::recheck(client, sandbox, ns)
        .await
        .map_err(|_| Error::Invalid("runtime namespace authority changed"))?;
    Ok(())
}

async fn sandbox_current(client: &Client, sandbox: &KarsSandbox) -> Result<(), Error> {
    let api: Api<KarsSandbox> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let live = api
        .get_metadata(&sandbox.name_any())
        .await
        .map_err(|e| api_error("recheck Sandbox", e))?;
    if identity(&live.metadata)? != identity(&sandbox.metadata)?
        || live.metadata.namespace != sandbox.metadata.namespace
        || live.metadata.name != sandbox.metadata.name
        || live.metadata.deletion_timestamp.is_some()
    {
        return Err(Error::Invalid("Sandbox identity/reference changed"));
    }
    Ok(())
}

fn source_owner(sandbox: &KarsSandbox) -> Result<OwnerReference, Error> {
    Ok(OwnerReference {
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsSandbox".into(),
        name: sandbox.name_any(),
        uid: identity(&sandbox.metadata)?.0.into(),
        controller: Some(true),
        block_owner_deletion: Some(false),
    })
}

fn source_valid(
    meta: &ObjectMeta,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    bound: bool,
) -> Result<(), Error> {
    let reference = sandbox
        .spec
        .credentials_ref
        .as_ref()
        .ok_or(Error::Invalid("reference absent"))?;
    if identity(meta)?.0 != reference.uid
        || meta.name.as_deref() != Some(reference.name.as_str())
        || meta.namespace != sandbox.metadata.namespace
        || meta.deletion_timestamp.is_some()
        || annotation(meta, PURPOSE) != Some(SOURCE_PURPOSE)
        || annotation(meta, TARGET) != sandbox.metadata.name.as_deref()
        || annotation(meta, WORKSPACE) != sandbox.metadata.namespace.as_deref()
        || annotation(meta, INTENT) != Some(BINDING_INTENT)
    {
        return Err(Error::Invalid(
            "source identity, purpose, target, or lifecycle is invalid",
        ));
    }
    let owner = source_owner(sandbox)?;
    let refs = meta.owner_references.as_deref().unwrap_or_default();
    if (!refs.is_empty() && refs != [owner.clone()]) || (bound && refs != [owner]) {
        return Err(Error::Invalid("source belongs to another owner"));
    }
    for (key, expected) in [
        (SANDBOX_UID, identity(&sandbox.metadata)?.0),
        (NAMESPACE_UID, identity(&ns.metadata)?.0),
    ] {
        match annotation(meta, key) {
            Some(value) if value == expected => {}
            None if !bound => {}
            _ => {
                return Err(Error::Invalid(
                    "source Sandbox/namespace UID binding is invalid",
                ));
            }
        }
    }
    Ok(())
}

fn validate_values(secret: &Secret) -> Result<(), Error> {
    if secret.type_.as_deref() != Some("Opaque")
        || secret
            .string_data
            .as_ref()
            .is_some_and(|data| !data.is_empty())
    {
        return Err(Error::Invalid("source must be a persisted Opaque Secret"));
    }
    let mut bytes = 0usize;
    for (key, value) in secret.data.iter().flatten() {
        if !AGENT_KEYS.contains(&key.as_str())
            || super::agent_env::RESERVED_PREFIXES
                .iter()
                .any(|prefix| key.starts_with(prefix))
            || value.0.contains(&0)
            || std::str::from_utf8(&value.0).is_err()
        {
            return Err(Error::Invalid(
                "source contains disallowed agent keys or invalid environment values",
            ));
        }
        bytes += value.0.len();
    }
    if bytes > 131_072 {
        return Err(Error::Invalid(
            "source credential collection exceeds 128 KiB",
        ));
    }
    Ok(())
}

async fn read_source(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    default_image: &str,
) -> Result<Secret, Error> {
    if sandbox.spec.credential_bindings.is_some()
        || sandbox
            .spec
            .credentials_ref
            .as_ref()
            .is_some_and(|r| r.name.starts_with(crate::credential_grant::BUNDLE_PREFIX))
    {
        let source = crate::credential_grants::sources::for_sandbox(client, sandbox)
            .await
            .map_err(|_| {
                Error::Invalid("governed credential source or operator grant is unavailable")
            })?;
        if sandbox
            .spec
            .upstream_compatibility
            .as_ref()
            .is_some_and(|value| value.is_overlay_mode())
        {
            return Err(Error::Invalid(
                "governed credentials require a controller-managed runtime",
            ));
        }
        let plan = super::runtime::build_runtime_plan(&sandbox.spec.runtime, default_image)
            .map_err(|_| Error::Invalid("governed credential runtime configuration is invalid"))?;
        let inputs: Value = annotation(&source.metadata, crate::credential_grant::INPUT_STATE)
            .and_then(|value| serde_json::from_str(value).ok())
            .ok_or(Error::Invalid("credential binding evidence missing"))?;
        let keys = inputs["bindings"]["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|selection| selection["keys"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if keys
            .iter()
            .any(|key| plan.runtime_extra_env.contains_key(*key))
            || plan.raw_env.iter().any(|entry| {
                entry["name"]
                    .as_str()
                    .is_some_and(|key| keys.contains(&key))
            })
        {
            return Err(Error::Invalid(
                "governed credentials conflict with runtime environment overrides",
            ));
        }
        return Ok(source);
    }
    let reference = sandbox
        .spec
        .credentials_ref
        .as_ref()
        .ok_or(Error::Invalid("reference absent"))?;
    if !valid_ref(reference, &sandbox.name_any()) {
        return Err(Error::Invalid(
            "reference must pin the reserved source name and exact UID",
        ));
    }
    if sandbox
        .spec
        .upstream_compatibility
        .as_ref()
        .is_some_and(|value| value.is_overlay_mode())
    {
        return Err(Error::Invalid(
            "credential sources require a controller-managed runtime",
        ));
    }
    let plan =
        super::runtime::build_runtime_plan(&sandbox.spec.runtime, default_image).map_err(|_| {
            Error::Invalid("credential source runtime configuration is invalid or unsupported")
        })?;
    if AGENT_KEYS
        .iter()
        .any(|key| plan.runtime_extra_env.contains_key(*key))
        || plan.raw_env.iter().any(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|key| AGENT_KEYS.contains(&key))
        })
    {
        return Err(Error::Invalid(
            "source mode cannot be combined with runtime credential env overrides",
        ));
    }
    let api: Api<Secret> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let meta = metadata(&api, &reference.name)
        .await?
        .ok_or(Error::Invalid("source Secret is missing"))?;
    source_valid(&meta.metadata, sandbox, ns, false)?;
    let mut source = api
        .get(&reference.name)
        .await
        .map_err(|e| api_error("read credential source", e))?;
    if identity(&source.metadata)? != identity(&meta.metadata)? {
        return Err(Error::Invalid("source changed during read"));
    }
    source_valid(&source.metadata, sandbox, ns, false)?;
    validate_values(&source)?;
    if source_valid(&source.metadata, sandbox, ns, true).is_err() {
        let (uid, rv) = identity(&source.metadata)?;
        let bound = api.patch_metadata(&reference.name, &PatchParams::default(), &Patch::Merge(json!({
            "metadata": {
                "uid": uid, "resourceVersion": rv,
                "ownerReferences": [source_owner(sandbox)?],
                "annotations": {SANDBOX_UID: sandbox.metadata.uid, NAMESPACE_UID: ns.metadata.uid}
            }
        }))).await.map_err(|e| api_error("bind credential source", e))?;
        source.metadata = bound.metadata;
        source_valid(&source.metadata, sandbox, ns, true)?;
    }
    Ok(source)
}

async fn inputs_current(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    source: &Secret,
) -> Result<(), Error> {
    sandbox_current(client, sandbox).await?;
    namespace_current(client, sandbox, ns).await?;
    if annotation(&source.metadata, PURPOSE) == Some(crate::credential_grant::BUNDLE_PURPOSE) {
        let current = crate::credential_grants::sources::for_sandbox(client, sandbox)
            .await
            .map_err(|_| Error::Invalid("governed credential inputs are no longer authorized"))?;
        if identity(&current.metadata)? != identity(&source.metadata)? {
            return Err(Error::Invalid(
                "governed credential bundle changed before projection write",
            ));
        }
        return Ok(());
    }
    let api: Api<Secret> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let live = metadata(&api, &source.name_any())
        .await?
        .ok_or(Error::Invalid("source disappeared"))?;
    source_valid(&live.metadata, sandbox, ns, true)?;
    if identity(&live.metadata)? != identity(&source.metadata)? {
        return Err(Error::Invalid("source changed before projection write"));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum Mode {
    Legacy,
    Source {
        uid: String,
        version: String,
        source_uid: String,
        source_version: String,
        source_keys: Vec<String>,
        source_inputs: Option<Value>,
    },
}

impl Mode {
    pub fn env_from(&self, sandbox: &str) -> Value {
        match self {
            Self::Legacy => {
                json!([{"secretRef": {"name": format!("{sandbox}-credentials"), "optional": true}}])
            }
            Self::Source { .. } => {
                json!([{"secretRef": {"name": projection_name(sandbox), "optional": false}}])
            }
        }
    }

    pub fn decorate(&self, deployment: &mut Deployment, sandbox: &KarsSandbox, ns: &Namespace) {
        if let Self::Source { uid, version, .. } = self {
            let annotations = deployment.metadata.annotations.get_or_insert_default();
            annotations.insert(
                SANDBOX_UID.into(),
                sandbox.metadata.uid.clone().unwrap_or_default(),
            );
            annotations.insert(
                NAMESPACE_UID.into(),
                ns.metadata.uid.clone().unwrap_or_default(),
            );
            let spec = deployment
                .spec
                .as_mut()
                .expect("controller builds a Deployment spec");
            spec.strategy = Some(DeploymentStrategy {
                type_: Some("Recreate".into()),
                rolling_update: None,
            });
            spec.template
                .metadata
                .get_or_insert_default()
                .annotations
                .get_or_insert_default()
                .insert(POD_VERSION.into(), format!("{uid}:{version}"));
        }
    }

    pub fn condition(
        &self,
        sandbox: &KarsSandbox,
    ) -> Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> {
        let prior = sandbox.status.as_ref().and_then(|status| {
            status
                .conditions
                .iter()
                .find(|condition| condition.type_ == "CredentialsReady")
        });
        let (status, reason, message) = match self {
            Self::Source {
                uid,
                version,
                source_uid,
                source_version,
                source_keys,
                source_inputs,
            } => (
                "True",
                if source_inputs.is_some() {
                    "GovernedProjected"
                } else {
                    "Projected"
                },
                json!({"sourceUid": source_uid, "sourceVersion": source_version,
                    "projectionUid": uid, "projectionVersion": version,
                    "configuredKeys":source_keys,"inputs":source_inputs})
                .to_string(),
            ),
            Self::Legacy if prior.is_some() => (
                "False",
                "DirectCredentials",
                "Explicit source is not configured".into(),
            ),
            Self::Legacy => return None,
        };
        Some(crate::status::conditions::preserve_transition_time(
            prior,
            "CredentialsReady",
            status,
            reason,
            &message,
            sandbox.metadata.generation,
        ))
    }

    pub fn needs_status_update(&self, sandbox: &KarsSandbox) -> bool {
        self.condition(sandbox).is_some_and(|expected| {
            !sandbox.status.as_ref().is_some_and(|status| {
                status.conditions.iter().any(|current| {
                    current.type_ == expected.type_
                        && current.status == expected.status
                        && current.reason == expected.reason
                        && current.message == expected.message
                })
            })
        })
    }
}

pub async fn reconcile(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: Option<&Namespace>,
    default_image: &str,
) -> Result<Mode, Error> {
    let ns = ns.ok_or(Error::Invalid("runtime namespace is not verified"))?;
    let configured =
        sandbox.spec.credentials_ref.is_some() || sandbox.spec.credential_bindings.is_some();
    let was_governed = sandbox.status.as_ref().is_some_and(|status| {
        status.conditions.iter().any(|condition| {
            condition.type_ == "CredentialsReady" && condition.reason == "GovernedProjected"
        })
    });
    let result = if configured {
        project(client, sandbox, ns, default_image).await
    } else if was_governed {
        Err(Error::Invalid(
            "governed credential bindings were removed; legacy values remain disabled",
        ))
    } else {
        projection::detach(client, sandbox, ns)
            .await
            .map(|()| Mode::Legacy)
    };
    if let Err(error) = result {
        // Try both operations: a transient Deployment error must not skip
        // projection revocation, or vice versa. Never echo API request bodies.
        let stopped = workloads::pause(client, sandbox, ns, !configured).await;
        let revoked = projection::revoke(client, sandbox, ns, false).await;
        let failure = stopped.err().or_else(|| revoked.err()).unwrap_or(error);
        report(client, sandbox, &failure).await?;
        return Err(failure);
    }
    result
}

async fn project(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    default_image: &str,
) -> Result<Mode, Error> {
    namespace_current(client, sandbox, ns).await?;
    let source = read_source(client, sandbox, ns, default_image).await?;
    let target = projection::anchor(client, sandbox, ns, &source).await?;
    let api: Api<Secret> = Api::namespaced(client.clone(), &ns.name_any());
    let current = api
        .get(&target.name_any())
        .await
        .map_err(|e| api_error("read owned projection", e))?;
    projection::validate(&current.metadata, sandbox, ns)?;
    if identity(&current.metadata)? != identity(&target.metadata)?
        || current.type_.as_deref() != Some("Opaque")
        || current.immutable == Some(true)
    {
        return Err(Error::Invalid("projection identity/type changed"));
    }
    let values: BTreeMap<String, ByteString> = source.data.clone().unwrap_or_default();
    let changed = current.data.clone().unwrap_or_default() != values
        || annotation(&current.metadata, SOURCE_UID) != source.metadata.uid.as_deref();
    let mut mode = Mode::Source {
        uid: identity(&current.metadata)?.0.into(),
        version: identity(&current.metadata)?.1.into(),
        source_uid: identity(&source.metadata)?.0.into(),
        source_version: identity(&source.metadata)?.1.into(),
        source_keys: source
            .data
            .iter()
            .flatten()
            .map(|(key, _)| key.clone())
            .collect(),
        source_inputs: annotation(&source.metadata, crate::credential_grant::INPUT_STATE)
            .and_then(|value| serde_json::from_str(value).ok()),
    };
    if changed || !workloads::current(client, sandbox, ns, &mode).await? {
        workloads::pause(client, sandbox, ns, false).await?;
    }
    inputs_current(client, sandbox, ns, &source).await?;
    if changed {
        let (uid, rv) = identity(&current.metadata)?;
        let mut data = serde_json::to_value(&values)
            .map_err(|_| Error::Invalid("credential serialization failed"))?;
        for key in current.data.iter().flat_map(|data| data.keys()) {
            if !values.contains_key(key) {
                data[key] = Value::Null;
            }
        }
        let written = api.patch_metadata(&target.name_any(), &PatchParams::default(), &Patch::Merge(json!({
            "metadata": {"uid": uid, "resourceVersion": rv, "annotations": {SOURCE_UID: source.metadata.uid}},
            "data": data
        }))).await.map_err(|e| api_error("write credential projection", e))?;
        projection::validate(&written.metadata, sandbox, ns)?;
        mode = Mode::Source {
            uid: identity(&written.metadata)?.0.into(),
            version: identity(&written.metadata)?.1.into(),
            source_uid: identity(&source.metadata)?.0.into(),
            source_version: identity(&source.metadata)?.1.into(),
            source_keys: source
                .data
                .iter()
                .flatten()
                .map(|(key, _)| key.clone())
                .collect(),
            source_inputs: annotation(&source.metadata, crate::credential_grant::INPUT_STATE)
                .and_then(|value| serde_json::from_str(value).ok()),
        };
    }
    Ok(mode)
}

async fn report(client: &Client, sandbox: &KarsSandbox, error: &Error) -> Result<(), Error> {
    let api: Api<KarsSandbox> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let live = api
        .get(&sandbox.name_any())
        .await
        .map_err(|e| api_error("read credential status target", e))?;
    if live.metadata.uid != sandbox.metadata.uid {
        return Err(Error::Invalid("Sandbox was recreated"));
    }
    let message = error.to_string();
    let mut patch =
        crate::status::build_degraded_status_patch(&live, "CredentialSourceUnavailable", &message);
    let existing = serde_json::to_value(&live.status)
        .map_err(|_| Error::Invalid("status serialization failed"))?;
    if patch["status"].as_object().is_some_and(|fields| {
        fields
            .iter()
            .all(|(key, val)| existing.get(key) == Some(val))
    }) {
        return Ok(());
    }
    let (uid, rv) = identity(&live.metadata)?;
    patch["metadata"] = json!({"uid": uid, "resourceVersion": rv});
    api.patch_status(
        &live.name_any(),
        &PatchParams::default(),
        &Patch::Merge(patch),
    )
    .await
    .map_err(|e| api_error("report credential failure", e))?;
    Ok(())
}

#[cfg(test)]
#[path = "credential_source_tests.rs"]
mod tests;
