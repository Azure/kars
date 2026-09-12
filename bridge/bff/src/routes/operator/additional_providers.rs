// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::{copilot_catalog_cached, is_dns1123_label, require_cluster, upstream};

// ─── Multi-provider inference (§ inference-provider-wizard) ─────────────────
//
// The single "Inference provider" flow above (`put_provider`) sets the ONE
// default provider every mission inherits. This section manages ADDITIONAL
// providers that can be configured *at the same time* — e.g. GitHub Copilot
// as the default, Azure AI Foundry also connected — so an InferencePolicy's
// `modelPreference.primary.provider` can route a specific sandbox's calls to
// whichever one actually serves the model it needs (a sub-agent on gpt-4.1
// via Foundry, a principal on opus-4.8 via Copilot, in the SAME cluster).
//
// Storage: the `kars-inference-providers` Secret in `kars-system`. Its KEYS
// are the literal env var names `inference-router::config::Config::from_env`
// already parses generically (`KARS_PROVIDER_<TAG>_ENDPOINT` + optional
// `_API_KEY`/`_TOKEN`, or the well-known `COPILOT_GITHUB_TOKEN` for the
// GitHub Copilot special case) — no router-side change needed to support a
// provider added here. The controller mirrors this ONE secret into every
// sandbox's own namespace (the same mechanism already used for
// `kars-github-app`), and every sandbox's router picks whichever provider a
// request's InferencePolicy names — never all-or-nothing, never guessed from
// what's merely present in the env.
pub(super) const INFERENCE_PROVIDERS_SECRET: &str = "kars-inference-providers";
pub(super) const INFERENCE_PROVIDERS_NS: &str = "kars-system";

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
        .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;
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
    // GitHub Copilot is a special case (well-known endpoint, no
    // KARS_PROVIDER_*_ENDPOINT needed — see resolve_provider in the router).
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
                if let Some(k) = key_val.clone() {
                    keys.insert("COPILOT_GITHUB_TOKEN".to_string(), k);
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
        .map_err(upstream)?;
    Ok(Json(serde_json::json!({
        "configured": true,
        "tag": tag,
        "note": "Every sandbox's router now has this provider available. Which one a given request actually uses is decided per-sandbox by its InferencePolicy.modelPreference — this alone doesn't make it the default."
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
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let is_copilot = tag == "github-copilot";
    cluster
        .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
            if is_copilot {
                keys.remove("COPILOT_GITHUB_TOKEN");
                keys.remove("KARS_PROVIDER_GITHUB_COPILOT_MODELS");
            } else {
                keys.remove(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"));
                keys.remove(&format!("KARS_PROVIDER_{tag_upper}_API_KEY"));
                keys.remove(&format!("KARS_PROVIDER_{tag_upper}_TOKEN"));
                keys.remove(&format!("KARS_PROVIDER_{tag_upper}_MODELS"));
            }
        })
        .await
        .map_err(upstream)?;
    Ok(Json(serde_json::json!({"removed": true, "tag": tag})))
}

/// `POST /api/operator/providers/additional/:tag/promote` — make an already-
/// connected additional provider the cluster's DEFAULT (patches the
/// controller's own env — every mission that leaves its model unset inherits
/// this). Reads the tag's endpoint/key/models straight from
/// `kars-inference-providers` server-side (never exposed to the browser) and
/// re-points the SAME secret+key via `secretKeyRef` — no key duplication.
///
/// `github-copilot` is rejected: it authenticates via `COPILOT_GITHUB_TOKEN`
/// exchanged for a short-lived Copilot JWT, a completely different mechanism
/// than the endpoint+key shape every other provider here uses — the same
/// reason `put_provider` already refuses to set it as default from the other
/// form (see that handler's comment). Every other tag (Foundry, Azure OpenAI,
/// Custom, GitHub Models, and a local in-cluster model) is a plain
/// endpoint(+optional key), which is exactly what `set_controller_catalog`
/// wires — so promoting any of THOSE genuinely works.
pub async fn promote_additional_provider(
    State(state): State<AppState>,
    axum::extract::Path(tag): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let tag = tag.trim().to_ascii_lowercase();
    // GitHub Copilot IS promotable now — the wizard's device sign-in stores a
    // Copilot-authorized token, which `set_copilot_as_default` wires onto the
    // controller (KARS_PROVIDER + COPILOT_GITHUB_TOKEN), unlike the endpoint+key
    // shape every other provider uses.
    if tag == "github-copilot" {
        let keys = cluster
            .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
            .await
            .map_err(upstream)?;
        if !keys.contains_key("COPILOT_GITHUB_TOKEN") {
            return Err(AppError::BadRequest(
                "Sign in to GitHub Copilot first (Connect a provider → GitHub Copilot) — then it can be set as the cluster default.".into(),
            ));
        }
        let models = keys
            .get("KARS_PROVIDER_GITHUB_COPILOT_MODELS")
            .cloned()
            .unwrap_or_default();
        let models = if models.trim().is_empty() {
            // No explicit selection stored — fall back to the live catalog so
            // the default catalogue isn't empty.
            copilot_catalog_cached(keys.get("COPILOT_GITHUB_TOKEN").unwrap())
                .await
                .into_iter()
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>()
                .join(",")
        } else {
            models
        };
        cluster
            .set_copilot_as_default(&models)
            .await
            .map_err(upstream)?;
        return Ok(Json(serde_json::json!({
            "promoted": true,
            "tag": tag,
            "note": "GitHub Copilot is now the cluster default; the controller is rolling to pick it up. Every mission that leaves its model unset now inherits it."
        })));
    }
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let keys = cluster
        .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;
    let endpoint = keys
        .get(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"))
        .cloned()
        .ok_or_else(|| AppError::BadRequest(format!("no connected provider tagged {tag:?} with an endpoint (GitHub Models has a well-known endpoint but no explicit one is stored, so it can't be promoted this way either)")))?;
    let models = keys
        .get(&format!("KARS_PROVIDER_{tag_upper}_MODELS"))
        .cloned()
        .unwrap_or_default();
    if models.trim().is_empty() {
        return Err(AppError::BadRequest(format!(
            "{tag} has no declared models to promote"
        )));
    }
    let key_ref = if keys.contains_key(&format!("KARS_PROVIDER_{tag_upper}_API_KEY")) {
        Some((
            INFERENCE_PROVIDERS_SECRET,
            format!("KARS_PROVIDER_{tag_upper}_API_KEY"),
        ))
    } else {
        None
    };
    cluster
        .set_controller_catalog(
            &models,
            Some(&endpoint),
            key_ref.as_ref().map(|(s, k)| (*s, k.as_str())),
        )
        .await
        .map_err(upstream)?;
    Ok(Json(serde_json::json!({
        "promoted": true,
        "tag": tag,
        "note": "Cluster default updated; the controller is rolling to pick it up. Every mission that leaves its model unset now inherits this provider."
    })))
}

#[derive(Debug, serde::Deserialize)]
pub struct SetDefaultModelRequest {
    pub deployment: String,
    /// The provider tag that serves this model, as shown in the catalogue
    /// (e.g. "github-copilot", "foundry", "local-llama-3-2-1b-instruct").
    pub provider: String,
}

/// `POST /api/operator/models/default` — make one specific MODEL the cluster
/// default (what the Model catalogue's "Set as default" does). Promotes the
/// model's provider AND pins that model as the default (moved to the front of
/// the catalog, which `set_*_default` treats as KARS_TASK_DEFAULT_MODEL).
pub async fn set_default_model(
    State(state): State<AppState>,
    Json(req): Json<SetDefaultModelRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let deployment = req.deployment.trim().to_string();
    let provider = req.provider.trim().to_ascii_lowercase();
    if deployment.is_empty() {
        return Err(AppError::BadRequest(
            "a model deployment id is required".into(),
        ));
    }
    let keys = cluster
        .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;

    // Reorder a comma list so `deployment` is first (becomes the default),
    // deduped; ensures the chosen model is present even if it wasn't listed.
    let reorder = |csv: &str| -> String {
        let mut out = vec![deployment.clone()];
        for m in csv.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if m != deployment {
                out.push(m.to_string());
            }
        }
        out.join(",")
    };

    if provider == "github-copilot" {
        if !keys.contains_key("COPILOT_GITHUB_TOKEN") {
            return Err(AppError::BadRequest(
                "Sign in to GitHub Copilot first.".into(),
            ));
        }
        let existing = keys
            .get("KARS_PROVIDER_GITHUB_COPILOT_MODELS")
            .cloned()
            .unwrap_or_default();
        let models = if existing.trim().is_empty() {
            // fall back to the live catalog so the catalog isn't just one model
            let mut live = copilot_catalog_cached(keys.get("COPILOT_GITHUB_TOKEN").unwrap())
                .await
                .into_iter()
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>()
                .join(",");
            if live.trim().is_empty() {
                live = deployment.clone();
            }
            reorder(&live)
        } else {
            reorder(&existing)
        };
        cluster
            .set_copilot_as_default(&models)
            .await
            .map_err(upstream)?;
        return Ok(Json(
            serde_json::json!({"ok": true, "default": deployment, "provider": provider}),
        ));
    }

    // Endpoint-based providers (foundry, azure-openai, custom, local-*): promote
    // via set_controller_catalog with the chosen model first.
    let tag_upper = provider.to_ascii_uppercase().replace('-', "_");
    let endpoint = keys
        .get(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"))
        .cloned()
        .ok_or_else(|| AppError::BadRequest(format!(
            "no connected provider {provider:?} with an endpoint serves {deployment:?} — connect it first"
        )))?;
    let existing = keys
        .get(&format!("KARS_PROVIDER_{tag_upper}_MODELS"))
        .cloned()
        .unwrap_or_default();
    let models = reorder(&existing);
    let key_ref = if keys.contains_key(&format!("KARS_PROVIDER_{tag_upper}_API_KEY")) {
        Some((
            INFERENCE_PROVIDERS_SECRET,
            format!("KARS_PROVIDER_{tag_upper}_API_KEY"),
        ))
    } else {
        None
    };
    cluster
        .set_controller_catalog(
            &models,
            Some(&endpoint),
            key_ref.as_ref().map(|(s, k)| (*s, k.as_str())),
        )
        .await
        .map_err(upstream)?;
    Ok(Json(
        serde_json::json!({"ok": true, "default": deployment, "provider": provider}),
    ))
}
