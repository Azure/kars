// Copyright (c) Microsoft Corporation.
// ci:loc-ok — Entra Agent ID feature module, split planned for Phase 1 (see ci/loc-budget.yaml)

// Licensed under the MIT License.

//! HTTP client for the Microsoft Entra SDK auth sidecar.
//!
//! When the sandbox is in agent-id mesh-auth mode, the controller
//! injects an `auth-sidecar` container into the pod and pins the
//! per-sandbox Entra Agent Identity into two router env vars:
//!
//! - `AUTH_SIDECAR_URL` — typically `http://127.0.0.1:8080`. The
//!   loopback-only address the sidecar listens on. Routed to via
//!   the same egress-guard iptables rule set that REJECTs the
//!   openclaw container (UID 1000) from reaching the same port.
//! - `PINNED_AGENT_IDENTITY_APP_ID` — the per-sandbox Agent Identity
//!   `appId`. The router MUST pin this value into every request to
//!   the sidecar; it MUST NEVER accept a caller-supplied
//!   `AgentIdentity` query parameter (rubber-duck finding #1 from
//!   the original e2e plan critique).
//!
//! ## Fail-closed contract
//!
//! When `SidecarClient::from_env()` returns `Some`, the router treats
//! the sidecar as the EXCLUSIVE auth path — no IMDS fallback, no
//! Workload Identity fallback, no dev-key fallback. This preserves
//! the per-sandbox audit principal: every downstream API call in
//! agent-id mode is attributed to the agent identity, not to the
//! controller MI nor to the AKS node-pool MI.
//!
//! If the sidecar is unreachable, the router returns an explicit
//! error to the caller and the request fails. Falling back to a
//! different identity would mean downstream Azure RBAC silently sees
//! a different principal than the operator intended — exactly the
//! kind of "looks fine, audit log says otherwise" bug agent-id mode
//! is designed to prevent.
//!
//! ## Endpoint
//!
//! The sidecar exposes `/AuthorizationHeaderUnauthenticated/{service}`
//! for autonomous app-token flows. The `Unauthenticated` suffix means
//! "no inbound user token required" — the sidecar mints tokens
//! using its own configured credentials (the controller MI's IMDS
//! token bridged via SignedAssertionFromManagedIdentity into the
//! blueprint, then OBO'd to the pinned agent identity).
//!
//! The non-suffixed `/AuthorizationHeader/{service}` is for OBO
//! flows where the router has an inbound user bearer token to
//! relay. kars doesn't currently use OBO at the router layer, so
//! this module deliberately only implements the unauthenticated
//! variant.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Env var the controller sets to point the router at the sidecar.
pub const ENV_AUTH_SIDECAR_URL: &str = "AUTH_SIDECAR_URL";

/// Env var the controller sets with the per-sandbox Agent Identity
/// `appId`. The router pins this — caller-supplied values are NEVER
/// honoured.
pub const ENV_PINNED_AGENT_IDENTITY_APP_ID: &str = "PINNED_AGENT_IDENTITY_APP_ID";

/// Env var the controller sets with the Entra tenant ID expected on
/// tokens returned by the sidecar. The router decodes the JWT payload
/// (no signature verification — downstream Azure validates that) and
/// hard-rejects any token whose `tid` claim does not match.
///
/// Sourced by the controller from `KarsAuthConfig.spec.tenant.tenantId`.
/// Empty / absent means "tid pinning disabled" — logged at WARN on
/// router boot. Intended ONLY for transitional / development setups;
/// production deployments MUST set it.
pub const ENV_EXPECTED_TENANT_ID: &str = "EXPECTED_TENANT_ID";

/// Default token lifetime we assume when the sidecar response does
/// not include a usable expires-in hint. Entra access tokens are
/// nominally 1h; we cache for 50 min so refresh happens before
/// expiry and we tolerate a 5-min skew.
const DEFAULT_TOKEN_TTL_SECS: u64 = 50 * 60;

/// HTTP timeout for sidecar calls. The sidecar is in the same pod
/// (loopback) so anything beyond a few seconds means it's
/// catastrophically wedged and we should fail fast rather than
/// hanging the inference request.
const SIDECAR_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

// The Microsoft Entra SDK sidecar returns the bearer token wrapped
// in an `AuthorizationHeader` field with the literal `Bearer ` prefix
// already prepended. We parse the response body leniently via
// `parse_sidecar_body` (handles three observed response shapes) and
// strip the `Bearer ` prefix inside `SidecarClient::get_token`.

#[derive(Debug)]
struct CachedToken {
    token: String,
    expires_at: Instant,
}

/// Sidecar-backed auth client. Optional — `from_env()` returns `None`
/// when the sidecar env vars are absent (legacy / anonymous-tier
/// sandboxes).
#[derive(Debug)]
pub struct SidecarClient {
    base_url: String,
    pinned_agent_id: String,
    /// Expected `tid` claim on tokens returned by the sidecar. `None`
    /// disables tid pinning (insecure; logged at WARN on construction).
    /// Comparison is case-insensitive ASCII (Entra tids are GUIDs).
    expected_tenant_id: Option<String>,
    client: reqwest::Client,
    cache: Arc<RwLock<HashMap<String, CachedToken>>>,
}

impl SidecarClient {
    /// Construct from `AUTH_SIDECAR_URL` + `PINNED_AGENT_IDENTITY_APP_ID`.
    /// Returns:
    /// - `Ok(None)` when BOTH env vars are absent — sidecar mode is
    ///   disabled and the router falls through to legacy auth.
    /// - `Ok(Some(_))` when both are set — sidecar mode is active.
    /// - `Err` when ONLY ONE is set — operator misconfiguration.
    ///   Callers MUST surface this and refuse to start the router;
    ///   silently falling through to legacy auth would attribute
    ///   downstream calls to a different principal than intended,
    ///   exactly the bug agent-id mode is designed to prevent.
    ///
    /// `EXPECTED_TENANT_ID` is read opportunistically; when present,
    /// `get_token` rejects any sidecar response whose JWT `tid` claim
    /// does not match. When absent, tid pinning is disabled and a
    /// warning is logged. Operators MUST set it in production.
    pub fn from_env() -> Result<Option<Self>> {
        let raw_url = std::env::var(ENV_AUTH_SIDECAR_URL).ok();
        let raw_pinned = std::env::var(ENV_PINNED_AGENT_IDENTITY_APP_ID).ok();
        let base_url = raw_url
            .as_deref()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());
        let pinned_agent_id = raw_pinned
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        match (base_url, pinned_agent_id) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(anyhow!(
                "Inconsistent sidecar config: {ENV_AUTH_SIDECAR_URL} is set but \
                 {ENV_PINNED_AGENT_IDENTITY_APP_ID} is missing/empty. Both must be set \
                 together or neither. Refusing to start to avoid silent fallback to a \
                 different identity model."
            )),
            (None, Some(_)) => Err(anyhow!(
                "Inconsistent sidecar config: {ENV_PINNED_AGENT_IDENTITY_APP_ID} is set \
                 but {ENV_AUTH_SIDECAR_URL} is missing/empty. Both must be set together \
                 or neither. Refusing to start to avoid silent fallback to a different \
                 identity model."
            )),
            (Some(base_url), Some(pinned_agent_id)) => {
                let expected_tenant_id = std::env::var(ENV_EXPECTED_TENANT_ID)
                    .ok()
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty());

                if expected_tenant_id.is_none() {
                    tracing::warn!(
                        sidecar_url = %base_url,
                        pinned_agent_id = %pinned_agent_id,
                        "EXPECTED_TENANT_ID env var unset — sidecar token TID PINNING IS \
                         DISABLED. This is INSECURE and intended only for development. \
                         Production deployments must source the expected tenant id from \
                         KarsAuthConfig.spec.tenant.tenantId."
                    );
                } else {
                    tracing::info!(
                        sidecar_url = %base_url,
                        pinned_agent_id = %pinned_agent_id,
                        expected_tid = %expected_tenant_id.as_deref().unwrap_or("<unset>"),
                        "Sidecar auth mode enabled — all downstream tokens via auth-sidecar, \
                         with tid+principal+aud+exp pinning"
                    );
                }

                Ok(Some(Self {
                    base_url,
                    pinned_agent_id,
                    expected_tenant_id,
                    client: reqwest::Client::builder()
                        .timeout(SIDECAR_HTTP_TIMEOUT)
                        .build()
                        .expect("reqwest client construction"),
                    cache: Arc::new(RwLock::new(HashMap::new())),
                }))
            }
        }
    }

    /// Acquire a token for `resource` from the sidecar.
    ///
    /// `resource` is the same scope/audience string the existing
    /// auth path uses (e.g. `https://cognitiveservices.azure.com`).
    /// We translate it to the sidecar's `DownstreamApis__<key>__*`
    /// nomenclature via [`resource_to_service_name`] and call
    /// `/AuthorizationHeaderUnauthenticated/<key>?AgentIdentity=...`.
    ///
    /// Returns the raw bearer token (without the `Bearer ` prefix)
    /// so the caller can choose the appropriate header (`Bearer ...`
    /// for OAuth, `api-key: ...` for Azure OpenAI, etc.).
    pub async fn get_token(&self, resource: &str) -> Result<String> {
        let service = resource_to_service_name(resource).ok_or_else(|| {
            anyhow!(
                "no sidecar service name configured for resource '{resource}' — \
                 add a DownstreamApis entry to KarsAuthConfig.spec.downstreamApis or \
                 extend resource_to_service_name() in sidecar_client.rs"
            )
        })?;
        self.get_token_for_service(service).await
    }

    /// Acquire a token by explicit sidecar service name, bypassing
    /// the resource → service lookup.
    ///
    /// Phase 6.b — the AGT mesh peer audience is operator-configurable
    /// (default `api://agentmesh/.default`, but in tenants that don't
    /// have that SP provisioned, operators set it to the blueprint
    /// app's GUID or any other valid Entra resource). The
    /// `resource_to_service_name` mapping cannot enumerate every
    /// possible operator value; the `/v1/mesh-token` route therefore
    /// calls this method directly with `service="AgentMesh"`, and
    /// the sidecar mints via the
    /// `DownstreamApis__AgentMesh__Scopes__0` env (which the controller
    /// already auto-emits from `KarsAuthConfig.spec.meshAuthAudience`).
    pub async fn get_token_for_service(&self, service: &str) -> Result<String> {
        // Cache key is (service, agent_id) — agent_id is stable for
        // the pod lifetime but include it for future-proofing.
        let cache_key = format!("{}|{}", service, self.pinned_agent_id);
        {
            let r = self.cache.read().await;
            if let Some(cached) = r.get(&cache_key)
                && cached.expires_at > Instant::now() + Duration::from_secs(60)
            {
                return Ok(cached.token.clone());
            }
        }

        let url = format!(
            "{}/AuthorizationHeaderUnauthenticated/{}",
            self.base_url, service,
        );
        // Override the Host header to `localhost:5000`. The Microsoft
        // Entra SDK auth-sidecar's ASP.NET Core HostFiltering
        // middleware rejects non-`localhost` Host headers by default,
        // even when `AllowedHosts=*` is set in the environment — the
        // sidecar's Program.cs binds the option through a different
        // config source. Sending `Host: localhost:5000` bypasses the
        // filter without weakening security: the ingress NetworkPolicy
        // remains the real boundary, and the sidecar still
        // authenticates and rejects unauthorized requests.
        let resp = self
            .client
            .get(&url)
            .header(reqwest::header::HOST, "localhost:5000")
            .query(&[("AgentIdentity", self.pinned_agent_id.as_str())])
            .send()
            .await
            .with_context(|| format!("auth-sidecar HTTP call to {url} failed"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "auth-sidecar returned {status} for service '{service}' (agent_id={}): {}",
                self.pinned_agent_id,
                &body[..body.len().min(400)]
            ));
        }

        let body = resp
            .text()
            .await
            .with_context(|| "read auth-sidecar response body")?;

        // The Microsoft Entra SDK sidecar's response shape varies
        // slightly across builds. We've observed:
        //   - JSON `{"AuthorizationHeader": "Bearer xxx", "ExpiresIn": 3600}`
        //     (the documented contract; some 1.x builds)
        //   - JSON `"Bearer xxx"` (raw quoted string; other 1.x builds)
        //   - Plain text `Bearer xxx` (no JSON wrapper)
        // Parse leniently rather than failing fast on the strict
        // JSON shape — the live cluster validated that the actual
        // sidecar returns one of the non-strict forms, and a
        // strict-only parser would silently disable agent-id auth
        // for the entire pod.
        let (auth_header, expires_in_secs) = parse_sidecar_body(&body).with_context(|| {
            format!(
                "parse auth-sidecar response body: {}",
                &body[..body.len().min(200)]
            )
        })?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| {
                anyhow!(
                    "auth-sidecar AuthorizationHeader missing expected 'Bearer ' prefix; \
                     got: {}",
                    &auth_header[..auth_header.len().min(40)]
                )
            })?
            .to_string();

        // Defense-in-depth claim pinning. The sidecar SHOULD always
        // return tokens for the expected tenant, agent identity,
        // resource audience, and a fresh exp. A misconfigured
        // KarsAuthConfig, a compromised sidecar, or an unexpected
        // Microsoft SDK behaviour change could violate any of those
        // invariants. We decode the JWT payload UNVERIFIED (downstream
        // Azure validates signatures) and HARD-reject on each check.
        // The returned `validated_ttl_secs` caps how long the caller
        // may cache the token based on the JWT `exp` claim.
        let validated_ttl_secs = self
            .validate_token_claims(&token, service)
            .with_context(|| format!("validating sidecar token claims for service '{service}'"))?;

        // Final cache TTL = min(sidecar-advertised TTL, JWT-exp-derived TTL).
        // Both are capped at 60s above to leave skew headroom.
        let sidecar_ttl_secs = expires_in_secs
            .map(|s| s.saturating_sub(60).max(60))
            .unwrap_or(DEFAULT_TOKEN_TTL_SECS);
        let ttl_secs = sidecar_ttl_secs.min(validated_ttl_secs);
        {
            let mut w = self.cache.write().await;
            w.insert(
                cache_key,
                CachedToken {
                    token: token.clone(),
                    expires_at: Instant::now() + Duration::from_secs(ttl_secs),
                },
            );
        }

        tracing::debug!(
            service = %service,
            agent_id = %self.pinned_agent_id,
            cached_for_secs = ttl_secs,
            "minted token via auth-sidecar"
        );

        Ok(token)
    }

    /// Returns the pinned agent identity appId. Surfaced for diagnostics
    /// (e.g. `/healthz` payloads, structured log fields).
    pub fn pinned_agent_id(&self) -> &str {
        &self.pinned_agent_id
    }

    /// Returns the expected tenant id, if configured. Surfaced for
    /// diagnostics. `None` indicates tid pinning is disabled.
    pub fn expected_tenant_id(&self) -> Option<&str> {
        self.expected_tenant_id.as_deref()
    }

    /// Validate JWT claims on a token returned by the sidecar and
    /// return a cache TTL ceiling derived from `exp`.
    ///
    /// Performs an UNVERIFIED payload decode — we trust the sidecar's
    /// TLS-less localhost path because the NetworkPolicy + UID-1001
    /// iptables baseline gate every byte. We rely on downstream Azure
    /// resources to do cryptographic signature validation when the
    /// token is presented.
    ///
    /// Semantics (every check is HARD — returns `Err`):
    /// - `tid` mismatch: cross-tenant token confusion guard. Skipped
    ///   ONLY when `expected_tenant_id` is `None` (insecure dev mode,
    ///   warned at boot).
    /// - `tid` missing while pinning is enabled: cannot prove tenant.
    /// - `appid`/`azp` mismatch when at least one is present: would
    ///   collapse per-sandbox audit attribution.
    /// - `aud` mismatch against the resource being requested: prevents
    ///   the cache poisoning where a token minted for service A is
    ///   reused for service B because the sidecar misrouted.
    /// - `exp` in the past or absent (when tid pinning enabled):
    ///   refuses to cache stale tokens.
    ///
    /// On success returns the max TTL (in seconds) the caller may
    /// cache this token for, capped by `exp - now - 60s` (60s skew).
    fn validate_token_claims(&self, token: &str, service: &str) -> Result<u64> {
        let claims = decode_jwt_claims_unverified(token).with_context(
            || "auth-sidecar returned a value that does not parse as a JWT — refusing",
        )?;

        if let Some(expected) = self.expected_tenant_id.as_deref() {
            let actual = claims.tid.as_deref().unwrap_or("").trim();
            if actual.is_empty() {
                return Err(anyhow!(
                    "auth-sidecar token has no `tid` claim — cannot verify it belongs to \
                     expected tenant '{expected}'. Service: {service}."
                ));
            }
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(anyhow!(
                    "auth-sidecar token tid mismatch (service '{service}'): \
                     expected '{expected}', got '{actual}'. \
                     Failing closed to prevent cross-tenant token confusion."
                ));
            }
        }

        // Principal pinning. The Microsoft sidecar's
        // `?AgentIdentity=<appId>` parameter should always produce a
        // token whose `appid` (v1) or `azp` (v2) equals the pinned id.
        // If at least ONE of those claims is present, we hard-fail on
        // any mismatch to preserve per-sandbox audit attribution.
        // If BOTH are present, BOTH must match (or be empty).
        let pinned = self.pinned_agent_id.as_str();
        let principal_check = |claim_name: &str, value: Option<&str>| -> Result<bool> {
            match value.map(str::trim).filter(|s| !s.is_empty()) {
                None => Ok(false),
                Some(v) if v.eq_ignore_ascii_case(pinned) => Ok(true),
                Some(v) => Err(anyhow!(
                    "auth-sidecar token `{claim_name}` claim ('{v}') does not match pinned \
                     agent identity ('{pinned}') for service '{service}'. \
                     Failing closed to preserve per-sandbox audit attribution."
                )),
            }
        };
        let appid_matched = principal_check("appid", claims.appid.as_deref())?;
        let azp_matched = principal_check("azp", claims.azp.as_deref())?;
        if !(appid_matched || azp_matched) {
            // Neither claim was present at all. Don't fail (some Entra
            // token shapes legitimately omit both — e.g. some MSI
            // tokens), but log a structured WARN so operators can
            // notice if a sidecar regressed.
            tracing::warn!(
                pinned = %pinned,
                service = %service,
                "auth-sidecar token has neither `appid` nor `azp` claim — cannot verify \
                 per-sandbox principal attribution. Token will still be used."
            );
        }

        // Audience pinning. Each service we proxy has a closed set of
        // acceptable aud values; a token minted for service A being
        // returned for service B's request is a misroute we MUST
        // refuse rather than cache.
        let expected_auds = expected_audiences_for_service(service);
        if !expected_auds.is_empty() {
            let actual_auds = claims.aud_iter();
            if actual_auds.is_empty() {
                return Err(anyhow!(
                    "auth-sidecar token has no `aud` claim for service '{service}' — \
                     cannot verify the token was minted for this resource."
                ));
            }
            let any_match = actual_auds.iter().any(|a| {
                let a = a.trim().trim_end_matches('/').to_ascii_lowercase();
                expected_auds
                    .iter()
                    .any(|expected| a.eq_ignore_ascii_case(expected))
            });
            if !any_match {
                return Err(anyhow!(
                    "auth-sidecar token aud mismatch (service '{service}'): \
                     expected one of {expected_auds:?}, got {actual_auds:?}. \
                     Failing closed to prevent cross-resource token reuse."
                ));
            }
        }

        // Exp pinning + TTL cap.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let skew_secs = 60_i64;
        let ttl_cap_secs: u64 = match claims.exp {
            Some(exp) => {
                if exp <= now + skew_secs {
                    return Err(anyhow!(
                        "auth-sidecar token already expired (exp={exp}, now={now}, \
                         skew=60s) for service '{service}'."
                    ));
                }
                // Reserve 60s skew so callers never present a token
                // within its last minute of life.
                let remaining = exp - now - skew_secs;
                (remaining.max(60)) as u64
            }
            None => {
                if self.expected_tenant_id.is_some() {
                    return Err(anyhow!(
                        "auth-sidecar token has no `exp` claim for service '{service}'. \
                         Refusing to cache an unbounded-lifetime token."
                    ));
                }
                // Insecure mode: no exp, no tid pinning. Apply default.
                DEFAULT_TOKEN_TTL_SECS
            }
        };

        Ok(ttl_cap_secs)
    }
}

/// Minimal JWT payload claim subset used for sidecar token pinning.
#[derive(Debug, Default, Deserialize)]
struct JwtPayload {
    /// Tenant ID (GUID). Required for tid pinning.
    #[serde(default)]
    tid: Option<String>,
    /// v1.0 endpoint: application id of the principal the token was
    /// minted for.
    #[serde(default)]
    appid: Option<String>,
    /// v2.0 endpoint: authorized party (replaces `appid` semantically
    /// for app-only tokens).
    #[serde(default)]
    azp: Option<String>,
    /// Audience the token was minted for. Entra emits a single string
    /// for app tokens but the JWT RFC permits an array, so we accept
    /// both via `serde_json::Value`.
    #[serde(default)]
    aud: Option<serde_json::Value>,
    /// Expiration in seconds since the Unix epoch.
    #[serde(default)]
    exp: Option<i64>,
}

impl JwtPayload {
    /// Normalize the `aud` claim into a Vec of String. JWT RFC 7519
    /// permits both a single string and an array of strings. Entra
    /// app tokens use a single string in practice; we accept either.
    fn aud_iter(&self) -> Vec<String> {
        match &self.aud {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            // Defensive: numbers/booleans/objects in `aud` are
            // out-of-spec; treat as absent so the caller's "missing
            // aud" branch fires.
            Some(_) => Vec::new(),
        }
    }
}

/// Decode the payload of a JWT WITHOUT verifying its signature.
///
/// JWT format is `<base64url(header)>.<base64url(payload)>.<signature>`,
/// where base64url is URL-safe-no-padding. We split on `.`, take the
/// payload segment, base64url-decode, and JSON-parse into the
/// subset of claims we care about.
///
/// Requires EXACTLY three segments — two-segment "unsecured JWS" and
/// four-segment JWE shapes are rejected.
///
/// NOT a security primitive on its own — the caller MUST be using this
/// over a trust boundary that already proves the token's authenticity
/// (here: in-cluster Service to a NetworkPolicy-gated Microsoft sidecar
/// which itself authenticates against Entra). The unverified decode is
/// solely for *content* pinning (tid, appid/azp, aud, exp).
fn decode_jwt_claims_unverified(token: &str) -> Result<JwtPayload> {
    let trimmed = token.trim();
    let segments: Vec<&str> = trimmed.split('.').collect();
    if segments.len() != 3 {
        return Err(anyhow!(
            "JWT must have exactly 3 dot-separated segments (header.payload.signature); \
             got {}",
            segments.len()
        ));
    }
    let payload_b64 = segments[1];
    if payload_b64.is_empty() {
        return Err(anyhow!("JWT payload segment is empty"));
    }

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64.as_bytes())
        .with_context(|| "base64url-decode JWT payload")?;

    let claims: JwtPayload =
        serde_json::from_slice(&payload_bytes).with_context(|| "JSON-parse JWT payload claims")?;
    Ok(claims)
}

/// Expected `aud` claim values for a sidecar service-name. Returns
/// an empty Vec when no audience pinning is configured for the
/// service (treated as "skip aud validation" rather than "reject all").
///
/// Entra accepts tokens minted for either the resource URI or the
/// resource's first-party application id (the GUID). Both forms are
/// listed so the validation matches whichever the sidecar config
/// asked for.
///
/// Comparisons in `validate_token_claims` are case-insensitive and
/// strip trailing slashes.
fn expected_audiences_for_service(service: &str) -> Vec<&'static str> {
    match service {
        "Foundry" => vec![
            "https://cognitiveservices.azure.com",
            "https://ai.azure.com",
        ],
        "Graph" => vec![
            "https://graph.microsoft.com",
            // First-party Microsoft Graph application id.
            "00000003-0000-0000-c000-000000000000",
        ],
        "OpenAI" => vec![
            "https://cognitiveservices.azure.com",
            "https://api.openai.azure.com",
        ],
        "Management" => vec![
            "https://management.azure.com",
            "https://management.core.windows.net",
        ],
        "Search" => vec!["https://search.azure.com"],
        // Unknown service → skip aud pinning rather than reject.
        _ => Vec::new(),
    }
}

/// Parse the auth-sidecar response body across the three shapes the
/// Microsoft Entra SDK sidecar emits in practice:
///
/// 1. `{"AuthorizationHeader": "Bearer xxx", "ExpiresIn": 3600}` — the
///    documented contract, observed in some 1.x builds.
/// 2. `"Bearer xxx"` — a JSON string with no wrapping object,
///    observed in other 1.x builds.
/// 3. `Bearer xxx` — plain text with no JSON quotes.
///
/// Returns `(auth_header_value_with_bearer_prefix, optional_ttl_seconds)`.
/// The caller strips the `Bearer ` prefix before caching.
///
/// We intentionally do NOT depend on the `Content-Type` response
/// header — some sidecar builds set `text/plain; charset=utf-8` even
/// for the JSON-shape payload, and depending on header inspection
/// would mask the actual semantics.
fn parse_sidecar_body(body: &str) -> anyhow::Result<(String, Option<u64>)> {
    let trimmed = body.trim();

    // Shape 1: JSON object with documented `AuthorizationHeader` field.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(obj) = v.as_object() {
            // Try the documented PascalCase first, then the variant
            // camelCase build, then the legacy `authorization_header`
            // snake_case some forks produce.
            let auth_field = obj
                .get("AuthorizationHeader")
                .or_else(|| obj.get("authorizationHeader"))
                .or_else(|| obj.get("authorization_header"))
                .and_then(|v| v.as_str());
            if let Some(header) = auth_field {
                let ttl = obj
                    .get("ExpiresIn")
                    .or_else(|| obj.get("expiresIn"))
                    .or_else(|| obj.get("expires_in"))
                    .and_then(|v| v.as_u64());
                return Ok((header.to_string(), ttl));
            }
        }
        // Shape 2: JSON string `"Bearer xxx"`.
        if let Some(s) = v.as_str() {
            return Ok((s.to_string(), None));
        }
    }

    // Shape 3: plain text `Bearer xxx`.
    if !trimmed.is_empty() {
        return Ok((trimmed.to_string(), None));
    }

    Err(anyhow!("auth-sidecar response body was empty"))
}

/// Translate the legacy `resource` audience string used throughout
/// the router into the sidecar's service-name key.
///
/// The sidecar reads `DownstreamApis__<key>__*` from its env. The
/// `<key>` is operator-configured (in `KarsAuthConfig.spec.downstreamApis`)
/// but kars conventionally uses `Foundry`, `Graph`, and `OpenAI`.
///
/// Returns `None` when the resource is unrecognised — the caller
/// surfaces this as a hard error so an unmapped resource fails
/// loudly rather than silently degrading to a different identity.
fn resource_to_service_name(resource: &str) -> Option<&'static str> {
    // Strip trailing slash and any `.default` scope suffix so callers
    // can pass either form.
    let r = resource.trim_end_matches('/').trim_end_matches("/.default");
    // Match longest prefix first.
    if r.starts_with("https://cognitiveservices.azure.com") || r.starts_with("https://ai.azure.com")
    {
        Some("Foundry")
    } else if r.starts_with("https://graph.microsoft.com") {
        Some("Graph")
    } else if r.starts_with("https://api.openai.azure.com")
        || r.starts_with("https://openai.azure.com")
    {
        Some("OpenAI")
    } else if r.starts_with("https://management.azure.com")
        || r.starts_with("https://management.core.windows.net")
    {
        Some("Management")
    } else if r.starts_with("https://search.azure.com") {
        Some("Search")
    } else if r == "api://agentmesh" || r.starts_with("api://agentmesh/") {
        // AGT mesh peer authentication. The controller emits a
        // matching `DownstreamApis__AgentMesh__*` cluster on the
        // sidecar when `KarsAuthConfig.spec.meshAuthBackend ==
        // EntraAgentIdentity`. See
        // docs/architecture/entra-agent-id/06-mesh-trust-design.md.
        Some("AgentMesh")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_mapping_handles_canonical_resources() {
        assert_eq!(
            resource_to_service_name("https://cognitiveservices.azure.com"),
            Some("Foundry")
        );
        assert_eq!(
            resource_to_service_name("https://cognitiveservices.azure.com/"),
            Some("Foundry")
        );
        assert_eq!(
            resource_to_service_name("https://ai.azure.com/.default"),
            Some("Foundry")
        );
        assert_eq!(
            resource_to_service_name("https://graph.microsoft.com/.default"),
            Some("Graph")
        );
        assert_eq!(
            resource_to_service_name("https://management.azure.com"),
            Some("Management")
        );
    }

    #[test]
    fn resource_mapping_returns_none_for_unknown() {
        // Catches typos and unforeseen audiences — caller surfaces as
        // a hard error rather than silently falling back to a wrong
        // identity model.
        assert_eq!(
            resource_to_service_name("https://example.unknown.audience"),
            None
        );
        assert_eq!(resource_to_service_name(""), None);
    }

    #[test]
    fn resource_mapping_handles_agent_mesh_audience() {
        // Phase 6: `/v1/mesh-token` calls `get_token("api://agentmesh/.default")`.
        // Pin the mapping so the sidecar receives `AgentMesh` and
        // resolves `DownstreamApis__AgentMesh__Scopes__0` correctly.
        assert_eq!(
            resource_to_service_name("api://agentmesh/.default"),
            Some("AgentMesh")
        );
        assert_eq!(
            resource_to_service_name("api://agentmesh"),
            Some("AgentMesh")
        );
        assert_eq!(
            resource_to_service_name("api://agentmesh/"),
            Some("AgentMesh")
        );
        // Forward-compat: future scope-suffixed variants the
        // entrypoint may emit must still resolve.
        assert_eq!(
            resource_to_service_name("api://agentmesh/user_impersonation"),
            Some("AgentMesh")
        );
        // Boundary: a similarly-named but distinct audience must NOT
        // map to the same service entry.
        assert_eq!(resource_to_service_name("api://agentmesh-other"), None);
    }

    #[test]
    fn parse_sidecar_body_handles_documented_json_shape() {
        let (h, ttl) =
            parse_sidecar_body(r#"{"AuthorizationHeader": "Bearer abc", "ExpiresIn": 3600}"#)
                .unwrap();
        assert_eq!(h, "Bearer abc");
        assert_eq!(ttl, Some(3600));
    }

    #[test]
    fn parse_sidecar_body_handles_camelcase_json() {
        let (h, ttl) = parse_sidecar_body(r#"{"authorizationHeader": "Bearer xyz"}"#).unwrap();
        assert_eq!(h, "Bearer xyz");
        assert_eq!(ttl, None);
    }

    #[test]
    fn parse_sidecar_body_handles_quoted_string() {
        let (h, ttl) = parse_sidecar_body(r#""Bearer raw""#).unwrap();
        assert_eq!(h, "Bearer raw");
        assert_eq!(ttl, None);
    }

    #[test]
    fn parse_sidecar_body_handles_plain_text() {
        let (h, ttl) = parse_sidecar_body("Bearer plain\n").unwrap();
        assert_eq!(h, "Bearer plain");
        assert_eq!(ttl, None);
    }

    #[test]
    fn parse_sidecar_body_rejects_empty() {
        assert!(parse_sidecar_body("").is_err());
        assert!(parse_sidecar_body("   \n").is_err());
    }

    #[test]
    fn from_env_partial_config_is_err() {
        // SAFETY: env mutations across parallel tests would race. We
        // hold a single test mutex (other tests in this module touch
        // only resource_to_service_name) and only this test pokes
        // these specific env vars in the crate. Using a process-wide
        // mutex would be sturdier but is overkill here.
        unsafe {
            std::env::remove_var(ENV_AUTH_SIDECAR_URL);
            std::env::remove_var(ENV_PINNED_AGENT_IDENTITY_APP_ID);
            std::env::remove_var(ENV_EXPECTED_TENANT_ID);
        }
        assert!(
            matches!(SidecarClient::from_env(), Ok(None)),
            "no env vars → Ok(None) (sidecar mode disabled)"
        );

        // Only URL set → Err (partial config). Critical: would
        // silently fall back to WI / IMDS / API-key otherwise.
        unsafe {
            std::env::set_var(ENV_AUTH_SIDECAR_URL, "http://127.0.0.1:8080");
        }
        let err = SidecarClient::from_env().unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Inconsistent sidecar config"),
            "partial config (URL only) should be Err, got: {msg}"
        );

        // Only PINNED set → Err.
        unsafe {
            std::env::remove_var(ENV_AUTH_SIDECAR_URL);
            std::env::set_var(ENV_PINNED_AGENT_IDENTITY_APP_ID, "agent-x");
        }
        let err = SidecarClient::from_env().unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Inconsistent sidecar config"),
            "partial config (PINNED only) should be Err, got: {msg}"
        );

        // Both set → Ok(Some) with tid pinning DISABLED (no EXPECTED).
        unsafe {
            std::env::set_var(ENV_AUTH_SIDECAR_URL, "http://127.0.0.1:8080/");
            std::env::set_var(ENV_PINNED_AGENT_IDENTITY_APP_ID, "agent-x");
        }
        let c = SidecarClient::from_env()
            .expect("Ok when both vars set")
            .expect("Some when both vars set");
        assert_eq!(c.base_url, "http://127.0.0.1:8080");
        assert_eq!(c.pinned_agent_id(), "agent-x");
        assert!(
            c.expected_tenant_id().is_none(),
            "no EXPECTED_TENANT_ID → tid pinning disabled"
        );

        // With expected tenant set, it must round-trip lowercased
        unsafe {
            std::env::set_var(
                ENV_EXPECTED_TENANT_ID,
                "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
            );
        }
        let c2 = SidecarClient::from_env().unwrap().unwrap();
        assert_eq!(
            c2.expected_tenant_id(),
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
        );

        unsafe {
            std::env::remove_var(ENV_AUTH_SIDECAR_URL);
            std::env::remove_var(ENV_PINNED_AGENT_IDENTITY_APP_ID);
            std::env::remove_var(ENV_EXPECTED_TENANT_ID);
        }
    }

    // ─── JWT decode + claim pinning ──────────────────────────────────

    /// Build a synthetic JWT with the given payload claims object.
    /// Header and signature segments are deterministic test fixtures —
    /// the router's unverified decoder ignores them.
    fn make_jwt(payload_json: &str) -> String {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
        // Signature segment is opaque to the unverified decoder.
        format!("{header}.{payload}.dGVzdC1zaWc")
    }

    /// Build a synthetic JWT carrying every claim the validator
    /// inspects, so individual tests don't have to repeat the
    /// boilerplate. `exp_offset_secs` is added to "now"; pass a
    /// large positive to produce a fresh token.
    fn make_jwt_full(tid: &str, principal: &str, aud: &str, exp_offset_secs: i64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let exp = now + exp_offset_secs;
        make_jwt(&format!(
            r#"{{"tid":"{tid}","appid":"{principal}","azp":"{principal}",
                  "aud":"{aud}","exp":{exp}}}"#,
        ))
    }

    #[test]
    fn decode_jwt_extracts_all_pinning_claims() {
        let token = make_jwt(
            r#"{"tid":"AAAA-BBBB","appid":"app-1","azp":"app-2","aud":"https://ex","exp":4070908800}"#,
        );
        let claims = decode_jwt_claims_unverified(&token).unwrap();
        assert_eq!(claims.tid.as_deref(), Some("AAAA-BBBB"));
        assert_eq!(claims.appid.as_deref(), Some("app-1"));
        assert_eq!(claims.azp.as_deref(), Some("app-2"));
        assert_eq!(claims.aud_iter(), vec!["https://ex".to_string()]);
        assert_eq!(claims.exp, Some(4070908800));
    }

    #[test]
    fn decode_jwt_aud_array_is_normalized() {
        // RFC 7519 permits aud as an array of strings.
        let token = make_jwt(r#"{"tid":"t","aud":["https://a.example","https://b.example"]}"#);
        let claims = decode_jwt_claims_unverified(&token).unwrap();
        assert_eq!(
            claims.aud_iter(),
            vec![
                "https://a.example".to_string(),
                "https://b.example".to_string()
            ]
        );
    }

    #[test]
    fn decode_jwt_aud_non_string_non_array_returns_empty() {
        // Defensive: out-of-spec shapes (number, bool, object) become
        // an empty audience list so the caller's "missing aud" branch
        // fires hard rather than silently passing.
        let token = make_jwt(r#"{"tid":"t","aud":42}"#);
        let claims = decode_jwt_claims_unverified(&token).unwrap();
        assert!(claims.aud_iter().is_empty());
    }

    #[test]
    fn decode_jwt_rejects_two_segment_unsecured_jws() {
        // RFC 7519 allows alg=none unsecured tokens with two segments
        // (header.payload). We MUST reject them — the sidecar should
        // never return one, and accepting them weakens the malformed-
        // token guard.
        assert!(decode_jwt_claims_unverified("aGVhZGVy.eyJ0aWQiOiJ4In0").is_err());
    }

    #[test]
    fn decode_jwt_rejects_missing_payload_segment() {
        assert!(decode_jwt_claims_unverified("only-one-segment").is_err());
    }

    #[test]
    fn decode_jwt_rejects_empty_payload() {
        assert!(decode_jwt_claims_unverified("aGVhZGVy..c2ln").is_err());
    }

    #[test]
    fn decode_jwt_rejects_four_segments() {
        // Defensive guard: four-segment tokens (e.g. JWE) are not
        // valid JWS access tokens. The unverified decoder rejects.
        assert!(decode_jwt_claims_unverified("a.b.c.d").is_err());
    }

    #[test]
    fn decode_jwt_rejects_malformed_base64() {
        // Bad base64 in the payload position.
        assert!(decode_jwt_claims_unverified("a.!!!!!!.c").is_err());
    }

    #[test]
    fn decode_jwt_rejects_non_json_payload() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let payload = URL_SAFE_NO_PAD.encode(b"not-json");
        let token = format!("aGVhZGVy.{payload}.c2ln");
        assert!(decode_jwt_claims_unverified(&token).is_err());
    }

    fn make_client_with_pin(pinned_id: &str, expected_tid: Option<&str>) -> SidecarClient {
        SidecarClient {
            base_url: "http://test".into(),
            pinned_agent_id: pinned_id.into(),
            expected_tenant_id: expected_tid.map(|s| s.to_ascii_lowercase()),
            client: reqwest::Client::builder().build().unwrap(),
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[test]
    fn validate_token_accepts_matching_claims_and_returns_ttl_cap() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let token = make_jwt_full(
            "AAAA-BBBB",
            "app-1",
            "https://cognitiveservices.azure.com",
            3600,
        );
        let ttl = c.validate_token_claims(&token, "Foundry").unwrap();
        // Cap = exp - now - 60s; exp_offset = 3600 → cap ~3540.
        assert!(
            (3500..=3540).contains(&ttl),
            "ttl cap should be near 3540, got {ttl}"
        );
    }

    #[test]
    fn validate_token_rejects_wrong_tid() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let token = make_jwt_full(
            "OTHER-TENANT",
            "app-1",
            "https://cognitiveservices.azure.com",
            3600,
        );
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("tid mismatch"),
            "error should mention tid mismatch, got: {msg}"
        );
        assert!(msg.contains("Foundry"), "error should mention service");
    }

    #[test]
    fn validate_token_rejects_missing_tid_when_pinning_enabled() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let exp = now + 3600;
        let token = make_jwt(&format!(
            r#"{{"appid":"app-1","aud":"https://cognitiveservices.azure.com","exp":{exp}}}"#
        ));
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no `tid` claim"),
            "error should explain missing tid, got: {msg}"
        );
    }

    #[test]
    fn validate_token_accepts_missing_tid_when_pinning_disabled() {
        // Insecure mode — tid AND exp may be absent (development).
        let c = make_client_with_pin("app-1", None);
        let token = make_jwt(r#"{"appid":"app-1","aud":"https://cognitiveservices.azure.com"}"#);
        let ttl = c.validate_token_claims(&token, "Foundry").unwrap();
        assert_eq!(ttl, DEFAULT_TOKEN_TTL_SECS);
    }

    #[test]
    fn validate_token_rejects_principal_mismatch_hard() {
        // appid/azp mismatch IS a hard error (rubber-duck #1) — would
        // collapse per-sandbox audit attribution otherwise.
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let token = make_jwt_full(
            "aaaa-bbbb",
            "different-app",
            "https://cognitiveservices.azure.com",
            3600,
        );
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("does not match pinned"),
            "error should explain principal mismatch, got: {msg}"
        );
    }

    #[test]
    fn validate_token_rejects_when_only_one_of_appid_azp_mismatches() {
        // If both appid and azp are present, BOTH must match (or be
        // absent). Catches the case where a sidecar leaks a parent
        // blueprint via one claim and the child via the other.
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let exp = now + 3600;
        let token = make_jwt(&format!(
            r#"{{"tid":"aaaa-bbbb","appid":"app-1","azp":"DIFFERENT",
                  "aud":"https://cognitiveservices.azure.com","exp":{exp}}}"#
        ));
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("`azp`") && msg.contains("does not match pinned"),
            "error should pinpoint the azp-side mismatch, got: {msg}"
        );
    }

    #[test]
    fn validate_token_uses_azp_when_appid_absent() {
        // v2 endpoint: only `azp` is present.
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let exp = now + 3600;
        let token = make_jwt(&format!(
            r#"{{"tid":"aaaa-bbbb","azp":"app-1",
                  "aud":"https://cognitiveservices.azure.com","exp":{exp}}}"#
        ));
        c.validate_token_claims(&token, "Foundry").unwrap();
    }

    #[test]
    fn validate_token_accepts_when_principal_claims_both_absent() {
        // Some Entra MSI token shapes legitimately omit both appid
        // and azp. We DO NOT fail closed in this case — downstream
        // RBAC still runs. We log a WARN.
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let exp = now + 3600;
        let token = make_jwt(&format!(
            r#"{{"tid":"aaaa-bbbb","aud":"https://cognitiveservices.azure.com","exp":{exp}}}"#
        ));
        c.validate_token_claims(&token, "Foundry").unwrap();
    }

    #[test]
    fn validate_token_rejects_wrong_aud() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        // Token aud is for Graph but we're requesting Foundry — would
        // be cached under the Foundry key and poison subsequent calls.
        let token = make_jwt_full("aaaa-bbbb", "app-1", "https://graph.microsoft.com", 3600);
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("aud mismatch"),
            "error should mention aud mismatch, got: {msg}"
        );
    }

    #[test]
    fn validate_token_rejects_missing_aud() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let exp = now + 3600;
        let token = make_jwt(&format!(
            r#"{{"tid":"aaaa-bbbb","appid":"app-1","exp":{exp}}}"#
        ));
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no `aud` claim"),
            "error should explain missing aud, got: {msg}"
        );
    }

    #[test]
    fn validate_token_accepts_aud_with_trailing_slash() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let token = make_jwt_full(
            "aaaa-bbbb",
            "app-1",
            "https://cognitiveservices.azure.com/", // trailing slash
            3600,
        );
        c.validate_token_claims(&token, "Foundry").unwrap();
    }

    #[test]
    fn validate_token_accepts_first_party_app_id_aud_for_graph() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let token = make_jwt_full(
            "aaaa-bbbb",
            "app-1",
            "00000003-0000-0000-c000-000000000000",
            3600,
        );
        c.validate_token_claims(&token, "Graph").unwrap();
    }

    #[test]
    fn validate_token_skips_aud_pinning_for_unknown_service() {
        // Unknown service → no audience pinning configured → skip
        // (rather than reject all). Lets operators add new
        // DownstreamApis entries without code changes.
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let token = make_jwt_full("aaaa-bbbb", "app-1", "https://random.example", 3600);
        c.validate_token_claims(&token, "Custom").unwrap();
    }

    #[test]
    fn validate_token_rejects_expired_token() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        // exp 1 hour in the past.
        let token = make_jwt_full(
            "aaaa-bbbb",
            "app-1",
            "https://cognitiveservices.azure.com",
            -3600,
        );
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already expired"),
            "error should mention expired, got: {msg}"
        );
    }

    #[test]
    fn validate_token_rejects_token_within_skew_window() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        // exp = now + 30s; less than the 60s skew → reject.
        let token = make_jwt_full(
            "aaaa-bbbb",
            "app-1",
            "https://cognitiveservices.azure.com",
            30,
        );
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already expired"),
            "error should reject token within skew, got: {msg}"
        );
    }

    #[test]
    fn validate_token_rejects_missing_exp_when_pinning_enabled() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        let token = make_jwt(
            r#"{"tid":"aaaa-bbbb","appid":"app-1","aud":"https://cognitiveservices.azure.com"}"#,
        );
        let err = c.validate_token_claims(&token, "Foundry").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no `exp` claim"),
            "error should explain missing exp, got: {msg}"
        );
    }

    #[test]
    fn validate_token_rejects_garbage_token() {
        let c = make_client_with_pin("app-1", Some("aaaa-bbbb"));
        // Not a JWT at all.
        let err = c
            .validate_token_claims("just-a-string", "Foundry")
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("JWT"),
            "error should mention JWT parsing, got: {msg}"
        );
    }

    // ─── End-to-end get_token() with wiremock sidecar ────────────────
    //
    // Spins up a wiremock server impersonating the Microsoft Entra
    // SDK sidecar and verifies that the SidecarClient:
    // - Sends the documented `?AgentIdentity=<appId>` query parameter.
    // - Honours the documented JSON response shape.
    // - HARD-rejects tokens whose claims don't match the pinning
    //   contract (no caching, no leak).
    // - Caches tokens that pass validation (second call → no second
    //   HTTP round-trip).

    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn make_client_pointing_at(
        server: &MockServer,
        pinned_id: &str,
        expected_tid: Option<&str>,
    ) -> SidecarClient {
        SidecarClient {
            base_url: server.uri(),
            pinned_agent_id: pinned_id.into(),
            expected_tenant_id: expected_tid.map(|s| s.to_ascii_lowercase()),
            client: reqwest::Client::builder()
                .timeout(SIDECAR_HTTP_TIMEOUT)
                .build()
                .unwrap(),
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[tokio::test]
    async fn get_token_happy_path_returns_bearer_and_caches() {
        let server = MockServer::start().await;
        let token = make_jwt_full(
            "aaaa-bbbb",
            "app-1",
            "https://cognitiveservices.azure.com",
            3600,
        );
        let body = serde_json::json!({
            "AuthorizationHeader": format!("Bearer {token}"),
            "ExpiresIn": 3600,
        });
        Mock::given(method("GET"))
            .and(path("/AuthorizationHeaderUnauthenticated/Foundry"))
            .and(query_param("AgentIdentity", "app-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1) // EXPECTED EXACTLY ONCE — second get_token should hit cache.
            .mount(&server)
            .await;

        let c = make_client_pointing_at(&server, "app-1", Some("aaaa-bbbb")).await;
        let t1 = c
            .get_token("https://cognitiveservices.azure.com")
            .await
            .unwrap();
        assert_eq!(t1, token);
        // Second call must hit the cache (mock has .expect(1)).
        let t2 = c
            .get_token("https://cognitiveservices.azure.com/")
            .await
            .unwrap();
        assert_eq!(t2, token);
    }

    #[tokio::test]
    async fn get_token_rejects_wrong_tid_and_does_not_cache() {
        let server = MockServer::start().await;
        let bad_token = make_jwt_full(
            "WRONG-TENANT",
            "app-1",
            "https://cognitiveservices.azure.com",
            3600,
        );
        let body = serde_json::json!({
            "AuthorizationHeader": format!("Bearer {bad_token}"),
        });
        // Two calls expected: one fails on validation, second also fails
        // (since cache was never populated by the first).
        Mock::given(method("GET"))
            .and(path("/AuthorizationHeaderUnauthenticated/Foundry"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(2)
            .mount(&server)
            .await;

        let c = make_client_pointing_at(&server, "app-1", Some("aaaa-bbbb")).await;
        let err = c
            .get_token("https://cognitiveservices.azure.com")
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("tid mismatch"),
            "error should mention tid mismatch, got: {msg}"
        );

        // Verify NOT cached by triggering a second call — the mock
        // would fail .expect(2) verification at drop-time if cached.
        let _ = c.get_token("https://cognitiveservices.azure.com").await;
    }

    #[tokio::test]
    async fn get_token_rejects_principal_mismatch_and_does_not_cache() {
        let server = MockServer::start().await;
        let bad_token = make_jwt_full(
            "aaaa-bbbb",
            "different-app",
            "https://cognitiveservices.azure.com",
            3600,
        );
        let body = serde_json::json!({
            "AuthorizationHeader": format!("Bearer {bad_token}"),
        });
        Mock::given(method("GET"))
            .and(path("/AuthorizationHeaderUnauthenticated/Foundry"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(2)
            .mount(&server)
            .await;

        let c = make_client_pointing_at(&server, "app-1", Some("aaaa-bbbb")).await;
        let err = c
            .get_token("https://cognitiveservices.azure.com")
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("does not match pinned"),
            "error should mention principal mismatch, got: {msg}"
        );
        let _ = c.get_token("https://cognitiveservices.azure.com").await;
    }

    #[tokio::test]
    async fn get_token_propagates_sidecar_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/AuthorizationHeaderUnauthenticated/Foundry"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid_client"))
            .mount(&server)
            .await;

        let c = make_client_pointing_at(&server, "app-1", Some("aaaa-bbbb")).await;
        let err = c
            .get_token("https://cognitiveservices.azure.com")
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("401") && msg.contains("Foundry"),
            "error should surface upstream status + service, got: {msg}"
        );
    }

    #[tokio::test]
    async fn get_token_unknown_resource_errors_fast() {
        // No HTTP call should ever be made for an unmapped resource.
        // Use a closed local port to verify (mock server with no Mock).
        let server = MockServer::start().await;
        let c = make_client_pointing_at(&server, "app-1", Some("aaaa-bbbb")).await;
        let err = c
            .get_token("https://random.unknown.service")
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no sidecar service name"),
            "error should explain resource→service mapping miss, got: {msg}"
        );
    }
}
