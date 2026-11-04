// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::{is_dns1123_label, require_cluster, upstream};

// ─── Multi-provider inference (§ inference-provider-wizard) ─────────────────
//
// `put_provider` configures controller bootstrap settings. This section manages
// connected providers and the persisted Bridge default for new work without
// modifying that bootstrap configuration. Multiple providers remain available
// at the same time — e.g. GitHub Copilot and Azure AI Foundry — so an InferencePolicy's
// `modelPreference.primary.provider` can route a specific sandbox's calls to
// whichever one actually serves the model it needs (a sub-agent on gpt-4.1
// via Foundry, a principal on opus-4.8 via Copilot, in the SAME cluster).
//
// Storage: the `kars-inference-providers` Secret in `kars-system`. Its KEYS
// are the literal env var names `inference-router::config::Config::from_env`
// already parses generically (`KARS_PROVIDER_<TAG>_ENDPOINT` + optional
// `_API_KEY`/`_TOKEN`). Copilot also retains `COPILOT_GITHUB_TOKEN` for the
// legacy default route, but named routes use their own explicit binding.
// No router-side change is needed to support a
// provider added here. The controller mirrors this ONE secret into every
// sandbox's own namespace (the same mechanism already used for
// `kars-github-app`), and every sandbox's router picks whichever provider a
// request's InferencePolicy names — never all-or-nothing, never guessed from
// what's merely present in the env.
pub(super) const INFERENCE_PROVIDERS_SECRET: &str = "kars-inference-providers";
pub(super) const INFERENCE_PROVIDERS_NS: &str = "kars-system";

pub(super) fn bind_copilot_route(
    keys: &mut std::collections::BTreeMap<String, String>,
    token: &str,
) {
    keys.insert("COPILOT_GITHUB_TOKEN".into(), token.into());
    keys.insert(
        "KARS_PROVIDER_GITHUB_COPILOT_ENDPOINT".into(),
        "https://api.githubcopilot.com".into(),
    );
    keys.insert("KARS_PROVIDER_GITHUB_COPILOT_TOKEN".into(), token.into());
    keys.remove("KARS_PROVIDER_GITHUB_COPILOT_API_KEY");
}

/// One additional provider, as surfaced to the operator (never the key/token
/// itself — `has_key` only tells you whether one is stored).
#[derive(Debug, Serialize)]
pub struct AdditionalProviderDto {
    pub tag: String,
    pub endpoint: Option<String>,
    pub has_key: bool,
    /// Deployment ids the operator declared this provider serves — these
    /// feed the shared model catalog (`GET /api/options`), tagged with this
    /// provider, so InferencePolicy's model picker can offer them.
    pub models: Vec<String>,
}

/// `GET /api/operator/providers/additional` — list every additional provider
/// configured on this cluster (beyond the single default from `put_provider`).
pub async fn list_additional_providers(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<AdditionalProviderDto>>> {
    let cluster = require_cluster(&state)?;
    let keys = cluster
        .additional_provider_keys()
        .await
        .map_err(AppError::from)?;
    let mut providers: std::collections::BTreeMap<String, AdditionalProviderDto> =
        std::collections::BTreeMap::new();
    for key in keys.keys() {
        if let Some(tag_part) = key
            .strip_prefix("KARS_PROVIDER_")
            .and_then(|r| r.strip_suffix("_ENDPOINT"))
        {
            let tag = tag_part.to_ascii_lowercase().replace('_', "-");
            providers
                .entry(tag.clone())
                .or_insert(AdditionalProviderDto {
                    tag,
                    endpoint: None,
                    has_key: false,
                    models: Vec::new(),
                });
        }
    }
    for (tag, dto) in providers.iter_mut() {
        let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
        dto.endpoint = keys
            .get(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"))
            .cloned();
        dto.has_key = keys.contains_key(&format!("KARS_PROVIDER_{tag_upper}_API_KEY"))
            || keys.contains_key(&format!("KARS_PROVIDER_{tag_upper}_TOKEN"));
        dto.models = keys
            .get(&format!("KARS_PROVIDER_{tag_upper}_MODELS"))
            .map(|m| {
                m.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
    }
    // Include historical connections before their next explicit save enrolls
    // the named route. Listing never mutates credentials.
    if keys.contains_key("COPILOT_GITHUB_TOKEN") {
        providers.insert(
            "github-copilot".to_string(),
            AdditionalProviderDto {
                tag: "github-copilot".to_string(),
                endpoint: Some("https://api.githubcopilot.com".to_string()),
                has_key: true,
                models: keys
                    .get("KARS_PROVIDER_GITHUB_COPILOT_MODELS")
                    .map(|m| {
                        m.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        );
    }
    Ok(Json(providers.into_values().collect()))
}

#[derive(Debug, serde::Deserialize)]
pub struct AdditionalProviderRequest {
    /// Lowercase, hyphenated tag (e.g. "foundry", "github-models"). The
    /// reserved tag "github-copilot" only needs `api_key` (its endpoint is
    /// the well-known Copilot API and is never user-editable).
    pub tag: String,
    pub endpoint: Option<String>,
    /// Dev-mode direct key/token (e.g. a GitHub Models PAT, or a second
    /// Azure OpenAI resource's key). Optional for providers that authenticate
    /// via Workload Identity in production (Foundry/Azure OpenAI need no key
    /// at all on AKS — see `inference-router::auth::WorkloadIdentityAuth`).
    pub api_key: Option<String>,
    /// Comma-separated deployment ids this provider serves — feeds the
    /// shared model catalog (`GET /api/options`), tagged with this provider,
    /// so InferencePolicy's model picker can offer "this model via THIS
    /// provider" without any change to that editor.
    pub models: Option<String>,
}

/// `PUT /api/operator/providers/additional` — add or update one additional
/// provider. Read-modify-write against the shared Secret so configuring one
/// provider never disturbs another already stored there.
pub async fn put_additional_provider(
    State(state): State<AppState>,
    Json(req): Json<AdditionalProviderRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let tag = req.tag.trim().to_ascii_lowercase();
    if !is_dns1123_label(&tag) {
        return Err(AppError::BadRequest(
            "tag must be lowercase letters, digits, hyphens (e.g. \"foundry\", \"github-models\")"
                .into(),
        ));
    }
    let is_copilot = tag == "github-copilot";
    if !is_copilot {
        let endpoint = req
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .ok_or_else(|| AppError::BadRequest("endpoint is required for this provider".into()))?;
        if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
            return Err(AppError::BadRequest("endpoint must be a URL".into()));
        }
    }
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let key_val = req
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string);
    let models: Vec<&str> = req
        .models
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if models.is_empty() {
        return Err(AppError::BadRequest(
            "at least one model deployment id is required (comma-separated) so InferencePolicy can offer it".into(),
        ));
    }
    let models_joined = models.join(",");
    cluster
        .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
            if is_copilot {
                if let Some(token) = key_val
                    .clone()
                    .or_else(|| keys.get("COPILOT_GITHUB_TOKEN").cloned())
                {
                    bind_copilot_route(keys, &token);
                }
                keys.insert(
                    "KARS_PROVIDER_GITHUB_COPILOT_MODELS".to_string(),
                    models_joined.clone(),
                );
            } else {
                if let Some(endpoint) = req
                    .endpoint
                    .as_deref()
                    .map(str::trim)
                    .filter(|e| !e.is_empty())
                {
                    keys.insert(
                        format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"),
                        endpoint.to_string(),
                    );
                }
                if let Some(k) = key_val.clone() {
                    keys.insert(format!("KARS_PROVIDER_{tag_upper}_API_KEY"), k);
                }
                keys.insert(
                    format!("KARS_PROVIDER_{tag_upper}_MODELS"),
                    models_joined.clone(),
                );
            }
        })
        .await
        .map_err(AppError::from)?;
    Ok(Json(serde_json::json!({
        "configured": true,
        "tag": tag,
        "note": "Provider configuration saved for new or restarted sandbox routers. InferencePolicy.modelPreference selects each sandbox's route; this does not change the cluster default."
    })))
}

/// `DELETE /api/operator/providers/additional/:tag` — remove one additional
/// provider's keys from the shared Secret (leaves other providers intact).
pub async fn delete_additional_provider(
    State(state): State<AppState>,
    axum::extract::Path(tag): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let tag = tag.trim().to_ascii_lowercase();
    if !is_dns1123_label(&tag) {
        return Err(AppError::BadRequest("Invalid provider tag.".into()));
    }
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let is_copilot = tag == "github-copilot";
    cluster
        .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
            if is_copilot {
                keys.remove("COPILOT_GITHUB_TOKEN");
            }
            keys.remove(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"));
            keys.remove(&format!("KARS_PROVIDER_{tag_upper}_API_KEY"));
            keys.remove(&format!("KARS_PROVIDER_{tag_upper}_TOKEN"));
            keys.remove(&format!("KARS_PROVIDER_{tag_upper}_MODELS"));
        })
        .await
        .map_err(AppError::from)?;
    Ok(Json(serde_json::json!({"removed": true, "tag": tag})))
}

/// Select the first configured model of a connected provider for new work.
pub async fn promote_additional_provider(
    State(state): State<AppState>,
    axum::extract::Path(tag): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    save_default_model(require_cluster(&state)?, &tag, None).await
}

#[derive(Debug, serde::Deserialize)]
pub struct SetDefaultModelRequest {
    pub deployment: String,
    /// The provider tag that serves this model, as shown in the catalogue
    /// (e.g. "github-copilot", "foundry", "local-llama-3-2-1b-instruct").
    pub provider: String,
}

/// Save a provider-qualified default without changing controller bootstrap configuration.
pub async fn set_default_model(
    State(state): State<AppState>,
    Json(req): Json<SetDefaultModelRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if req.deployment.trim().is_empty() {
        return Err(AppError::BadRequest(
            "a model deployment id is required".into(),
        ));
    }
    save_default_model(
        require_cluster(&state)?,
        &req.provider,
        Some(req.deployment.trim()),
    )
    .await
}

async fn save_default_model(
    cluster: &crate::kars::cluster::Cluster,
    provider: &str,
    deployment: Option<&str>,
) -> AppResult<Json<serde_json::Value>> {
    let provider = provider.trim().to_ascii_lowercase();
    let (_, expected) = cluster
        .integration_store(&cluster.core_namespace(), INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(default_selection_error)?;
    // Read the available catalogue independently of a possibly removed saved default,
    // so operators can repair a stale selection through this same action.
    let options = crate::routes::options::build_available_options(cluster).await?;
    let selected = options.models.iter().find(|model|
        model.provider == provider && deployment.is_none_or(|id| id == model.deployment)
    ).ok_or_else(|| AppError::BadRequest(
        "Choose a model from the connected provider's configured catalogue; the default was not changed.".into()
    ))?;
    let model = crate::kars::task::TaskModel {
        provider: selected.provider.clone(),
        deployment: selected.deployment.clone(),
    };
    cluster
        .write_model_default(&model, &expected)
        .await
        .map_err(default_selection_error)?;
    Ok(Json(serde_json::json!({
        "ok": true, "promoted": true, "default": model.deployment,
        "provider": model.provider, "tag": provider,
        "note": "Default saved. Inheriting Orchestrator and SRE policies reconcile automatically; check their applied status. Explicit and reviewed model routes are unchanged."
    })))
}

fn default_selection_error(error: kube::Error) -> AppError {
    match error {
        kube::Error::Api(status) if status.code == 409 || status.code == 422 => AppError::Rejected(
            "Provider configuration or its enrolled identity changed. Refresh Configuration and try again; the default was not saved.".into()
        ),
        kube::Error::Api(status) if status.code == 403 => AppError::Rejected(
            "The enrolled provider registry is not writable. Restore its credential grant before changing the default.".into()
        ),
        error => upstream(error),
    }
}
