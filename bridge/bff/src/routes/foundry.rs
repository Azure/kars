// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — operator Foundry onboarding.
//
// Discovery configuration and inference routes use the enrolled provider store.
// Connecting never changes sealed controller settings, defaults, or memory bindings.
// Legacy controller projects remain readable until explicitly replaced or disconnected.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

fn upstream(e: kube::Error) -> AppError {
    AppError::from(e)
}

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

#[derive(Debug, Deserialize)]
pub struct FoundryConnectRequest {
    /// The Foundry PROJECT endpoint, e.g.
    /// `https://<res>.services.ai.azure.com/api/projects/<project>`.
    pub project_endpoint: String,
    /// Optional Foundry inference endpoint (Models), e.g.
    /// `https://<res>.openai.azure.com/`.
    #[serde(default)]
    pub inference_endpoint: Option<String>,
    /// Optional default memory store id for team knowledge commons.
    #[serde(default)]
    pub memory_store_id: Option<String>,
    /// "api" or "managed-identity".
    pub auth: String,
    /// The project API key — required (and only used) for `auth = "api"`.
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FoundryStatus {
    pub connected: bool,
    pub project_endpoint: Option<String>,
    pub inference_endpoint: Option<String>,
    pub memory_store_id: Option<String>,
    /// "api", "managed-identity", or null when not connected. Derived from
    /// whether a key is wired.
    pub auth: Option<String>,
    /// True when an API key is wired (the key itself is never returned).
    pub has_api_key: bool,
    pub connections: Option<Vec<FoundryConnection>>,
    pub services_discovered_at: Option<String>,
    /// Default for unbound agents. Explicit KarsMemory bindings always select Foundry.
    pub memory_backend: &'static str,
}

/// Extract the host from an https URL, for DNS checks.
fn host_of(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    url.host_str().map(str::to_string)
}

/// Only Azure's documented resource aliases may share a saved credential.
fn azure_resource(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = host_of(url)?;
    if parsed.port_or_known_default() != Some(443)
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    [
        ".services.ai.azure.com",
        ".openai.azure.com",
        ".cognitiveservices.azure.com",
    ]
    .iter()
    .find_map(|suffix| {
        host.strip_suffix(suffix)
            .filter(|name| !name.is_empty() && !name.contains('.'))
            .map(str::to_owned)
    })
}

fn project_inference_endpoint(project: &str) -> Result<String, AppError> {
    let mut url = reqwest::Url::parse(project).map_err(|_| {
        AppError::BadRequest("Enter a Foundry project URL ending in /api/projects/<project>".into())
    })?;
    let path = url.path().trim_end_matches('/');
    let valid = path
        .strip_prefix("/api/projects/")
        .is_some_and(|name| !name.is_empty() && !name.contains('/') && !name.contains('%'));
    if azure_resource(project).is_none()
        || !url
            .host_str()
            .is_some_and(|h| h.ends_with(".services.ai.azure.com"))
        || !valid
    {
        return Err(AppError::BadRequest("Enter an HTTPS Foundry project URL ending in /api/projects/<project>, without credentials, a query, or a fragment".into()));
    }
    url.set_path("/");
    Ok(url.to_string())
}

fn matching_provider_key(
    keys: &std::collections::BTreeMap<String, String>,
    project: &str,
) -> Result<Option<String>, AppError> {
    let resource = azure_resource(project)
        .ok_or_else(|| AppError::BadRequest("Invalid Foundry resource".into()))?;
    let mut candidates = std::collections::BTreeSet::new();
    for (name, endpoint) in keys {
        if let Some(prefix) = name
            .strip_suffix("_ENDPOINT")
            .filter(|p| p.starts_with("KARS_PROVIDER_"))
            && azure_resource(endpoint).as_ref() == Some(&resource)
            && let Some(key) = keys
                .get(&format!("{prefix}_API_KEY"))
                .filter(|key| !key.trim().is_empty())
        {
            candidates.insert(key.clone());
        }
    }
    if candidates.len() > 1 {
        return Err(AppError::BadRequest("Several different credentials are saved for this Foundry resource. Select API-key authentication and provide the intended key.".into()));
    }
    Ok(candidates.into_iter().next())
}

async fn resolves(host: &str) -> bool {
    tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host(format!("{host}:443")),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .map(|mut it| it.next().is_some())
    .unwrap_or(false)
}

struct DiscoverySnapshot {
    secret: k8s_openapi::api::core::v1::Secret,
    project: Option<crate::kars::credentials::FoundryProject>,
    api_key: Option<String>,
}

async fn discovery_snapshot(
    cluster: &crate::kars::cluster::Cluster,
) -> AppResult<DiscoverySnapshot> {
    use crate::kars::credentials::{FoundryProject, saved_foundry_project};
    let (_, secret) = cluster
        .integration_store(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;
    let (project, api_key) = match saved_foundry_project(&secret).map_err(upstream)? {
        Some(Some(project)) => {
            let read = |name: &str| -> AppResult<Option<String>> {
                secret
                    .data
                    .as_ref()
                    .and_then(|data| data.get(name))
                    .map(|value| {
                        String::from_utf8(value.0.clone()).map_err(|_| {
                            AppError::Upstream("Provider configuration is not UTF-8".into())
                        })
                    })
                    .transpose()
            };
            match read("KARS_PROVIDER_FOUNDRY_ENDPOINT")?.filter(|v| !v.is_empty()) {
                None => (None, None),
                Some(endpoint) if endpoint == project.inference_endpoint => {
                    let key =
                        read("KARS_PROVIDER_FOUNDRY_API_KEY")?.filter(|v| !v.trim().is_empty());
                    if project.api_key_auth != key.is_some() {
                        return Err(AppError::Upstream("Foundry authentication changed; reconnect the project before refreshing".into()));
                    }
                    (Some(project), key)
                }
                Some(_) => return Err(AppError::Upstream(
                    "Foundry provider endpoint changed; reconnect its project before refreshing"
                        .into(),
                )),
            }
        }
        Some(None) => (None, None),
        None => {
            let (project, inference, _, has_key) = cluster
                .get_foundry_connection()
                .await
                .map_err(AppError::Upstream)?;
            let project = project
                .map(|project| -> AppResult<FoundryProject> {
                    let derived = project_inference_endpoint(&project)?;
                    Ok(FoundryProject {
                        project_endpoint: project,
                        inference_endpoint: inference.filter(|v| !v.is_empty()).unwrap_or(derived),
                        api_key_auth: has_key,
                        ..Default::default()
                    })
                })
                .transpose()?;
            let key = if has_key {
                cluster
                    .read_secret_all("kars-system", FOUNDRY_CREDENTIALS_SECRET)
                    .await
                    .map_err(upstream)?
                    .get("FOUNDRY_API_KEY")
                    .filter(|v| !v.trim().is_empty())
                    .cloned()
            } else {
                None
            };
            if has_key && key.is_none() {
                return Err(AppError::Upstream("The configured Foundry API key is unavailable; no identity fallback was attempted".into()));
            }
            (project, key)
        }
    };
    Ok(DiscoverySnapshot {
        secret,
        project,
        api_key,
    })
}

/// `GET /api/operator/foundry` — the current Foundry connection status.
pub async fn get_foundry(State(state): State<AppState>) -> AppResult<Json<FoundryStatus>> {
    let cluster = require_cluster(&state)?;
    let snapshot = discovery_snapshot(cluster).await?;
    let (runtime_project, _, store, _) = cluster
        .get_foundry_connection()
        .await
        .map_err(AppError::Upstream)?;
    let project = snapshot
        .project
        .as_ref()
        .map(|p| p.project_endpoint.clone());
    let inference = snapshot
        .project
        .as_ref()
        .map(|p| p.inference_endpoint.clone());
    let has_key = snapshot.api_key.is_some();
    let connected = project.is_some();
    let auth = if !connected {
        None
    } else if has_key {
        Some("api".to_string())
    } else {
        Some("managed-identity".to_string())
    };
    Ok(Json(FoundryStatus {
        connected,
        project_endpoint: project,
        inference_endpoint: inference,
        memory_store_id: store,
        auth,
        has_api_key: has_key,
        connections: snapshot.project.as_ref().and_then(|p| p.services.clone()),
        services_discovered_at: snapshot
            .project
            .as_ref()
            .and_then(|p| p.services_discovered_at.clone()),
        memory_backend: if runtime_project.is_some() {
            "foundry"
        } else {
            "builtin"
        },
    }))
}

/// `POST /api/operator/foundry` — onboard / update a Foundry connection.
pub async fn connect_foundry(
    State(state): State<AppState>,
    Json(req): Json<FoundryConnectRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let project = req.project_endpoint.trim();
    let derived_inference = project_inference_endpoint(project)?;
    let inference = req
        .inference_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .unwrap_or(&derived_inference);
    if azure_resource(inference) != azure_resource(project) {
        return Err(AppError::BadRequest(
            "The inference endpoint must belong to the same Foundry resource".into(),
        ));
    }
    if req.auth != "api"
        && req.auth != "managed-identity"
        && req.auth != "auto"
        && req.auth != "identity"
    {
        return Err(AppError::BadRequest(
            "auth must be 'auto', 'managed-identity', or 'api'".into(),
        ));
    }
    if req
        .memory_store_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
    {
        return Err(AppError::BadRequest("Project discovery does not change memory configuration. Configure memory through the team's knowledge commons.".into()));
    }
    let (_, expected) = cluster
        .integration_store(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;
    let mut selected_key = if req.auth == "api" {
        req.api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_owned)
    } else {
        None
    };
    if selected_key.is_none() && matches!(req.auth.as_str(), "api" | "auto") {
        let keys = expected
            .data
            .as_ref()
            .into_iter()
            .flatten()
            .map(|(key, value)| {
                String::from_utf8(value.0.clone()).map(|value| (key.clone(), value))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
            .map_err(|_| AppError::Upstream("Provider configuration is not UTF-8".into()))?;
        selected_key = matching_provider_key(&keys, project)?;
        if selected_key.is_none() {
            let (legacy_project, _, _, has_key) = cluster
                .get_foundry_connection()
                .await
                .map_err(AppError::Upstream)?;
            if has_key
                && legacy_project.as_deref().and_then(azure_resource) == azure_resource(project)
            {
                selected_key = cluster
                    .read_secret_all("kars-system", FOUNDRY_CREDENTIALS_SECRET)
                    .await
                    .map_err(upstream)?
                    .get("FOUNDRY_API_KEY")
                    .filter(|key| !key.trim().is_empty())
                    .cloned();
                if selected_key.is_none() {
                    return Err(AppError::Upstream("The configured Foundry API key is unavailable; no identity fallback was attempted".into()));
                }
            }
        }
    }
    if req.auth == "api" && selected_key.is_none() {
        return Err(AppError::BadRequest(
            "No matching saved key. Provide a Foundry API key or select Azure identity.".into(),
        ));
    }
    let aad = if selected_key.is_none() {
        foundry_bearer_token().await
    } else {
        None
    };
    if selected_key.is_none() && aad.is_none() {
        return Err(AppError::BadRequest("No matching saved API key or Azure identity is available for project discovery. Nothing was saved.".into()));
    }
    let http = foundry_https_client(Duration::from_secs(10))
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let apply = |rb: reqwest::RequestBuilder| {
        if let Some(key) = selected_key.as_deref() {
            rb.header("api-key", key)
        } else if let Some((token, _)) = &aad {
            rb.bearer_auth(token)
        } else {
            rb
        }
    };
    let url = format!(
        "{}/deployments?api-version={FOUNDRY_API_VERSION}",
        project.trim_end_matches('/')
    );
    let models = list_names(&discover_pages(&http, &url, &apply).await.map_err(|e| {
        AppError::Upstream(format!(
            "Deployment discovery failed; nothing was saved: {e}"
        ))
    })?);
    let connection = crate::kars::credentials::FoundryProject {
        project_endpoint: project.to_string(),
        inference_endpoint: inference.to_string(),
        api_key_auth: selected_key.is_some(),
        ..Default::default()
    };
    cluster
        .write_foundry_catalog(
            &expected,
            Some(&connection),
            &models,
            selected_key.as_deref(),
        )
        .await
        .map_err(upstream)?;

    Ok(Json(serde_json::json!({
        "connected": true,
        "auth": if selected_key.is_some() { "api" } else { "managed-identity" },
        "project_endpoint": project,
        "inference_endpoint": inference,
        "note": "Foundry project and discovered model routes saved in the governed provider store. Controller settings, the default model, and existing memory bindings are unchanged. Refresh checks configured services; listing a service does not enable or invoke it.",
    })))
}

/// `DELETE /api/operator/foundry` — disconnect Foundry.
pub async fn disconnect_foundry(
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    cluster
        .require_provider_removable("foundry")
        .await
        .map_err(AppError::from)?;
    let (_, expected) = cluster
        .integration_store(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;
    cluster
        .write_foundry_catalog(&expected, None, &[], None)
        .await
        .map_err(upstream)?;
    Ok(Json(
        serde_json::json!({ "connected": false, "note": "Foundry disconnected; its models were removed from the catalogue." }),
    ))
}

#[derive(Debug, Serialize)]
pub struct FoundryCheck {
    pub label: String,
    pub status: String, // "pass" | "warn" | "fail"
    pub detail: String,
}

#[derive(Debug, Serialize, Default)]
pub struct FoundryDiscovered {
    /// Model deployments available in the project (data-plane `/deployments`).
    pub models: Vec<String>,
    /// True only after complete discovery and a successful catalogue write.
    pub models_catalogued: bool,
    pub models_discovered: bool,
    pub connections_discovered: bool,
    /// Connected services (Bing grounding, storage, etc.) — data-plane `/connections`.
    pub connections: Vec<FoundryConnection>,
    /// Whether the configured memory store id was found among the project's
    /// agent memory stores. `None` when no store id is configured or not checked.
    pub memory_store_found: Option<bool>,
}

pub use crate::kars::credentials::FoundryService as FoundryConnection;

#[derive(Debug, Serialize)]
pub struct FoundryVerifyResult {
    pub checks: Vec<FoundryCheck>,
    pub discovered: FoundryDiscovered,
}

/// The Foundry data-plane api-version (2025 GA).
const FOUNDRY_API_VERSION: &str = "2025-05-01";
/// The OAuth2 scope for the Foundry project data-plane.
const FOUNDRY_SCOPE: &str = "https://ai.azure.com/.default";

fn foundry_https_client(timeout: Duration) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .https_only(true)
        // Neither the federated assertion nor a data-plane key may follow a redirect.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
}

/// Acquire an AAD bearer token for the Foundry data-plane using the same
/// no-Azure-SDK REST paths the router uses, mirroring DefaultAzureCredential's
/// order: (1) AKS **workload identity** (federated token file → AAD exchange),
/// (2) **IMDS** managed identity, (3) the developer's **Azure CLI** login (local
/// dev — a kind cluster has no managed identity). Returns `(token, source)` or
/// `None` when no credential is available.
async fn foundry_bearer_token() -> Option<(String, &'static str)> {
    let client = foundry_https_client(Duration::from_secs(8)).ok()?;

    // (1) Workload identity: federated token file + AAD token endpoint.
    if let (Ok(client_id), Ok(tenant), Ok(token_file)) = (
        std::env::var("AZURE_CLIENT_ID"),
        std::env::var("AZURE_TENANT_ID"),
        std::env::var("AZURE_FEDERATED_TOKEN_FILE"),
    ) && let Ok(assertion) = std::fs::read_to_string(&token_file)
    {
        let authority = std::env::var("AZURE_AUTHORITY_HOST")
            .unwrap_or_else(|_| "https://login.microsoftonline.com".into());
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            authority.trim_end_matches('/'),
            tenant
        );
        let form = [
            ("client_id", client_id.as_str()),
            ("scope", FOUNDRY_SCOPE),
            ("grant_type", "client_credentials"),
            (
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            ),
            ("client_assertion", assertion.trim()),
        ];
        if let Ok(resp) = client.post(&url).form(&form).send().await
            && let Ok(v) = resp.json::<serde_json::Value>().await
            && let Some(t) = v.get("access_token").and_then(|t| t.as_str())
        {
            return Some((t.to_string(), "workload identity"));
        }
    }

    // (2) IMDS is a fixed link-local HTTP service, not a secret-bearing outbound
    // request. Keep it off proxies and redirects, separate from the HTTPS client.
    if let Ok(imds) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        && let Ok(resp) = imds
            .get("http://169.254.169.254/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https://ai.azure.com/")
            .header("Metadata", "true")
            .send()
            .await
        && let Ok(v) = resp.json::<serde_json::Value>().await
        && let Some(t) = v.get("access_token").and_then(|t| t.as_str())
    {
        return Some((t.to_string(), "managed identity"));
    }

    // (3) Azure CLI (local dev) — the developer's existing `az login`, mirroring
    // DefaultAzureCredential's AzureCliCredential fallback. This only READS an
    // existing session (`az account get-access-token`); it never runs `az login`.
    // Gated behind an explicit opt-in so the BFF never shells out to `az`
    // autonomously — the admin sets KARS_FOUNDRY_ALLOW_AZ_CLI=1 when they want
    // dev discovery via their own Azure login.
    let az_allowed = matches!(
        std::env::var("KARS_FOUNDRY_ALLOW_AZ_CLI").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    );
    if az_allowed
        && let Ok(out) = tokio::process::Command::new("az")
            .args([
                "account",
                "get-access-token",
                "--resource",
                "https://ai.azure.com",
                "--output",
                "json",
            ])
            .output()
            .await
        && out.status.success()
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        && let Some(t) = v.get("accessToken").and_then(|t| t.as_str())
    {
        return Some((t.to_string(), "your Azure CLI login"));
    }
    None
}

/// True when the Azure-CLI discovery fallback is explicitly enabled.
fn az_cli_enabled() -> bool {
    matches!(
        std::env::var("KARS_FOUNDRY_ALLOW_AZ_CLI").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// The tag Foundry's discovered models are registered under in the shared
/// `kars-inference-providers` Secret (see `operator::list_additional_providers`
/// / `build_options` in `options.rs`) — this is what makes a Foundry
/// deployment show up in the Model catalogue tagged `foundry`, and lets an
/// InferencePolicy route to it, with NO separate wizard pass.
const INFERENCE_PROVIDERS_SECRET: &str = "kars-inference-providers";
const INFERENCE_PROVIDERS_NS: &str = "kars-system";
const FOUNDRY_CREDENTIALS_SECRET: &str = "kars-foundry-credentials";

/// Refresh deployments and configured services. Catalogue publication is fenced
/// to the provider snapshot used for discovery, without changing memory bindings.
pub async fn verify_foundry(State(state): State<AppState>) -> AppResult<Json<FoundryVerifyResult>> {
    let cluster = require_cluster(&state)?;
    let snapshot = discovery_snapshot(cluster).await?;
    let mut checks: Vec<FoundryCheck> = Vec::new();
    let mut discovered = FoundryDiscovered::default();

    let Some(connection) = snapshot.project.as_ref() else {
        checks.push(FoundryCheck {
            label: "Foundry is not connected".into(),
            status: "fail".into(),
            detail: "Onboard a Foundry project first.".into(),
        });
        return Ok(Json(FoundryVerifyResult { checks, discovered }));
    };
    let project = connection
        .project_endpoint
        .trim_end_matches('/')
        .to_string();
    project_inference_endpoint(&project)?;
    if azure_resource(&connection.inference_endpoint) != azure_resource(&project) {
        return Err(AppError::BadRequest(
            "Saved inference endpoint belongs to another Foundry resource".into(),
        ));
    }

    // 1. DNS reachability of the project endpoint host.
    let host = host_of(&project).unwrap_or_default();
    let dns_ok = !host.is_empty() && resolves(&host).await;
    checks.push(FoundryCheck {
        label: format!("Project endpoint host {host} resolves"),
        status: if dns_ok { "pass" } else { "fail" }.into(),
        detail: if dns_ok {
            "The Foundry project host resolves in DNS.".into()
        } else {
            "The project host did not resolve — check the endpoint URL.".into()
        },
    });

    let http = foundry_https_client(Duration::from_secs(10))
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // Honor the configured authentication method rather than substituting an
    // ambient identity that may not have access to this project.
    let aad = if snapshot.api_key.is_some() {
        None
    } else {
        foundry_bearer_token().await
    };
    let api_key = snapshot.api_key.clone();

    enum FoundryAuth {
        Bearer(String, &'static str),
        ApiKey(String),
    }
    let auth = match (aad, api_key) {
        (Some((tok, src)), _) => Some(FoundryAuth::Bearer(tok, src)),
        (None, Some(key)) => Some(FoundryAuth::ApiKey(key)),
        (None, None) => None,
    };
    let apply = |rb: reqwest::RequestBuilder| match &auth {
        Some(FoundryAuth::Bearer(t, _)) => rb.bearer_auth(t),
        Some(FoundryAuth::ApiKey(k)) => rb.header("api-key", k.as_str()),
        None => rb,
    };

    match &auth {
        Some(mode) => {
            let (label, detail) = match mode {
                FoundryAuth::Bearer(_, src) => (
                    format!("Discovery credential loaded from {src}"),
                    "Obtained an AAD token for the https://ai.azure.com data-plane.".to_string(),
                ),
                FoundryAuth::ApiKey(_) => (
                    "Saved API key loaded for discovery".to_string(),
                    "Using the project API key against the data-plane (dev/local).".to_string(),
                ),
            };
            checks.push(FoundryCheck {
                label,
                status: "pass".into(),
                detail,
            });
            // 3a. Model deployments.
            let url = format!("{project}/deployments?api-version={FOUNDRY_API_VERSION}");
            match discover_pages(&http, &url, &apply).await {
                Ok(items) => {
                    discovered.models = list_names(&items);
                    discovered.models_discovered = true;
                    checks.push(FoundryCheck {
                        label: format!(
                            "{} model deployment(s) discovered",
                            discovered.models.len()
                        ),
                        status: "pass".into(),
                        detail: if discovered.models.is_empty() {
                            "The project has no model deployments yet.".into()
                        } else {
                            discovered.models.join(", ")
                        },
                    });
                }
                Err(e) => checks.push(FoundryCheck {
                    label: "Model deployments".into(),
                    status: "warn".into(),
                    detail: format!(
                        "Could not refresh deployments: {e}. Saved models were retained."
                    ),
                }),
            }
            // 3b. Connected services.
            let url = format!("{project}/connections?api-version={FOUNDRY_API_VERSION}");
            match discover_pages(&http, &url, &apply).await {
                Ok(items) => {
                    discovered.connections = list_connections(&items);
                    discovered.connections_discovered = true;
                    checks.push(FoundryCheck {
                        label: format!("{} connected service(s) discovered", discovered.connections.len()),
                        status: "pass".into(),
                        detail: if discovered.connections.is_empty() { "No connections (e.g. Bing grounding, storage) are attached to this project.".into() } else { discovered.connections.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(", ") },
                    });
                }
                Err(e) => checks.push(FoundryCheck {
                    label: "Connected services".into(),
                    status: "warn".into(),
                    detail: format!("Could not query connections: {e}"),
                }),
            }
            if discovered.models_discovered {
                let mut refreshed = connection.clone();
                if discovered.connections_discovered {
                    refreshed.services = Some(discovered.connections.clone());
                    refreshed.services_discovered_at = Some(chrono::Utc::now().to_rfc3339());
                }
                match cluster
                    .write_foundry_catalog(
                        &snapshot.secret,
                        Some(&refreshed),
                        &discovered.models,
                        snapshot.api_key.as_deref(),
                    )
                    .await
                {
                    Ok(()) => {
                        discovered.models_catalogued = true;
                        checks.push(FoundryCheck {
                            label: "Model catalogue updated".into(),
                            status: "pass".into(),
                            detail: format!(
                                "{} model(s) tagged `foundry` are now available to missions and teams. The default selection is unchanged.",
                                discovered.models.len()
                            ),
                        });
                    }
                    Err(e) => checks.push(FoundryCheck {
                        label: "Model catalogue sync".into(),
                        status: "warn".into(),
                        detail: format!("Discovery succeeded but the catalogue update failed: {e}"),
                    }),
                }
            }
        }
        None => {
            let hint = if az_cli_enabled() {
                "Couldn't get an AAD token from workload identity, IMDS, or the Azure CLI, and no API key is stored. On AKS, federate a workload identity with 'Azure AI User' on the project; in dev, connect with an API key or run `az login`."
            } else {
                "No credential available for discovery. In dev/local, connect with an API key (Advanced → Use an API key) — the data-plane accepts it directly. On AKS, use workload identity."
            };
            checks.push(FoundryCheck {
                label: "No credential available for discovery".into(),
                status: "warn".into(),
                detail: hint.into(),
            });
        }
    }

    Ok(Json(FoundryVerifyResult { checks, discovered }))
}

/// Preview deployments without changing a provider, catalogue, or default.
pub(crate) async fn discover_azure_models(
    state: &AppState,
    endpoint: &str,
    supplied_key: Option<&str>,
) -> AppResult<Vec<String>> {
    let cluster = require_cluster(state)?;
    let project = if project_inference_endpoint(endpoint).is_ok() {
        endpoint.to_string()
    } else {
        discovery_snapshot(cluster).await?.project.map(|p| p.project_endpoint)
            .ok_or_else(|| AppError::BadRequest("This inference connection is saved. Add its Foundry project URL under Foundry models & services to enable deployment refresh; an inference URL alone cannot identify the project.".into()))?
    };
    project_inference_endpoint(&project)?;
    if azure_resource(endpoint).is_none() || azure_resource(endpoint) != azure_resource(&project) {
        return Err(AppError::BadRequest("The connected Foundry project belongs to a different inference resource. Use this resource's project URL for discovery.".into()));
    }
    let key = match supplied_key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => Some(key.to_string()),
        None => {
            let keys = cluster
                .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
                .await
                .map_err(upstream)?;
            matching_provider_key(&keys, &project)?
        }
    };
    let bearer = if key.is_none() {
        foundry_bearer_token().await
    } else {
        None
    };
    if key.is_none() && bearer.is_none() {
        return Err(AppError::BadRequest(
            "No matching saved resource key or Azure identity is available for discovery.".into(),
        ));
    }
    let http =
        foundry_https_client(Duration::from_secs(10)).map_err(|e| AppError::Internal(e.into()))?;
    let url = format!(
        "{}/deployments?api-version={FOUNDRY_API_VERSION}",
        project.trim_end_matches('/')
    );
    let rows = discover_pages(&http, &url, |request| {
        if let Some(key) = &key {
            request.header("api-key", key)
        } else if let Some((token, _)) = &bearer {
            request.bearer_auth(token)
        } else {
            request
        }
    })
    .await
    .map_err(AppError::Upstream)?;
    Ok(list_names(&rows))
}

async fn discover_pages(
    http: &reqwest::Client,
    url: &str,
    apply: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<Vec<serde_json::Value>, String> {
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        collect_pages(url, |url| {
            let request = apply(http.get(url));
            async move {
                let mut response = request
                    .send()
                    .await
                    .map_err(|_| "Request failed".to_string())?;
                if !response.status().is_success() {
                    return Err(format!(
                        "Data-plane returned HTTP {}",
                        response.status().as_u16()
                    ));
                }
                let mut body = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| "Response read failed".to_string())?
                {
                    if body.len() + chunk.len() > 8 * 1024 * 1024 {
                        return Err("Discovery response exceeds the 8 MiB safety limit".into());
                    }
                    body.extend_from_slice(&chunk);
                }
                serde_json::from_slice(&body).map_err(|_| "Invalid discovery JSON".to_string())
            }
        }),
    )
    .await;
    result.map_err(|_| "Discovery timed out; no partial results were saved".to_string())?
}

async fn collect_pages<F, Fut>(url: &str, mut fetch: F) -> Result<Vec<serde_json::Value>, String>
where
    F: FnMut(reqwest::Url) -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value, String>>,
{
    let first = reqwest::Url::parse(url).map_err(|_| "Invalid discovery URL".to_string())?;
    if host_of(url).is_none() || first.fragment().is_some() {
        return Err("Discovery requires an HTTPS URL without credentials or fragments".into());
    }
    let mut next = Some(first.clone());
    let mut visited = std::collections::HashSet::new();
    let mut items = Vec::new();
    let mut bytes = 0;
    while let Some(url) = next {
        if !visited.insert(url.to_string()) {
            return Err("Discovery pagination loop".into());
        }
        let page = fetch(url).await?;
        let rows = page
            .get("value")
            .and_then(serde_json::Value::as_array)
            .ok_or("Discovery response is missing its value array")?;
        if rows.iter().any(|row| {
            row.get("name")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|name| name.trim().is_empty())
        }) {
            return Err("Discovery response contains an invalid name".into());
        }
        bytes += page.to_string().len();
        if bytes > 8 * 1024 * 1024 {
            return Err("Complete discovery exceeds the 8 MiB safety limit".into());
        }
        next = match page.get("nextLink") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(link)) if link.is_empty() => None,
            Some(serde_json::Value::String(link)) => {
                let url = first.join(link).map_err(|_| "Invalid discovery nextLink")?;
                // Never send the saved credential to another origin or API path.
                if url.origin() != first.origin()
                    || url.path() != first.path()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                {
                    return Err("Discovery nextLink is outside the requested collection".into());
                }
                Some(url)
            }
            Some(_) => return Err("Invalid discovery nextLink".into()),
        };
        items.extend(rows.iter().cloned());
    }
    Ok(items)
}

fn list_names(items: &[serde_json::Value]) -> Vec<String> {
    let mut names = items
        .iter()
        .filter_map(|x| x.get("name")?.as_str().map(String::from))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn list_connections(items: &[serde_json::Value]) -> Vec<FoundryConnection> {
    items
        .iter()
        .filter_map(|x| {
            let name = x.get("name")?.as_str()?.to_string();
            let category = x
                .get("properties")
                .and_then(|p| p.get("category").or_else(|| p.get("connectionType")))
                .and_then(|c| c.as_str())
                .map(String::from)
                .or_else(|| x.get("type").and_then(|t| t.as_str()).map(String::from));
            Some(FoundryConnection { name, category })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_url_derives_inference_without_guessing_project_names() {
        assert_eq!(
            project_inference_endpoint(
                "https://resource.services.ai.azure.com/api/projects/project/"
            )
            .unwrap(),
            "https://resource.services.ai.azure.com/"
        );
        for invalid in [
            "https://resource.openai.azure.com/",
            "https://resource.services.ai.azure.com/",
            "https://resource.services.ai.azure.com/api/projects/",
            "https://resource.services.ai.azure.com/api/projects/p/extra",
            "https://resource.services.ai.azure.com/api/projects/p?x=1",
            "https://resource.services.ai.azure.com/api/projects/p#x",
            "https://resource.services.ai.azure.com.evil.test/api/projects/p",
            "https://user@resource.services.ai.azure.com/api/projects/p",
            "http://resource.services.ai.azure.com/api/projects/p",
        ] {
            assert!(project_inference_endpoint(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn saved_key_reuse_is_bound_to_resource_and_rejects_ambiguity() {
        let project = "https://resource.services.ai.azure.com/api/projects/project";
        let mut keys = std::collections::BTreeMap::from([
            (
                "KARS_PROVIDER_AZURE_OPENAI_ENDPOINT".into(),
                "https://resource.cognitiveservices.azure.com/".into(),
            ),
            (
                "KARS_PROVIDER_AZURE_OPENAI_API_KEY".into(),
                "test-key".into(),
            ),
        ]);
        assert_eq!(
            matching_provider_key(&keys, project).unwrap().as_deref(),
            Some("test-key")
        );
        for unsafe_endpoint in [
            "https://other.openai.azure.com/",
            "https://resource.openai.azure.com.evil.test/",
            "https://sub.resource.openai.azure.com/",
            "http://resource.openai.azure.com/",
            "https://resource.openai.azure.com:444/",
            "https://user@resource.openai.azure.com/",
        ] {
            keys.insert(
                "KARS_PROVIDER_AZURE_OPENAI_ENDPOINT".into(),
                unsafe_endpoint.into(),
            );
            assert!(
                matching_provider_key(&keys, project).unwrap().is_none(),
                "{unsafe_endpoint}"
            );
        }
        keys.insert(
            "KARS_PROVIDER_AZURE_OPENAI_ENDPOINT".into(),
            "https://resource.openai.azure.com/".into(),
        );
        keys.insert(
            "KARS_PROVIDER_SECOND_ENDPOINT".into(),
            "https://resource.services.ai.azure.com/".into(),
        );
        keys.insert("KARS_PROVIDER_SECOND_API_KEY".into(), "test-key".into());
        assert_eq!(
            matching_provider_key(&keys, project).unwrap().as_deref(),
            Some("test-key")
        );
        keys.insert(
            "KARS_PROVIDER_SECOND_API_KEY".into(),
            "different-key".into(),
        );
        assert!(matching_provider_key(&keys, project).is_err());
    }

    #[test]
    fn host_of_extracts_hostname() {
        assert_eq!(
            host_of("https://r.services.ai.azure.com/api/projects/p").as_deref(),
            Some("r.services.ai.azure.com")
        );
        assert_eq!(
            host_of("https://x.openai.azure.com:443/").as_deref(),
            Some("x.openai.azure.com")
        );
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn project_host_requires_https_without_url_credentials() {
        for url in [
            "http://r.services.ai.azure.com/api/projects/p",
            "https://",
            "https://user@r.services.ai.azure.com/api/projects/p",
            "https://user:password@r.services.ai.azure.com/api/projects/p",
        ] {
            assert_eq!(host_of(url), None, "{url}");
        }
    }

    #[tokio::test]
    async fn credential_client_rejects_plaintext_before_connecting() {
        let client = foundry_https_client(Duration::from_secs(1)).unwrap();
        for url in [
            "http://127.0.0.1:9/oauth2/v2.0/token",
            "http://127.0.0.1:9/api/projects/p/deployments",
        ] {
            let error = client.post(url).send().await.unwrap_err();
            assert!(error.is_builder(), "{error}");
        }
    }

    #[test]
    fn list_names_parses_deployments() {
        let v = vec![
            json!({"name":"gpt-4o"}),
            json!({"name":"o3-mini"}),
            json!({"name":"gpt-4o"}),
        ];
        assert_eq!(
            list_names(&v),
            vec!["gpt-4o".to_string(), "o3-mini".to_string()]
        );
        assert!(list_names(&[]).is_empty());
    }

    #[tokio::test]
    async fn discovery_collects_all_pages_and_accepts_genuinely_empty_results() {
        let mut count = 0;
        let items = collect_pages(
            "https://r.services.ai.azure.com/api/projects/p/deployments",
            |url| {
                count += 1;
                let page = if url.query().is_none() {
                    json!({"value":[{"name":"first"}],"nextLink":"?page=2"})
                } else {
                    json!({"value":[{"name":"second"}]})
                };
                std::future::ready(Ok(page))
            },
        )
        .await
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(list_names(&items), ["first", "second"]);
        assert!(
            collect_pages("https://r.test/deployments", |_| std::future::ready(Ok(
                json!({"value":[]})
            )))
            .await
            .unwrap()
            .is_empty()
        );
    }

    #[tokio::test]
    async fn discovery_rejects_malformed_partial_and_looping_responses() {
        for invalid in [
            json!({}),
            json!({"value":{}}),
            json!({"value":[{}]}),
            json!({"value":[{"name":" "}]}),
            json!({"value":[],"nextLink":42}),
        ] {
            assert!(
                collect_pages("https://r.test/deployments", |_| std::future::ready(Ok(
                    invalid.clone()
                )))
                .await
                .is_err()
            );
        }
        let mut count = 0;
        let result = collect_pages("https://r.test/deployments", |_| {
            count += 1;
            std::future::ready(if count == 1 {
                Ok(json!({"value":[{"name":"first"}],"nextLink":"?page=2"}))
            } else {
                Err("HTTP 403".into())
            })
        })
        .await;
        assert!(
            result.is_err(),
            "partial results must never reach catalogue sync"
        );
        assert_eq!(count, 2);
        assert!(
            collect_pages("https://r.test/deployments", |_| std::future::ready(Ok(
                json!({"value":[],"nextLink":"https://r.test/deployments"})
            )))
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn discovery_never_follows_credential_unsafe_next_links() {
        for link in [
            "http://r.test/deployments",
            "https://elsewhere.test/deployments",
            "https://r.test:444/deployments",
            "/other",
            "//elsewhere.test/deployments",
            "https://user@r.test/deployments",
            "?page=2#fragment",
        ] {
            let mut calls = 0;
            let result = collect_pages("https://r.test/deployments", |_| {
                calls += 1;
                std::future::ready(Ok(json!({"value":[],"nextLink":link})))
            })
            .await;
            assert!(result.is_err(), "{link}");
            assert_eq!(calls, 1, "credential must not be sent to {link}");
        }
    }

    #[test]
    fn list_connections_parses_name_and_category() {
        let v = json!({"value":[
            {"name":"bing","properties":{"category":"GroundingWithBingSearch"}},
            {"name":"blob","type":"AzureStorageAccount"}
        ]});
        let c = list_connections(v["value"].as_array().unwrap());
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "bing");
        assert_eq!(c[0].category.as_deref(), Some("GroundingWithBingSearch"));
        assert_eq!(c[1].category.as_deref(), Some("AzureStorageAccount"));
    }
}
