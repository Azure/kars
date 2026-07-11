// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// ci:loc-ok — Phase 2 multi-CRD reconciler / generated module; intentional. Tracked in plan.md §S15 follow-up.
//! McpServer reconciler — Phase 2 §8 entry 1.
//!
//! Watches `McpServer` CRs and, for each:
//!
//! 1. Ensures a finalizer (`kars.azure.com/mcpserver-cleanup`) so
//!    cascading Secret + ConfigMap deletion runs synchronously when the
//!    CR is removed.
//! 2. Generates an Ed25519 signing keypair the first time we see the CR
//!    and stores it as a `Secret` of type
//!    `kars.azure.com/mcp-signing-key`. Subsequent reconciles
//!    reuse the existing Secret — rotation is a Phase 3 hardening
//!    concern (see audit doc §4).
//! 3. When `spec.productionMode == true` and `spec.oauth.issuer` is set,
//!    fetches `<issuer>/.well-known/openid-configuration`, then fetches
//!    the `jwks_uri` it advertises, and caches the raw JWKSet bytes
//!    into a ConfigMap. Failure → `Degraded=True/JwksFetchFailed` and
//!    a 60-second requeue, never blackhole.
//! 4. Sets `status.observedGeneration`, `status.phase`,
//!    `status.conditions[]`, `status.signingKeyRef`,
//!    `status.jwksConfigMapRef`.
//!
//! ## Reuse map
//!
//! Per the no-duplication rule (§0.2/§0.3): condition vocabulary +
//! transition-time helpers come from [`crate::status::conditions`].
//! Reconciler shape (Controller::new + non-fatal CRD missing) mirrors
//! [`crate::pairing_reconciler`]. JWKS verification (router side) lives
//! in `inference-router/src/mcp/oauth.rs` and is **not** duplicated
//! here — the controller only fetches and caches.

use anyhow::Result;
use base64::Engine;
use ed25519_dalek::SigningKey;
use futures::StreamExt;
use k8s_openapi::ByteString;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Secret, Service};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Client, ResourceExt,
    api::{Api, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use rand::RngCore;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::mcp_server::{LocalObjectRef, ManagedMcpPreset, McpServer, McpServerStatus};
use crate::status::conditions::{self, reason, status as cond_status};
use crate::status::phase::{PHASE_DEGRADED, PHASE_PENDING, PHASE_READY};

/// Field manager for SSA patches emitted by this reconciler. A unique
/// suffix per reconciler is the §10.4 #1 craftsmanship requirement —
/// detects out-of-band tampering.
const FIELD_MANAGER: &str = crate::field_managers::MCP_SERVER;

/// Finalizer name (DNS subdomain). Mirrors
/// `crate::reconciler::FINALIZER` shape.
const FINALIZER: &str = "kars.azure.com/mcpserver-cleanup";

/// Custom Secret type — makes a `kubectl get secrets` listing
/// self-documenting and lets RBAC carve permissions per type.
const SECRET_TYPE: &str = "kars.azure.com/mcp-signing-key";

/// Annotation written on the Secret holding the JWK `kid` (key id) the
/// router will see in the matching `verifying-key`. Useful for
/// operator-side rotation work and audit-log correlation.
const KID_ANNOTATION: &str = "kars.azure.com/mcp-signing-kid";

/// Maximum size of a JWKS document we will accept. Issuers serve
/// well-formed JWKS responses in the low-kilobytes; anything past 256 KiB
/// is almost certainly an attack or a misconfigured edge that returned
/// HTML. Matches the upper bound used by `mcp/oauth.rs::JwkSet` parsing
/// before deserialization rejects huge inputs anyway.
const MAX_JWKS_BYTES: usize = 256 * 1024;

/// Timeout for the issuer discovery + JWKS HTTP GETs. Bounded — the
/// reconciler should never hang on a slow issuer.
const HTTP_TIMEOUT_SECS: u64 = 10;

/// Namespace holding controller-managed MCP workloads. Keeping third-party
/// servers out of `kars-system` makes their trust boundary and resource use
/// visible, while sandbox NetworkPolicies can admit only this namespace/port.
const MANAGED_MCP_NAMESPACE_DEFAULT: &str = "kars-mcp";

/// Immutable official Playwright MCP multi-arch image index resolved on
/// 2026-07-11. Operators may override it cluster-wide for private-registry
/// mirroring via `MCP_PLAYWRIGHT_IMAGE`; the CR cannot choose arbitrary images.
const PLAYWRIGHT_IMAGE_DEFAULT: &str = "mcr.microsoft.com/playwright/mcp@sha256:3d871c22ea2d4cca0966e2cfb1860e1cb03eb7353725a3d6cffd133296fb04eb";

/// Kars-built image containing
/// `@modelcontextprotocol/server-everything@2026.7.4`. The release pipeline
/// publishes it alongside the other Kars images; private clusters override via
/// `MCP_EVERYTHING_IMAGE`.
const EVERYTHING_IMAGE_DEFAULT: &str = "ghcr.io/azure/kars/mcp-everything:latest";

/// Requeue cadence on success.
const REQUEUE_OK: Duration = Duration::from_secs(300);

/// Requeue cadence on transient failure (JWKS fetch, etc).
const REQUEUE_FAIL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
    #[error("MCP configuration error: {0}")]
    Configuration(String),
}

struct Ctx {
    client: Client,
    /// Override hook for tests — swap the JWKS fetcher with a mock.
    jwks_fetcher: Arc<dyn JwksFetcher>,
    probe_client: reqwest::Client,
}

#[derive(Debug, Clone)]
struct ManagedWorkloadPlan {
    namespace: String,
    workload_name: String,
    image: String,
    port: u16,
    args: Vec<String>,
    env: Vec<(String, String)>,
    cpu_request: &'static str,
    memory_request: &'static str,
    cpu_limit: &'static str,
    memory_limit: &'static str,
}

impl ManagedWorkloadPlan {
    fn endpoint(&self) -> String {
        format!(
            "http://{}.{}.svc.cluster.local:{}/mcp",
            self.workload_name, self.namespace, self.port
        )
    }

    fn workload_ref(&self) -> String {
        format!("{}/{}", self.namespace, self.workload_name)
    }
}

fn managed_namespace() -> String {
    std::env::var("MCP_MANAGED_NAMESPACE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| MANAGED_MCP_NAMESPACE_DEFAULT.to_string())
}

fn managed_workload_plan(
    source_namespace: &str,
    name: &str,
    uid: Option<&str>,
    preset: &ManagedMcpPreset,
) -> ManagedWorkloadPlan {
    let namespace = managed_namespace();
    let identity = format!("{source_namespace}/{}", uid.unwrap_or(name));
    let suffix = &hex::encode(Sha256::digest(identity.as_bytes()))[..10];
    let max_name_len = 63usize.saturating_sub("mcp--".len() + suffix.len());
    let trimmed_name = name.trim_matches('-');
    let safe_name = if trimmed_name.len() > max_name_len {
        trimmed_name[..max_name_len].trim_end_matches('-')
    } else {
        trimmed_name
    };
    let workload_name = format!("mcp-{safe_name}-{suffix}");
    match preset {
        ManagedMcpPreset::Playwright => {
            let image = std::env::var("MCP_PLAYWRIGHT_IMAGE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| PLAYWRIGHT_IMAGE_DEFAULT.to_string());
            let allowed_hosts = format!(
                "{workload_name}.{namespace}.svc.cluster.local:8931,\
                 {workload_name}.{namespace}:8931,{workload_name}:8931,\
                 localhost:8931,localhost"
            )
            .replace(' ', "");
            ManagedWorkloadPlan {
                namespace,
                workload_name,
                image,
                port: 8931,
                args: vec![
                    "--port=8931".into(),
                    "--host=0.0.0.0".into(),
                    "--headless".into(),
                    "--browser=chromium".into(),
                    "--no-sandbox".into(),
                    format!("--allowed-hosts={allowed_hosts}"),
                ],
                env: Vec::new(),
                cpu_request: "250m",
                memory_request: "512Mi",
                cpu_limit: "2",
                memory_limit: "2Gi",
            }
        }
        ManagedMcpPreset::Everything => {
            let image = std::env::var("MCP_EVERYTHING_IMAGE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| EVERYTHING_IMAGE_DEFAULT.to_string());
            ManagedWorkloadPlan {
                namespace,
                workload_name,
                image,
                port: 3001,
                args: Vec::new(),
                env: vec![("PORT".into(), "3001".into())],
                cpu_request: "50m",
                memory_request: "128Mi",
                cpu_limit: "500m",
                memory_limit: "512Mi",
            }
        }
    }
}

#[derive(Debug, Clone)]
struct UpstreamProbe {
    tool_names: Vec<String>,
    schema_digest: String,
}

/// Pluggable JWKS fetcher — production uses [`HttpJwksFetcher`], tests
/// provide deterministic fixtures.
#[async_trait::async_trait]
trait JwksFetcher: Send + Sync + std::fmt::Debug {
    /// Return `(jwks_uri, raw_jwks_bytes)`. `error_class` strings on
    /// failure: `"dns" | "tls" | "timeout" | "http_status" | "invalid_jwks_format"`.
    async fn fetch(&self, issuer: &str) -> Result<FetchedJwks, FetchError>;
}

#[derive(Debug, Clone)]
struct FetchedJwks {
    jwks_uri: String,
    raw: Vec<u8>,
    /// Number of keys parsed from `raw`. Audit-event payload only.
    key_count: usize,
}

#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error("issuer discovery: {class}: {detail}")]
    Discovery { class: &'static str, detail: String },
    #[error("JWKS fetch: {class}: {detail}")]
    Jwks { class: &'static str, detail: String },
    #[error("JWKS payload not a JWKSet: {0}")]
    InvalidJwks(String),
}

impl FetchError {
    fn class(&self) -> &'static str {
        match self {
            FetchError::Discovery { class, .. } => class,
            FetchError::Jwks { class, .. } => class,
            FetchError::InvalidJwks(_) => "invalid_jwks_format",
        }
    }
}

#[derive(Debug)]
struct HttpJwksFetcher {
    client: reqwest::Client,
}

impl HttpJwksFetcher {
    fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .https_only(true)
            .build()
            .expect("reqwest client builder");
        Self { client }
    }
}

#[async_trait::async_trait]
impl JwksFetcher for HttpJwksFetcher {
    async fn fetch(&self, issuer: &str) -> Result<FetchedJwks, FetchError> {
        let trimmed = issuer.trim_end_matches('/');
        let discovery_url = format!("{trimmed}/.well-known/openid-configuration");
        let resp = self.client.get(&discovery_url).send().await.map_err(|e| {
            let class = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "dns"
            } else {
                "tls"
            };
            FetchError::Discovery {
                class,
                detail: e.to_string(),
            }
        })?;
        if !resp.status().is_success() {
            return Err(FetchError::Discovery {
                class: "http_status",
                detail: resp.status().to_string(),
            });
        }
        let discovery: serde_json::Value =
            resp.json().await.map_err(|e| FetchError::Discovery {
                class: "invalid_jwks_format",
                detail: e.to_string(),
            })?;
        let jwks_uri = discovery
            .get("jwks_uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FetchError::Discovery {
                class: "invalid_jwks_format",
                detail: "discovery document missing jwks_uri".into(),
            })?
            .to_string();

        let resp = self.client.get(&jwks_uri).send().await.map_err(|e| {
            let class = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "dns"
            } else {
                "tls"
            };
            FetchError::Jwks {
                class,
                detail: e.to_string(),
            }
        })?;
        if !resp.status().is_success() {
            return Err(FetchError::Jwks {
                class: "http_status",
                detail: resp.status().to_string(),
            });
        }
        let bytes = resp.bytes().await.map_err(|e| FetchError::Jwks {
            class: "tls",
            detail: e.to_string(),
        })?;
        if bytes.len() > MAX_JWKS_BYTES {
            return Err(FetchError::InvalidJwks(format!(
                "JWKS exceeds {MAX_JWKS_BYTES} bytes"
            )));
        }
        let raw = bytes.to_vec();
        let key_count = parse_jwks_key_count(&raw)?;
        Ok(FetchedJwks {
            jwks_uri,
            raw,
            key_count,
        })
    }
}

/// Parse `keys` array length from a raw JWKSet payload. Used both by the
/// production fetcher and by the audit-event emitter.
fn parse_jwks_key_count(raw: &[u8]) -> Result<usize, FetchError> {
    let v: serde_json::Value = serde_json::from_slice(raw)
        .map_err(|e| FetchError::InvalidJwks(format!("not JSON: {e}")))?;
    let keys = v
        .get("keys")
        .and_then(|k| k.as_array())
        .ok_or_else(|| FetchError::InvalidJwks("missing or non-array `keys`".into()))?;
    Ok(keys.len())
}

async fn ensure_managed_namespace(client: &Client, namespace: &str) -> Result<(), ReconcileError> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let body = json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": {
            "name": namespace,
            "labels": {
                "app.kubernetes.io/name": "kars-managed-mcp",
                "app.kubernetes.io/managed-by": "kars-controller",
                "kubernetes.io/metadata.name": namespace,
                "pod-security.kubernetes.io/enforce": "restricted",
                "pod-security.kubernetes.io/audit": "restricted",
                "pod-security.kubernetes.io/warn": "restricted"
            }
        }
    });
    namespaces
        .patch(
            namespace,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(body),
        )
        .await?;
    Ok(())
}

async fn mirror_managed_pull_secret(
    client: &Client,
    namespace: &str,
) -> Result<Option<String>, ReconcileError> {
    let Some(secret_name) = std::env::var("IMAGE_PULL_SECRET_NAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        return Ok(None);
    };
    let source_namespace = std::env::var("POD_NAMESPACE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "kars-system".to_string());
    let source: Api<Secret> = Api::namespaced(client.clone(), &source_namespace);
    let target: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = source.get(&secret_name).await?;
    let body = Secret {
        metadata: ObjectMeta {
            name: Some(secret_name.clone()),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                (
                    "app.kubernetes.io/part-of".into(),
                    "kars-managed-mcp".into(),
                ),
            ])),
            ..Default::default()
        },
        type_: secret.type_,
        data: secret.data,
        string_data: secret.string_data,
        ..Default::default()
    };
    target
        .patch(
            &secret_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(body),
        )
        .await?;
    Ok(Some(secret_name))
}

async fn ensure_managed_workload(
    client: &Client,
    owner: &str,
    plan: &ManagedWorkloadPlan,
) -> Result<bool, ReconcileError> {
    ensure_managed_namespace(client, &plan.namespace).await?;
    let pull_secret = mirror_managed_pull_secret(client, &plan.namespace).await?;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &plan.namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), &plan.namespace);
    let policies: Api<NetworkPolicy> = Api::namespaced(client.clone(), &plan.namespace);

    let labels = json!({
        "app.kubernetes.io/name": plan.workload_name,
        "app.kubernetes.io/component": "mcp-server",
        "app.kubernetes.io/managed-by": "kars-controller",
        "kars.azure.com/mcp-server": owner
    });
    let env: Vec<serde_json::Value> = plan
        .env
        .iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    let image_pull_secrets: Vec<serde_json::Value> = pull_secret
        .iter()
        .map(|name| json!({"name": name}))
        .collect();
    let deployment = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": plan.workload_name,
            "namespace": plan.namespace,
            "labels": labels
        },
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"kars.azure.com/mcp-server": owner}},
            "template": {
                "metadata": {"labels": labels},
                "spec": {
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": 1000,
                        "seccompProfile": {"type": "RuntimeDefault"}
                    },
                    "imagePullSecrets": image_pull_secrets,
                    "containers": [{
                        "name": "mcp",
                        "image": plan.image,
                        "imagePullPolicy": "IfNotPresent",
                        "args": plan.args,
                        "env": env,
                        "ports": [{"name": "mcp", "containerPort": plan.port}],
                        "readinessProbe": {
                            "tcpSocket": {"port": "mcp"},
                            "initialDelaySeconds": 3,
                            "periodSeconds": 5
                        },
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "capabilities": {"drop": ["ALL"]}
                        },
                        "resources": {
                            "requests": {
                                "cpu": plan.cpu_request,
                                "memory": plan.memory_request
                            },
                            "limits": {
                                "cpu": plan.cpu_limit,
                                "memory": plan.memory_limit
                            }
                        }
                    }]
                }
            }
        }
    });
    deployments
        .patch(
            &plan.workload_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(deployment),
        )
        .await?;

    let service = json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": plan.workload_name,
            "namespace": plan.namespace,
            "labels": labels
        },
        "spec": {
            "selector": {"kars.azure.com/mcp-server": owner},
            "ports": [{"name": "mcp", "port": plan.port, "targetPort": "mcp"}]
        }
    });
    services
        .patch(
            &plan.workload_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(service),
        )
        .await?;

    // Only sandbox routers and the controller namespace may initiate MCP
    // sessions. This is ingress isolation; preset-specific browser egress is
    // governed separately by tool policy and remains visible in the witness.
    let policy = json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": plan.workload_name,
            "namespace": plan.namespace,
            "labels": labels
        },
        "spec": {
            "podSelector": {"matchLabels": {"kars.azure.com/mcp-server": owner}},
            "policyTypes": ["Ingress"],
            "ingress": [{
                "from": [
                    {"namespaceSelector": {"matchLabels": {"kars.azure.com/role": "sandbox"}}},
                    {"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kars-system"}}}
                ],
                "ports": [{"protocol": "TCP", "port": plan.port}]
            }]
        }
    });
    policies
        .patch(
            &plan.workload_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(policy),
        )
        .await?;

    let current = deployments.get(&plan.workload_name).await?;
    let ready = current
        .status
        .as_ref()
        .and_then(|s| s.ready_replicas)
        .unwrap_or(0)
        >= 1;
    Ok(ready)
}

async fn cleanup_managed_workload(
    client: &Client,
    workload_ref: &str,
) -> Result<(), ReconcileError> {
    let (namespace, name) = workload_ref.split_once('/').ok_or_else(|| {
        ReconcileError::Configuration(format!(
            "invalid managed MCP workloadRef '{workload_ref}'"
        ))
    })?;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), namespace);
    let policies: Api<NetworkPolicy> = Api::namespaced(client.clone(), namespace);
    for result in [
        deployments
            .delete(name, &DeleteParams::default())
            .await
            .map(|_| ()),
        services
            .delete(name, &DeleteParams::default())
            .await
            .map(|_| ()),
        policies
            .delete(name, &DeleteParams::default())
            .await
            .map(|_| ()),
    ] {
        if let Err(e) = result
            && !matches!(e, kube::Error::Api(ref ae) if ae.code == 404)
        {
            return Err(e.into());
        }
    }
    Ok(())
}

fn extract_jsonrpc_payload(content_type: &str, body: &str) -> Result<serde_json::Value, String> {
    if content_type.contains("text/event-stream") {
        for line in body.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if !data.is_empty() {
                    return serde_json::from_str(data)
                        .map_err(|e| format!("invalid SSE JSON-RPC payload: {e}"));
                }
            }
        }
        return Err("SSE response carried no data event".into());
    }
    serde_json::from_str(body).map_err(|e| format!("invalid JSON-RPC payload: {e}"))
}

async fn probe_upstream_tools(
    client: &reqwest::Client,
    endpoint: &str,
    allowed_tools: &[String],
) -> Result<UpstreamProbe, String> {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": "kars-controller-initialize",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "kars-controller", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    let init = client
        .post(endpoint)
        .header("accept", "application/json, text/event-stream")
        .json(&initialize)
        .send()
        .await
        .map_err(|e| format!("initialize request failed: {e}"))?;
    if !init.status().is_success() {
        return Err(format!("initialize returned HTTP {}", init.status()));
    }
    let session = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let init_content_type = init
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let init_body = init
        .text()
        .await
        .map_err(|e| format!("initialize body read failed: {e}"))?;
    let init_value = extract_jsonrpc_payload(&init_content_type, &init_body)?;
    if let Some(error) = init_value.get("error") {
        return Err(format!("initialize JSON-RPC error: {error}"));
    }
    let protocol = init_value
        .pointer("/result/protocolVersion")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "initialize result missing protocolVersion".to_string())?
        .to_string();

    let result = async {
        let initialized = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        let mut initialized_request = client
            .post(endpoint)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", &protocol)
            .json(&initialized);
        if let Some(session_id) = session.as_deref() {
            initialized_request = initialized_request.header("mcp-session-id", session_id);
        }
        let response = initialized_request
            .send()
            .await
            .map_err(|e| format!("notifications/initialized failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "notifications/initialized returned HTTP {}",
                response.status()
            ));
        }

        let list = json!({
            "jsonrpc": "2.0",
            "id": "kars-controller-tools-list",
            "method": "tools/list",
            "params": {}
        });
        let mut request = client
            .post(endpoint)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", &protocol)
            .json(&list);
        if let Some(session_id) = session.as_deref() {
            request = request.header("mcp-session-id", session_id);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("tools/list request failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("tools/list returned HTTP {}", response.status()));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = response
            .text()
            .await
            .map_err(|e| format!("tools/list body read failed: {e}"))?;
        let value = extract_jsonrpc_payload(&content_type, &body)?;
        if let Some(error) = value.get("error") {
            return Err(format!("tools/list JSON-RPC error: {error}"));
        }
        let tools = value
            .pointer("/result/tools")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "tools/list response missing result.tools".to_string())?;
        let allow_all = allowed_tools.iter().any(|t| t == "*");
        let mut definitions: Vec<serde_json::Value> = tools
            .iter()
            .filter(|tool| {
                let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
                allow_all || allowed_tools.iter().any(|allowed| allowed == name)
            })
            .cloned()
            .collect();
        definitions.sort_by(|a, b| {
            a.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
        });
        if definitions.is_empty() {
            return Err(format!(
                "upstream exposed no tools matching allowedTools={allowed_tools:?}"
            ));
        }
        let tool_names = definitions
            .iter()
            .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect();
        let canonical =
            serde_json::to_vec(&definitions).map_err(|e| format!("tool catalog serialize: {e}"))?;
        let schema_digest = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
        Ok(UpstreamProbe {
            tool_names,
            schema_digest,
        })
    }
    .await;

    if let Some(session_id) = session.as_deref() {
        let _ = client
            .delete(endpoint)
            .header("mcp-session-id", session_id)
            .header("mcp-protocol-version", &protocol)
            .send()
            .await;
    }
    result
}

async fn reconcile(mcp: Arc<McpServer>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = mcp.name_any();
    let ns = mcp.namespace().unwrap_or_else(|| "kars-system".into());
    tracing::info!(mcp = %name, ns = %ns, "Reconciling McpServer");

    let api: Api<McpServer> = Api::namespaced(ctx.client.clone(), &ns);
    let secrets: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);

    // Deletion path — finalizer-cascading cleanup.
    if mcp.metadata.deletion_timestamp.is_some() {
        return finalize(&ctx.client, &api, &secrets, &configmaps, &mcp, &name).await;
    }

    // Add finalizer if missing.
    if !mcp
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"McpServer","metadata":{"finalizers":[FINALIZER]}});
        api.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let prior_conditions = mcp
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let observed_generation = mcp.metadata.generation;

    // Resolve the effective spec: either pass the CR verbatim (inline
    // path) or fetch + cosign-verify the referenced OCI bundle and
    // merge its content onto the CR's `allowedSandboxes` selector
    // (signed path). See [`resolve_mcp_source`] doc-comment.
    let (mut effective_spec, bundle_ref_digest, source_degraded) = resolve_mcp_source(&mcp).await;
    let mut managed_plan: Option<ManagedWorkloadPlan> = None;
    let mut pending: Option<String> = None;
    let mut discovered_tools: Option<Vec<String>> = None;
    let mut tool_schema_digest: Option<String> = None;
    let mut degraded: Option<(&'static str, String)> = source_degraded;

    // Mode transition Managed → External: remove the workload recorded by the
    // previous status before publishing external metadata. Cleanup identity is
    // persisted in status, so it remains stable even if the current spec/env
    // changed.
    if effective_spec.managed.is_none()
        && mcp
            .status
            .as_ref()
            .and_then(|s| s.mode.as_deref())
            == Some("Managed")
        && let Some(workload_ref) = mcp
            .status
            .as_ref()
            .and_then(|s| s.workload_ref.as_deref())
        && let Err(e) = cleanup_managed_workload(&ctx.client, workload_ref).await
    {
        degraded = Some(("ManagedCleanupFailed", e.to_string()));
    }

    // A managed preset owns a real Deployment + Service. Derive the endpoint
    // before writing router metadata so sandboxes consume the Service DNS name,
    // never a fake placeholder URL from the Bridge catalog.
    if degraded.is_none()
        && let Some(managed) = effective_spec.managed.as_ref()
    {
        let plan = managed_workload_plan(
            &ns,
            &name,
            mcp.metadata.uid.as_deref(),
            &managed.preset,
        );
        effective_spec.url = Some(plan.endpoint());
        effective_spec.production_mode = Some(false);
        match ensure_managed_workload(&ctx.client, &name, &plan).await {
            Ok(true) => {
                let allowed = effective_spec.allowed_tools.clone().unwrap_or_default();
                match probe_upstream_tools(&ctx.probe_client, &plan.endpoint(), &allowed).await {
                    Ok(probe) => {
                        discovered_tools = Some(probe.tool_names);
                        tool_schema_digest = Some(probe.schema_digest);
                    }
                    Err(e) => degraded = Some(("McpProbeFailed", e)),
                }
            }
            Ok(false) => {
                pending = Some(format!(
                    "managed MCP workload {} is not Ready yet",
                    plan.workload_ref()
                ));
            }
            Err(e) => {
                degraded = Some(("ManagedWorkloadFailed", e.to_string()));
            }
        }
        managed_plan = Some(plan);
    }

    // 1. Ensure signing keypair Secret.
    let secret_name = format!("mcp-{name}-signing");
    let signing_kid = ensure_signing_secret(&secrets, &secret_name, &name).await?;

    // 2. Ensure metadata/JWKS ConfigMap. The CM (`mcp-{name}-jwks`) is
    // ALWAYS created — its `meta.json` carries the upstream `url` +
    // `allowedTools` that the inference-router's `McpServerRegistry`
    // needs to forward calls, and its presence is also what the sandbox
    // reconciler mirrors into the sandbox namespace at
    // `/etc/kars/mcp/<name>/`. When `productionMode=false` we emit
    // an empty `{"keys": []}` JWKS default (no inbound OAuth
    // verification needed in dev mode — `/mcp` is mounted on the
    // loopback-only dev surface) but still register the URL so
    // outbound forwarding works. When `productionMode=true` the JWKS
    // is fetched from `oauth.issuer` and replaces the default.
    let cm_name = format!("mcp-{name}-jwks");
    let meta = McpServerMeta::from_spec(&effective_spec);
    let mut jwks_ref: Option<LocalObjectRef> = None;
    let production = effective_spec.production_mode.unwrap_or(false);

    if degraded.is_none() && !production {
        // Dev mode: write metadata + empty JWKS default so the
        // router can discover the upstream URL even without inbound
        // OAuth. The router's `/mcp` route is mounted in dev mode
        // (no OAuth) when no `productionMode=true` McpServer is bound.
        let empty_jwks = b"{\"keys\":[]}";
        ensure_jwks_configmap(&configmaps, &cm_name, &name, empty_jwks, &meta).await?;
        jwks_ref = Some(LocalObjectRef {
            name: cm_name.clone(),
        });
    }

    if degraded.is_none() && production {
        let issuer_opt = effective_spec.oauth.as_ref().map(|o| o.issuer.clone());
        match issuer_opt {
            Some(issuer) if !issuer.is_empty() => {
                let cm_name = format!("mcp-{name}-jwks");
                match ctx.jwks_fetcher.fetch(&issuer).await {
                    Ok(fetched) => {
                        let meta = McpServerMeta::from_spec(&effective_spec);
                        ensure_jwks_configmap(&configmaps, &cm_name, &name, &fetched.raw, &meta)
                            .await?;
                        jwks_ref = Some(LocalObjectRef {
                            name: cm_name.clone(),
                        });
                        tracing::info!(
                            mcp = %name,
                            jwks_uri = %fetched.jwks_uri,
                            key_count = fetched.key_count,
                            "McpServerJwksFetched"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            mcp = %name,
                            error_class = e.class(),
                            "McpServerJwksFetchFailed"
                        );
                        degraded = Some(("JwksFetchFailed", e.to_string()));
                    }
                }
            }
            _ => {
                // Admission CEL forbids this combination — a CR that
                // reaches the reconciler with productionMode=true and
                // empty issuer means CRD CEL was bypassed (e.g.,
                // controller upgraded ahead of CRD). Fail loudly.
                degraded = Some((
                    "SpecInvalid",
                    "productionMode=true requires spec.oauth.issuer (inline or via bundleRef)"
                        .into(),
                ));
            }
        }
    }

    // 3. Build & write status.
    let signing_ref = LocalObjectRef { name: secret_name };
    let new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        pending.as_deref(),
        degraded
            .as_ref()
            .map(|(reason, msg)| (*reason, msg.as_str())),
    );
    let phase = if degraded.is_some() {
        PHASE_DEGRADED
    } else if pending.is_some() {
        PHASE_PENDING
    } else {
        // Slice 0 honesty: McpServer reconciler today binds exactly
        // one server per KarsSandbox via `spec.mcp:` (singular).
        // Slice 4 of crd-well-oiled-machine introduces a plural
        // multi-server model + per-server enable/disable. We keep
        // `Ready` here (the singular path *does* work end-to-end and
        // the router consumes it), but publish a `LimitedSupport`
        // Warning Event so operators reading `kubectl describe` see
        // the upcoming change before they ship CRs that assume
        // multi-MCP today.
        PHASE_READY
    };

    // SSA requires apiVersion + kind in the patch body — without
    // them, the API server returns "invalid object type: /, Kind=".
    let status_patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "McpServer",
        "status": McpServerStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            last_probed_at: discovered_tools.as_ref().map(|_| rfc3339_now()),
            signing_key_ref: Some(signing_ref),
            jwks_config_map_ref: jwks_ref,
            bundle_ref_digest: bundle_ref_digest.clone(),
            mode: Some(
                if managed_plan.is_some() {
                    "Managed"
                } else {
                    "External"
                }
                .into(),
            ),
            endpoint: effective_spec.url.clone(),
            workload_ref: managed_plan.as_ref().map(ManagedWorkloadPlan::workload_ref),
            discovered_tools,
            tool_schema_digest,
        }
    });
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(status_patch),
    )
    .await?;

    tracing::info!(mcp = %name, phase = phase, kid = %signing_kid, "McpServerReconciled");

    if degraded.is_some() {
        Ok(Action::requeue(REQUEUE_FAIL))
    } else if pending.is_some() {
        Ok(Action::requeue(Duration::from_secs(5)))
    } else {
        // (Removed) Per-reconcile `LimitedSupport` event explaining
        // the singular-vs-plural `spec.mcp` migration roadmap was
        // emitted here. It re-fired on every reconcile (~15s cycle)
        // and flooded the Headlamp event view with the same advisory
        // text. The information now lives in:
        //   • the McpServer CRD `description` (visible in
        //     `kubectl explain mcpserver.spec`)
        //   • docs/blueprints/crd-well-oiled-machine.md (Slice 4 roadmap)
        // K8s Events should carry actionable per-incident signal,
        // not static design notes.
        Ok(Action::requeue(REQUEUE_OK))
    }
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Build the Conditions vector preserving prior `lastTransitionTime`
/// where status hasn't flipped. Always emits `Ready` and `Degraded`;
/// `Progressing=False/Reconciled` is emitted on success.
fn build_conditions(
    prior: &[Condition],
    observed_generation: Option<i64>,
    pending: Option<&str>,
    degraded: Option<(&str, &str)>,
) -> Vec<Condition> {
    let mut out: Vec<Condition> = Vec::with_capacity(3);
    let prior_ready = conditions::find(prior, conditions::TYPE_READY);
    let prior_progressing = conditions::find(prior, conditions::TYPE_PROGRESSING);
    let prior_degraded = conditions::find(prior, conditions::TYPE_DEGRADED);

    match degraded {
        Some((reason_value, message)) => {
            out.push(conditions::preserve_transition_time(
                prior_ready,
                conditions::TYPE_READY,
                cond_status::FALSE,
                reason_value,
                message,
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_progressing,
                conditions::TYPE_PROGRESSING,
                cond_status::FALSE,
                reason::FAILED,
                "reconcile failed",
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_degraded,
                conditions::TYPE_DEGRADED,
                cond_status::TRUE,
                reason_value,
                message,
                observed_generation,
            ));
        }
        None if pending.is_some() => {
            let message = pending.unwrap_or("managed MCP workload is progressing");
            out.push(conditions::preserve_transition_time(
                prior_ready,
                conditions::TYPE_READY,
                cond_status::FALSE,
                reason::RECONCILING,
                message,
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_progressing,
                conditions::TYPE_PROGRESSING,
                cond_status::TRUE,
                reason::RECONCILING,
                message,
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_degraded,
                conditions::TYPE_DEGRADED,
                cond_status::FALSE,
                reason::RECONCILING,
                "no error; waiting for managed MCP readiness",
                observed_generation,
            ));
        }
        None => {
            out.push(conditions::preserve_transition_time(
                prior_ready,
                conditions::TYPE_READY,
                cond_status::TRUE,
                reason::RECONCILED,
                "MCP server reconciled",
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_progressing,
                conditions::TYPE_PROGRESSING,
                cond_status::FALSE,
                reason::RECONCILED,
                "reconcile complete",
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_degraded,
                conditions::TYPE_DEGRADED,
                cond_status::FALSE,
                reason::RECONCILED,
                "no errors",
                observed_generation,
            ));
        }
    }
    out
}

/// Ensure a Secret holding an Ed25519 keypair exists. If a Secret with
/// this name exists already we reuse it (rotation is Phase 3). Returns
/// the kid (first 16 hex chars of the SHA-256 over the public key) for
/// audit logs.
async fn ensure_signing_secret(
    api: &Api<Secret>,
    secret_name: &str,
    owner: &str,
) -> Result<String, ReconcileError> {
    if let Ok(existing) = api.get(secret_name).await {
        if let Some(kid) = existing
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(KID_ANNOTATION))
            .cloned()
        {
            return Ok(kid);
        }
        // Secret exists but no kid annotation — could happen when the
        // operator hand-created one. Compute kid from existing public
        // bytes if present, otherwise leave empty.
        let pub_bytes = existing
            .data
            .as_ref()
            .and_then(|d| d.get("signing-key.public"))
            .map(|b| b.0.clone())
            .unwrap_or_default();
        return Ok(kid_from_public_bytes(&pub_bytes));
    }

    let (private_raw, public_raw, kid) = {
        let mut rng = rand::rng();
        let mut seed = [0u8; 32];
        rng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let private_raw: [u8; 32] = signing.to_bytes();
        let public_raw: [u8; 32] = signing.verifying_key().to_bytes();
        let kid = kid_from_public_bytes(&public_raw);
        (private_raw, public_raw, kid)
    };

    let mut data: BTreeMap<String, ByteString> = BTreeMap::new();
    data.insert(
        "signing-key.private".into(),
        ByteString(private_raw.to_vec()),
    );
    data.insert("signing-key.public".into(), ByteString(public_raw.to_vec()));
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(KID_ANNOTATION.into(), kid.clone());

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(secret_name.into()),
            annotations: Some(annotations),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                ("kars.azure.com/mcp-server".into(), owner.into()),
            ])),
            ..Default::default()
        },
        type_: Some(SECRET_TYPE.into()),
        data: Some(data),
        ..Default::default()
    };
    api.patch(
        secret_name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&secret),
    )
    .await?;
    tracing::info!(secret = secret_name, kid = %kid, "McpServerSigningKeyCreated");
    Ok(kid)
}

fn kid_from_public_bytes(public: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    if public.is_empty() {
        return String::new();
    }
    let digest = Sha256::digest(public);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..16])
}

/// Slice 4d.3 — per-server OAuth metadata.
///
/// Written by the controller into the `mcp-{name}-jwks` ConfigMap under
/// the `meta.json` key so the router's `McpServerRegistry` can build a
/// multi-issuer `OAuthVerifierConfig` keyed by `issuer`. Plural
/// `audiences` because some IdPs (e.g. Entra) issue tokens whose `aud`
/// claim is a list; we accept whichever audience matches the server's
/// configured `audience` (validator handles list-vs-string).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerMeta {
    /// OAuth 2.1 issuer URL.
    pub issuer: String,
    /// Single audience the validator pins on for this server. Optional
    /// because some self-managed MCP servers omit the `aud` claim
    /// (RFC 6749 silence). When absent, the router treats this server's
    /// JWKS as audience-agnostic (the global `MCP_OAUTH_AUDIENCE`
    /// env-var still applies as a floor for the dev-mode legacy path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// OAuth 2.1 scopes the router uses to gate fronted calls. Empty =
    /// no scope requirement at the OAuth layer (per-tool gating lives
    /// in ToolPolicy).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Slice 4d.4 — upstream MCP server URL the router forwards
    /// `tools/call` requests to. Empty when the source `McpServerSpec`
    /// has no `url` (defensive — admission CEL rejects empty URL but
    /// be conservative). The router's forwarder skips servers with an
    /// empty URL.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Slice 4d.4 — allowed-tools allowlist mirrored from
    /// `McpServerSpec.allowedTools`. Empty list = no tools allowed
    /// (fail-closed); `["*"]` = all tools the upstream advertises.
    /// The router's forwarder filters its discovered catalog through
    /// this list before exposing tools to the agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    /// Slice 4d.4.1 — outbound static-bearer source.
    ///
    /// When non-empty, names an environment variable that the router
    /// reads at discovery time and attaches as
    /// `Authorization: Bearer <env value>` on every outbound MCP call
    /// to this server. Empty (default) = no outbound auth.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bearer_from_env: String,
}

impl McpServerMeta {
    /// Build the meta record from a reconciled `McpServerSpec`.
    pub fn from_spec(spec: &crate::mcp_server::McpServerSpec) -> Self {
        let (issuer, audience) = match spec.oauth.as_ref() {
            Some(o) => (
                o.issuer.clone(),
                o.audience.clone().filter(|a| !a.is_empty()),
            ),
            None => (String::new(), None),
        };
        Self {
            issuer,
            audience,
            scopes: spec.scopes.clone().unwrap_or_default(),
            url: spec.url.clone().unwrap_or_default(),
            allowed_tools: spec.allowed_tools.clone().unwrap_or_default(),
            bearer_from_env: spec.bearer_from_env.clone().unwrap_or_default(),
        }
    }
}

async fn ensure_jwks_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    owner: &str,
    raw_jwks: &[u8],
    meta: &McpServerMeta,
) -> Result<(), ReconcileError> {
    let s = match std::str::from_utf8(raw_jwks) {
        Ok(s) => s.to_string(),
        Err(_) => return Ok(()), // skip — invalid_jwks_format already classified
    };
    let meta_json = serde_json::to_string(meta).unwrap_or_else(|_| "{}".to_string());
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("jwks.json".into(), s);
    // Slice 4d.3 — per-server OAuth metadata consumed by the router's
    // `McpServerRegistry`. Keys: `issuer`, `audience`, `scopes`. The
    // router builds a multi-issuer `OAuthVerifierConfig` from these
    // mirrored ConfigMaps so each McpServer's tokens are validated
    // against that server's JWKS + audience.
    data.insert("meta.json".into(), meta_json);
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.into()),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                ("kars.azure.com/mcp-server".into(), owner.into()),
            ])),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };
    api.patch(
        cm_name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&cm),
    )
    .await?;
    Ok(())
}

async fn finalize(
    client: &Client,
    api: &Api<McpServer>,
    secrets: &Api<Secret>,
    configmaps: &Api<ConfigMap>,
    mcp: &McpServer,
    name: &str,
) -> Result<Action, ReconcileError> {
    let secret_name = format!("mcp-{name}-signing");
    let cm_name = format!("mcp-{name}-jwks");
    secrets
        .delete(&secret_name, &Default::default())
        .await
        .map(|_| ())
        .or_else(|e: kube::Error| -> Result<(), kube::Error> {
            if matches!(e, kube::Error::Api(ref ae) if ae.code == 404) {
                Ok(())
            } else {
                Err(e)
            }
        })?;
    configmaps
        .delete(&cm_name, &Default::default())
        .await
        .map(|_| ())
        .or_else(|e: kube::Error| -> Result<(), kube::Error> {
            if matches!(e, kube::Error::Api(ref ae) if ae.code == 404) {
                Ok(())
            } else {
                Err(e)
            }
        })?;

    if let Some(workload_ref) = mcp
        .status
        .as_ref()
        .and_then(|s| s.workload_ref.as_deref())
    {
        cleanup_managed_workload(client, workload_ref).await?;
    }

    let finalizers: Vec<String> = mcp
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"McpServer","metadata":{"finalizers": finalizers}});
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(Action::await_change())
}

fn error_policy(mcp: Arc<McpServer>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    let class = match error {
        ReconcileError::Kube(_) => "kube_api",
        ReconcileError::SerdeJson(_) => "serde",
        ReconcileError::Configuration(_) => "configuration",
    };
    crate::metrics::record_reconcile_error("McpServer", class);
    tracing::warn!(
        mcp = %mcp.name_any(),
        error = %error,
        "McpServer reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

/// Start the controller loop. Non-fatal CRD-missing exit mirrors
/// `pairing_reconciler::run`.
pub async fn run(client: Client) -> Result<()> {
    let mcps: Api<McpServer> = Api::all(client.clone());
    match mcps.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("McpServer CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("McpServer CRD not installed — MCP 2026 reconciler disabled: {e}");
            // Park forever so the tokio::select! in main() does not see
            // this reconciler exit cleanly and tear the whole controller
            // down. The CRD is only optional from the controller's
            // perspective; its absence is operator config, not a fatal
            // condition.
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        jwks_fetcher: Arc::new(HttpJwksFetcher::new()),
        probe_client: reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .expect("MCP probe reqwest client"),
    });
    Controller::new(mcps, crate::watch_config::bounded())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("McpServer", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("McpServer reconciled {:?}", o),
                Err(e) => tracing::warn!("McpServer reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}

/// Resolve the effective spec the reconciler will operate on.
///
/// Slice 1c.5 of `crd-well-oiled-machine` introduces a signed
/// `bundleRef` authoring path for `McpServer`. This helper closes the
/// inline-vs-bundle authoring choice with a single normalised
/// `McpServerSpec` returned to the reconcile loop:
///
/// - **Inline (back-compat, no signature)**: any of `url`, `oauth`,
///   `productionMode`, `scopes`, `allowedTools`, `displayName` set; no
///   `bundleRef`. Returns the spec verbatim. `bundle_ref_digest = None`.
/// - **Signed bundle**: `bundleRef` set, content fields all `None`.
///   Fetches + verifies the OCI artifact via
///   [`crate::policy_fetcher::fetch_and_verify_generic`] parameterised
///   by [`crate::policy_canonical::mcp_server::McpServerKind`]. The
///   bundle's content fields are merged onto the CR's
///   `allowedSandboxes` selector. `bundle_ref_digest = Some(<digest>)`.
/// - **Selector-only**: no `bundleRef` and no content fields set.
///   Acceptable shape (the CR carries only a selector) but
///   `productionMode` defaults to `false` and `url` resolves to empty
///   — the reconciler treats this as a degraded `SpecInvalid` only
///   when `productionMode` would also be `true`; selector-only
///   prod-mode-false is intentionally allowed for in-progress
///   authoring drafts.
/// - **Both inline + bundleRef** *(rejected at runtime as
///   defense-in-depth — admission CEL already rejects)*: returns
///   `(InvalidSpec, msg)` without performing the fetch.
async fn resolve_mcp_source(
    mcp: &crate::mcp_server::McpServer,
) -> (
    crate::mcp_server::McpServerSpec,
    Option<String>,
    Option<(&'static str, String)>,
) {
    let spec = &mcp.spec;
    let inline_any = spec.url.is_some()
        || spec.managed.is_some()
        || spec.oauth.is_some()
        || spec.production_mode.is_some()
        || spec.scopes.is_some()
        || spec.allowed_tools.is_some()
        || spec.display_name.is_some();
    let bundle_set = spec.bundle_ref.is_some();

    if inline_any && bundle_set {
        return (
            // selector-only synthesis; we won't compile this branch
            crate::mcp_server::McpServerSpec {
                allowed_sandboxes: spec.allowed_sandboxes.clone(),
                ..Default::default()
            },
            None,
            Some((
                "InvalidSpec",
                "spec.bundleRef is mutually exclusive with spec.url, spec.managed, spec.oauth, \
                 spec.productionMode, spec.scopes, spec.allowedTools, and \
                 spec.displayName"
                    .into(),
            )),
        );
    }

    if !bundle_set {
        return (spec.clone(), None, None);
    }

    let bundle_ref = spec
        .bundle_ref
        .as_ref()
        .expect("bundle_set implies Some")
        .clone();

    let signer_policy_handle = crate::signer_policy::global();
    let verify_result = match signer_policy_handle.snapshot() {
        crate::signer_policy::SignerPolicyState::FromConfigMap(p) => {
            let cfg: crate::policy_fetcher::SignerPolicyConfig = p.into();
            crate::policy_fetcher::fetch_and_verify_generic::<
                crate::policy_canonical::mcp_server::McpServerKind,
            >(&bundle_ref, &cfg)
            .await
        }
        crate::signer_policy::SignerPolicyState::Malformed(msg) => Err(
            crate::policy_fetcher::FetchError::SignerPolicyMalformed(msg),
        ),
        crate::signer_policy::SignerPolicyState::Absent => {
            let cfg = crate::policy_fetcher::SignerPolicyConfig::from_env();
            crate::policy_fetcher::fetch_and_verify_generic::<
                crate::policy_canonical::mcp_server::McpServerKind,
            >(&bundle_ref, &cfg)
            .await
        }
    };

    match verify_result {
        Ok(verified) => {
            let effective = merge_bundle_with_selector(spec, &verified);
            (effective, Some(verified.digest), None)
        }
        Err(e) => {
            let (reason, msg) = fetch_error_to_degraded(&e);
            tracing::warn!(
                mcpserver = %mcp.name_any(),
                registry = %bundle_ref.registry,
                repository = %bundle_ref.repository,
                digest = %bundle_ref.digest,
                reason,
                "McpServer bundleRef fetch/verify failed: {msg}"
            );
            (
                crate::mcp_server::McpServerSpec {
                    allowed_sandboxes: spec.allowed_sandboxes.clone(),
                    ..Default::default()
                },
                None,
                Some((reason, msg)),
            )
        }
    }
}

/// Merge the verified bundle's content fields onto the CR's
/// `allowedSandboxes` selector. The bundle owns the content; the CR
/// owns the selector — same pattern as InferencePolicy + KarsMemory.
fn merge_bundle_with_selector(
    cr_spec: &crate::mcp_server::McpServerSpec,
    verified: &crate::policy_canonical::mcp_server::VerifiedMcpServerBundle,
) -> crate::mcp_server::McpServerSpec {
    use crate::mcp_server::{McpOAuthConfig, McpServerSpec};

    let oauth = verified.oauth.as_ref().map(|o| McpOAuthConfig {
        issuer: o.issuer.clone(),
        audience: o.audience.clone(),
        resource: o.resource.clone(),
        pkce: o.pkce.clone().unwrap_or_else(|| "S256".to_string()),
    });

    McpServerSpec {
        url: verified.url.clone(),
        managed: None,
        oauth,
        production_mode: verified.production_mode,
        scopes: verified.scopes.clone(),
        allowed_tools: verified.allowed_tools.clone(),
        allowed_sandboxes: cr_spec.allowed_sandboxes.clone(),
        display_name: verified.display_name.clone(),
        bundle_ref: None,
        // Bundle-sourced spec does not carry outbound bearer config —
        // bearer hookup is a CR-level concern (per-deployment), not
        // part of the signed policy bundle.
        bearer_from_env: cr_spec.bearer_from_env.clone(),
    }
}

/// Map [`crate::policy_fetcher::FetchError`] to the `(reason, message)`
/// degraded pair. Mirrors the same helper in the other 1c.x reconcilers
/// — the controller's class table stays closed.
fn fetch_error_to_degraded(e: &crate::policy_fetcher::FetchError) -> (&'static str, String) {
    let reason = crate::policy_fetcher::reason_for_error(e).unwrap_or("Transient");
    (reason, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock fetcher returning a known JWKS.
    #[derive(Debug)]
    struct MockOk;
    #[async_trait::async_trait]
    impl JwksFetcher for MockOk {
        async fn fetch(&self, _: &str) -> Result<FetchedJwks, FetchError> {
            let raw = br#"{"keys":[{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"AAA"}]}"#.to_vec();
            Ok(FetchedJwks {
                jwks_uri: "https://example/.well-known/jwks.json".into(),
                raw,
                key_count: 1,
            })
        }
    }

    /// Mock fetcher always failing with a discovery error.
    #[derive(Debug)]
    struct MockFailDns;
    #[async_trait::async_trait]
    impl JwksFetcher for MockFailDns {
        async fn fetch(&self, _: &str) -> Result<FetchedJwks, FetchError> {
            Err(FetchError::Discovery {
                class: "dns",
                detail: "name resolution failed".into(),
            })
        }
    }

    #[test]
    fn parse_jwks_key_count_works() {
        let raw = br#"{"keys":[{"kid":"a"},{"kid":"b"}]}"#;
        assert_eq!(parse_jwks_key_count(raw).unwrap(), 2);

        let bad = br#"{"foo":"bar"}"#;
        assert!(parse_jwks_key_count(bad).is_err());
    }

    #[test]
    fn kid_from_public_is_deterministic_and_short() {
        let pub_bytes = [0u8; 32];
        let kid = kid_from_public_bytes(&pub_bytes);
        // 16 bytes -> URL-safe-no-pad b64 is 22 chars
        assert_eq!(kid.len(), 22);
        assert_eq!(kid, kid_from_public_bytes(&pub_bytes));
        assert!(!kid.contains('='));
    }

    #[test]
    fn build_conditions_emits_three_types_on_success() {
        let conds = build_conditions(&[], Some(7), None, None);
        assert_eq!(conds.len(), 3);
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "True");
        let progressing = conds.iter().find(|c| c.type_ == "Progressing").unwrap();
        assert_eq!(progressing.status, "False");
        let degraded = conds.iter().find(|c| c.type_ == "Degraded").unwrap();
        assert_eq!(degraded.status, "False");
        for c in &conds {
            assert_eq!(c.observed_generation, Some(7));
        }
    }

    #[test]
    fn build_conditions_emits_degraded_true_on_failure() {
        let conds = build_conditions(&[], Some(2), None, Some(("JwksFetchFailed", "boom")));
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "JwksFetchFailed");
        let degraded = conds.iter().find(|c| c.type_ == "Degraded").unwrap();
        assert_eq!(degraded.status, "True");
        assert_eq!(degraded.message, "boom");
    }

    #[test]
    fn build_conditions_preserves_transition_time_on_repeat_success() {
        let prior = build_conditions(&[], Some(1), None, None);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let next = build_conditions(&prior, Some(1), None, None);
        let p_ready = prior.iter().find(|c| c.type_ == "Ready").unwrap();
        let n_ready = next.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(p_ready.last_transition_time, n_ready.last_transition_time);
    }

    #[test]
    fn build_conditions_stamps_new_time_on_status_flip() {
        let prior = build_conditions(&[], Some(1), None, None);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let next = build_conditions(&prior, Some(1), None, Some(("JwksFetchFailed", "x")));
        let p_ready = prior.iter().find(|c| c.type_ == "Ready").unwrap();
        let n_ready = next.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_ne!(p_ready.last_transition_time, n_ready.last_transition_time);
    }

    #[test]
    fn build_conditions_pending_is_not_ready_or_degraded() {
        let conds = build_conditions(
            &[],
            Some(4),
            Some("managed workload is starting"),
            None,
        );
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        let progressing = conds.iter().find(|c| c.type_ == "Progressing").unwrap();
        let degraded = conds.iter().find(|c| c.type_ == "Degraded").unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(progressing.status, "True");
        assert_eq!(degraded.status, "False");
    }

    #[test]
    fn managed_playwright_plan_derives_internal_endpoint_and_pinned_image() {
        let plan = managed_workload_plan(
            "kars-system",
            "browser",
            Some("uid-browser"),
            &ManagedMcpPreset::Playwright,
        );
        assert_eq!(plan.namespace, "kars-mcp");
        assert_eq!(
            plan.endpoint(),
            format!(
                "http://{}.kars-mcp.svc.cluster.local:8931/mcp",
                plan.workload_name
            )
        );
        assert!(plan.image.contains("@sha256:"));
        assert!(
            plan.args
                .iter()
                .any(|a| a.contains(&format!(
                    "{}.kars-mcp.svc.cluster.local:8931",
                    plan.workload_name
                )))
        );
    }

    #[test]
    fn managed_everything_plan_uses_hermetic_kars_image() {
        let plan = managed_workload_plan(
            "kars-system",
            "utility",
            Some("uid-utility"),
            &ManagedMcpPreset::Everything,
        );
        assert_eq!(plan.port, 3001);
        assert_eq!(plan.image, EVERYTHING_IMAGE_DEFAULT);
        assert_eq!(plan.env, vec![("PORT".into(), "3001".into())]);
    }

    #[test]
    fn managed_workload_identity_is_unique_per_source_object() {
        let a = managed_workload_plan(
            "tenant-a",
            "browser",
            Some("uid-a"),
            &ManagedMcpPreset::Playwright,
        );
        let b = managed_workload_plan(
            "tenant-b",
            "browser",
            Some("uid-b"),
            &ManagedMcpPreset::Playwright,
        );
        assert_ne!(a.workload_name, b.workload_name);
        assert!(a.workload_name.len() <= 63);
        assert!(b.workload_name.len() <= 63);
    }

    #[test]
    fn fetch_error_class_buckets_are_safe_strings() {
        // Audit-event policy: error_class is always a fixed bucket,
        // never a raw error message. Verify the enum's `class()` method
        // only ever yields one of the documented strings.
        for class in [
            FetchError::Discovery {
                class: "dns",
                detail: "x".into(),
            }
            .class(),
            FetchError::Discovery {
                class: "tls",
                detail: "x".into(),
            }
            .class(),
            FetchError::Discovery {
                class: "timeout",
                detail: "x".into(),
            }
            .class(),
            FetchError::Discovery {
                class: "http_status",
                detail: "x".into(),
            }
            .class(),
            FetchError::Jwks {
                class: "tls",
                detail: "x".into(),
            }
            .class(),
            FetchError::InvalidJwks("x".into()).class(),
        ] {
            assert!(
                matches!(
                    class,
                    "dns" | "tls" | "timeout" | "http_status" | "invalid_jwks_format"
                ),
                "class={class:?}"
            );
        }
    }

    #[test]
    fn mock_fetchers_compile_and_do_not_panic() {
        // Tokio-free smoke: the trait object is constructable.
        let _ok: Arc<dyn JwksFetcher> = Arc::new(MockOk);
        let _fail: Arc<dyn JwksFetcher> = Arc::new(MockFailDns);
    }

    #[tokio::test]
    async fn mock_ok_returns_one_key() {
        let m = MockOk;
        let f = m.fetch("https://example").await.unwrap();
        assert_eq!(f.key_count, 1);
    }

    #[tokio::test]
    async fn mock_fail_dns_classifies() {
        let m = MockFailDns;
        let e = m.fetch("https://example").await.unwrap_err();
        assert_eq!(e.class(), "dns");
    }
}
