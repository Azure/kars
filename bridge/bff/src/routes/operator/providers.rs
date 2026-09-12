// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::additional_providers::{INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET};
use super::{require_cluster, upstream};

// ─── Provider onboarding (model providers for missions + envelope gen) ───────

#[derive(Debug, serde::Deserialize)]
pub struct ProviderRequest {
    /// "github-models" | "azure-openai" | "foundry".
    pub kind: String,
    /// Auth mode: "api" (key), "workload" (workload identity), "agentid".
    pub auth: String,
    pub endpoint: Option<String>,
    /// Comma-separated deployment ids to expose in the catalog.
    pub models: String,
    /// Optional key when auth=api; stored write-only in kars-system.
    pub key: Option<String>,
}

/// `POST /api/operator/providers` — onboard a model provider. Sets the catalog
/// the controller serves, records the endpoint, and (for api auth) stores the
/// key as a write-only secret. Workload/agentid auth store no secret — the
/// controller authenticates via its identity. The catalog feeds both mission
/// models and envelope generation. Patches the controller deployment env.
pub async fn put_provider(
    State(state): State<AppState>,
    Json(req): Json<ProviderRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    // GUARD: this endpoint only ever wires FOUNDRY_ENDPOINT + AZURE_OPENAI_API_KEY
    // onto the controller (set_controller_catalog below). GitHub Copilot needs a
    // COPILOT_GITHUB_TOKEN (exchanged for a Copilot JWT by the router's copilot_auth
    // path) and GitHub Models needs its catalog endpoint recognized by the router's
    // is_github_models() host check — neither is wired by this route. Silently
    // "succeeding" here would tell the operator the cluster default changed when it
    // did not. Reject until real backend wiring exists; both kinds work correctly
    // today via the "additional provider" flow (POST .../providers/additional),
    // which does propagate a real per-provider tag + credential to every sandbox.
    if req.kind == "github-copilot" || req.kind == "github-models" {
        return Err(AppError::BadRequest(format!(
            "{} can't be set as the cluster's default provider from this form yet \
             (it only wires an Azure-style endpoint/key). Add it as an additional \
             provider instead — every sandbox can already route to it per-request \
             via an InferencePolicy model preference.",
            if req.kind == "github-copilot" {
                "GitHub Copilot"
            } else {
                "GitHub Models"
            }
        )));
    }
    let models: Vec<&str> = req
        .models
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if models.is_empty() {
        return Err(AppError::BadRequest(
            "at least one model deployment is required".into(),
        ));
    }
    let mut key_secret: Option<(String, String)> = None;
    if req.auth == "api" {
        if let Some(k) = req.key.as_deref().filter(|k| !k.trim().is_empty()) {
            let secret = format!("kars-provider-{}", req.kind);
            cluster.upsert_secret("kars-system", &secret, serde_json::json!({
                "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                "metadata": {"name": secret, "namespace": "kars-system", "labels": {"app.kubernetes.io/managed-by": "kars-bridge"}},
                "stringData": {"API_KEY": k},
            })).await.map_err(upstream)?;
            key_secret = Some((secret, "API_KEY".to_string()));
        } else {
            return Err(AppError::BadRequest(
                "auth=api requires a provider API key".into(),
            ));
        }
    }
    let key_ref = key_secret.as_ref().map(|(s, k)| (s.as_str(), k.as_str()));
    cluster
        .set_controller_catalog(&models.join(","), req.endpoint.as_deref(), key_ref)
        .await
        .map_err(upstream)?;
    Ok(Json(
        serde_json::json!({"onboarded": true, "kind": req.kind, "auth": req.auth, "models": models,
        "note": if key_secret.is_some() {
            "Catalog updated and the API key wired into the controller via secretKeyRef (AZURE_OPENAI_API_KEY), which the controller propagates to sandbox pods. The controller is rolling to pick it up."
        } else {
            "Catalog updated; the controller is rolling. workload/agentid auth use the controller's own identity — no key stored."
        }}),
    ))
}

/// One discoverable model, browser-facing.
#[derive(Debug, Serialize)]
pub struct DiscoveredModelDto {
    /// The exact id to feed back into `ProviderRequest.models` (e.g. `openai/gpt-4o`).
    pub id: String,
    /// Human label, when richer than the id (e.g. "OpenAI GPT-4o").
    pub label: Option<String>,
    /// True for a highlighted/pre-selected pick. For GitHub Copilot this is
    /// every model in Copilot's own `powerful` picker category (the flagship
    /// tier), derived LIVE from the `/models` endpoint — not a hand-picked id
    /// that goes stale. Absent/false for GitHub Models / Azure OpenAI, which
    /// have no "best pick" signal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recommended: bool,
    /// Short human detail (e.g. "Anthropic · 1.0M ctx · powerful"), when the
    /// provider exposes it (GitHub Copilot's live catalog does). Shown in the
    /// Model catalogue so a model isn't just an opaque id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The same editor/integration headers the router's `copilot_auth` sends, so
/// the seat sees a consistent client identity across discovery and inference.
const COPILOT_EDITOR_VERSION: &str = "vscode/1.107.0";
const COPILOT_INTEGRATION_ID: &str = "vscode-chat";
/// Public OAuth client id for the GitHub Copilot device-flow integration — the
/// SAME id the CLI's `copilotDeviceLogin` uses (cli/src/github-copilot.ts). A
/// token minted through this flow is authorized for the `copilot_internal/v2/
/// token` exchange, unlike a stock `gh auth login` token (which 404s there).
const COPILOT_OAUTH_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

/// `POST /api/operator/providers/copilot/login/start` — begin the GitHub
/// device-flow OAuth so the operator can sign in to Copilot properly (no
/// hand-pasted token). Returns the user code + verification URL to show, and
/// the device code the client polls with.
pub async fn copilot_login_start() -> AppResult<Json<serde_json::Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let resp = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("User-Agent", "kars-bridge")
        .json(&serde_json::json!({ "client_id": COPILOT_OAUTH_CLIENT_ID, "scope": "read:user" }))
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("device-code request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Upstream(format!(
            "GitHub device-code returned {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("bad device-code JSON: {e}")))?;
    Ok(Json(serde_json::json!({
        "device_code": body.get("device_code").and_then(|v| v.as_str()).unwrap_or_default(),
        "user_code": body.get("user_code").and_then(|v| v.as_str()).unwrap_or_default(),
        "verification_uri": body.get("verification_uri").and_then(|v| v.as_str()).unwrap_or("https://github.com/login/device"),
        "interval": body.get("interval").and_then(|v| v.as_u64()).unwrap_or(5),
        "expires_in": body.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(900),
    })))
}

#[derive(Debug, serde::Deserialize)]
pub struct CopilotLoginPollRequest {
    pub device_code: String,
}

/// `POST /api/operator/providers/copilot/login/poll` — poll the device flow.
/// While the user hasn't approved yet, returns `{status:"pending"}`. On
/// approval it: (1) verifies the minted token is Copilot-entitled, (2) stores
/// it server-side as the Copilot provider credential (COPILOT_GITHUB_TOKEN in
/// the shared providers secret) — the token NEVER returns to the browser,
/// (3) busts the live-catalog cache, and (4) returns the seat's live model
/// list so the wizard can show it immediately.
pub async fn copilot_login_poll(
    State(state): State<AppState>,
    Json(req): Json<CopilotLoginPollRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let resp = client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .header("User-Agent", "kars-bridge")
        .json(&serde_json::json!({
            "client_id": COPILOT_OAUTH_CLIENT_ID,
            "device_code": req.device_code,
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        }))
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("device poll failed: {e}")))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("bad poll JSON: {e}")))?;

    if let Some(token) = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
    {
        // Verify the seat is genuinely Copilot-entitled before storing.
        copilot_jwt(token).await?;
        // Store server-side as the Copilot provider credential (never returned
        // to the browser). Also refresh the controller's default credential so
        // a cluster whose default IS Copilot starts working immediately.
        cluster
            .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
                keys.insert("COPILOT_GITHUB_TOKEN".to_string(), token.to_string());
            })
            .await
            .map_err(upstream)?;
        // Fresh token → invalidate any cached catalog for the old one.
        invalidate_copilot_catalog_cache();
        let models = copilot_catalog_cached(token).await;
        return Ok(Json(serde_json::json!({
            "status": "authorized",
            "models": models.iter().map(|(id, rec, detail)| serde_json::json!({"id": id, "recommended": rec, "detail": detail})).collect::<Vec<_>>(),
        })));
    }

    match body.get("error").and_then(|v| v.as_str()) {
        Some("authorization_pending") | Some("slow_down") => {
            Ok(Json(serde_json::json!({ "status": "pending" })))
        }
        Some("expired_token") => Err(AppError::Rejected(
            "The sign-in code expired before it was approved. Start again.".into(),
        )),
        Some("access_denied") => Err(AppError::Rejected(
            "Sign-in was cancelled on GitHub.".into(),
        )),
        Some(other) => Err(AppError::Upstream(format!(
            "GitHub device flow error: {other}"
        ))),
        None => Ok(Json(serde_json::json!({ "status": "pending" }))),
    }
}

/// Exchange a GitHub OAuth token / PAT for a short-lived Copilot JWT — the
/// exact same endpoint (and `chat_enabled` eligibility semantics) the CLI's
/// `checkCopilotEligibility` and the router's `copilot_auth` use. A 200 with a
/// token and `chat_enabled != false` means the router will actually be able to
/// serve inference for this seat, not merely that the token parses. Returns
/// the JWT so the caller can immediately query the live `/models` catalog with
/// it (no second exchange).
pub(crate) async fn copilot_jwt(gh_token: &str) -> Result<String, AppError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("token {gh_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "kars-bridge")
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("Copilot eligibility check failed: {e}")))?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err(AppError::Rejected(
            "This GitHub token isn't entitled to Copilot. Enable Copilot at https://github.com/settings/copilot, or use a token from an account with an active seat.".into(),
        ));
    }
    if !resp.status().is_success() {
        return Err(AppError::Upstream(format!(
            "Copilot token endpoint returned {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("bad Copilot token response: {e}")))?;
    if body.get("chat_enabled").and_then(|c| c.as_bool()) == Some(false) {
        return Err(AppError::Rejected(
            "Copilot subscription is active but Chat is disabled. Enable it at https://github.com/settings/copilot/features.".into(),
        ));
    }
    body.get("token")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Upstream("Copilot token endpoint returned no token".into()))
}

/// Parse GitHub Copilot's live `/models` response into the browser DTO. Pure
/// (no I/O) so it's unit-testable against a captured sample. Surfaces ONLY the
/// models a seat can actually reason with:
///   • `capabilities.type == "chat"` — excludes embeddings.
///   • `model_picker_enabled == true` — Copilot's own "show in picker" flag;
///     drops legacy/hidden aliases (gpt-4o, gpt-3.5-turbo, dated snapshots).
///   • policy absent, OR `policy.state == "enabled"` — a gated preview the
///     seat hasn't opted into is not usable, so it's hidden.
/// Ordering: Copilot's picker category (powerful → versatile → lightweight),
/// then context window desc, then id — so the flagship tier leads. Every
/// `powerful`-category model is marked `recommended` (pre-checked in the
/// wizard). This is entirely live: a new flagship (gpt-5.7, opus-4.9, …)
/// appears and is categorised by GitHub, with no code change here.
pub(crate) fn parse_copilot_models(body: &serde_json::Value) -> Vec<DiscoveredModelDto> {
    fn category_rank(cat: &str) -> u8 {
        match cat {
            "powerful" => 0,
            "versatile" => 1,
            "lightweight" => 2,
            _ => 3,
        }
    }
    let mut rows: Vec<(u8, u64, String, DiscoveredModelDto)> = Vec::new();
    let Some(data) = body.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    for m in data {
        let caps = m.get("capabilities");
        let is_chat = caps.and_then(|c| c.get("type")).and_then(|t| t.as_str()) == Some("chat");
        let picker = m
            .get("model_picker_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // policy absent => generally available; present => must be "enabled".
        let policy_ok = match m.get("policy") {
            None => true,
            Some(p) => p.get("state").and_then(|s| s.as_str()) == Some("enabled"),
        };
        if !(is_chat && picker && policy_ok) {
            continue;
        }
        let Some(id) = m
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(id);
        let vendor = m.get("vendor").and_then(|v| v.as_str()).unwrap_or("");
        let category = m
            .get("model_picker_category")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let ctx = caps
            .and_then(|c| c.get("limits"))
            .and_then(|l| l.get("max_context_window_tokens"))
            .and_then(|v| v.as_u64());
        let ctx_label = ctx
            .map(|c| {
                if c >= 1_000_000 {
                    format!("{:.1}M ctx", c as f64 / 1_000_000.0)
                } else {
                    format!("{}k ctx", c / 1000)
                }
            })
            .unwrap_or_default();
        let label = [vendor, &ctx_label, category]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ");
        let detail = if label.is_empty() {
            None
        } else {
            Some(label.clone())
        };
        rows.push((
            category_rank(category),
            ctx.unwrap_or(0),
            id.to_string(),
            DiscoveredModelDto {
                id: id.to_string(),
                label: (name != id || !label.is_empty()).then(|| {
                    if label.is_empty() {
                        name.to_string()
                    } else {
                        format!("{name} — {label}")
                    }
                }),
                recommended: category == "powerful",
                detail,
            },
        ));
    }
    // Sort: powerful first, then largest context, then id desc (newer version
    // numbers tend to sort higher) — purely presentational.
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
    rows.into_iter().map(|(_, _, _, dto)| dto).collect()
}

/// Fetch the live Copilot model catalog for a seat, given its exchanged JWT.
pub(crate) async fn fetch_copilot_models(jwt: &str) -> Result<Vec<DiscoveredModelDto>, AppError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let resp = client
        .get("https://api.githubcopilot.com/models")
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Editor-Version", COPILOT_EDITOR_VERSION)
        .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("Copilot /models request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Upstream(format!(
            "Copilot /models returned {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("bad Copilot /models JSON: {e}")))?;
    Ok(parse_copilot_models(&body))
}

/// A short-TTL cache for the live Copilot catalog so `build_options` (hit on
/// every Configuration page load AND every orchestrator compose) doesn't do a
/// token-exchange + /models round-trip each time. Keyed by the token so a
/// changed seat re-fetches; 5-minute freshness is plenty for a model list.
type CopilotCatalog = Vec<(String, bool, Option<String>)>;
type CachedCopilotCatalog = (String, std::time::Instant, CopilotCatalog);
static COPILOT_CATALOG_CACHE: std::sync::Mutex<Option<CachedCopilotCatalog>> =
    std::sync::Mutex::new(None);

/// Drop the cached Copilot catalog — call after a fresh sign-in so the next
/// `build_options` re-fetches against the new token immediately.
pub(crate) fn invalidate_copilot_catalog_cache() {
    *COPILOT_CATALOG_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = None;
}

/// Live (cached) Copilot model catalog for a seat token: `(deployment_id,
/// recommended, detail)` for every model the seat can actually use. Best-effort
/// — on any auth/network failure it returns the last good cache if still
/// present, else empty, so a transient Copilot outage never blanks the catalogue.
pub(crate) async fn copilot_catalog_cached(gh_token: &str) -> Vec<(String, bool, Option<String>)> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(300);
    {
        let guard = COPILOT_CATALOG_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some((tok, at, models)) = guard.as_ref()
            && tok == gh_token
            && at.elapsed() < TTL
        {
            return models.clone();
        }
    }
    let fetched = async {
        let jwt = copilot_jwt(gh_token).await.ok()?;
        let models = fetch_copilot_models(&jwt).await.ok()?;
        Some(
            models
                .into_iter()
                .map(|m| (m.id, m.recommended, m.detail))
                .collect::<Vec<_>>(),
        )
    }
    .await;
    match fetched {
        Some(models) => {
            let mut guard = COPILOT_CATALOG_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *guard = Some((
                gh_token.to_string(),
                std::time::Instant::now(),
                models.clone(),
            ));
            models
        }
        None => {
            // Fetch failed — reuse a still-present cache entry (even if stale)
            // rather than blanking the catalogue on a transient hiccup.
            let guard = COPILOT_CATALOG_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            guard
                .as_ref()
                .filter(|(tok, _, _)| tok == gh_token)
                .map(|(_, _, m)| m.clone())
                .unwrap_or_default()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct DiscoverModelsRequest {
    /// "github-models" | "azure-openai" | "github-copilot". (Foundry already
    /// discovers models via the existing /api/operator/foundry/verify.)
    pub kind: String,
    pub endpoint: Option<String>,
    pub key: Option<String>,
}

/// `POST /api/operator/providers/discover` — real, live model discovery so the
/// operator never hand-types a deployment id. GitHub Models queries the public
/// catalog (no auth). Azure OpenAI queries the data-plane `/openai/deployments`
/// endpoint using the operator-supplied endpoint + key (a live round-trip, so a
/// wrong key/endpoint surfaces as an immediate, actionable error). GitHub
/// Copilot exchanges the supplied token for a Copilot JWT (verifying the seat +
/// Chat entitlement live) and then queries the seat's LIVE `/models` catalog —
/// so the picker always reflects the models GitHub currently serves this seat
/// (gpt-5.6, claude-opus-4.8, gemini-3.1-pro, …), never a hand-maintained list
/// that goes stale.
pub async fn discover_models(
    Json(req): Json<DiscoverModelsRequest>,
) -> AppResult<Json<Vec<DiscoveredModelDto>>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match req.kind.as_str() {
        "github-models" => {
            let resp = client
                .get("https://models.github.ai/catalog/models")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
                .map_err(|e| {
                    AppError::Upstream(format!("GitHub Models catalog request failed: {e}"))
                })?;
            if !resp.status().is_success() {
                return Err(AppError::Upstream(format!(
                    "GitHub Models catalog returned {}",
                    resp.status()
                )));
            }
            let body: Vec<serde_json::Value> = resp
                .json()
                .await
                .map_err(|e| AppError::Upstream(format!("bad catalog JSON: {e}")))?;
            let models = body
                .iter()
                .filter_map(|m| {
                    let id = m.get("id").and_then(|v| v.as_str())?.to_string();
                    let name = m.get("name").and_then(|v| v.as_str()).map(str::to_string);
                    Some(DiscoveredModelDto {
                        id,
                        label: name,
                        recommended: false,
                        detail: None,
                    })
                })
                .collect();
            Ok(Json(models))
        }
        "azure-openai" => {
            let endpoint = req
                .endpoint
                .as_deref()
                .map(|e| e.trim().trim_end_matches('/'))
                .filter(|e| !e.is_empty())
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "endpoint is required to discover Azure OpenAI deployments".into(),
                    )
                })?;
            let key = req
                .key
                .as_deref()
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| AppError::BadRequest(
                    "an API key is required to discover deployments (workload/agentid auth can't be exercised from the browser — enter deployment ids manually, or discover once with a temporary key)".into(),
                ))?;
            let url = format!("{endpoint}/openai/deployments?api-version=2023-05-15");
            let resp = client
                .get(&url)
                .header("api-key", key)
                .send()
                .await
                .map_err(|e| AppError::Upstream(format!("Azure OpenAI request failed: {e}")))?;
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(AppError::Rejected(format!(
                    "Azure OpenAI rejected the discovery request ({status}) — check the endpoint and key: {body_text}"
                )));
            }
            let body: serde_json::Value = serde_json::from_str(&body_text)
                .map_err(|e| AppError::Upstream(format!("bad deployments JSON: {e}")))?;
            let models = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| {
                            let id = d.get("id").and_then(|v| v.as_str())?.to_string();
                            let base = d
                                .get("model")
                                .and_then(|v| v.as_str())
                                .map(|m| format!("deployment of {m}"));
                            Some(DiscoveredModelDto {
                                id,
                                label: base,
                                recommended: false,
                                detail: None,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(Json(models))
        }
        "github-copilot" => {
            let token = req
                .key
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .ok_or_else(|| AppError::BadRequest(
                    "a GitHub token (OAuth token or PAT with Copilot access) is required to verify the seat before showing the model catalog".into(),
                ))?;
            let jwt = copilot_jwt(token).await?;
            let models = fetch_copilot_models(&jwt).await?;
            Ok(Json(models))
        }
        other => Err(AppError::BadRequest(format!(
            "unknown provider kind '{other}' for discovery"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_copilot_models;
    use serde_json::json;

    #[test]
    fn parse_copilot_models_filters_and_categorises() {
        // A trimmed but faithful sample of the real /models response shape
        // (captured live 2026-07): a flagship chat model, a versatile one, an
        // embeddings model (must be dropped), a legacy non-picker chat model
        // (must be dropped), and a gated preview the seat hasn't enabled
        // (must be dropped).
        let body = json!({"data": [
            {
                "id": "claude-opus-4.8", "name": "Claude Opus 4.8", "vendor": "Anthropic",
                "model_picker_enabled": true, "model_picker_category": "powerful",
                "policy": {"state": "enabled"},
                "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 1_000_000}}
            },
            {
                "id": "gpt-5.6-terra", "name": "GPT-5.6 Terra", "vendor": "OpenAI",
                "model_picker_enabled": true, "model_picker_category": "versatile",
                "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 1_050_000}}
            },
            {
                "id": "text-embedding-3-small", "name": "Embedding V3 small", "vendor": "Azure OpenAI",
                "model_picker_enabled": false, "capabilities": {"type": "embeddings"}
            },
            {
                "id": "gpt-4o", "name": "GPT-4o", "vendor": "Azure OpenAI",
                "model_picker_enabled": false,
                "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 128_000}}
            },
            {
                "id": "some-preview", "name": "Gated Preview", "vendor": "OpenAI",
                "model_picker_enabled": true, "model_picker_category": "powerful",
                "policy": {"state": "unconfigured"},
                "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 200_000}}
            }
        ]});
        let out = parse_copilot_models(&body);
        let ids: Vec<&str> = out.iter().map(|m| m.id.as_str()).collect();
        // Only the two enabled, picker-enabled chat models survive — embeddings,
        // the legacy non-picker gpt-4o, and the un-enabled preview are dropped.
        assert_eq!(ids, vec!["claude-opus-4.8", "gpt-5.6-terra"]);
        // powerful sorts before versatile.
        assert!(out[0].recommended, "powerful model must be recommended");
        assert!(
            !out[1].recommended,
            "versatile model must not be recommended"
        );
        // Label carries the human name + context.
        assert!(out[0].label.as_deref().unwrap().contains("Claude Opus 4.8"));
        assert!(out[0].label.as_deref().unwrap().contains("1.0M ctx"));
    }

    #[test]
    fn parse_copilot_models_empty_on_missing_data() {
        assert!(parse_copilot_models(&json!({})).is_empty());
        assert!(parse_copilot_models(&json!({"data": []})).is_empty());
    }
}
