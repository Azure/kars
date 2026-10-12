// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Retire the pre-AL3 blocklist CronJob. Feed refresh belongs to the router.
//! Keep this migration until upgrades from controllers that created it end.

use super::{ReconcileError, namespace_ownership};
use crate::{crd::KarsSandbox, field_managers::CLAWSANDBOX};
use k8s_openapi::api::{batch::v1::CronJob, core::v1::Namespace};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, Preconditions, PropagationPolicy},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const LEGACY_SCRIPT_SHA256: &str =
    "5abe4ae24af4e3bf58a054271690e50140b76f446a112745df8370511d7f65f4";

fn conflict(name: &str) -> ReconcileError {
    ReconcileError::Configuration(format!(
        "Blocklist refresh migration cannot prove ownership of {name}; leave it unchanged and review docs/egress-proxy.md#legacy-cronjob-upgrades"
    ))
}

fn legacy_pod_shape(pod: &Value) -> bool {
    let mut normalized = pod.clone();
    // Ignore only known API-server defaults, not added workload configuration.
    let strip_defaults = |value: &mut Value, defaults: Value| {
        if let (Some(actual), Some(defaults)) = (value.as_object_mut(), defaults.as_object()) {
            for (key, default) in defaults {
                if actual.get(key) == Some(default) {
                    actual.remove(key);
                }
            }
        }
    };
    strip_defaults(
        &mut normalized,
        json!({
            "dnsPolicy": "ClusterFirst", "schedulerName": "default-scheduler",
            "terminationGracePeriodSeconds": 30, "securityContext": {},
            "enableServiceLinks": true, "preemptionPolicy": "PreemptLowerPriority",
            "serviceAccount": "sandbox", "initContainers": [],
            "ephemeralContainers": [], "volumes": []
        }),
    );
    let Some(container) = normalized
        .get_mut("containers")
        .and_then(Value::as_array_mut)
        .filter(|containers| containers.len() == 1)
        .map(|containers| &mut containers[0])
    else {
        return false;
    };
    strip_defaults(
        container,
        json!({
            "imagePullPolicy": "IfNotPresent",
            "terminationMessagePath": "/dev/termination-log",
            "terminationMessagePolicy": "File", "volumeMounts": []
        }),
    );
    let Some(read_only) = container["securityContext"]["readOnlyRootFilesystem"].as_bool() else {
        return false;
    };
    let mut expected = json!({
        "serviceAccountName": "sandbox", "restartPolicy": "OnFailure",
        "containers": [{
            "name": "refresh", "image": "mcr.microsoft.com/cbl-mariner/base/core:2.0",
            "command": container["command"], "args": container["args"],
            "resources": {
                "requests": {"cpu": "50m", "memory": "64Mi"},
                "limits": {"cpu": "200m", "memory": "256Mi"}
            },
            "securityContext": {
                "runAsNonRoot": true, "runAsUser": 65534,
                "readOnlyRootFilesystem": read_only, "allowPrivilegeEscalation": false
            }
        }]
    });
    if read_only {
        expected["volumes"] = json!([{"name": "scratch", "emptyDir": {"sizeLimit": "128Mi"}}]);
        expected["containers"][0]["volumeMounts"] =
            json!([{"name": "scratch", "mountPath": "/tmp"}]);
    }
    normalized == expected
}

fn deletion_params(
    job: &CronJob,
    sandbox_name: &str,
    namespace: &str,
) -> Result<DeleteParams, ReconcileError> {
    let name = format!("{sandbox_name}-blocklist-refresh");
    let value = serde_json::to_value(job)?;
    let label = |key: &str| {
        job.metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(key))
    };
    let expected_args = format!(
        "{sandbox_name}-blocklist --from-file=domains.txt=/tmp/domains.txt -n {namespace} --dry-run=client -o yaml | kubectl apply -f -"
    );
    let pod = &value["spec"]["jobTemplate"]["spec"]["template"]["spec"];
    let containers = pod["containers"].as_array();
    let container = &pod["containers"][0];
    let command = container["command"].as_array();
    let args = container["args"].as_array();
    let managed = job.metadata.managed_fields.as_ref().is_some_and(|fields| {
        fields.iter().any(|entry| {
            entry.manager.as_deref() == Some(CLAWSANDBOX)
                && entry.operation.as_deref() == Some("Apply")
                && entry.subresource.as_deref().is_none_or(str::is_empty)
                && entry.fields_v1.as_ref().is_some_and(|fields| {
                    let fields = &fields.0;
                    let labels = &fields["f:metadata"]["f:labels"];
                    let container = &fields["f:spec"]["f:jobTemplate"]["f:spec"]
                        ["f:template"]["f:spec"]["f:containers"]["k:{\"name\":\"refresh\"}"];
                    labels.get("f:kars.azure.com/sandbox").is_some()
                        && labels.get("f:kars.azure.com/component").is_some()
                        && ["f:image", "f:command", "f:args"]
                            .iter()
                            .all(|&key| container.get(key).is_some())
                })
        })
    });
    let identity = job
        .metadata
        .uid
        .as_deref()
        .zip(job.metadata.resource_version.as_deref())
        .filter(|(uid, rv)| !uid.is_empty() && !rv.is_empty());
    if job.metadata.name.as_deref() != Some(&name)
        || job.metadata.namespace.as_deref() != Some(namespace)
        || job
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|owners| !owners.is_empty())
        || label("kars.azure.com/sandbox").map(String::as_str) != Some(sandbox_name)
        || label("kars.azure.com/component").map(String::as_str) != Some("blocklist-refresh")
        || !managed
        || identity.is_none()
        || !containers.is_some_and(|items| items.len() == 1)
        || pod["initContainers"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
        || pod["ephemeralContainers"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
        || pod["serviceAccountName"] != "sandbox"
        || container["name"] != "refresh"
        || container["image"] != "mcr.microsoft.com/cbl-mariner/base/core:2.0"
        || !command.is_some_and(|items| {
            items.len() == 3
                && items[0] == "sh"
                && items[1] == "-c"
                && items[2].as_str().is_some_and(|script| {
                    hex::encode(Sha256::digest(script.as_bytes())) == LEGACY_SCRIPT_SHA256
                })
        })
        || !args.is_some_and(|items| items.len() == 1 && items[0] == expected_args)
        || !legacy_pod_shape(pod)
        || [
            &value["spec"]["jobTemplate"]["metadata"],
            &value["spec"]["jobTemplate"]["spec"]["template"]["metadata"],
        ]
        .iter()
        .any(|metadata| {
            metadata
                .as_object()
                .is_some_and(|fields| !fields.is_empty())
        })
    {
        return Err(conflict(&name));
    }
    let (uid, rv) = identity.ok_or_else(|| conflict(&name))?;
    Ok(DeleteParams {
        propagation_policy: Some(PropagationPolicy::Foreground),
        preconditions: Some(Preconditions {
            uid: Some(uid.into()),
            resource_version: Some(rv.into()),
        }),
        ..Default::default()
    })
}

pub(super) async fn retire(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), ReconcileError> {
    namespace_ownership::recheck(client, sandbox, namespace).await?;
    let name = format!("{}-blocklist-refresh", sandbox.name_any());
    let jobs: Api<CronJob> = Api::namespaced(client.clone(), &namespace.name_any());
    let Some(job) = jobs.get_opt(&name).await? else {
        return Ok(());
    };
    let params = deletion_params(&job, &sandbox.name_any(), &namespace.name_any())?;
    namespace_ownership::recheck(client, sandbox, namespace).await?;
    match jobs.delete(&name, &params).await {
        Ok(_) => {
            tracing::info!(cronjob = %name, "Retiring legacy blocklist refresh job; feed refresh is router-owned")
        }
        Err(kube::Error::Api(error)) if error.code == 404 => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
#[path = "legacy_blocklist_tests.rs"]
mod tests;
