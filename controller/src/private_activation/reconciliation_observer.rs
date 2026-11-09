// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Project only the exact controller-rendered, router-only observer additions.

use super::super::ERROR;
use crate::{crd::KarsSandbox, credential_grant::KarsCredentialGrant, service_observer};
use k8s_openapi::api::core::v1::Namespace;
use kube::ResourceExt;
use serde_json::{Value, json};

pub(super) const VERSION: &str = "kars.azure.com/services-observer-version";

fn remove(values: &mut Value, expected: Value, default_mode: bool) -> Result<(), String> {
    let values = values.as_array_mut().ok_or(ERROR)?;
    let indices: Vec<_> = values
        .iter()
        .enumerate()
        .filter(|(_, value)| value["name"] == expected["name"])
        .map(|(index, _)| index)
        .collect();
    if indices.len() != 1 {
        return Err(ERROR.into());
    }
    let mut actual = values[indices[0]].clone();
    // The API defaults SecretVolumeSource.defaultMode, unlike the renderer.
    if default_mode && actual["secret"]["defaultMode"] == 420 {
        actual["secret"]
            .as_object_mut()
            .ok_or(ERROR)?
            .remove("defaultMode");
    }
    if actual != expected {
        return Err(ERROR.into());
    }
    values.remove(indices[0]);
    Ok(())
}

pub(super) fn project(template: &mut Value) -> Result<Option<String>, String> {
    let Some(version) = template
        .pointer("/metadata/annotations")
        .and_then(|fields| fields.get(VERSION))
    else {
        return Ok(None);
    };
    let version = version
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or(ERROR)?
        .to_owned();
    let pod = template.get_mut("spec").ok_or(ERROR)?;
    for (name, secret, items) in [
        (
            "service-observations",
            service_observer::SECRET,
            json!([{"key":service_observer::TOKEN_KEY,"path":service_observer::TOKEN_KEY},
                {"key":"config.json","path":"config.json"}]),
        ),
        (
            "service-observation-identity",
            service_observer::TLS_SECRET,
            json!([{"key":"config.json","path":"config.json"}]),
        ),
    ] {
        remove(
            &mut pod["volumes"],
            json!({"name":name,"secret":{"secretName":secret,"items":items}}),
            true,
        )?;
    }
    let containers = pod["containers"].as_array_mut().ok_or(ERROR)?;
    let routers: Vec<_> = containers
        .iter()
        .enumerate()
        .filter(|(_, container)| container["name"] == "inference-router")
        .map(|(index, _)| index)
        .collect();
    if routers.len() != 1 {
        return Err(ERROR.into());
    }
    let router = &mut containers[routers[0]];
    remove(
        &mut router["env"],
        json!({"name":service_observer::VERSION_ENV,"value":version}),
        false,
    )?;
    for (name, path) in [
        ("service-observations", service_observer::DIRECTORY),
        (
            "service-observation-identity",
            service_observer::TLS_DIRECTORY,
        ),
    ] {
        remove(
            &mut router["volumeMounts"],
            json!({"name":name,"mountPath":path,"readOnly":true}),
            false,
        )?;
    }
    template["metadata"]["annotations"]
        .as_object_mut()
        .ok_or(ERROR)?
        .remove(VERSION);
    Ok(Some(version))
}

pub(super) fn verify_rendering(
    actual: &Value,
    desired: &Value,
    source: &KarsSandbox,
    fresh: &KarsSandbox,
    grant: &KarsCredentialGrant,
    namespace: &Namespace,
    deployment_uid: &str,
) -> Result<(), String> {
    let status = fresh
        .status
        .as_ref()
        .and_then(|s| s.service_observation.as_ref());
    if serde_json::to_value(
        source
            .status
            .as_ref()
            .and_then(|s| s.service_observation.as_ref()),
    )
    .map_err(|_| ERROR)?
        != serde_json::to_value(status).map_err(|_| ERROR)?
    {
        return Err("Private observer issuance changed during rendering".into());
    }
    let issued = status.filter(|s| {
        matches!(s.phase.as_str(), "Ready" | "Prepared")
            && s.capability == service_observer::CAPABILITY
    });
    let expected = issued.map(|s| s.version.clone());
    if project(&mut actual.clone())? != expected || project(&mut desired.clone())? != expected {
        return Err(
            "Private observer rollout differs from the current controller-issued capability".into(),
        );
    }
    if let Some(status) = issued
        && (grant.metadata.deletion_timestamp.is_some()
            || Some(&status.grant.uid) != grant.metadata.uid.as_ref()
            || status.grant.name != grant.name_any()
            || status.namespace_uid != namespace.uid().ok_or(ERROR)?
            || status.deployment_uid.as_deref() != Some(deployment_uid)
            || status.privacy_revision != crate::sre_privacy::REVISION
            || status.secret.name != service_observer::SECRET
            || status.secret.uid.is_empty()
            || status
                .version
                .split_once(':')
                .is_none_or(|(uid, version)| uid != status.secret.uid || version.is_empty())
            || !grant.spec.observation_targets.iter().any(|target| {
                target.kind == "KarsSandbox"
                    && Some(&target.namespace) == fresh.metadata.namespace.as_ref()
                    && target.name == fresh.name_any()
                    && Some(&target.uid) == fresh.metadata.uid.as_ref()
            }))
    {
        return Err("Private observer issuance no longer matches its target authority".into());
    }
    Ok(())
}
