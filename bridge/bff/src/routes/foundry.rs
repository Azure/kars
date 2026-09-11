// kars Bridge BFF — operator Foundry onboarding.
//
// The admin connects an Azure AI Foundry project so the cluster can use Foundry
// services (memory store, connections, models). Two auth modes, matching how
// kars authenticates everywhere else:
//   • api               — a project API key (dev / non-AKS). Stored write-only in
//                          a Secret and wired into the controller via secretKeyRef.
//   • managed-identity  — the cluster's workload identity (AKS). No secret; the
//                          router exchanges an IMDS token for the Foundry
//                          data-plane audience (https://ai.azure.com) at runtime.
//
// Onboarding patches the `kars-controller` Deployment env (FOUNDRY_PROJECT_ENDPOINT
// etc.), which the controller propagates to every sandbox router. `verify` runs a
// real preflight: for api mode a live authenticated call to the project endpoint;
// for managed-identity mode DNS reachability + a workload-identity-wired check.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

fn upstream(e: kube::Error) -> AppError {
    AppError::Upstream(e.to_string())
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
}

/// Extract the host from an https URL, for DNS checks.
fn host_of(url: &str) -> Option<String> {
    let s = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    Some(s.split(['/', ':']).next().unwrap_or(s).to_lowercase())
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

/// `GET /api/operator/foundry` — the current Foundry connection status.
pub async fn get_foundry(State(state): State<AppState>) -> AppResult<Json<FoundryStatus>> {
    let cluster = require_cluster(&state)?;
    let (project, inference, store, has_key) = cluster.get_foundry_connection().await;
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
    }))
}

/// `POST /api/operator/foundry` — onboard / update a Foundry connection.
pub async fn connect_foundry(
    State(state): State<AppState>,
    Json(req): Json<FoundryConnectRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let project = req.project_endpoint.trim();
    if !project.starts_with("https://") || host_of(project).is_none() {
        return Err(AppError::BadRequest(
            "project_endpoint must be an https:// Foundry project URL".into(),
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
    let use_key = req.auth == "api";

    let mut key_secret: Option<(String, String)> = None;
    if use_key {
        let key = req
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                AppError::BadRequest("auth=api requires a Foundry project api_key".into())
            })?;
        let secret = "kars-foundry-credentials";
        cluster
            .upsert_secret(
                "kars-system",
                secret,
                serde_json::json!({
                    "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                    "metadata": {"name": secret, "namespace": "kars-system", "labels": {"app.kubernetes.io/managed-by": "kars-bridge"}},
                    "stringData": {"FOUNDRY_API_KEY": key},
                }),
            )
            .await
            .map_err(upstream)?;
        key_secret = Some((secret.to_string(), "FOUNDRY_API_KEY".to_string()));
    }

    let key_ref = key_secret.as_ref().map(|(s, k)| (s.as_str(), k.as_str()));
    cluster
        .set_foundry_connection(
            project,
            req.inference_endpoint.as_deref(),
            req.memory_store_id.as_deref(),
            key_ref,
        )
        .await
        .map_err(upstream)?;

    Ok(Json(serde_json::json!({
        "connected": true,
        "auth": req.auth,
        "project_endpoint": project,
        "note": if use_key {
            "Foundry connected with an API key (wired into the controller via secretKeyRef and propagated to sandbox routers). The controller is rolling to pick it up. Run Verify to confirm access."
        } else {
            "Foundry connected via the cluster's managed/workload identity — no key stored. Access uses an IMDS token for the https://ai.azure.com audience at runtime. The controller is rolling. Run Verify to confirm."
        },
    })))
}

/// `DELETE /api/operator/foundry` — disconnect Foundry.
pub async fn disconnect_foundry(
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    // Blank the endpoint (the controller treats empty as "unset"). The key
    // secret is left in place (harmless, write-only) but the env stops pointing
    // to it on the next set.
    cluster
        .set_foundry_connection("", None, None, None)
        .await
        .map_err(upstream)?;
    // Also remove any `foundry`-tagged Model catalogue entries — a
    // disconnected project must not leave models an orchestrator or
    // InferencePolicy could still pick (mirrors delete_additional_provider's
    // cleanup for every other tag).
    sync_foundry_catalog_provider(cluster, "", &[], false)
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
    /// Connected services (Bing grounding, storage, etc.) — data-plane `/connections`.
    pub connections: Vec<FoundryConnection>,
    /// Whether the configured memory store id was found among the project's
    /// agent memory stores. `None` when no store id is configured or not checked.
    pub memory_store_found: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct FoundryConnection {
    pub name: String,
    pub category: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FoundryVerifyResult {
    pub checks: Vec<FoundryCheck>,
    pub discovered: FoundryDiscovered,
}

/// The Foundry data-plane api-version (2025 GA).
const FOUNDRY_API_VERSION: &str = "2025-05-01";
/// The OAuth2 scope for the Foundry project data-plane.
const FOUNDRY_SCOPE: &str = "https://ai.azure.com/.default";

/// Acquire an AAD bearer token for the Foundry data-plane using the same
/// no-Azure-SDK REST paths the router uses, mirroring DefaultAzureCredential's
/// order: (1) AKS **workload identity** (federated token file → AAD exchange),
/// (2) **IMDS** managed identity, (3) the developer's **Azure CLI** login (local
/// dev — a kind cluster has no managed identity). Returns `(token, source)` or
/// `None` when no credential is available.
async fn foundry_bearer_token() -> Option<(String, &'static str)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;

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

    // (2) IMDS managed identity.
    let imds = "http://169.254.169.254/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https://ai.azure.com/";
    if let Ok(resp) = client.get(imds).header("Metadata", "true").send().await
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

/// Keep the shared Model catalogue's `foundry`-tagged entries in sync with
/// what's ACTUALLY discovered from the connected project — called after a
/// real (successful or empty) discovery in `verify_foundry`, and with an
/// empty model list from `disconnect_foundry`. This is what removes the
/// second, redundant "Azure AI Foundry" wizard pass the operator used to
/// need: connecting + verifying IS the catalogue registration now.
///
/// When `models` is empty (no deployments discovered, or disconnecting),
/// every `KARS_PROVIDER_FOUNDRY_*` key is removed — mirroring
/// `delete_additional_provider`'s cleanup for any other tag, so a
/// disconnected/model-less Foundry project can never leave stale catalogue
/// entries an orchestrator or InferencePolicy could still pick.
async fn sync_foundry_catalog_provider(
    cluster: &crate::kars::cluster::Cluster,
    endpoint: &str,
    models: &[String],
    has_key: bool,
) -> Result<(), kube::Error> {
    if models.is_empty() {
        return cluster
            .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
                keys.remove("KARS_PROVIDER_FOUNDRY_ENDPOINT");
                keys.remove("KARS_PROVIDER_FOUNDRY_API_KEY");
                keys.remove("KARS_PROVIDER_FOUNDRY_MODELS");
            })
            .await;
    }
    // For auth=api, mirror the SAME key value already stored in
    // kars-foundry-credentials (never re-prompt / re-type it) so the
    // additional-provider entry can actually authenticate in dev. For
    // managed/workload identity, no key is copied — the router's
    // `is_azure_ai_host()` already recognizes `*.services.ai.azure.com`
    // project hosts as eligible for an ambient WI/IMDS token, exactly like
    // the cluster-default Foundry path.
    let api_key = if has_key {
        cluster
            .read_secret_all("kars-system", FOUNDRY_CREDENTIALS_SECRET)
            .await?
            .get("FOUNDRY_API_KEY")
            .cloned()
    } else {
        None
    };
    let endpoint = endpoint.to_string();
    let models_joined = models.join(",");
    cluster
        .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
            keys.insert(
                "KARS_PROVIDER_FOUNDRY_ENDPOINT".to_string(),
                endpoint.clone(),
            );
            keys.insert(
                "KARS_PROVIDER_FOUNDRY_MODELS".to_string(),
                models_joined.clone(),
            );
            if let Some(k) = api_key.clone() {
                keys.insert("KARS_PROVIDER_FOUNDRY_API_KEY".to_string(), k);
            } else {
                keys.remove("KARS_PROVIDER_FOUNDRY_API_KEY");
            }
        })
        .await
}

/// `POST /api/operator/foundry/verify` — a real preflight of the connection,
/// including live discovery of the project's models, services, and memory store.
/// A successful discovery ALSO syncs the discovered models into the shared
/// Model catalogue tagged `foundry` (`sync_foundry_catalog_provider`) — no
/// separate wizard pass needed to make a Foundry deployment usable by a
/// mission or team.
pub async fn verify_foundry(State(state): State<AppState>) -> AppResult<Json<FoundryVerifyResult>> {
    let cluster = require_cluster(&state)?;
    let (project, inference, store, has_key) = cluster.get_foundry_connection().await;
    let mut checks: Vec<FoundryCheck> = Vec::new();
    let mut discovered = FoundryDiscovered::default();
    let _ = inference;

    let Some(project) = project else {
        checks.push(FoundryCheck {
            label: "Foundry is not connected".into(),
            status: "fail".into(),
            detail: "Onboard a Foundry project first.".into(),
        });
        return Ok(Json(FoundryVerifyResult { checks, discovered }));
    };
    let project = project.trim_end_matches('/').to_string();

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

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // 2. Determine the data-plane credential. Prefer an ambient AAD token
    // (workload identity on AKS, IMDS, or the dev az-cli). If none is available
    // but an API key is stored (the dev path — the operator connected with a
    // key), use it directly: the Foundry project data-plane accepts the
    // `api-key` header on /deployments + /connections, so discovery works fully
    // in local/dev with just the key (no in-cluster Azure identity needed).
    let aad = foundry_bearer_token().await;
    let api_key: Option<String> = if aad.is_none() && has_key {
        cluster
            .read_secret_all("kars-system", "kars-foundry-credentials")
            .await
            .ok()
            .and_then(|k| k.get("FOUNDRY_API_KEY").cloned())
            .filter(|k| !k.trim().is_empty())
    } else {
        None
    };

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
                    format!("Authenticated to Foundry via {src}"),
                    "Obtained an AAD token for the https://ai.azure.com data-plane.".to_string(),
                ),
                FoundryAuth::ApiKey(_) => (
                    "Authenticated to Foundry via API key".to_string(),
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
            match apply(http.get(&url)).send().await {
                Ok(r) if r.status().is_success() => {
                    if let Ok(v) = r.json::<serde_json::Value>().await {
                        discovered.models = list_names(&v);
                    }
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
                Ok(r) => checks.push(FoundryCheck {
                    label: "Model deployments".into(),
                    status: "warn".into(),
                    detail: format!(
                        "Data-plane returned {} — check the identity's access.",
                        r.status().as_u16()
                    ),
                }),
                Err(e) => checks.push(FoundryCheck {
                    label: "Model deployments".into(),
                    status: "warn".into(),
                    detail: format!("Could not query deployments: {e}"),
                }),
            }
            // 3b. Connected services.
            let url = format!("{project}/connections?api-version={FOUNDRY_API_VERSION}");
            match apply(http.get(&url)).send().await {
                Ok(r) if r.status().is_success() => {
                    if let Ok(v) = r.json::<serde_json::Value>().await {
                        discovered.connections = list_connections(&v);
                    }
                    checks.push(FoundryCheck {
                        label: format!("{} connected service(s) discovered", discovered.connections.len()),
                        status: "pass".into(),
                        detail: if discovered.connections.is_empty() { "No connections (e.g. Bing grounding, storage) are attached to this project.".into() } else { discovered.connections.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(", ") },
                    });
                }
                Ok(r) => checks.push(FoundryCheck {
                    label: "Connected services".into(),
                    status: "warn".into(),
                    detail: format!(
                        "Data-plane returned {} for /connections.",
                        r.status().as_u16()
                    ),
                }),
                Err(e) => checks.push(FoundryCheck {
                    label: "Connected services".into(),
                    status: "warn".into(),
                    detail: format!("Could not query connections: {e}"),
                }),
            }
            // 3c. Memory: memory stores are PER-TEAM, not cluster-wide — each
            // team's knowledge-commons maps to its own Foundry memory store,
            // configured when the team is set up (p3). Nothing cluster-wide here.
            let _ = &store;

            // 3d. Sync the just-discovered models into the shared Model
            // catalogue, tagged `foundry` — this IS the catalogue
            // registration; no separate wizard pass needed.
            match sync_foundry_catalog_provider(cluster, &project, &discovered.models, has_key)
                .await
            {
                Ok(()) => {
                    if !discovered.models.is_empty() {
                        checks.push(FoundryCheck {
                            label: "Model catalogue updated".into(),
                            status: "pass".into(),
                            detail: format!(
                                "{} model(s) tagged `foundry` are now available to missions and teams.",
                                discovered.models.len()
                            ),
                        });
                    }
                }
                Err(e) => checks.push(FoundryCheck {
                    label: "Model catalogue sync".into(),
                    status: "warn".into(),
                    detail: format!("Discovery succeeded but the catalogue update failed: {e}"),
                }),
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

/// Extract `value[].name` from a Foundry data-plane list response.
fn list_names(v: &serde_json::Value) -> Vec<String> {
    v.get("value")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract connections (name + category) from a `/connections` list response.
fn list_connections(v: &serde_json::Value) -> Vec<FoundryConnection> {
    v.get("value")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| {
                    let name = x.get("name").and_then(|n| n.as_str())?.to_string();
                    let category = x
                        .get("properties")
                        .and_then(|p| p.get("category").or_else(|| p.get("connectionType")))
                        .and_then(|c| c.as_str())
                        .map(String::from)
                        .or_else(|| x.get("type").and_then(|t| t.as_str()).map(String::from));
                    Some(FoundryConnection { name, category })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn list_names_parses_deployments() {
        let v = json!({"value":[{"name":"gpt-4o"},{"name":"o3-mini"},{"noname":1}]});
        assert_eq!(
            list_names(&v),
            vec!["gpt-4o".to_string(), "o3-mini".to_string()]
        );
        assert!(list_names(&json!({})).is_empty());
    }

    #[test]
    fn list_connections_parses_name_and_category() {
        let v = json!({"value":[
            {"name":"bing","properties":{"category":"GroundingWithBingSearch"}},
            {"name":"blob","type":"AzureStorageAccount"}
        ]});
        let c = list_connections(&v);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "bing");
        assert_eq!(c[0].category.as_deref(), Some("GroundingWithBingSearch"));
        assert_eq!(c[1].category.as_deref(), Some("AzureStorageAccount"));
    }
}
