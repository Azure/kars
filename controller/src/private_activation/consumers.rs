// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Complete Pod inventory and private-material consumption classification.

use super::{EPOCH, ERROR, PREFIX, bundle, field, hash, live};
use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::{Api, Client, ResourceExt, api::ListParams};
use serde_json::{Value, json};

#[cfg(test)]
pub(crate) fn private_material(pod: &Pod) -> bool {
    private_material_in(pod, None)
}

fn private_material_in(pod: &Pod, namespace: Option<&Namespace>) -> bool {
    let value = serde_json::to_value(pod).expect("Pod serializes");
    let spec = &value["spec"];
    let definition = bundle();
    let extra = namespace.and_then(|namespace| {
        (field(namespace, "budget-namespace").ok().as_deref()
            == Some(namespace.name_any().as_str()))
        .then(|| field(namespace, "budget-tls-name").ok())
        .flatten()
    });
    let protected = |value: &Value| {
        definition["secrets"]
            .as_array()
            .is_some_and(|names| value.is_string() && names.contains(value))
            || extra
                .as_deref()
                .is_some_and(|name| value.as_str() == Some(name))
    };
    if spec["volumes"].as_array().is_some_and(|volumes| {
        volumes.iter().any(|volume| {
            protected(&volume["secret"]["secretName"])
                || protected(&volume["csi"]["nodePublishSecretRef"]["name"])
                || volume["projected"]["sources"]
                    .as_array()
                    .is_some_and(|sources| {
                        sources.iter().any(|source| {
                            protected(&source["secret"]["name"])
                                || definition["tokenAudiences"].as_array().is_some_and(
                                    |audiences| {
                                        source["serviceAccountToken"]["audience"].is_string()
                                            && audiences.contains(
                                                &source["serviceAccountToken"]["audience"],
                                            )
                                    },
                                )
                        })
                    })
                || [
                    "azureFile",
                    "cephfs",
                    "cinder",
                    "flexVolume",
                    "iscsi",
                    "rbd",
                    "scaleIO",
                    "storageos",
                ]
                .iter()
                .any(|kind| {
                    protected(&volume[*kind]["secretName"])
                        || protected(&volume[*kind]["secretRef"]["name"])
                })
        })
    }) || spec["imagePullSecrets"]
        .as_array()
        .is_some_and(|values| values.iter().any(|value| protected(&value["name"])))
    {
        return true;
    }
    ["containers", "initContainers", "ephemeralContainers"]
        .iter()
        .any(|kind| {
            spec[*kind].as_array().is_some_and(|containers| {
                containers.iter().any(|container| {
                    container["envFrom"].as_array().is_some_and(|values| {
                        values
                            .iter()
                            .any(|value| protected(&value["secretRef"]["name"]))
                    }) || container["env"].as_array().is_some_and(|values| {
                        values
                            .iter()
                            .any(|value| protected(&value["valueFrom"]["secretKeyRef"]["name"]))
                    })
                })
            })
        })
}

pub(crate) async fn retired_material_consumers(
    client: &Client,
    namespace: &str,
) -> Result<bool, String> {
    let scope = Api::<Namespace>::all(client.clone())
        .get(namespace)
        .await
        .map_err(|_| ERROR)?;
    let pods = Api::<Pod>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await
        .map_err(|_| ERROR)?;
    if pods
        .metadata
        .continue_
        .as_ref()
        .is_some_and(|v| !v.is_empty())
        || pods.items.iter().any(|pod| {
            pod.spec.is_none()
                || pod.metadata.uid.as_deref().is_none_or(str::is_empty)
                || pod
                    .metadata
                    .resource_version
                    .as_deref()
                    .is_none_or(str::is_empty)
        })
    {
        return Err(ERROR.into());
    }

    Ok(!pods
        .items
        .iter()
        .any(|pod| private_material_in(pod, Some(&scope))))
}

pub(crate) async fn inspect_namespace(
    client: &Client,
    namespace: &Namespace,
    epoch: &str,
) -> Result<(), String> {
    let pods = Api::<Pod>::namespaced(client.clone(), &namespace.name_any())
        .list(&ListParams::default())
        .await
        .map_err(|_| ERROR)?;
    if pods
        .metadata
        .continue_
        .as_ref()
        .is_some_and(|v| !v.is_empty())
    {
        return Err(ERROR.into());
    }
    let annotations = namespace.metadata.annotations.as_ref().ok_or(ERROR)?;
    for pod in pods {
        let uid = pod
            .metadata
            .uid
            .as_deref()
            .filter(|v| !v.is_empty())
            .ok_or(ERROR)?;
        let spec = pod.spec.as_ref().ok_or(ERROR)?;
        let raw = serde_json::to_value(spec).map_err(|_| ERROR)?;
        let sa = spec.service_account_name.as_deref().unwrap_or("default");
        let private_identity = (namespace.name_any() == field(namespace, "root-namespace")?
            && sa == field(namespace, "root-account")?)
            || (namespace.name_any() == "kars-sre" && sa == "sre-api-router")
            || (namespace.name_any() == "kube-system"
                && bundle()["controllers"]
                    .as_array()
                    .is_some_and(|names| names.contains(&json!(sa))));
        let projected_token = raw["volumes"].as_array().is_some_and(|volumes| {
            volumes.iter().any(|volume| {
                volume["projected"]["sources"]
                    .as_array()
                    .is_some_and(|sources| {
                        sources
                            .iter()
                            .any(|source| source.get("serviceAccountToken").is_some())
                    })
            })
        });
        let dangerous = ["hostPID", "hostIPC", "hostNetwork"]
            .iter()
            .any(|key| raw[*key] == true)
            || raw["volumes"].as_array().is_some_and(|volumes| {
                volumes
                    .iter()
                    .any(|volume| volume.get("hostPath").is_some())
            })
            || ["containers", "initContainers", "ephemeralContainers"]
                .iter()
                .any(|key| {
                    raw[*key].as_array().is_some_and(|containers| {
                        containers.iter().any(|container| {
                            container["securityContext"]["privileged"] == true
                                || container["securityContext"]["capabilities"]["add"]
                                    .as_array()
                                    .is_some_and(|caps| {
                                        caps.iter().any(|cap| {
                                            [
                                                "ALL",
                                                "SYS_ADMIN",
                                                "SYS_PTRACE",
                                                "SYS_MODULE",
                                                "SYS_RAWIO",
                                                "BPF",
                                                "PERFMON",
                                                "CHECKPOINT_RESTORE",
                                                "DAC_READ_SEARCH",
                                            ]
                                            .iter()
                                            .any(|name| cap.as_str() == Some(*name))
                                        })
                                    })
                        })
                    })
                });
        let material = private_material_in(&pod, Some(namespace));
        let marked = pod.metadata.annotations.as_ref().and_then(|a| a.get(EPOCH));
        if !material
            && !dangerous
            && !(private_identity
                && (spec.automount_service_account_token != Some(false) || projected_token))
            && marked.is_none()
        {
            continue;
        }
        // Current-epoch consumers were admitted under this exact enforcing
        // bundle. The policy requires authenticated actor authority as well.
        if marked.map(String::as_str) == Some(epoch) {
            use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
            let owners: Vec<_> = pod
                .metadata
                .owner_references
                .as_ref()
                .into_iter()
                .flatten()
                .filter(|owner| owner.controller == Some(true))
                .collect();
            if owners.len() == 1 {
                let owner = owners[0];
                let group = match (owner.api_version.as_str(), owner.kind.as_str()) {
                    ("apps/v1", "ReplicaSet" | "Deployment" | "StatefulSet" | "DaemonSet") => {
                        "apps"
                    }
                    ("batch/v1", "Job" | "CronJob") => "batch",
                    ("v1", "ReplicationController") => "",
                    _ => return Err(ERROR.into()),
                };
                let resource =
                    ApiResource::from_gvk(&GroupVersionKind::gvk(group, "v1", &owner.kind));
                let parent = Api::<DynamicObject>::namespaced_with(
                    client.clone(),
                    &namespace.name_any(),
                    &resource,
                )
                .get(&owner.name)
                .await
                .map_err(|_| ERROR)?;
                if live(&parent.metadata)?.0 != owner.uid {
                    return Err(ERROR.into());
                }
                let template = if owner.kind == "CronJob" {
                    &parent.data["spec"]["jobTemplate"]["spec"]["template"]
                } else {
                    &parent.data["spec"]["template"]
                };
                if template["metadata"]["annotations"][EPOCH] == epoch
                    || annotations
                        .get(&format!("{PREFIX}parent-{}", owner.uid))
                        .map(String::as_str)
                        == Some(epoch)
                {
                    continue;
                }
            }
        }
        if material
            || annotations
                .get(&format!("{PREFIX}pod-{uid}"))
                .map(String::as_str)
                != Some(epoch)
            || annotations.get(&format!("{PREFIX}pod-spec-{uid}")) != Some(&hash(&raw))
        {
            return Err("Unexplained or prior-epoch private consumer preserved; operator qualification is required".into());
        }
    }
    Ok(())
}
