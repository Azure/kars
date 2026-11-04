// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::BTreeMap;

use crate::providers::signing::sha256_hex;
use kube::{ResourceExt, core::DynamicObject};

use crate::error::{AppError, AppResult};
use crate::kars::{cluster::Cluster, task::KarsTask};
use crate::routes::options::RefOption;

fn metadata(kind: &str, option: &RefOption) -> Option<serde_json::Value> {
    fn present(value: &Option<String>) -> Option<&str> {
        value.as_deref().filter(|value| !value.trim().is_empty())
    }
    match kind {
        "McpServer" => Some(serde_json::json!({
            "schema": present(&option.tool_schema_digest)?,
            "endpoint": option.summary,
            "mode": option.mode,
        })),
        "KarsMemory" => Some(serde_json::json!({
            "backend": present(&option.backend)?,
            "digest": present(&option.compiled_digest)?,
        })),
        "KarsSkill" => Some(serde_json::json!({
            "version": present(&option.version_digest)?,
        })),
        _ => None,
    }
}

pub(crate) fn admit_resource(
    evidence: Result<bool, String>,
    qualification_run: bool,
    kind: &str,
    option: &RefOption,
) -> Result<bool, String> {
    // Always evaluate retained evidence, including malformed configuration.
    evidence.map(|admitted| admitted || (qualification_run && metadata(kind, option).is_some()))
}

fn fingerprint(kind: &str, resource: &DynamicObject) -> AppResult<String> {
    let uid = resource
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| AppError::Conflict("Qualification resource has no live UID".into()))?;
    let spec = resource
        .data
        .get("spec")
        .filter(|spec| spec.is_object())
        .ok_or_else(|| AppError::Conflict("Qualification resource has no specification".into()))?;
    if kind == "KarsSkill" {
        crate::skill_package::approved_digest(&serde_json::json!(resource))
            .map_err(AppError::Conflict)?;
    }
    if kind == "KarsMemory" {
        let status = &resource.data["status"];
        let observed = status["observedGeneration"].as_i64();
        if !matches!(status["phase"].as_str(), Some("Compiled" | "Ready"))
            || observed.is_none()
            || observed != resource.metadata.generation
        {
            return Err(AppError::Conflict(
                "Qualification memory must have its current generation compiled and not failed"
                    .into(),
            ));
        }
    }
    let option = match kind {
        "McpServer" => crate::routes::options::mcp_server_option(resource),
        "KarsMemory" => crate::routes::options::memory_option(resource),
        "KarsSkill" => crate::routes::options::skill_option(resource),
        _ => {
            return Err(AppError::BadRequest(
                "Unsupported qualification resource".into(),
            ));
        }
    };
    let version = metadata(kind, &option).ok_or_else(|| {
        AppError::Conflict(format!(
            "{kind} {} lacks current qualification metadata",
            resource.name_any()
        ))
    })?;
    let bound = serde_json::json!({
        "kind": kind, "namespace": resource.namespace(), "name": resource.name_any(),
        "uid": uid, "spec": spec, "version": version,
    });
    Ok(format!(
        "sha256:{}",
        sha256_hex(bound.to_string().as_bytes())
    ))
}

pub(crate) async fn resource_bindings(
    cluster: &Cluster,
    task: &KarsTask,
) -> AppResult<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    let Some(bp) = task.spec.blueprint.as_ref() else {
        return Ok(bindings);
    };
    let namespace = task.namespace().unwrap_or_default();
    let memory = bp.memory.iter().cloned().collect::<Vec<_>>();
    for (kind, names) in [
        ("McpServer", &bp.mcp_servers),
        ("KarsMemory", &memory),
        ("KarsSkill", &bp.skills),
    ] {
        for name in names {
            let resource = cluster
                .get_kind(&namespace, kind, name)
                .await
                .map_err(|error| {
                    AppError::Upstream(format!("Qualification resource read failed: {error}"))
                })?
                .ok_or_else(|| {
                    AppError::Conflict(format!("Qualification resource {kind}/{name} is absent"))
                })?;
            bindings.insert(format!("{kind}/{name}"), fingerprint(kind, &resource)?);
        }
    }
    Ok(bindings)
}

pub(crate) async fn check_resources(cluster: &Cluster, task: &KarsTask) -> AppResult<()> {
    let record = super::record(task)?
        .ok_or_else(|| AppError::Conflict("Qualification authorization is absent".into()))?;
    if record.resource_bindings != resource_bindings(cluster, task).await? {
        return Err(AppError::Conflict("A reviewed qualification resource identity, specification or version changed; create a fresh explicitly reviewed Mission".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server() -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"McpServer",
            "metadata":{"name":"docs", "namespace":"workspace", "uid":"original", "generation":1},
            "spec":{"url":"https://docs.example/mcp", "allowedTools":["read"]},
            "status":{"phase":"Ready", "toolSchemaDigest":"sha256:original", "endpoint":"https://docs.example/mcp"}
        })).unwrap()
    }

    #[test]
    fn resource_exception_is_explicit_bound_and_does_not_hide_errors() {
        let resource = server();
        let mut option = crate::routes::options::mcp_server_option(&resource);
        assert_eq!(
            admit_resource(Ok(false), false, "McpServer", &option),
            Ok(false)
        );
        assert_eq!(
            admit_resource(Ok(false), true, "McpServer", &option),
            Ok(true)
        );
        assert_eq!(
            admit_resource(Err("invalid".into()), true, "McpServer", &option),
            Err("invalid".into())
        );
        assert_eq!(
            admit_resource(Ok(false), true, "Channel", &option),
            Ok(false)
        );
        option.tool_schema_digest = None;
        assert_eq!(
            admit_resource(Ok(false), true, "McpServer", &option),
            Ok(false)
        );
        option.tool_schema_digest = Some(" ".into());
        assert_eq!(
            admit_resource(Ok(false), true, "McpServer", &option),
            Ok(false)
        );
    }

    #[tokio::test]
    async fn live_resource_bindings_fail_closed_on_changes_absence_and_api_errors() {
        use axum::{
            body::Body,
            http::{Request, Response},
        };
        use std::sync::{Arc, Mutex};
        let response = Arc::new(Mutex::new((
            200u16,
            serde_json::to_value(server()).unwrap(),
        )));
        let current = response.clone();
        let service = tower::service_fn(move |request: Request<kube::client::Body>| {
            assert_eq!(request.method(), "GET");
            assert!(request.uri().path().contains("/namespaces/workspace/"));
            let (status, body) = current.lock().unwrap().clone();
            async move {
                Ok::<_, std::io::Error>(
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
            }
        });
        let cluster = Cluster::for_test_client(kube::Client::new(service, "workspace"));
        let mut task: KarsTask = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
            "metadata":{"name":"qualify", "namespace":"workspace"},
            "spec":{"objective":"Assess the public repository using its documentation.",
                "envelope":{"tier":2,"authorityCeiling":2,"delegationDepth":0},
                "blueprint":{"runtime":"Hermes","mcpServers":["docs"]}}
        }))
        .unwrap();
        let bindings = resource_bindings(&cluster, &task).await.unwrap();
        let principal = crate::auth::Principal {
            sub: "operator-sub".into(),
            name: "Operator".into(),
            roles: vec!["operator".into()],
        };
        super::super::stamp(&mut task, &principal, bindings).unwrap();
        assert!(check_resources(&cluster, &task).await.is_ok());
        response.lock().unwrap().1["metadata"]["uid"] = json!("replacement");
        assert!(check_resources(&cluster, &task).await.is_err());
        *response.lock().unwrap() = (200, serde_json::to_value(server()).unwrap());
        let mut changed = task.clone();
        changed.spec.blueprint.as_mut().unwrap().mcp_servers.clear();
        assert!(check_resources(&cluster, &changed).await.is_err());
        changed = task.clone();
        changed
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .mcp_servers
            .push("additional".into());
        assert!(check_resources(&cluster, &changed).await.is_err());
        for (status, reason) in [
            (404, "NotFound"),
            (403, "Forbidden"),
            (500, "InternalError"),
        ] {
            *response.lock().unwrap() = (
                status,
                json!({"apiVersion":"v1","kind":"Status",
                "status":"Failure","reason":reason,"code":status,"message":"test failure"}),
            );
            assert!(check_resources(&cluster, &task).await.is_err(), "{status}");
        }
        *response.lock().unwrap() = (200, serde_json::to_value(server()).unwrap());
        super::super::stamp(&mut task, &principal, BTreeMap::new()).unwrap();
        assert!(
            check_resources(&cluster, &task).await.is_err(),
            "legacy unbound resources"
        );
        task.spec.blueprint.as_mut().unwrap().mcp_servers.clear();
        assert!(
            check_resources(&cluster, &task).await.is_ok(),
            "legacy resource-free package"
        );
    }

    #[test]
    fn memory_versions_are_bound_and_unapproved_skills_remain_blocked() {
        let memory: DynamicObject = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsMemory",
            "metadata":{"name":"commons","namespace":"workspace","uid":"memory-uid","generation":1},
            "spec":{"backend":"foundry"},
            "status":{"phase":"Compiled","observedGeneration":1,"backend":"foundry","compiledDigest":"sha256:current"}
        }))
        .unwrap();
        let bound = fingerprint("KarsMemory", &memory).unwrap();
        for phase in ["Failed", "Pending", ""] {
            let mut changed = memory.clone();
            changed.data["status"]["phase"] = json!(phase);
            assert!(fingerprint("KarsMemory", &changed).is_err());
        }
        for generation in [json!(0), serde_json::Value::Null] {
            let mut changed = memory.clone();
            changed.data["status"]["observedGeneration"] = generation;
            assert!(fingerprint("KarsMemory", &changed).is_err());
        }
        let mut ready = memory.clone();
        ready.data["status"]["phase"] = json!("Ready");
        assert_eq!(bound, fingerprint("KarsMemory", &ready).unwrap());
        for field in ["backend", "compiledDigest"] {
            let mut changed = memory.clone();
            changed.data["status"][field] = json!("changed");
            assert_ne!(bound, fingerprint("KarsMemory", &changed).unwrap());
            changed.data["status"]["compiledDigest"] = serde_json::Value::Null;
            assert!(fingerprint("KarsMemory", &changed).is_err());
        }
        let skill: DynamicObject = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSkill",
            "metadata":{"name":"research","namespace":"workspace","uid":"skill-uid","generation":1},
            "spec":{"version":"1.0.0","recipe":"research"},
            "status":{"phase":"Pending","versionDigest":"sha256:current"}
        }))
        .unwrap();
        assert!(fingerprint("KarsSkill", &skill).is_err());
    }

    #[test]
    fn binding_rejects_resource_replacement_spec_endpoint_and_schema_changes() {
        let original = server();
        let bound = fingerprint("McpServer", &original).unwrap();
        for (pointer, value) in [
            ("/metadata/uid", json!("replacement")),
            ("/metadata/namespace", json!("other")),
            ("/spec/url", json!("https://other.example/mcp")),
            ("/spec/allowedTools", json!(["write"])),
            ("/status/endpoint", json!("https://redirected.example/mcp")),
            ("/status/toolSchemaDigest", json!("sha256:changed")),
        ] {
            let mut changed = serde_json::to_value(&original).unwrap();
            *changed.pointer_mut(pointer).unwrap() = value;
            let changed = serde_json::from_value(changed).unwrap();
            assert_ne!(
                bound,
                fingerprint("McpServer", &changed).unwrap(),
                "{pointer}"
            );
        }
        let mut changed = original.clone();
        changed.metadata.resource_version = Some("new-status-write".into());
        changed.data["status"]["lastProbeTime"] = json!("later");
        assert_eq!(bound, fingerprint("McpServer", &changed).unwrap());
        changed.metadata.uid = None;
        assert!(fingerprint("McpServer", &changed).is_err());
    }
}
