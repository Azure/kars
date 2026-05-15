// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// ci:loc-ok — Phase 2 multi-CRD reconciler / generated module; intentional. Tracked in plan.md §S15 follow-up.
//! McpServer reconciler — Phase 2 §8 entry 1.
//!
//! Watches `McpServer` CRs and, for each:
//!
//! 1. Ensures a finalizer (`azureclaw.azure.com/mcpserver-cleanup`) so
//!    cascading Secret + ConfigMap deletion runs synchronously when the
//!    CR is removed.
//! 2. Generates an Ed25519 signing keypair the first time we see the CR
//!    and stores it as a `Secret` of type
//!    `azureclaw.azure.com/mcp-signing-key`. Subsequent reconciles
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
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, ObjectMeta, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use rand::RngCore;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::mcp_server::{LocalObjectRef, McpServer, McpServerStatus};
use crate::status::conditions::{self, reason, status as cond_status};
use crate::status::phase::{PHASE_DEGRADED, PHASE_READY, PhaseEventReporter};

/// Field manager for SSA patches emitted by this reconciler. A unique
/// suffix per reconciler is the §10.4 #1 craftsmanship requirement —
/// detects out-of-band tampering.
const FIELD_MANAGER: &str = crate::field_managers::MCP_SERVER;

/// Finalizer name (DNS subdomain). Mirrors
/// `crate::reconciler::FINALIZER` shape.
const FINALIZER: &str = "azureclaw.azure.com/mcpserver-cleanup";

/// Custom Secret type — makes a `kubectl get secrets` listing
/// self-documenting and lets RBAC carve permissions per type.
const SECRET_TYPE: &str = "azureclaw.azure.com/mcp-signing-key";

/// Annotation written on the Secret holding the JWK `kid` (key id) the
/// router will see in the matching `verifying-key`. Useful for
/// operator-side rotation work and audit-log correlation.
const KID_ANNOTATION: &str = "azureclaw.azure.com/mcp-signing-kid";

/// Maximum size of a JWKS document we will accept. Issuers serve
/// well-formed JWKS responses in the low-kilobytes; anything past 256 KiB
/// is almost certainly an attack or a misconfigured edge that returned
/// HTML. Matches the upper bound used by `mcp/oauth.rs::JwkSet` parsing
/// before deserialization rejects huge inputs anyway.
const MAX_JWKS_BYTES: usize = 256 * 1024;

/// Timeout for the issuer discovery + JWKS HTTP GETs. Bounded — the
/// reconciler should never hang on a slow issuer.
const HTTP_TIMEOUT_SECS: u64 = 10;

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
}

struct Ctx {
    client: Client,
    /// Override hook for tests — swap the JWKS fetcher with a mock.
    jwks_fetcher: Arc<dyn JwksFetcher>,
    /// Publisher for `LimitedSupport` Warning Events. Optional so
    /// unit tests can construct a `Ctx` without a real `Client` —
    /// production builds always wire it via `run()`.
    phase_reporter: Option<PhaseEventReporter>,
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

async fn reconcile(mcp: Arc<McpServer>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = mcp.name_any();
    let ns = mcp.namespace().unwrap_or_else(|| "azureclaw-system".into());
    tracing::info!(mcp = %name, ns = %ns, "Reconciling McpServer");

    let api: Api<McpServer> = Api::namespaced(ctx.client.clone(), &ns);
    let secrets: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);

    // Deletion path — finalizer-cascading cleanup.
    if mcp.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &secrets, &configmaps, &mcp, &name).await;
    }

    // Add finalizer if missing.
    if !mcp
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({"apiVersion":"azureclaw.azure.com/v1alpha1","kind":"McpServer","metadata":{"finalizers":[FINALIZER]}});
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
    let (effective_spec, bundle_ref_digest, source_degraded) = resolve_mcp_source(&mcp).await;

    // 1. Ensure signing keypair Secret.
    let secret_name = format!("mcp-{name}-signing");
    let signing_kid = ensure_signing_secret(&secrets, &secret_name, &name).await?;

    // 2. Ensure metadata/JWKS ConfigMap. The CM (`mcp-{name}-jwks`) is
    // ALWAYS created — its `meta.json` carries the upstream `url` +
    // `allowedTools` that the inference-router's `McpServerRegistry`
    // needs to forward calls, and its presence is also what the sandbox
    // reconciler mirrors into the sandbox namespace at
    // `/etc/azureclaw/mcp/<name>/`. When `productionMode=false` we emit
    // an empty `{"keys": []}` JWKS default (no inbound OAuth
    // verification needed in dev mode — `/mcp` is mounted on the
    // loopback-only dev surface) but still register the URL so
    // outbound forwarding works. When `productionMode=true` the JWKS
    // is fetched from `oauth.issuer` and replaces the default.
    let cm_name = format!("mcp-{name}-jwks");
    let meta = McpServerMeta::from_spec(&effective_spec);
    let mut jwks_ref: Option<LocalObjectRef> = None;
    let mut degraded: Option<(&'static str, String)> = source_degraded;
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
        degraded
            .as_ref()
            .map(|(reason, msg)| (*reason, msg.as_str())),
    );
    let phase = if degraded.is_some() {
        PHASE_DEGRADED
    } else {
        // Slice 0 honesty: McpServer reconciler today binds exactly
        // one server per ClawSandbox via `spec.mcp:` (singular).
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
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "McpServer",
        "status": McpServerStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            last_probed_at: Some(rfc3339_now()),
            signing_key_ref: Some(signing_ref),
            jwks_config_map_ref: jwks_ref,
            bundle_ref_digest: bundle_ref_digest.clone(),
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
    } else {
        // Slice 0 honesty event: tell operators the singular
        // `spec.mcp:` model is intentional-today / migrating in
        // Slice 4. Best-effort — never fail reconcile on Event
        // publish.
        if let Some(reporter) = &ctx.phase_reporter
            && let Err(e) = reporter
                .warn_limited_support(
                    &*mcp,
                    "BindMcpServer",
                    "McpServer is reconciled via a singular `spec.mcp` binding today; \
                     a plural multi-server model lands in crd-well-oiled-machine Slice 4. \
                     CRs assuming a list of MCP servers will be migrated automatically.",
                )
                .await
        {
            tracing::warn!(error = %e, "failed to publish LimitedSupport event");
        }
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
                    "azureclaw-controller".into(),
                ),
                ("azureclaw.azure.com/mcp-server".into(), owner.into()),
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
                    "azureclaw-controller".into(),
                ),
                ("azureclaw.azure.com/mcp-server".into(), owner.into()),
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
    api: &Api<McpServer>,
    secrets: &Api<Secret>,
    configmaps: &Api<ConfigMap>,
    mcp: &McpServer,
    name: &str,
) -> Result<Action, ReconcileError> {
    let secret_name = format!("mcp-{name}-signing");
    let cm_name = format!("mcp-{name}-jwks");
    let _ = secrets
        .delete(&secret_name, &Default::default())
        .await
        .map(|_| ())
        .or_else(|e: kube::Error| -> Result<(), kube::Error> {
            if matches!(e, kube::Error::Api(ref ae) if ae.code == 404) {
                Ok(())
            } else {
                Err(e)
            }
        });
    let _ = configmaps
        .delete(&cm_name, &Default::default())
        .await
        .map(|_| ())
        .or_else(|e: kube::Error| -> Result<(), kube::Error> {
            if matches!(e, kube::Error::Api(ref ae) if ae.code == 404) {
                Ok(())
            } else {
                Err(e)
            }
        });

    let finalizers: Vec<String> = mcp
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({"apiVersion":"azureclaw.azure.com/v1alpha1","kind":"McpServer","metadata":{"finalizers": finalizers}});
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
        phase_reporter: Some(PhaseEventReporter::new(client, "McpServer")),
    });
    Controller::new(mcps, kube::runtime::watcher::Config::default())
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
                "spec.bundleRef is mutually exclusive with spec.url, spec.oauth, \
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
/// owns the selector — same pattern as InferencePolicy + ClawMemory.
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
        let conds = build_conditions(&[], Some(7), None);
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
        let conds = build_conditions(&[], Some(2), Some(("JwksFetchFailed", "boom")));
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "JwksFetchFailed");
        let degraded = conds.iter().find(|c| c.type_ == "Degraded").unwrap();
        assert_eq!(degraded.status, "True");
        assert_eq!(degraded.message, "boom");
    }

    #[test]
    fn build_conditions_preserves_transition_time_on_repeat_success() {
        let prior = build_conditions(&[], Some(1), None);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let next = build_conditions(&prior, Some(1), None);
        let p_ready = prior.iter().find(|c| c.type_ == "Ready").unwrap();
        let n_ready = next.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(p_ready.last_transition_time, n_ready.last_transition_time);
    }

    #[test]
    fn build_conditions_stamps_new_time_on_status_flip() {
        let prior = build_conditions(&[], Some(1), None);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let next = build_conditions(&prior, Some(1), Some(("JwksFetchFailed", "x")));
        let p_ready = prior.iter().find(|c| c.type_ == "Ready").unwrap();
        let n_ready = next.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_ne!(p_ready.last_transition_time, n_ready.last_transition_time);
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
