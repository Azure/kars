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
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, ObjectMeta, Patch, PatchParams, PostParams},
    runtime::controller::{Action, Controller},
};
use rand::RngCore;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::mcp_server::{LocalObjectRef, McpServer, McpServerStatus};
use crate::status::conditions::{self, reason, status as cond_status};
use crate::status::phase::{PHASE_DEGRADED, PHASE_PENDING, PHASE_READY};

mod auxiliary;
mod events;
mod jwks;
pub(crate) mod managed;
mod source;
use auxiliary::{ensure_aux_owner, finalize};
#[cfg(test)]
use jwks::{FetchError, FetchedJwks, parse_jwks_key_count};
use jwks::{HttpJwksFetcher, JwksFetcher};
use source::resolve_mcp_source;

/// Field manager for SSA patches emitted by this reconciler. A unique
/// suffix per reconciler is the §10.4 #1 craftsmanship requirement —
/// detects out-of-band tampering.
const FIELD_MANAGER: &str = crate::field_managers::MCP_SERVER;
const SOURCE_UID: &str = "kars.azure.com/mcp-source-uid";

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
    #[error("MCP configuration: {0}")]
    Configuration(String),
}

struct Ctx {
    client: Client,
    /// Override hook for tests — swap the JWKS fetcher with a mock.
    jwks_fetcher: Arc<dyn JwksFetcher>,
    probe_client: reqwest::Client,
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
        if !managed::cleanup(&ctx.client, &mcp)
            .await
            .map_err(ReconcileError::Configuration)?
        {
            return Ok(Action::requeue(Duration::from_secs(5)));
        }
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
        let mut finalizers = mcp.metadata.finalizers.clone().unwrap_or_default();
        finalizers.push(FINALIZER.into());
        let patch = json!({"metadata":{"uid":mcp.metadata.uid,"resourceVersion":mcp.metadata.resource_version,"finalizers":finalizers}});
        api.patch(&name, &PatchParams::default(), &Patch::Merge(patch))
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
    let managed_mode = mcp.spec.managed.is_some();
    let mut managed_outcome = None;
    let mut pending = None;
    let mut degraded = source_degraded;
    if !managed_mode
        && mcp
            .status
            .as_ref()
            .and_then(|status| status.workload_ref.as_ref())
            .is_some()
        && !managed::cleanup(&ctx.client, &mcp)
            .await
            .map_err(ReconcileError::Configuration)?
    {
        return Ok(Action::requeue(Duration::from_secs(5)));
    }
    if managed_mode && degraded.is_none() {
        match managed::reconcile(&ctx.client, &mcp, &ctx.probe_client).await {
            Ok(outcome) => {
                effective_spec.url = outcome.endpoint.clone();
                pending = outcome.pending.clone();
                managed_outcome = Some(outcome);
            }
            Err(error) => degraded = Some(("ManagedMcpUnqualified", error)),
        }
    }
    if managed_mode {
        effective_spec.url = managed_outcome
            .as_ref()
            .and_then(|outcome| outcome.endpoint.clone());
        effective_spec.oauth = None;
        effective_spec.production_mode = None;
        effective_spec.scopes = None;
        effective_spec.bearer_from_env = None;
    }

    // Managed private upstreams need no new signer or endpoint credentials.
    let secret_name = format!("mcp-{name}-signing");
    let signing_kid = if managed_mode {
        None
    } else {
        Some(ensure_signing_secret(&secrets, &secret_name, &mcp).await?)
    };

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
    let mut meta = McpServerMeta::from_spec(&effective_spec);
    if managed_mode {
        meta.source_uid = mcp.uid();
        meta.source_generation = mcp.metadata.generation;
        meta.binding_revision = managed_outcome
            .as_ref()
            .map(|outcome| outcome.revision.clone());
        if degraded.is_some() || pending.is_some() {
            meta.url.clear();
        }
    }
    let mut jwks_ref: Option<LocalObjectRef> = None;
    let production = effective_spec.production_mode.unwrap_or(false);

    if (degraded.is_none() && !production) || managed_mode {
        // Dev mode: write metadata + empty JWKS default so the
        // router can discover the upstream URL even without inbound
        // OAuth. The router's `/mcp` route is mounted in dev mode
        // (no OAuth) when no `productionMode=true` McpServer is bound.
        let empty_jwks = b"{\"keys\":[]}";
        ensure_jwks_configmap(&configmaps, &cm_name, &mcp, empty_jwks, &meta).await?;
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
                        ensure_jwks_configmap(&configmaps, &cm_name, &mcp, &fetched.raw, &meta)
                            .await?;
                        jwks_ref = Some(LocalObjectRef {
                            name: cm_name.clone(),
                        });
                        tracing::info!(
                            mcp = %name,
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
    let signing_ref = signing_kid
        .as_ref()
        .map(|_| LocalObjectRef { name: secret_name });
    let mut new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        degraded
            .as_ref()
            .map(|(reason, msg)| (*reason, msg.as_str())),
    );
    if let Some(message) = pending.as_ref() {
        new_conditions =
            managed::pending_conditions(&prior_conditions, observed_generation, message);
    }
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
    let mut status_patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "McpServer",
        "metadata": {"uid":mcp.metadata.uid,"resourceVersion":mcp.metadata.resource_version},
        "status": McpServerStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            last_probed_at: Some(rfc3339_now()),
            signing_key_ref: signing_ref,
            jwks_config_map_ref: jwks_ref,
            bundle_ref_digest: bundle_ref_digest.clone(),
            mode: Some(if managed_mode {"Managed"} else {"External"}.into()),
            endpoint: if degraded.is_none() && pending.is_none() {effective_spec.url.clone()} else {None},
            workload_ref: managed_outcome.as_ref().map(|outcome| outcome.workload_ref.clone())
                .or_else(|| managed_mode.then(|| mcp.status.as_ref()?.workload_ref.clone()).flatten()),
            managed_namespace_uid: managed_outcome.as_ref().map(|outcome| outcome.namespace_uid.clone())
                .or_else(|| managed_mode.then(|| mcp.status.as_ref()?.managed_namespace_uid.clone()).flatten()),
            workload_generation: managed_outcome.as_ref().and_then(|outcome| outcome.workload_generation),
            workload_image: managed_outcome.as_ref().and_then(|outcome| outcome.workload_image.clone()),
            discovered_tools: managed_outcome.as_ref().and_then(|outcome| outcome.tools.clone()),
            tool_schema_digest: managed_outcome.as_ref().and_then(|outcome| outcome.schema_digest.clone()),
        }
    });
    status_patch["status"]["bundleRefDigest"] = json!(bundle_ref_digest);
    api.patch_status(&name, &PatchParams::default(), &Patch::Merge(status_patch))
        .await?;

    tracing::info!(mcp = %name, phase = phase, "McpServerReconciled");

    if pending.is_some() {
        Ok(Action::requeue(Duration::from_secs(5)))
    } else if degraded.is_some() {
        Ok(Action::requeue(REQUEUE_FAIL))
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
        Ok(Action::requeue(if managed_mode {
            Duration::from_secs(30)
        } else {
            REQUEUE_OK
        }))
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
    owner: &McpServer,
) -> Result<String, ReconcileError> {
    if let Some(existing) = api.get_opt(secret_name).await? {
        ensure_aux_owner(&existing.metadata, owner, false)?;
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
    annotations.insert(
        SOURCE_UID.into(),
        owner
            .uid()
            .ok_or_else(|| ReconcileError::Configuration("McpServer UID is missing".into()))?,
    );

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(secret_name.into()),
            annotations: Some(annotations),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                ("kars.azure.com/mcp-server".into(), owner.name_any()),
            ])),
            ..Default::default()
        },
        type_: Some(SECRET_TYPE.into()),
        data: Some(data),
        ..Default::default()
    };
    api.create(
        &PostParams {
            field_manager: Some(FIELD_MANAGER.into()),
            ..Default::default()
        },
        &secret,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_revision: Option<String>,
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
            source_uid: None,
            source_generation: None,
            binding_revision: None,
        }
    }
}

async fn ensure_jwks_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    owner: &McpServer,
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
    let existing = api.get_opt(cm_name).await?;
    if let Some(existing) = existing.as_ref() {
        ensure_aux_owner(&existing.metadata, owner, owner.spec.managed.is_some())?;
    }
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.into()),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                ("kars.azure.com/mcp-server".into(), owner.name_any()),
            ])),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };
    if let Some(existing) = existing {
        if existing.data != cm.data {
            api.patch(cm_name, &PatchParams::default(), &Patch::Merge(json!({
                "metadata":{"uid":existing.metadata.uid,"resourceVersion":existing.metadata.resource_version},
                "data":cm.data,
            }))).await?;
        }
    } else {
        let mut cm = cm;
        cm.metadata.annotations = Some(BTreeMap::from([(
            SOURCE_UID.into(),
            owner
                .uid()
                .ok_or_else(|| ReconcileError::Configuration("McpServer UID is missing".into()))?,
        )]));
        api.create(
            &PostParams {
                field_manager: Some(FIELD_MANAGER.into()),
                ..Default::default()
            },
            &cm,
        )
        .await?;
    }
    Ok(())
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
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| anyhow::anyhow!("MCP probe client initialization failed"))?,
    });
    Controller::new(mcps, crate::watch_config::bounded())
        .watches(
            Api::<crate::crd::KarsSandbox>::all(client.clone()),
            crate::watch_config::bounded(),
            events::sandbox_references,
        )
        .watches(
            Api::<k8s_openapi::api::apps::v1::Deployment>::all(client),
            crate::watch_config::bounded(),
            events::workload_reference,
        )
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

#[cfg(test)]
mod tests;
