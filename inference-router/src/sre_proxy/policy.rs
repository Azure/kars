// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Closed Kubernetes route/query/media contract for an untrusted SRE agent.

use axum::http::{Method, Uri};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    Json,
    Secrets,
    Logs,
    Proposal,
}

fn label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
        && value != "."
        && value != ".."
}

fn resource(group: &str, version: &str, name: &str) -> bool {
    match (group, version) {
        ("", "v1") => [
            "pods",
            "services",
            "endpoints",
            "events",
            "configmaps",
            "secrets",
            "serviceaccounts",
            "resourcequotas",
            "limitranges",
            "nodes",
            "namespaces",
        ]
        .contains(&name),
        ("apps", "v1") => {
            ["deployments", "replicasets", "statefulsets", "daemonsets"].contains(&name)
        }
        ("batch", "v1") => ["jobs", "cronjobs"].contains(&name),
        ("networking.k8s.io", "v1") => ["networkpolicies", "ingresses"].contains(&name),
        ("discovery.k8s.io", "v1") => name == "endpointslices",
        ("events.k8s.io", "v1") => name == "events",
        ("metrics.k8s.io", "v1beta1") => ["pods", "nodes"].contains(&name),
        ("apiextensions.k8s.io", "v1") => name == "customresourcedefinitions",
        ("rbac.authorization.k8s.io", "v1") => [
            "roles",
            "rolebindings",
            "clusterroles",
            "clusterrolebindings",
        ]
        .contains(&name),
        ("kars.azure.com", "v1alpha1") => [
            "karssandboxes",
            "inferencepolicies",
            "toolpolicies",
            "mcpservers",
            "karsmemories",
            "karsevals",
            "karstasks",
            "karsteams",
            "karsprofiles",
            "karsskills",
            "karsreceipts",
            "karsapprovals",
            "egressapprovals",
            "karssreactions",
            "karsauthconfigs",
            "trustgraphs",
            "a2aagents",
            "karspairings",
        ]
        .contains(&name),
        _ => false,
    }
}

pub(super) fn route(method: &Method, uri: &Uri) -> Result<Route, &'static str> {
    let path = uri.path();
    if uri.scheme().is_some()
        || uri.authority().is_some()
        || path.contains('%')
        || path.contains('\\')
        || path.contains("//")
        || path.ends_with('/')
        || path.bytes().any(|b| b.is_ascii_control())
    {
        return Err("Noncanonical Kubernetes path");
    }
    let parts: Vec<_> = path.split('/').skip(1).collect();
    if parts.iter().any(|part| !label(part)) {
        return Err("Invalid Kubernetes path component");
    }
    let (group, version, tail) = match parts.as_slice() {
        ["api", version, tail @ ..] => ("", *version, tail),
        ["apis", group, version, tail @ ..] => (*group, *version, tail),
        _ => return Err("Kubernetes discovery/proxy paths are not part of the SRE contract"),
    };
    let (namespace, kind, name, subresource) = match tail {
        [kind] => (None, *kind, None, None),
        ["namespaces", ns, kind] => (Some(*ns), *kind, None, None),
        ["namespaces", ns, kind, name] => (Some(*ns), *kind, Some(*name), None),
        ["namespaces", ns, kind, name, sub] => (Some(*ns), *kind, Some(*name), Some(*sub)),
        [kind, name]
            if [
                "nodes",
                "namespaces",
                "customresourcedefinitions",
                "clusterroles",
                "clusterrolebindings",
            ]
            .contains(kind) =>
        {
            (None, *kind, Some(*name), None)
        }
        _ => return Err("Kubernetes subresource is not allowed"),
    };
    if !resource(group, version, kind) {
        return Err("Kubernetes resource is not allowed");
    }
    if method == Method::POST {
        if group == "kars.azure.com"
            && version == "v1alpha1"
            && namespace == Some("kars-sre")
            && kind == "karssreactions"
            && name.is_none()
            && subresource.is_none()
            && uri.query().is_none()
        {
            return Ok(Route::Proposal);
        }
        return Err("Only Pending SRE proposal creation is permitted");
    }
    if method != Method::GET {
        return Err("Kubernetes writes are not permitted");
    }
    let result = match subresource {
        Some("log") if group.is_empty() && kind == "pods" => Route::Logs,
        Some(_) => return Err("Kubernetes exec/proxy/token subresources are not permitted"),
        None if group.is_empty() && kind == "secrets" => Route::Secrets,
        None => Route::Json,
    };
    validate_query(uri.query(), result)?;
    Ok(result)
}

fn validate_query(query: Option<&str>, route: Route) -> Result<(), &'static str> {
    let Some(query) = query else { return Ok(()) };
    let mut keys = BTreeSet::new();
    let mut parsed = reqwest::Url::parse("https://localhost/").expect("static URL");
    parsed.set_query(Some(query));
    for (key, value) in parsed.query_pairs() {
        if !keys.insert(key.to_string())
            || value.len() > 2048
            || value.chars().any(char::is_control)
        {
            return Err("Invalid or duplicate Kubernetes query parameter");
        }
        match (route, key.as_ref()) {
            (Route::Logs, "container") if label(&value) => {}
            (Route::Logs, "tailLines") if value.parse::<u32>().is_ok_and(|n| n <= 1000) => {}
            (Route::Logs, "limitBytes") if value.parse::<u32>().is_ok_and(|n| n <= 262144) => {}
            (Route::Logs, "sinceSeconds") if value.parse::<u32>().is_ok_and(|n| n <= 86400) => {}
            (Route::Logs, "timestamps" | "previous")
                if matches!(value.as_ref(), "true" | "false") => {}
            (Route::Json | Route::Secrets, "limit")
                if value.parse::<u32>().is_ok_and(|n| n > 0 && n <= 500) => {}
            (Route::Json | Route::Secrets, "labelSelector" | "fieldSelector") => {}
            (Route::Json | Route::Secrets, "continue") if value.len() <= 1024 => {}
            _ => return Err("Kubernetes query parameter is not permitted"),
        }
    }
    Ok(())
}

fn secret(value: &Value) -> Result<Value, &'static str> {
    if value["kind"] != "Secret" || !value["metadata"].is_object() {
        return Err("Malformed Secret response");
    }
    let mut metadata = serde_json::Map::new();
    for key in [
        "name",
        "namespace",
        "uid",
        "resourceVersion",
        "creationTimestamp",
        "deletionTimestamp",
    ] {
        if let Some(value) = value["metadata"].get(key) {
            metadata.insert(key.into(), value.clone());
        }
    }
    let keys: serde_json::Map<String, Value> = value
        .get("data")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, _)| (key.clone(), Value::String(String::new())))
        .collect();
    Ok(
        json!({"apiVersion":"v1","kind":"Secret","metadata":metadata,
        "type":value.get("type").cloned().unwrap_or(Value::Null),"data":keys}),
    )
}

pub(super) fn secret_projection(value: &Value) -> Result<Value, &'static str> {
    if value["kind"] == "Secret" {
        return secret(value);
    }
    if value["kind"] != "SecretList" {
        return Err("Unexpected Secret response kind");
    }
    let items = value["items"].as_array().ok_or("Malformed Secret list")?;
    let items = items.iter().map(secret).collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"apiVersion":"v1","kind":"SecretList",
        "metadata":{"resourceVersion":value["metadata"]["resourceVersion"],"continue":value["metadata"]["continue"]},
        "items":items}))
}

pub(super) fn proposal(value: &Value) -> Result<Value, &'static str> {
    if value["apiVersion"] != "kars.azure.com/v1alpha1"
        || value["kind"] != "KarsSREAction"
        || value.get("status").is_some()
        || !value["spec"].is_object()
    {
        return Err("Invalid SRE proposal");
    }
    let metadata = value["metadata"]
        .as_object()
        .ok_or("Proposal metadata missing")?;
    if metadata
        .keys()
        .any(|key| !["name", "generateName", "namespace", "labels"].contains(&key.as_str()))
        || metadata.get("namespace").is_some_and(|ns| ns != "kars-sre")
        || !metadata
            .get("name")
            .or_else(|| metadata.get("generateName"))
            .and_then(Value::as_str)
            .is_some_and(label)
    {
        return Err("Proposal identity is invalid");
    }
    if let Some(labels) = metadata.get("labels") {
        let labels = labels
            .as_object()
            .ok_or("Proposal labels must be an object")?;
        if labels.iter().any(|(key, label)| match key.as_str() {
            "app.kubernetes.io/component" => label != "sre",
            "kars.azure.com/sre-action-type" => label != &value["spec"]["action"]["type"],
            _ => true,
        }) {
            return Err("Only the existing SRE diagnostic labels are permitted");
        }
    }
    let spec = value["spec"].as_object().unwrap();
    if spec.keys().any(|key| {
        !["action", "rationale", "diagnosis", "approval", "ttlMinutes"].contains(&key.as_str())
    }) || spec
        .get("approval")
        .is_some_and(|approval| approval != &json!({"state":"Pending"}))
        || ![
            "DeleteResourceQuota",
            "PatchDeploymentImage",
            "ScaleDeployment",
            "RolloutRestart",
            "DeletePod",
        ]
        .contains(&value["spec"]["action"]["type"].as_str().unwrap_or_default())
    {
        return Err("SRE proposals cannot grant approval or change unsupported authority");
    }
    let mut result = value.clone();
    result["metadata"]["namespace"] = "kars-sre".into();
    result["spec"]["approval"] = json!({"state":"Pending"});
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_contract_keeps_reads_logs_metrics_and_only_pending_proposals() {
        for path in [
            "/api/v1/namespaces",
            "/api/v1/namespaces/kars-sre/pods",
            "/api/v1/namespaces/kars-sre/pods/sre-123/log?tailLines=100&timestamps=true",
            "/apis/metrics.k8s.io/v1beta1/nodes",
            "/apis/kars.azure.com/v1alpha1/karssandboxes",
            "/apis/apps/v1/namespaces/kars-system/deployments/kars-controller",
        ] {
            assert!(
                route(&Method::GET, &path.parse().unwrap()).is_ok(),
                "{path}"
            );
        }
        assert_eq!(
            route(
                &Method::POST,
                &"/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions"
                    .parse()
                    .unwrap()
            )
            .unwrap(),
            Route::Proposal
        );
    }

    #[test]
    fn path_query_and_write_escapes_fail_closed() {
        for path in [
            "/api/v1/namespaces/kars-sre/secrets?watch=true",
            "/api/v1/namespaces/kars-sre/pods/sre/log?follow=true",
            "/api/v1/namespaces/kars-sre/pods/sre/log?tailLines=1&tailLines=2",
            "/api/v1/namespaces/kars-sre/pods/sre/exec",
            "/api/v1/namespaces/kars-sre/pods/sre/proxy",
            "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router/token",
            "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical",
            "/api/v1/namespaces/kars-sre/secrets/../pods",
            "/api/v1/namespaces/kars-sre/%73ecrets",
            "/api/v1/namespaces/kars-sre/pods%2fsre%2fproxy",
            "/api//v1/namespaces/kars-sre/secrets",
            "/api/v1/namespaces/kars-sre/secrets/",
            "https://kubernetes.default.svc/api/v1/secrets",
        ] {
            assert!(
                route(&Method::GET, &path.parse().unwrap()).is_err(),
                "{path}"
            );
        }
        for method in [Method::PUT, Method::PATCH, Method::DELETE, Method::CONNECT] {
            assert!(
                route(
                    &method,
                    &"/api/v1/namespaces/kars-sre/secrets/secret"
                        .parse()
                        .unwrap()
                )
                .is_err()
            );
        }
        assert!(
            route(
                &Method::POST,
                &"/apis/kars.azure.com/v1alpha1/namespaces/other/karssreactions"
                    .parse()
                    .unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn secrets_and_lists_keep_key_names_without_any_value_or_annotation_copy() {
        let secret = json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"router-services-admin","namespace":"kars-test","uid":"uid",
                "annotations":{"kubectl.kubernetes.io/last-applied-configuration":"PRIVATE_VALUE"},
                "labels":{"copied":"PRIVATE_VALUE"},"managedFields":[{"copy":"PRIVATE_VALUE"}]},
            "data":{"control-token":"PRIVATE_VALUE"},"stringData":{"copy":"PRIVATE_VALUE"}});
        for input in [
            secret.clone(),
            json!({"kind":"SecretList","metadata":{},"items":[secret]}),
        ] {
            let output = secret_projection(&input).unwrap();
            let encoded = serde_json::to_string(&output).unwrap();
            assert!(!encoded.contains("PRIVATE_VALUE"));
            assert!(!encoded.contains("annotations"));
            assert!(!encoded.contains("stringData"));
            assert!(!encoded.contains("managedFields"));
            assert!(encoded.contains("control-token"));
        }
    }

    #[test]
    fn proposals_cannot_self_approve_or_inject_status_ownership_or_extra_fields() {
        let base = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSREAction",
            "metadata":{"name":"proposal","namespace":"kars-sre"},
            "spec":{"action":{"type":"RolloutRestart","params":{"namespace":"kars-test","name":"app"}}}});
        assert_eq!(
            proposal(&base).unwrap()["spec"]["approval"],
            json!({"state":"Pending"})
        );
        for (pointer, value) in [
            ("/spec/approval", json!({"state":"Approved"})),
            ("/metadata/namespace", json!("other")),
        ] {
            let mut changed = base.clone();
            if pointer == "/spec/approval" {
                changed["spec"]["approval"] = value;
            } else {
                changed["metadata"]["namespace"] = value;
            }
            assert!(proposal(&changed).is_err());
        }
        let mut changed = base.clone();
        changed["status"] = json!({"phase":"Approved"});
        assert!(proposal(&changed).is_err());
        let mut changed = base;
        changed["metadata"]["ownerReferences"] = json!([]);
        assert!(proposal(&changed).is_err());
    }
}
