// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Error, Result, annotation, api_error, identity, owner, same_snapshot};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, ReplicaSet},
        core::v1::Pod,
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{Api, Client, ResourceExt, api::ListParams};
use serde_json::Value;

const HASH: &str = "pod-template-hash";
const REVISION: &str = "deployment.kubernetes.io/revision";

pub(super) struct Workload {
    pub deployment: Deployment,
    pub replica_set: ReplicaSet,
    pub pod: Pod,
}
fn label<'a>(meta: &'a ObjectMeta, key: &str) -> Option<&'a str> {
    meta.labels.as_ref()?.get(key).map(String::as_str)
}
fn observed(value: &Value) -> bool {
    value
        .pointer("/metadata/generation")
        .and_then(Value::as_i64)
        .is_some_and(|generation| {
            generation > 0
                && generation <= 9_007_199_254_740_991
                && value
                    .pointer("/status/observedGeneration")
                    .and_then(Value::as_i64)
                    == Some(generation)
        })
}
fn count(value: &Value, pointer: &str, expected: i64) -> bool {
    value.pointer(pointer).and_then(Value::as_i64) == Some(expected)
}
fn template(value: &Value) -> Option<Value> {
    let mut template = value.pointer("/spec/template")?.clone();
    if !template.pointer("/spec/containers")?.as_array()?.is_empty() {
        if let Some(labels) = template
            .pointer_mut("/metadata/labels")
            .and_then(Value::as_object_mut)
        {
            labels.remove(HASH);
        }
        Some(template)
    } else {
        None
    }
}
fn value<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|_| Error::Invalid("cannot validate workload representation"))
}
fn exact_namespace(meta: &ObjectMeta, namespace: &str) -> Result<()> {
    identity(meta)?;
    if meta.name.as_deref().is_none_or(str::is_empty)
        || meta.namespace.as_deref() != Some(namespace)
    {
        return Err(Error::Invalid("workload resource identity mismatch"));
    }
    Ok(())
}

pub(super) fn validate(workload: &Workload) -> Result<()> {
    let Workload {
        deployment,
        replica_set,
        pod,
    } = workload;
    let namespace = deployment
        .namespace()
        .ok_or(Error::Invalid("missing workload namespace"))?;
    for meta in [&deployment.metadata, &replica_set.metadata, &pod.metadata] {
        exact_namespace(meta, &namespace)?;
    }
    if !owner(
        &replica_set.metadata,
        "apps/v1",
        "Deployment",
        &deployment.name_any(),
        identity(&deployment.metadata)?.0,
    ) || !owner(
        &pod.metadata,
        "apps/v1",
        "ReplicaSet",
        &replica_set.name_any(),
        identity(&replica_set.metadata)?.0,
    ) {
        return Err(Error::Invalid("workload controller ownership mismatch"));
    }
    let d = value(deployment)?;
    let r = value(replica_set)?;
    let revision = annotation(&deployment.metadata, REVISION)
        .filter(|v| !v.is_empty() && !v.starts_with('0') && v.bytes().all(|b| b.is_ascii_digit()));
    let hash = label(&replica_set.metadata, HASH).filter(|v| !v.is_empty());
    if !observed(&d)
        || !observed(&r)
        || d.pointer("/spec/paused") == Some(&Value::Bool(true))
        || ![&d, &r].into_iter().all(|v| {
            [
                "/spec/replicas",
                "/status/replicas",
                "/status/readyReplicas",
                "/status/availableReplicas",
            ]
            .iter()
            .all(|path| count(v, path, 1))
                && v.pointer("/status/terminatingReplicas")
                    .is_none_or(|c| c.as_i64() == Some(0))
        })
        || !count(&d, "/status/updatedReplicas", 1)
        || revision.is_none()
        || annotation(&replica_set.metadata, REVISION) != revision
        || hash.is_none()
        || r.pointer("/spec/template/metadata/labels/pod-template-hash")
            .and_then(Value::as_str)
            != hash
        || label(&pod.metadata, HASH) != hash
        || template(&d).is_none()
        || template(&d) != template(&r)
        || pod.status.as_ref().is_none_or(|s| {
            s.phase.as_deref() != Some("Running")
                || !s
                    .conditions
                    .iter()
                    .flatten()
                    .any(|c| c.type_ == "Ready" && c.status == "True")
        })
    {
        return Err(Error::NotReady(
            "workload has not converged to one current Ready pod",
        ));
    }
    Ok(())
}

pub(super) async fn recheck(client: &Client, workload: &Workload) -> Result<()> {
    let namespace = workload
        .deployment
        .namespace()
        .ok_or(Error::Invalid("missing workload namespace"))?;
    let pod = Api::<Pod>::namespaced(client.clone(), &namespace)
        .get_metadata(&workload.pod.name_any())
        .await
        .map_err(|e| api_error("recheck mission pod", e))?;
    same_snapshot(&workload.pod.metadata, &pod.metadata)?;
    let rs = Api::<ReplicaSet>::namespaced(client.clone(), &namespace)
        .get_metadata(&workload.replica_set.name_any())
        .await
        .map_err(|e| api_error("recheck mission ReplicaSet", e))?;
    same_snapshot(&workload.replica_set.metadata, &rs.metadata)?;
    let deployment = Api::<Deployment>::namespaced(client.clone(), &namespace)
        .get_metadata(&workload.deployment.name_any())
        .await
        .map_err(|e| api_error("recheck mission Deployment", e))?;
    same_snapshot(&workload.deployment.metadata, &deployment.metadata)
}

/// List the whole namespace so an unlabelled old/terminating pod cannot be hidden
/// by a selector. Bounded incomplete lists are never treated as a complete proof.
pub(super) async fn read(client: &Client, deployment: Deployment) -> Result<Workload> {
    let namespace = deployment
        .namespace()
        .ok_or(Error::Invalid("missing workload namespace"))?;
    identity(&deployment.metadata)?;
    let sets = Api::<ReplicaSet>::namespaced(client.clone(), &namespace)
        .list(&ListParams::default().limit(1000))
        .await
        .map_err(|e| api_error("list mission ReplicaSets", e))?;
    let pods = Api::<Pod>::namespaced(client.clone(), &namespace)
        .list(&ListParams::default().limit(1000))
        .await
        .map_err(|e| api_error("list mission pods", e))?;
    if sets
        .metadata
        .continue_
        .as_deref()
        .is_some_and(|v| !v.is_empty())
        || pods
            .metadata
            .continue_
            .as_deref()
            .is_some_and(|v| !v.is_empty())
    {
        return Err(Error::Invalid(
            "workload list exceeded bounded verification window",
        ));
    }
    let sets: Vec<_> = sets
        .items
        .into_iter()
        .filter(|r| {
            owner(
                &r.metadata,
                "apps/v1",
                "Deployment",
                &deployment.name_any(),
                deployment.metadata.uid.as_deref().unwrap_or_default(),
            )
        })
        .collect();
    let mut owned_pods: Vec<_> = pods
        .items
        .into_iter()
        .filter(|p| {
            sets.iter().any(|r| {
                owner(
                    &p.metadata,
                    "apps/v1",
                    "ReplicaSet",
                    &r.name_any(),
                    r.metadata.uid.as_deref().unwrap_or_default(),
                )
            })
        })
        .collect();
    if owned_pods.len() != 1 {
        return Err(Error::NotReady("workload has overlapping or missing pods"));
    }
    let pod = owned_pods.remove(0);
    let mut current = None;
    for rs in sets {
        if owner(
            &pod.metadata,
            "apps/v1",
            "ReplicaSet",
            &rs.name_any(),
            identity(&rs.metadata)?.0,
        ) {
            current = Some(rs);
        } else {
            let r = value(&rs)?;
            if r.pointer("/spec/replicas").and_then(Value::as_i64) != Some(0)
                || [
                    "replicas",
                    "readyReplicas",
                    "availableReplicas",
                    "terminatingReplicas",
                ]
                .iter()
                .any(|field| {
                    r.get("status")
                        .and_then(|s| s.get(field))
                        .is_some_and(|c| c.as_i64() != Some(0))
                })
            {
                return Err(Error::NotReady(
                    "previous workload ReplicaSet is not quiescent",
                ));
            }
        }
    }
    let workload = Workload {
        deployment,
        replica_set: current.ok_or(Error::Invalid("pod ReplicaSet missing"))?,
        pod,
    };
    validate(&workload)?;
    recheck(client, &workload).await?;
    Ok(workload)
}
