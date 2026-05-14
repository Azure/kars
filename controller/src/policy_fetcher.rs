// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// ci:loc-ok — Phase 2 multi-CRD reconciler / generated module; intentional. Tracked in plan.md §S15 follow-up.
//! Signed egress-allowlist artifact fetcher (S12.b status-only → S12.e authoritative).
//!
//! Pulls a content-addressed OCI artifact referenced by
//! [`crate::crd::OciArtifactRef`], verifies the cosign signature against a
//! cluster [`SignerPolicyConfig`] (Fulcio issuer + SAN patterns), and
//! re-validates the byte-stable canonical-form rules from
//! `docs/internal/policy-canonical-format.md`.
//!
//! ## Authoritative mode (S12.e)
//!
//! When `spec.networkPolicy.allowlistRef` is set on a `ClawSandbox`, the
//! verified canonical artifact is the **authoritative** source of egress
//! endpoints — inline `allowedEndpoints` is ignored (a non-empty inline
//! that differs from the artifact surfaces as `AllowlistDrift=True`).
//! When verification fails the controller fails closed: it preserves the
//! last-known-good (LKG) endpoint set from the prior successful reconcile
//! if available, and otherwise stamps `Degraded` without writing a
//! NetworkPolicy that opens egress beyond the always-allowed defaults.
//! See [`resolve_allowlist`].
//!
//! The S12.b `AZURECLAW_FEATURE_SIGNED_ALLOWLIST` env gate was lifted in
//! S12.e — the verification + authoritative resolution path is always-on
//! when `allowlistRef` is set; existing deployments without `allowlistRef`
//! observe no behavior change.
//!
//! ## SignerPolicy — S12.d (landed)
//!
//! The cluster `SignerPolicy` is now sourced from the watched
//! `azureclaw-signer-policy` ConfigMap in the controller namespace
//! (see [`crate::signer_policy`]). The env vars
//! `AZURECLAW_SIGNER_FULCIO_ISSUERS` / `AZURECLAW_SIGNER_SAN_PATTERNS`
//! remain as an emergency-override fallback that activates only when
//! the ConfigMap is absent. A *malformed* ConfigMap surfaces as
//! [`FetchError::SignerPolicyMalformed`] and does **not** silently
//! fall back — operators get a hard signal on every affected
//! `ClawSandbox`.
//!
//! ## ACR auth via Workload Identity
//!
//! [`acr_token_for_pull`] implements the four-step Azure Container
//! Registry token-exchange flow against AKS Workload Identity:
//!
//! 1. Read federated token from `AZURE_FEDERATED_TOKEN_FILE` (mounted by
//!    the AKS WI mutating webhook).
//! 2. Exchange the federated token at Entra
//!    `https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token` via
//!    `client_credentials` + `urn:ietf:params:oauth:client-assertion-type:jwt-bearer`.
//! 3. Exchange the AAD token at `https://{registry}/oauth2/exchange` for
//!    an ACR refresh token.
//! 4. Exchange the refresh token at `https://{registry}/oauth2/token`
//!    scoped to `repository:{repo}:pull` for an ACR access token.
//!
//! See:
//! - <https://learn.microsoft.com/en-us/azure/container-registry/container-registry-authentication-oauth2>
//! - <https://learn.microsoft.com/en-us/entra/workload-id/workload-identity-federation>

use crate::crd::OciArtifactRef;
use crate::policy_canonical::PolicyKind;
use crate::policy_canonical::egress::EgressKind;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Comma-separated list of Fulcio issuer URLs (e.g.
/// `https://token.actions.githubusercontent.com`). See [`SignerPolicyConfig`].
const SIGNER_FULCIO_ISSUERS_ENV: &str = "AZURECLAW_SIGNER_FULCIO_ISSUERS";
/// Comma-separated list of SAN glob patterns. See [`SignerPolicyConfig`].
const SIGNER_SAN_PATTERNS_ENV: &str = "AZURECLAW_SIGNER_SAN_PATTERNS";

/// Re-export the egress-allowlist v1 OCI media type from the canonical
/// module so existing call sites in this crate keep compiling without
/// touching the seam.
#[allow(unused_imports)] // re-export keeps existing call sites in this crate compiling
pub use crate::policy_canonical::egress::EGRESS_ALLOWLIST_V1_MEDIA_TYPE;

/// Cache TTL for verified artifacts (per plan §S12.b — "1h"). Public
/// so per-kind cache impls in [`crate::policy_canonical`] can apply
/// the same TTL.
pub const CACHE_TTL: Duration = Duration::from_secs(3600);

/// Errors surfaced by the fetcher. Each variant maps 1:1 to a Condition
/// `reason` value emitted on `ClawSandbox.status` — see
/// [`reason_for_error`].
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("SignerPolicy is not configured for this cluster")]
    SignerPolicyMissing,
    /// S12.d — `SignerPolicy` ConfigMap was present but failed to
    /// parse. Distinct from [`Self::SignerPolicyMissing`] so operators
    /// can disambiguate "I never installed one" from "the one I
    /// installed is broken — fix it before trusting any allowlist".
    #[error("SignerPolicy ConfigMap is malformed: {0}")]
    SignerPolicyMalformed(String),
    #[error("invalid artifact reference: {0}")]
    InvalidRef(String),
    #[error("OCI registry auth failed: {0}")]
    Unauthorized(String),
    #[error("artifact not found at {0}")]
    NotFound(String),
    #[error("digest mismatch: expected {expected}, got {actual}")]
    #[allow(dead_code)]
    DigestMismatch { expected: String, actual: String },
    #[error("cosign signature verification failed: {0}")]
    SignatureVerifyFailed(String),
    #[error("signer identity does not match cluster SignerPolicy: {0}")]
    IdentityMismatch(String),
    #[error("canonical-form violation in artifact bytes: {0}")]
    CanonicalFormViolation(String),
    #[error("transient error: {0}")]
    Transient(String),
}

/// PascalCase Condition `reason` for a given error. `Transient` is
/// represented as `None` so the reconciler can preserve last-known-good
/// status (see plan §S12.b dispatch table).
pub fn reason_for_error(err: &FetchError) -> Option<&'static str> {
    match err {
        FetchError::SignerPolicyMissing => Some("SignerPolicyMissing"),
        FetchError::SignerPolicyMalformed(_) => Some("SignerPolicyMalformed"),
        FetchError::InvalidRef(_) => Some("InvalidRef"),
        FetchError::Unauthorized(_) => Some("Unauthorized"),
        FetchError::NotFound(_) => Some("NotFound"),
        FetchError::DigestMismatch { .. } => Some("DigestMismatch"),
        FetchError::SignatureVerifyFailed(_) => Some("SignatureVerifyFailed"),
        FetchError::IdentityMismatch(_) => Some("IdentityMismatch"),
        FetchError::CanonicalFormViolation(_) => Some("CanonicalFormViolation"),
        FetchError::Transient(_) => None,
    }
}

/// Re-exported from [`crate::policy_canonical::egress`] so existing
/// call sites in this crate (reconciler, status, signer_policy, tests)
/// continue to refer to `policy_fetcher::VerifiedAllowlist` /
/// `CanonicalEndpoint` unchanged after the 1c.1 trait extraction. New
/// per-kind work should reference the canonical module directly.
pub use crate::policy_canonical::egress::{CanonicalEndpoint, VerifiedAllowlist};

/// Cluster-wide SignerPolicy. In S12.b this is read from env at call
/// time; S12.d replaces this with a watched ConfigMap. The wire shape of
/// this struct will stay stable across that transition — only the
/// constructor (`from_env` → `from_configmap`) is expected to change.
#[derive(Debug, Clone, Default)]
pub struct SignerPolicyConfig {
    /// Allowed Fulcio issuer URLs (e.g.
    /// `https://token.actions.githubusercontent.com`). Empty → reject all.
    pub fulcio_issuers: Vec<String>,
    /// Allowed SAN patterns (glob). Each pattern is matched against the
    /// signer cert's `Subject.email` (legacy mode) or `Subject.uri`
    /// (keyless OIDC). Glob wildcards: `*` matches any run of non-`/`
    /// chars; `?` matches one. Empty → reject all.
    pub san_patterns: Vec<String>,
    /// Registered Ed25519 keys for the grant lane (Slice 5e+). Mirrored
    /// from the SignerPolicy ConfigMap and surfaced through the
    /// verifier so future grant-lane consumers can read it without a
    /// second env-var path. **Not consulted** by the data-plane policy
    /// verify path (egress / tools / inference / memory / mcp-server);
    /// kept opaque here. Empty when the env constructor is used.
    #[allow(dead_code)] // Slice 5e+ grant-lane verifier consumer.
    pub ed25519_keys: Vec<crate::signer_policy::Ed25519Key>,
}

impl SignerPolicyConfig {
    /// Read from env. Both vars are comma-separated; empty / unset →
    /// empty vector. Whitespace is trimmed; empty entries are dropped.
    pub fn from_env() -> Self {
        Self {
            fulcio_issuers: split_csv_env(SIGNER_FULCIO_ISSUERS_ENV),
            san_patterns: split_csv_env(SIGNER_SAN_PATTERNS_ENV),
            ed25519_keys: Vec::new(),
        }
    }

    /// A SignerPolicy is "configured" when *both* an issuer allow-list
    /// and a SAN allow-list are present. Either one alone is unsafe (any
    /// SAN with no issuer pinning permits arbitrary trust roots; any
    /// issuer with no SAN pinning permits arbitrary identities under
    /// that issuer), so we require both. `ed25519_keys` does not
    /// participate — the grant lane has its own readiness gate (Slice
    /// 5e+).
    pub fn is_configured(&self) -> bool {
        !self.fulcio_issuers.is_empty() && !self.san_patterns.is_empty()
    }
}

impl From<crate::signer_policy::SignerPolicy> for SignerPolicyConfig {
    fn from(s: crate::signer_policy::SignerPolicy) -> Self {
        Self {
            fulcio_issuers: s.fulcio_issuers,
            san_patterns: s.san_patterns,
            ed25519_keys: s.ed25519_keys,
        }
    }
}

fn split_csv_env(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Slice 5c.2: `REQUIRE_SIGNED_ALLOWLIST=true` flips the inline-only
/// (no `allowlistRef`) path from *allow with `Unsigned` warning* to
/// fail-closed. Default `false`. Sourced via helm value
/// `egress.requireSigned` → controller deployment env.
///
/// Accepts `true|1|yes|on` (case-insensitive) as truthy; anything else
/// is falsy. Read on every reconcile so operators can toggle without a
/// controller restart by editing the Deployment env.
pub(crate) fn require_signed_allowlist() -> bool {
    matches!(
        std::env::var("REQUIRE_SIGNED_ALLOWLIST")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "true" | "1" | "yes" | "on"
    )
}

// ─────────────────────────── cache ───────────────────────────
//
// 1c.1: the per-kind verified-artifact cache moved into
// `policy_canonical::<kind>` so each `PolicyKind` impl owns a
// process-wide `OnceLock<Mutex<HashMap<…>>>` parameterized over its
// own `Output` type. `cache_key` remains here because the registry +
// repository + digest tuple is kind-agnostic (kind isolation is
// already enforced by per-kind storage).

fn cache_key(r: &OciArtifactRef) -> String {
    format!("{}/{}@{}", r.registry, r.repository, r.digest)
}

/// Lazily initialize and cache a process-wide Sigstore Public Good trust
/// root (Fulcio CA chain + Rekor public keys + CTfe keys). The first
/// call performs a TUF refresh against `tuf-repo-cdn.sigstore.dev` (with
/// the crate's embedded `root.json` as the anchor); subsequent calls
/// reuse the in-memory result. Wrapped in `Arc` so we can hand out
/// borrows without holding a long-lived lock across `.await` points.
async fn trust_root_cache()
-> Result<std::sync::Arc<sigstore::trust::sigstore::SigstoreTrustRoot>, FetchError> {
    use sigstore::trust::sigstore::SigstoreTrustRoot;
    use std::sync::Arc;
    use tokio::sync::OnceCell;
    static ROOT: OnceCell<Arc<SigstoreTrustRoot>> = OnceCell::const_new();
    let root = ROOT
        .get_or_try_init(|| async {
            // `cache_dir = None` keeps the trusted_root in memory only.
            // Network call is to `tuf-repo-cdn.sigstore.dev` (Sigstore PGI).
            SigstoreTrustRoot::new(None)
                .await
                .map(Arc::new)
                .map_err(|e| FetchError::Transient(format!("sigstore trust root: {e}")))
        })
        .await?;
    Ok(root.clone())
}

// ─────────────────────── last-known-good cache (S12.e) ───────────────────────
//
// Per-(namespace, sandbox-name) cache of the most recently verified
// allowlist endpoints. Distinct from the digest-keyed `cache` above:
// the digest cache memoizes "this artifact's bytes are already
// verified, skip the cosign roundtrip"; the LKG cache memoizes "the
// last time we successfully resolved endpoints for THIS sandbox, this
// is what we got" so a subsequent verify failure can fall back to
// known-safe state instead of broadening egress.
//
// In-memory only by design — controller restart deliberately drops
// the LKG so the first post-restart reconcile of a verify-failing
// sandbox fails closed (no broad egress) instead of carrying a stale
// allowlist across operator-visible controller events. See audit doc
// `docs/internal/security-audits/2026-04-30-phase2-s12-e-authoritative.md`.

/// Per-sandbox state remembered across reconciles to support
/// fail-closed degradation and drift-cleared debouncing.
#[derive(Debug, Clone, Default)]
struct LkgEntry {
    /// Last successfully verified endpoint set, in canonical form.
    /// `Some(empty)` is meaningful (deny-all artifact) and distinct
    /// from `None` (never seen a successful verify).
    endpoints: Option<Vec<crate::crd::EndpointConfig>>,
    /// Number of consecutive reconciles where inline was empty *after*
    /// drift was previously observed. Used to debounce
    /// `AllowlistDrift=False/InlineCleared` for ≤2 reconciles before
    /// dropping the condition entirely.
    drift_clear_counter: u8,
    /// Whether the most recent reconcile observed drift.
    drift_active: bool,
}

fn lkg_cache() -> &'static Mutex<HashMap<(String, String), LkgEntry>> {
    static LKG: OnceLock<Mutex<HashMap<(String, String), LkgEntry>>> = OnceLock::new();
    LKG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lkg_get(ns: &str, name: &str) -> LkgEntry {
    lkg_cache()
        .lock()
        .ok()
        .and_then(|g| g.get(&(ns.to_string(), name.to_string())).cloned())
        .unwrap_or_default()
}

fn lkg_put(ns: &str, name: &str, entry: LkgEntry) {
    if let Ok(mut g) = lkg_cache().lock() {
        g.insert((ns.to_string(), name.to_string()), entry);
    }
}

/// Drop the LKG entry for a given sandbox (test-only — used to
/// simulate controller restart in unit tests).
#[cfg(test)]
#[allow(dead_code)]
pub fn lkg_clear() {
    if let Ok(mut g) = lkg_cache().lock() {
        g.clear();
    }
}

/// Convert a verified canonical endpoint list into the
/// [`crate::crd::EndpointConfig`] shape consumed by the reconciler's
/// NetworkPolicy builder. v1 canonical artifacts only carry
/// `(host, port)`, so `methods` / `paths` are left `None` — the
/// router (not the NetworkPolicy) is the enforcement point for those.
fn canonical_to_endpoint_config(
    endpoints: &[CanonicalEndpoint],
) -> Vec<crate::crd::EndpointConfig> {
    endpoints
        .iter()
        .map(|e| crate::crd::EndpointConfig {
            host: e.host.clone(),
            port: Some(e.port),
        })
        .collect()
}

/// Compare two endpoint lists for set-equality after normalizing
/// (host lowercase, port defaults to 443 when absent). Used by drift
/// detection — operators may write inline endpoints in any order /
/// case, so a strict `Vec::eq` would over-flag.
fn endpoint_lists_equivalent(
    a: &[crate::crd::EndpointConfig],
    b: &[crate::crd::EndpointConfig],
) -> bool {
    let norm = |list: &[crate::crd::EndpointConfig]| -> Vec<(String, u16)> {
        let mut v: Vec<(String, u16)> = list
            .iter()
            .map(|e| (e.host.to_ascii_lowercase(), e.port.unwrap_or(443)))
            .collect();
        v.sort();
        v.dedup();
        v
    };
    norm(a) == norm(b)
}

/// Structured drift summary: what's in `inline` but not the verified
/// artifact (`added`), and what's in the artifact but not inline
/// (`removed`). Computed only when drift is detected so we can attach
/// a machine-readable payload to the `AllowlistDrift=True` condition
/// message. Entries are formatted `host:port` (port normalized to 443
/// when absent) and sorted lexicographically.
///
/// **Wire contract** — the headlamp plugin parses this from the
/// condition message as JSON. Keep field names stable. Adding fields
/// is OK; renames are not.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DriftSummary {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

fn normalize_endpoints_for_summary(
    list: &[crate::crd::EndpointConfig],
) -> std::collections::BTreeSet<(String, u16)> {
    list.iter()
        .map(|e| (e.host.to_ascii_lowercase(), e.port.unwrap_or(443)))
        .collect()
}

/// Compute a structured drift summary between an inline endpoint
/// list and the verified-artifact-derived list. `added` are entries
/// only present inline (operator intent diverging from the signed
/// authority); `removed` are entries only in the artifact. Returns
/// empty lists when the two sets are equivalent.
#[must_use]
pub fn compute_drift_summary(
    inline: &[crate::crd::EndpointConfig],
    derived: &[crate::crd::EndpointConfig],
) -> DriftSummary {
    let inline_set = normalize_endpoints_for_summary(inline);
    let derived_set = normalize_endpoints_for_summary(derived);
    let fmt = |(h, p): &(String, u16)| format!("{h}:{p}");
    let mut added: Vec<String> = inline_set.difference(&derived_set).map(fmt).collect();
    let mut removed: Vec<String> = derived_set.difference(&inline_set).map(fmt).collect();
    added.sort();
    removed.sort();
    DriftSummary { added, removed }
}

// ─────────────────────────── public entry ───────────────────────────

/// Pull, verify, and parse a signed egress-allowlist artifact.
///
/// Returns `Err(FetchError::SignerPolicyMissing)` when no SignerPolicy
/// is configured (cluster ConfigMap absent and env-fallback empty).
///
/// On success, the result is cached by `<registry>/<repository>@<digest>`
/// for [`CACHE_TTL`].
///
/// 1c.1: this is now a thin wrapper around the kind-generic
/// [`fetch_and_verify_generic`]. New per-kind reconcilers should call
/// the generic form directly with their `PolicyKind` discriminator
/// (see `docs/internal/crd-well-oiled-machine/slice-1c-real-signing-generalization.md`).
pub async fn fetch_and_verify(
    artifact_ref: &OciArtifactRef,
    signer_policy: &SignerPolicyConfig,
) -> Result<VerifiedAllowlist, FetchError> {
    fetch_and_verify_generic::<EgressKind>(artifact_ref, signer_policy).await
}

/// Kind-agnostic core of the signing pipeline: ref-shape validation,
/// SignerPolicy presence check, per-kind cache lookup, cosign + Fulcio
/// verification, OCI pull, per-kind canonical-form re-validation,
/// per-kind cache write.
///
/// The `K` discriminator chooses:
/// - which OCI `artifactType` to accept (via [`PolicyKind::MEDIA_TYPE`])
/// - which canonical parser runs over the verified bytes (via
///   [`PolicyKind::parse`])
/// - which output struct is returned (via [`PolicyKind::Output`])
/// - which process-wide cache slot is used (via
///   [`PolicyKind::cache_get`] / [`PolicyKind::cache_put`])
///
/// The cosign trust root, ACR auth, signer-identity verification, and
/// [`FetchError`] taxonomy are inherited unchanged.
pub async fn fetch_and_verify_generic<K: PolicyKind>(
    artifact_ref: &OciArtifactRef,
    signer_policy: &SignerPolicyConfig,
) -> Result<K::Output, FetchError> {
    validate_ref_shape::<K>(artifact_ref)?;

    if !signer_policy.is_configured() {
        // Without identity pinning we treat "valid sig" alone as
        // insufficient authority. The reconciler surfaces this as
        // `AllowlistVerified=False/SignerPolicyMissing`.
        return Err(FetchError::SignerPolicyMissing);
    }

    let key = cache_key(artifact_ref);
    if let Some(hit) = K::cache_get(&key, Instant::now()) {
        tracing::debug!(
            kind = K::KIND,
            digest = %artifact_ref.digest,
            "policy_fetcher cache hit"
        );
        return Ok(hit);
    }

    let verified = verify_via_sigstore::<K>(artifact_ref, signer_policy).await?;

    K::cache_put(key, verified.clone());
    Ok(verified)
}

fn validate_ref_shape<K: PolicyKind>(r: &OciArtifactRef) -> Result<(), FetchError> {
    if r.registry.is_empty() || r.registry.contains(' ') || r.registry.contains('/') {
        return Err(FetchError::InvalidRef(format!(
            "registry `{}` invalid (must be host[:port])",
            r.registry
        )));
    }
    if r.repository.is_empty() || r.repository.contains(' ') {
        return Err(FetchError::InvalidRef(format!(
            "repository `{}` invalid",
            r.repository
        )));
    }
    if !r.digest.starts_with("sha256:") || r.digest.len() != "sha256:".len() + 64 {
        return Err(FetchError::InvalidRef(format!(
            "digest `{}` must be sha256:<64 hex chars>",
            r.digest
        )));
    }
    if !r.digest["sha256:".len()..]
        .chars()
        .all(|c| c.is_ascii_hexdigit() && (!c.is_alphabetic() || c.is_ascii_lowercase()))
    {
        return Err(FetchError::InvalidRef(format!(
            "digest `{}` must use lowercase hex",
            r.digest
        )));
    }
    if r.artifact_type != K::MEDIA_TYPE {
        return Err(FetchError::InvalidRef(format!(
            "artifactType `{}` is not the expected `{}`",
            r.artifact_type,
            K::MEDIA_TYPE
        )));
    }
    Ok(())
}

// ─────────────────────────── verify path ───────────────────────────

async fn verify_via_sigstore<K: PolicyKind>(
    artifact_ref: &OciArtifactRef,
    signer_policy: &SignerPolicyConfig,
) -> Result<K::Output, FetchError> {
    use sigstore::cosign::verification_constraint::cert_subject_email_verifier::StringVerifier;
    use sigstore::cosign::verification_constraint::{
        CertSubjectEmailVerifier, CertSubjectUrlVerifier, VerificationConstraintVec,
    };
    use sigstore::cosign::{ClientBuilder, CosignCapabilities, verify_constraints};
    use sigstore::registry::OciReference;

    // Lazily initialize a Sigstore Public Good trust root (Fulcio CA +
    // Rekor + CTfe keys) from the embedded TUF root and remote metadata.
    // Without this, sigstore-rs builds clients with `Fulcio integration
    // disabled` and `signature_layer.certificate_signature` is `None`,
    // causing every `CertSubject*Verifier` constraint to fail closed.
    // Cached process-wide; refreshed on controller restart (TUF metadata
    // is checked-in via the crate's embedded snapshot, with TUF refresh
    // performed once on first reconcile).
    let trust_root = trust_root_cache().await?;

    let mut client = ClientBuilder::default()
        .with_trust_repository(trust_root.as_ref())
        .map_err(|e| FetchError::Transient(format!("cosign client (trust repo): {e}")))?
        .build()
        .map_err(|e| FetchError::Transient(format!("cosign client: {e}")))?;

    let auth = pick_registry_auth(&artifact_ref.registry, &artifact_ref.repository).await?;

    let image = OciReference::with_digest(
        artifact_ref.registry.clone(),
        artifact_ref.repository.clone(),
        artifact_ref.digest.clone(),
    );

    // Step A: discover the cosign signature reference for this digest.
    let (cosign_image, source_digest) = client
        .triangulate(&image, &auth)
        .await
        .map_err(map_oci_error)?;

    // Step B: pull + locally-verify each signature layer (sig over
    // payload, optional cert chain, optional Rekor bundle). The returned
    // layers may have `certificate_signature: None` if the trust roots
    // weren't injected — that's fine, the constraint check below will
    // fail closed in that case.
    let layers = client
        .trusted_signature_layers(&auth, &source_digest, &cosign_image)
        .await
        .map_err(map_oci_error)?;

    // Build + apply constraints in a sub-scope so the non-`Send`
    // `Vec<Box<dyn VerificationConstraint>>` is dropped before the next
    // `.await` (kube-rs requires the reconcile future to be `Send`).
    {
        use sigstore::cosign::signature_layers::SignatureLayer;
        use sigstore::cosign::verification_constraint::VerificationConstraint;
        use sigstore::errors::Result as SigstoreResult;

        // OR adapter: passes if ANY inner constraint passes. Sigstore-rs'
        // `verify_constraints` ANDs across constraints by design; we need
        // OR semantics across the cartesian (issuer × SAN) pairs because
        // an admin lists multiple acceptable signer combos and a single
        // cert can only match one of them.
        #[derive(Debug)]
        struct AnyOf(Vec<Box<dyn VerificationConstraint>>);
        impl VerificationConstraint for AnyOf {
            fn verify(&self, sl: &SignatureLayer) -> SigstoreResult<bool> {
                for c in &self.0 {
                    if c.verify(sl)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }

        let mut inner: Vec<Box<dyn VerificationConstraint>> = Vec::new();
        for issuer in &signer_policy.fulcio_issuers {
            for san in &signer_policy.san_patterns {
                if san.starts_with("https://") || san.starts_with("http://") {
                    inner.push(Box::new(CertSubjectUrlVerifier {
                        url: san.clone(),
                        issuer: issuer.clone(),
                    }));
                } else {
                    let email_v = if let Some(re) = san.strip_prefix("re:") {
                        match regex::Regex::new(re) {
                            Ok(r) => StringVerifier::Regex(r),
                            Err(e) => {
                                return Err(FetchError::IdentityMismatch(format!(
                                    "invalid regex SAN pattern `{san}`: {e}"
                                )));
                            }
                        }
                    } else {
                        StringVerifier::ExactMatch(san.clone())
                    };
                    inner.push(Box::new(CertSubjectEmailVerifier {
                        email: email_v,
                        issuer: Some(StringVerifier::ExactMatch(issuer.clone())),
                    }));
                }
            }
        }
        let any_of: VerificationConstraintVec = vec![Box::new(AnyOf(inner))];
        verify_constraints(&layers, any_of.iter())
            .map_err(|e| FetchError::IdentityMismatch(format!("{e:?}")))?;
    }

    // Step D: pull artifact bytes (digest-pinned).
    //
    // The pull is content-addressed by manifest digest (`@sha256:...`), so the
    // registry cannot substitute a different manifest. `oci_client::pull`
    // verifies each layer descriptor against the manifest as part of the pull,
    // which authenticates the bytes we get back. We deliberately do NOT
    // recompute `sha256(layer_bytes)` here and compare it to `artifact_ref.digest`:
    // those are different things (layer digest vs. manifest digest) and would
    // never match for any non-trivial artifact.
    let bytes = pull_artifact_bytes(artifact_ref, &auth, K::MEDIA_TYPE).await?;

    let mut parsed = K::parse(&bytes)?;
    K::finalize(&mut parsed, artifact_ref.digest.clone(), SystemTime::now());
    Ok(parsed)
}

fn map_oci_error(e: sigstore::errors::SigstoreError) -> FetchError {
    let msg = format!("{e}");
    let ml = msg.to_ascii_lowercase();
    if ml.contains("not found") || ml.contains("404") {
        FetchError::NotFound(msg)
    } else if ml.contains("unauthorized") || ml.contains("401") || ml.contains("403") {
        FetchError::Unauthorized(msg)
    } else if ml.contains("signature")
        || ml.contains("verif")
        || ml.contains("rekor")
        || ml.contains("fulcio")
    {
        FetchError::SignatureVerifyFailed(msg)
    } else {
        FetchError::Transient(msg)
    }
}

async fn pull_artifact_bytes(
    artifact_ref: &OciArtifactRef,
    auth: &sigstore::registry::Auth,
    media_type: &'static str,
) -> Result<Vec<u8>, FetchError> {
    use oci_client::Reference;
    use oci_client::client::{Client, ClientConfig};
    use oci_client::secrets::RegistryAuth;

    let client = Client::new(ClientConfig::default());
    let reference: Reference = format!(
        "{}/{}@{}",
        artifact_ref.registry, artifact_ref.repository, artifact_ref.digest
    )
    .parse()
    .map_err(|e: oci_client::ParseError| {
        FetchError::InvalidRef(format!("oci reference parse: {e}"))
    })?;
    let oci_auth: RegistryAuth = match auth {
        sigstore::registry::Auth::Anonymous => RegistryAuth::Anonymous,
        sigstore::registry::Auth::Basic(u, p) => RegistryAuth::Basic(u.clone(), p.clone()),
        sigstore::registry::Auth::Bearer(t) => RegistryAuth::Bearer(t.clone()),
    };
    let data = client
        .pull(&reference, &oci_auth, vec![media_type])
        .await
        .map_err(map_oci_distribution_error)?;
    let mut out = Vec::new();
    for layer in data.layers {
        out.extend_from_slice(&layer.data);
    }
    Ok(out)
}

fn map_oci_distribution_error(e: oci_client::errors::OciDistributionError) -> FetchError {
    use oci_client::errors::OciDistributionError;
    match &e {
        OciDistributionError::AuthenticationFailure(_) => FetchError::Unauthorized(format!("{e}")),
        OciDistributionError::ImageManifestNotFoundError(_) => FetchError::NotFound(format!("{e}")),
        _ => FetchError::Transient(format!("{e}")),
    }
}

#[cfg(test)]
#[allow(dead_code)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

async fn pick_registry_auth(
    registry: &str,
    repository: &str,
) -> Result<sigstore::registry::Auth, FetchError> {
    // ACR registries advertise the WI exchange flow; for any other
    // registry (including localhost test registries) we fall through to
    // anonymous. Operators who need basic-auth or static bearer tokens
    // should layer Workload Identity on top — there is intentionally no
    // path for static credentials in S12.b (no plumbing for static
    // secrets per `docs/security-model.md`).
    if is_acr_host(registry) && std::env::var("AZURE_FEDERATED_TOKEN_FILE").is_ok() {
        match acr_token_for_pull(registry, repository).await {
            Ok(tok) => return Ok(sigstore::registry::Auth::Bearer(tok)),
            Err(FetchError::Unauthorized(msg)) => {
                tracing::warn!(%registry, error = %msg, "ACR WI exchange failed; falling back to anonymous");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(sigstore::registry::Auth::Anonymous)
}

fn is_acr_host(registry: &str) -> bool {
    let r = registry.to_ascii_lowercase();
    r.ends_with(".azurecr.io") || r.ends_with(".azurecr.cn") || r.ends_with(".azurecr.us")
}

/// ACR pull-token exchange via AKS Workload Identity.
///
/// Steps 1-4 in the module-level docs. Returns an ACR access token
/// suitable for `Authorization: Bearer <tok>` against
/// `https://{registry}/v2/{repo}/...`.
///
/// References:
/// - <https://learn.microsoft.com/en-us/azure/container-registry/container-registry-authentication-oauth2>
/// - <https://learn.microsoft.com/en-us/entra/workload-id/workload-identity-federation>
///
/// Required environment (mounted by the AKS WI mutating webhook):
/// - `AZURE_FEDERATED_TOKEN_FILE` — path to a projected SA token (JWT).
/// - `AZURE_TENANT_ID` — directory tenant.
/// - `AZURE_CLIENT_ID` — workload identity client (app) ID.
pub async fn acr_token_for_pull(registry: &str, repository: &str) -> Result<String, FetchError> {
    let token_path = std::env::var("AZURE_FEDERATED_TOKEN_FILE")
        .map_err(|_| FetchError::Unauthorized("AZURE_FEDERATED_TOKEN_FILE unset".into()))?;
    let tenant = std::env::var("AZURE_TENANT_ID")
        .map_err(|_| FetchError::Unauthorized("AZURE_TENANT_ID unset".into()))?;
    let client_id = std::env::var("AZURE_CLIENT_ID")
        .map_err(|_| FetchError::Unauthorized("AZURE_CLIENT_ID unset".into()))?;
    let federated = tokio::fs::read_to_string(&token_path)
        .await
        .map_err(|e| FetchError::Unauthorized(format!("read federated token: {e}")))?;
    let federated = federated.trim();

    let http = reqwest::Client::builder()
        .build()
        .map_err(|e| FetchError::Transient(format!("http client: {e}")))?;

    // Step 2: exchange federated JWT for AAD access token.
    let aad_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");
    let aad_resp = http
        .post(&aad_url)
        .form(&[
            ("client_id", client_id.as_str()),
            ("grant_type", "client_credentials"),
            (
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            ),
            ("client_assertion", federated),
            // ACR documents an ACR-specific resource scope for token
            // exchange; "https://management.azure.com/.default" also
            // works for the exchange endpoint and is the documented
            // value in the WI examples.
            ("scope", "https://management.azure.com/.default"),
        ])
        .send()
        .await
        .map_err(|e| FetchError::Transient(format!("aad post: {e}")))?;
    if !aad_resp.status().is_success() {
        let status = aad_resp.status();
        let body = aad_resp.text().await.unwrap_or_default();
        return Err(FetchError::Unauthorized(format!(
            "aad token exchange: {status}: {body}"
        )));
    }
    let aad_json: serde_json::Value = aad_resp
        .json()
        .await
        .map_err(|e| FetchError::Transient(format!("aad json: {e}")))?;
    let aad_token = aad_json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FetchError::Unauthorized("aad response missing access_token".into()))?
        .to_string();

    // Step 3: exchange AAD token for ACR refresh token.
    let exchange_url = format!("https://{registry}/oauth2/exchange");
    let acr_refresh_resp = http
        .post(&exchange_url)
        .form(&[
            ("grant_type", "access_token"),
            ("service", registry),
            ("access_token", aad_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| FetchError::Transient(format!("acr exchange post: {e}")))?;
    if !acr_refresh_resp.status().is_success() {
        let status = acr_refresh_resp.status();
        let body = acr_refresh_resp.text().await.unwrap_or_default();
        return Err(FetchError::Unauthorized(format!(
            "acr refresh exchange: {status}: {body}"
        )));
    }
    let acr_refresh_json: serde_json::Value = acr_refresh_resp
        .json()
        .await
        .map_err(|e| FetchError::Transient(format!("acr exchange json: {e}")))?;
    let refresh_token = acr_refresh_json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FetchError::Unauthorized("acr exchange missing refresh_token".into()))?
        .to_string();

    // Step 4: trade refresh token for repository-scoped access token.
    let token_url = format!("https://{registry}/oauth2/token");
    let scope = format!("repository:{repository}:pull");
    let acr_access_resp = http
        .post(&token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("service", registry),
            ("scope", scope.as_str()),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| FetchError::Transient(format!("acr token post: {e}")))?;
    if !acr_access_resp.status().is_success() {
        let status = acr_access_resp.status();
        let body = acr_access_resp.text().await.unwrap_or_default();
        return Err(FetchError::Unauthorized(format!(
            "acr access token: {status}: {body}"
        )));
    }
    let acr_access_json: serde_json::Value = acr_access_resp
        .json()
        .await
        .map_err(|e| FetchError::Transient(format!("acr token json: {e}")))?;
    let access_token = acr_access_json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FetchError::Unauthorized("acr token missing access_token".into()))?
        .to_string();
    Ok(access_token)
}

// ─────────────────────── reconciler integration (S12.e) ───────────────────────
//
// `resolve_allowlist_with_handle` is the single entry point the
// reconciler calls per reconcile of a `ClawSandbox`. It encapsulates the
// authoritative-mode decision tree (artifact wins; LKG fallback;
// fail-closed when no LKG; legacy inline path when no `allowlistRef`)
// and emits the three S12.e conditions:
//
// - `AllowlistVerified`  — only emitted when `allowlistRef` is set.
// - `AllowlistAuthoritative` — emitted whenever any user-visible
//   networkPolicy concept is on the CR (ref or inline endpoints).
// - `AllowlistDrift` — emitted when ref + inline both set and they
//   differ; or briefly (≤2 reconciles) on the cleared transition.
//
// The reconciler consumes `endpoints` to build the NetworkPolicy and
// `conditions` to merge into the status patch.

/// Outcome of resolving the egress allowlist for a single sandbox.
/// See [`resolve_allowlist_with_handle`].
#[derive(Debug, Clone, Default)]
pub struct AllowlistResolution {
    /// Endpoints the controller should program into the user-defined
    /// portion of the sandbox NetworkPolicy. `None` means "do not add
    /// any user endpoints" — the always-allowed defaults (DNS, IMDS,
    /// HTTPS for the inference-router, mesh) still apply, but the
    /// sandbox's own additional egress is denied. Distinct from
    /// `Some(empty)` which is a deliberate deny-all-user-egress signal
    /// from a verified canonical artifact (also treated as no extra
    /// rules, but with `AllowlistAuthoritative=True/Verified`).
    pub endpoints: Option<Vec<crate::crd::EndpointConfig>>,
    /// Conditions to surface. Caller upserts these into
    /// `status.conditions` via the existing `_with_extras` helpers.
    pub conditions: Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>,
    /// True when `allowlistRef` is set, verify failed, and there is
    /// no LKG to fall back to. Caller stamps `Degraded` in the status
    /// patch (in addition to merging `conditions`) and requeues.
    pub fail_closed_no_lkg: bool,
}

/// Resolve the effective egress allowlist for a `ClawSandbox`.
///
/// Decision tree (see slice S12.e):
///
/// 1. **No `allowlistRef`** (legacy / inline path) → endpoints =
///    inline; `AllowlistAuthoritative=False/Inline`; no
///    `AllowlistVerified`; no `AllowlistDrift`. Zero behavior change
///    versus pre-S12.e.
/// 2. **`allowlistRef` set, verify ok** → endpoints = artifact;
///    `AllowlistVerified=True/Verified`;
///    `AllowlistAuthoritative=True/Verified`. If inline non-empty +
///    differs → `AllowlistDrift=True/InlineDiffersFromArtifact`. The
///    LKG cache is updated with the just-verified set.
/// 3. **`allowlistRef` set, verify fails, LKG present** → endpoints =
///    LKG; `AllowlistVerified=False/<reason>`;
///    `AllowlistAuthoritative=False/StaleLKG`. Caller should also
///    stamp Degraded (the verify-fail reason is surfaced via
///    `AllowlistVerified`'s `reason`).
/// 4. **`allowlistRef` set, verify fails, no LKG** → endpoints =
///    None; `AllowlistVerified=False/<reason>`;
///    `AllowlistAuthoritative=False/FailedClosed`;
///    `fail_closed_no_lkg = true`.
///
/// `Transient` fetch errors preserve the prior `AllowlistVerified`
/// condition and re-use the prior LKG (if any) — a network blip must
/// not collapse a working sandbox.
pub async fn resolve_allowlist_with_handle(
    sandbox: &crate::crd::ClawSandbox,
    signer_policy_handle: &crate::signer_policy::SharedSignerPolicy,
) -> AllowlistResolution {
    use crate::signer_policy::SignerPolicyState;
    use crate::status::conditions::{
        TYPE_ALLOWLIST_AUTHORITATIVE, TYPE_ALLOWLIST_DRIFT, TYPE_ALLOWLIST_VERIFIED, new_condition,
        preserve_transition_time, reason as cond_reason, status as cond_status,
    };

    let np = match sandbox.spec.network_policy.as_ref() {
        Some(np) => np,
        None => {
            // No networkPolicy at all → no user endpoints, no
            // conditions to emit. The reconciler builds the standard
            // baseline NetworkPolicy and that's it.
            return AllowlistResolution::default();
        }
    };

    let inline = np.allowed_endpoints.clone().unwrap_or_default();
    let inline_present = !inline.is_empty();

    let prior_conditions: &[_] = sandbox
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);
    let prior_verified = prior_conditions
        .iter()
        .find(|c| c.type_ == TYPE_ALLOWLIST_VERIFIED);
    let prior_authoritative = prior_conditions
        .iter()
        .find(|c| c.type_ == TYPE_ALLOWLIST_AUTHORITATIVE);
    let prior_drift = prior_conditions
        .iter()
        .find(|c| c.type_ == TYPE_ALLOWLIST_DRIFT);
    let generation = sandbox.metadata.generation;

    // ─── Branch 1: legacy inline path ───
    let Some(artifact_ref) = np.allowlist_ref.as_ref() else {
        let mut conditions = Vec::new();
        if inline_present {
            conditions.push(preserve_transition_time(
                prior_authoritative,
                TYPE_ALLOWLIST_AUTHORITATIVE,
                cond_status::FALSE,
                cond_reason::INLINE,
                "no allowlistRef set; using inline allowedEndpoints",
                generation,
            ));
            // Slice 5c.2: surface inline-only as `AllowlistVerified=False/Unsigned`.
            // Default behaviour is *allow with warning* — leave
            // `endpoints` set and let the reconciler still program the
            // L4 NetworkPolicy + L7 router mount. Operators flip
            // `egress.requireSigned: true` (helm) to fail-closed; see
            // below.
            conditions.push(preserve_transition_time(
                prior_verified,
                TYPE_ALLOWLIST_VERIFIED,
                cond_status::FALSE,
                cond_reason::UNSIGNED,
                "inline allowedEndpoints have no cosign attestation \
                 (set spec.networkPolicy.allowlistRef to sign the bundle)",
                generation,
            ));
        }
        let require_signed = require_signed_allowlist();
        if require_signed && inline_present {
            // Fail-closed: drop the endpoints + flip authoritative to
            // FailedClosed so the reconciler stamps Degraded.
            return AllowlistResolution {
                endpoints: None,
                conditions: vec![
                    preserve_transition_time(
                        prior_authoritative,
                        TYPE_ALLOWLIST_AUTHORITATIVE,
                        cond_status::FALSE,
                        cond_reason::FAILED_CLOSED,
                        "REQUIRE_SIGNED_ALLOWLIST=true and inline allowedEndpoints \
                         have no allowlistRef; refusing to program user egress",
                        generation,
                    ),
                    preserve_transition_time(
                        prior_verified,
                        TYPE_ALLOWLIST_VERIFIED,
                        cond_status::FALSE,
                        cond_reason::UNSIGNED,
                        "REQUIRE_SIGNED_ALLOWLIST=true: inline allowedEndpoints rejected",
                        generation,
                    ),
                ],
                fail_closed_no_lkg: true,
            };
        }
        return AllowlistResolution {
            endpoints: if inline_present { Some(inline) } else { None },
            conditions,
            fail_closed_no_lkg: false,
        };
    };

    // ─── Branches 2–4: allowlistRef is set ───
    let ns = sandbox
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| "default".into());
    let name = sandbox
        .metadata
        .name
        .clone()
        .unwrap_or_else(|| "unknown".into());
    let mut lkg_entry = lkg_get(&ns, &name);

    // Run the verify path.
    let verify: Result<VerifiedAllowlist, FetchError> = match signer_policy_handle.snapshot() {
        SignerPolicyState::FromConfigMap(p) => {
            let cfg: SignerPolicyConfig = p.into();
            fetch_and_verify(artifact_ref, &cfg).await
        }
        SignerPolicyState::Malformed(msg) => Err(FetchError::SignerPolicyMalformed(msg)),
        SignerPolicyState::Absent => {
            let cfg = SignerPolicyConfig::from_env();
            fetch_and_verify(artifact_ref, &cfg).await
        }
    };

    let mut conditions: Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> = Vec::new();
    let emit_drift_condition =
        |conds: &mut Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>,
         entry: &mut LkgEntry,
         is_drift: bool,
         msg: &str| {
            if is_drift {
                entry.drift_active = true;
                entry.drift_clear_counter = 0;
                conds.push(preserve_transition_time(
                    prior_drift,
                    TYPE_ALLOWLIST_DRIFT,
                    cond_status::TRUE,
                    cond_reason::INLINE_DIFFERS_FROM_ARTIFACT,
                    msg,
                    generation,
                ));
            } else if entry.drift_active {
                // Drift just cleared — emit False/InlineCleared and
                // count down. After 2 successive cleared reconciles we
                // stop emitting (condition drops out of status on the
                // next merge-patch).
                entry.drift_clear_counter = entry.drift_clear_counter.saturating_add(1);
                if entry.drift_clear_counter <= 2 {
                    conds.push(preserve_transition_time(
                        prior_drift,
                        TYPE_ALLOWLIST_DRIFT,
                        cond_status::FALSE,
                        cond_reason::INLINE_CLEARED,
                        msg,
                        generation,
                    ));
                } else {
                    entry.drift_active = false;
                    entry.drift_clear_counter = 0;
                }
            }
        };

    match verify {
        // ── Branch 2: verify ok ──
        Ok(verified) => {
            tracing::info!(
                digest = %verified.digest,
                gen = verified.generation,
                "AllowlistVerified=True; AllowlistAuthoritative=True"
            );
            let derived = canonical_to_endpoint_config(&verified.endpoints);
            let drift = inline_present && !endpoint_lists_equivalent(&inline, &derived);
            let drift_summary = if drift {
                Some(compute_drift_summary(&inline, &derived))
            } else {
                None
            };

            conditions.push(preserve_transition_time(
                prior_verified,
                TYPE_ALLOWLIST_VERIFIED,
                cond_status::TRUE,
                cond_reason::VERIFIED,
                &format!(
                    "digest={}, generation={}",
                    verified.digest, verified.generation
                ),
                generation,
            ));
            conditions.push(preserve_transition_time(
                prior_authoritative,
                TYPE_ALLOWLIST_AUTHORITATIVE,
                cond_status::TRUE,
                cond_reason::VERIFIED,
                &format!(
                    "deriving NetworkPolicy egress from artifact {}",
                    verified.digest
                ),
                generation,
            ));
            // Drift message format: human prefix + ` | drift=<json>` so
            // the plugin can parse the trailing JSON without losing the
            // human-readable summary. Plugins must split on " | drift="
            // and parse what follows as `DriftSummary`.
            let drift_msg = if let Some(ref s) = drift_summary {
                let json = serde_json::to_string(s)
                    .unwrap_or_else(|_| r#"{"added":[],"removed":[]}"#.to_string());
                format!(
                    "inline allowedEndpoints differs from verified artifact; artifact wins | drift={json}"
                )
            } else {
                "inline allowedEndpoints empty or matches artifact".to_string()
            };
            emit_drift_condition(&mut conditions, &mut lkg_entry, drift, &drift_msg);

            // Update LKG with the freshly verified endpoints.
            lkg_entry.endpoints = Some(derived.clone());
            lkg_put(&ns, &name, lkg_entry);

            AllowlistResolution {
                endpoints: Some(derived),
                conditions,
                fail_closed_no_lkg: false,
            }
        }
        // ── Transient: preserve prior; do not stamp anew. ──
        Err(FetchError::Transient(msg)) => {
            tracing::warn!(error = %msg, "policy_fetcher transient; preserving prior conditions");
            if let Some(c) = prior_verified.cloned() {
                conditions.push(c);
            }
            if let Some(c) = prior_authoritative.cloned() {
                conditions.push(c);
            }
            if let Some(c) = prior_drift.cloned() {
                conditions.push(c);
            }
            // Endpoints: prefer LKG; otherwise None (fail-closed).
            let lkg_endpoints = lkg_entry.endpoints.clone();
            let fail_closed = lkg_endpoints.is_none();
            AllowlistResolution {
                endpoints: lkg_endpoints,
                conditions,
                fail_closed_no_lkg: fail_closed,
            }
        }
        // ── Branches 3–4: verify failed (terminal). ──
        Err(other) => {
            let reason = reason_for_error(&other).unwrap_or("Failed");
            let msg = other.to_string();
            tracing::warn!(reason = %reason, error = %msg, "AllowlistVerified=False");
            conditions.push(new_condition(
                TYPE_ALLOWLIST_VERIFIED,
                cond_status::FALSE,
                reason,
                &msg,
                generation,
            ));
            // Do not overwrite prior_verified's transition time when
            // status flipped — `preserve_transition_time` already
            // handles that via the prior condition lookup. Use it:
            let last = conditions.last_mut().unwrap();
            *last = preserve_transition_time(
                prior_verified,
                TYPE_ALLOWLIST_VERIFIED,
                cond_status::FALSE,
                reason,
                &msg,
                generation,
            );

            match lkg_entry.endpoints.clone() {
                // Branch 3: LKG present → use it.
                Some(lkg_endpoints) => {
                    conditions.push(preserve_transition_time(
                        prior_authoritative,
                        TYPE_ALLOWLIST_AUTHORITATIVE,
                        cond_status::FALSE,
                        cond_reason::STALE_LKG,
                        &format!("verify failed ({reason}); preserving last-known-good endpoints"),
                        generation,
                    ));
                    // Drift is undefined when verify fails (we don't
                    // know the artifact). Preserve the prior drift
                    // condition if any so operators don't lose
                    // visibility across a flap.
                    if let Some(c) = prior_drift.cloned() {
                        conditions.push(c);
                    }
                    lkg_put(&ns, &name, lkg_entry);
                    AllowlistResolution {
                        endpoints: Some(lkg_endpoints),
                        conditions,
                        fail_closed_no_lkg: false,
                    }
                }
                // Branch 4: no LKG → fail closed.
                None => {
                    conditions.push(preserve_transition_time(
                        prior_authoritative,
                        TYPE_ALLOWLIST_AUTHORITATIVE,
                        cond_status::FALSE,
                        cond_reason::FAILED_CLOSED,
                        &format!(
                            "verify failed ({reason}) and no last-known-good endpoints; refusing to broaden egress"
                        ),
                        generation,
                    ));
                    AllowlistResolution {
                        endpoints: None,
                        conditions,
                        fail_closed_no_lkg: true,
                    }
                }
            }
        }
    }
}

/// Production-side wrapper around [`resolve_allowlist_with_handle`]
/// using the process-global signer-policy handle.
pub async fn resolve_allowlist(sandbox: &crate::crd::ClawSandbox) -> AllowlistResolution {
    resolve_allowlist_with_handle(sandbox, &crate::signer_policy::global()).await
}

/// Reconciler-side driver: when the sandbox has a
/// `networkPolicy.allowlistRef`, run the fetcher and translate the
/// outcome into a Condition (with `lastTransitionTime` preserved across
/// same-status reconciles via [`crate::status::conditions::preserve_transition_time`]).
///
/// Kept as a thin wrapper around [`resolve_allowlist`] — returns just
/// the `AllowlistVerified` condition. New code should consume
/// [`resolve_allowlist`] directly to also get `AllowlistAuthoritative`
/// / `AllowlistDrift` and the resolved endpoints.
///
/// Returns `None` when no `allowlistRef` is set, or when the fetch
/// returned a `Transient` error and there is no prior condition to
/// preserve.
#[allow(dead_code)]
pub async fn maybe_verify_allowlist(
    sandbox: &crate::crd::ClawSandbox,
) -> Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> {
    maybe_verify_allowlist_with_handle(sandbox, &crate::signer_policy::global()).await
}

/// Test-friendly variant: callers can inject a
/// [`crate::signer_policy::SharedSignerPolicy`] in any state without
/// spinning up the watcher. Production code uses
/// [`maybe_verify_allowlist`], which reads the process-global handle.
#[allow(dead_code)]
pub async fn maybe_verify_allowlist_with_handle(
    sandbox: &crate::crd::ClawSandbox,
    signer_policy_handle: &crate::signer_policy::SharedSignerPolicy,
) -> Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> {
    use crate::status::conditions::TYPE_ALLOWLIST_VERIFIED;
    sandbox
        .spec
        .network_policy
        .as_ref()
        .and_then(|np| np.allowlist_ref.as_ref())?;
    let resolution = resolve_allowlist_with_handle(sandbox, signer_policy_handle).await;
    resolution
        .conditions
        .into_iter()
        .find(|c| c.type_ == TYPE_ALLOWLIST_VERIFIED)
}

// ─────────────────────────── tests ───────────────────────────

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;

    /// Process-wide env mutex. Rust tests run in parallel by default, so
    /// concurrent `set_var`/`remove_var` calls race; serialize them.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// RAII helper that snapshots an env var on construction and
    /// restores it on drop. Lets each test mutate env without leaking
    /// state to siblings.
    struct EnvGuard {
        name: &'static str,
        prior: Option<String>,
    }
    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var(name).ok();
            // SAFETY: tests serialize env mutation via env_lock().
            unsafe { std::env::set_var(name, value) };
            Self { name, prior }
        }
        fn unset(name: &'static str) -> Self {
            let prior = std::env::var(name).ok();
            // SAFETY: tests serialize env mutation via env_lock().
            unsafe { std::env::remove_var(name) };
            Self { name, prior }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: tests serialize env mutation via env_lock().
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(self.name, v),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    fn good_ref() -> OciArtifactRef {
        OciArtifactRef {
            registry: "myacr.azurecr.io".into(),
            repository: "azureclaw/policies/sandbox-foo".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            artifact_type: EGRESS_ALLOWLIST_V1_MEDIA_TYPE.to_string(),
        }
    }

    fn canonical_doc() -> String {
        // Two endpoints, sorted, deduped, lowercase, explicit ports.
        "apiVersion: azureclaw.dev/v1alpha1\n\
         kind: EgressAllowlist\n\
         metadata:\n  generation: 1\n\
         spec:\n  endpoints:\n  \
         - host: api.github.com\n    port: 443\n  \
         - host: dev.azure.com\n    port: 443\n"
            .to_string()
    }

    #[test]
    fn signer_policy_unconfigured_when_env_unset() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e2 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        let p = SignerPolicyConfig::from_env();
        assert!(p.fulcio_issuers.is_empty());
        assert!(p.san_patterns.is_empty());
        assert!(!p.is_configured());
    }

    #[test]
    fn signer_policy_configured_from_env() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::set(
            SIGNER_FULCIO_ISSUERS_ENV,
            "https://token.actions.githubusercontent.com,https://login.microsoftonline.com",
        );
        let _e2 = EnvGuard::set(
            SIGNER_SAN_PATTERNS_ENV,
            "https://github.com/Azure/azureclaw/.github/workflows/*.yml@*,signer@example.com",
        );
        let p = SignerPolicyConfig::from_env();
        assert_eq!(p.fulcio_issuers.len(), 2);
        assert_eq!(p.san_patterns.len(), 2);
        assert!(p.is_configured());
    }

    #[test]
    fn signer_policy_requires_both_lists_for_is_configured() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::set(SIGNER_FULCIO_ISSUERS_ENV, "https://issuer.example");
        let _e2 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        let p = SignerPolicyConfig::from_env();
        assert!(!p.is_configured(), "issuers without SAN patterns is unsafe");
    }

    #[test]
    fn canonical_parser_accepts_valid_artifact() {
        let v = crate::policy_canonical::egress::parse(canonical_doc().as_bytes())
            .expect("valid canonical");
        assert_eq!(v.api_version, EgressKind::API_VERSION);
        assert_eq!(v.kind, EgressKind::KIND);
        assert_eq!(v.generation, 1);
        assert_eq!(v.endpoints.len(), 2);
        assert_eq!(v.endpoints[0].host, "api.github.com");
        assert_eq!(v.endpoints[0].port, 443);
        assert_eq!(v.endpoints[1].host, "dev.azure.com");
    }

    #[test]
    fn canonical_parser_rejects_unsorted_endpoints() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints:\n  \
                   - host: dev.azure.com\n    port: 443\n  \
                   - host: api.github.com\n    port: 443\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("not sorted"))
        );
    }

    #[test]
    fn canonical_parser_rejects_duplicate_endpoints() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints:\n  \
                   - host: api.github.com\n    port: 443\n  \
                   - host: api.github.com\n    port: 443\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("duplicate"))
        );
    }

    #[test]
    fn canonical_parser_rejects_missing_generation() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  notGeneration: 1\n\
                   spec:\n  endpoints: []\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("generation"))
        );
    }

    #[test]
    fn canonical_parser_rejects_zero_generation() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 0\n\
                   spec:\n  endpoints: []\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("> 0")));
    }

    #[test]
    fn canonical_parser_rejects_uppercase_host() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints:\n  \
                   - host: API.github.com\n    port: 443\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("invalid byte"))
        );
    }

    #[test]
    fn canonical_parser_rejects_wildcard_host() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints:\n  \
                   - host: '*.example.com'\n    port: 443\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("wildcards"))
        );
    }

    #[test]
    fn canonical_parser_rejects_out_of_range_port() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints:\n  \
                   - host: api.github.com\n    port: 65536\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn canonical_parser_rejects_swapped_endpoint_keys() {
        // host then port required (rule #11).
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints:\n  \
                   - port: 443\n    host: api.github.com\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("key order"))
        );
    }

    #[test]
    fn canonical_parser_rejects_missing_trailing_newline() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints: []";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("trailing newline"))
        );
    }

    #[test]
    fn canonical_parser_rejects_top_level_key_out_of_order() {
        let doc = "kind: EgressAllowlist\n\
                   apiVersion: azureclaw.dev/v1alpha1\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints: []\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("out of order") || m.contains("missing"))
        );
    }

    #[test]
    fn canonical_parser_rejects_comments() {
        let doc = "# header\n\
                   apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints: []\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("comments")));
    }

    #[test]
    fn validate_ref_shape_rejects_bad_digest() {
        let mut r = good_ref();
        r.digest = "sha512:xyz".into();
        let err = validate_ref_shape::<EgressKind>(&r).unwrap_err();
        assert!(matches!(err, FetchError::InvalidRef(_)));
    }

    #[test]
    fn validate_ref_shape_rejects_bad_artifact_type() {
        let mut r = good_ref();
        r.artifact_type = "application/vnd.something.else".into();
        let err = validate_ref_shape::<EgressKind>(&r).unwrap_err();
        assert!(matches!(err, FetchError::InvalidRef(ref m) if m.contains("artifactType")));
    }

    #[test]
    fn validate_ref_shape_rejects_uppercase_digest() {
        let mut r = good_ref();
        r.digest = format!("sha256:{}", "A".repeat(64));
        let err = validate_ref_shape::<EgressKind>(&r).unwrap_err();
        assert!(matches!(err, FetchError::InvalidRef(ref m) if m.contains("lowercase")));
    }

    #[test]
    fn validate_ref_shape_accepts_valid_ref() {
        validate_ref_shape::<EgressKind>(&good_ref()).expect("good ref");
    }

    #[test]
    fn reason_for_error_maps_each_variant() {
        assert_eq!(
            reason_for_error(&FetchError::SignerPolicyMissing),
            Some("SignerPolicyMissing")
        );
        assert_eq!(
            reason_for_error(&FetchError::SignerPolicyMalformed("x".into())),
            Some("SignerPolicyMalformed")
        );
        assert_eq!(
            reason_for_error(&FetchError::Unauthorized("x".into())),
            Some("Unauthorized")
        );
        assert_eq!(
            reason_for_error(&FetchError::NotFound("x".into())),
            Some("NotFound")
        );
        assert_eq!(
            reason_for_error(&FetchError::InvalidRef("x".into())),
            Some("InvalidRef")
        );
        assert_eq!(
            reason_for_error(&FetchError::SignatureVerifyFailed("x".into())),
            Some("SignatureVerifyFailed")
        );
        assert_eq!(
            reason_for_error(&FetchError::IdentityMismatch("x".into())),
            Some("IdentityMismatch")
        );
        assert_eq!(
            reason_for_error(&FetchError::CanonicalFormViolation("x".into())),
            Some("CanonicalFormViolation")
        );
        assert_eq!(
            reason_for_error(&FetchError::DigestMismatch {
                expected: "a".into(),
                actual: "b".into()
            }),
            Some("DigestMismatch")
        );
        // Transient is the only Some-but-no-condition case.
        assert_eq!(reason_for_error(&FetchError::Transient("x".into())), None);
    }

    #[test]
    fn cache_round_trips_and_expires() {
        EgressKind::cache_clear();
        let r = good_ref();
        let key = cache_key(&r);
        let now = Instant::now();
        assert!(EgressKind::cache_get(&key, now).is_none(), "cold cache");

        let v = crate::policy_canonical::egress::parse(canonical_doc().as_bytes()).unwrap();
        EgressKind::cache_put(key.clone(), v.clone());
        let hit = EgressKind::cache_get(&key, now).expect("hit");
        assert_eq!(hit.generation, v.generation);

        // Simulated expiry: a synthetic "now" beyond TTL should miss
        // even though the entry is still in the map. (We can't actually
        // advance Instant::now(), but cache_get computes
        // `now.duration_since(entry.inserted)`, so passing a future
        // `now` exercises the TTL branch.)
        let future = now + CACHE_TTL + Duration::from_secs(1);
        assert!(EgressKind::cache_get(&key, future).is_none(), "expired");
        EgressKind::cache_clear();
    }

    #[test]
    fn is_acr_host_recognises_global_clouds() {
        assert!(is_acr_host("myacr.azurecr.io"));
        assert!(is_acr_host("MyAcr.AzureCr.Io"));
        assert!(is_acr_host("foo.azurecr.cn"));
        assert!(is_acr_host("foo.azurecr.us"));
        assert!(!is_acr_host("ghcr.io"));
        assert!(!is_acr_host("docker.io"));
    }

    #[tokio::test]
    async fn fetch_returns_signer_policy_missing_when_no_signer_policy() {
        let _g = env_lock().lock().unwrap();
        let _e2 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e3 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        let policy = SignerPolicyConfig::from_env();
        let err = fetch_and_verify(&good_ref(), &policy).await.unwrap_err();
        assert!(
            matches!(err, FetchError::SignerPolicyMissing),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_returns_invalid_ref_for_bad_digest() {
        let _g = env_lock().lock().unwrap();
        let mut r = good_ref();
        r.digest = "not-a-digest".into();
        let policy = SignerPolicyConfig::default();
        let err = fetch_and_verify(&r, &policy).await.unwrap_err();
        assert!(matches!(err, FetchError::InvalidRef(_)));
    }

    #[tokio::test]
    async fn acr_token_for_pull_requires_federated_token_env() {
        let _g = env_lock().lock().unwrap();
        let _e = EnvGuard::unset("AZURE_FEDERATED_TOKEN_FILE");
        let err = acr_token_for_pull("myacr.azurecr.io", "repo")
            .await
            .unwrap_err();
        assert!(
            matches!(err, FetchError::Unauthorized(ref m) if m.contains("AZURE_FEDERATED_TOKEN_FILE"))
        );
    }

    // ─────── S12.d: SharedSignerPolicy resolution in maybe_verify_allowlist ───────
    //
    // These tests exercise the new `_with_handle` variant directly so
    // they don't touch the OnceLock global (parallel-test safe). Each
    // case constructs a `ClawSandbox` carrying an `allowlistRef` + an
    // injected handle in a specific [`SignerPolicyState`], asserts on
    // the resulting `Condition.reason`. Network IO never executes
    // because we either short-circuit on policy state (Malformed) or
    // we use an obviously-fake registry that the cosign client will
    // refuse before any DNS lookup; the assertion only inspects the
    // mapped reason.

    use crate::crd::{ClawSandbox, ClawSandboxSpec, NetworkPolicyConfig};
    use crate::signer_policy::{
        SharedSignerPolicy, SignerPolicy as ParsedSignerPolicy, SignerPolicyState,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    fn sandbox_with_allowlist_ref() -> ClawSandbox {
        ClawSandbox {
            metadata: ObjectMeta {
                name: Some("demo".into()),
                namespace: Some("azureclaw-demo".into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: ClawSandboxSpec {
                network_policy: Some(NetworkPolicyConfig {
                    allowlist_ref: Some(good_ref()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            status: None,
        }
    }

    #[tokio::test]
    async fn with_handle_malformed_emits_signer_policy_malformed_condition() {
        let _g = env_lock().lock().unwrap();
        let sb = sandbox_with_allowlist_ref();
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Malformed(
            "missing required key `fulcioIssuers`".into(),
        ));
        let cond = maybe_verify_allowlist_with_handle(&sb, &h)
            .await
            .expect("condition emitted");
        assert_eq!(cond.type_, "AllowlistVerified");
        assert_eq!(cond.status, "False");
        assert_eq!(cond.reason, "SignerPolicyMalformed");
        assert!(
            cond.message.contains("missing required key"),
            "message preserves parse-error detail: {cond:?}"
        );
    }

    #[tokio::test]
    async fn with_handle_malformed_does_not_fall_back_to_env() {
        // Critical safety property: even with a fully-configured env
        // emergency-override, a malformed ConfigMap MUST surface as
        // SignerPolicyMalformed — operators need the signal that
        // their cluster config is broken.
        let _g = env_lock().lock().unwrap();
        let _e2 = EnvGuard::set(SIGNER_FULCIO_ISSUERS_ENV, "https://issuer.example");
        let _e3 = EnvGuard::set(SIGNER_SAN_PATTERNS_ENV, "signer@example.com");
        let sb = sandbox_with_allowlist_ref();
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Malformed("broken yaml".into()));
        let cond = maybe_verify_allowlist_with_handle(&sb, &h)
            .await
            .expect("condition emitted");
        assert_eq!(cond.reason, "SignerPolicyMalformed");
    }

    #[tokio::test]
    async fn with_handle_absent_falls_back_to_env_missing() {
        let _g = env_lock().lock().unwrap();
        let _e2 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e3 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        let sb = sandbox_with_allowlist_ref();
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let cond = maybe_verify_allowlist_with_handle(&sb, &h)
            .await
            .expect("condition emitted");
        assert_eq!(cond.reason, "SignerPolicyMissing");
    }

    #[tokio::test]
    async fn with_handle_configmap_takes_precedence_over_env() {
        // ConfigMap configured → env vars MUST be ignored. (Otherwise
        // operators couldn't roll out a stricter policy without
        // simultaneously unsetting potentially-stale env vars on
        // every replica.)
        let _g = env_lock().lock().unwrap();
        let _e2 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e3 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        let sb = sandbox_with_allowlist_ref();
        let h =
            SharedSignerPolicy::from_state(SignerPolicyState::FromConfigMap(ParsedSignerPolicy {
                fulcio_issuers: vec!["https://token.actions.githubusercontent.com".into()],
                san_patterns: vec!["signer@example.com".into()],
                ed25519_keys: Vec::new(),
            }));
        // ConfigMap is configured so we proceed past the policy gate;
        // the next failure mode is whatever sigstore does with our
        // fake registry — which is `SignatureVerifyFailed` /
        // `Unauthorized` / `NotFound` / `Transient`. The key
        // assertion is that we did NOT exit at SignerPolicyMissing.
        let cond_opt = maybe_verify_allowlist_with_handle(&sb, &h).await;
        if let Some(cond) = cond_opt {
            assert_ne!(
                cond.reason, "SignerPolicyMissing",
                "ConfigMap was configured; must not fall back to env-missing path"
            );
            assert_ne!(cond.reason, "SignerPolicyMalformed");
        }
    }

    // ─────────────── S12.e: resolver / LKG / drift tests ───────────────

    fn ep(host: &str, port: Option<u16>) -> crate::crd::EndpointConfig {
        crate::crd::EndpointConfig {
            host: host.into(),
            port,
        }
    }

    fn sandbox_with_inline_only(eps: Vec<crate::crd::EndpointConfig>) -> ClawSandbox {
        ClawSandbox {
            metadata: ObjectMeta {
                name: Some("inline".into()),
                namespace: Some("azureclaw-inline".into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: ClawSandboxSpec {
                network_policy: Some(NetworkPolicyConfig {
                    allowed_endpoints: if eps.is_empty() { None } else { Some(eps) },
                    allowlist_ref: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            status: None,
        }
    }

    fn sandbox_with_ref_and_inline(
        name: &str,
        ns: &str,
        inline: Vec<crate::crd::EndpointConfig>,
    ) -> ClawSandbox {
        ClawSandbox {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(ns.into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: ClawSandboxSpec {
                network_policy: Some(NetworkPolicyConfig {
                    allowed_endpoints: if inline.is_empty() {
                        None
                    } else {
                        Some(inline)
                    },
                    allowlist_ref: Some(good_ref()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            status: None,
        }
    }

    fn cond_by_type<'a>(
        conds: &'a [k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
        ty: &str,
    ) -> Option<&'a k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> {
        conds.iter().find(|c| c.type_ == ty)
    }

    #[tokio::test]
    async fn resolve_no_network_policy_returns_default() {
        let _g = env_lock().lock().unwrap();
        let sb = ClawSandbox {
            metadata: ObjectMeta {
                name: Some("nope".into()),
                namespace: Some("azureclaw-nope".into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: ClawSandboxSpec {
                network_policy: None,
                ..Default::default()
            },
            status: None,
        };
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(res.endpoints.is_none());
        assert!(res.conditions.is_empty());
        assert!(!res.fail_closed_no_lkg);
    }

    #[tokio::test]
    async fn resolve_inline_only_emits_authoritative_inline() {
        let _g = env_lock().lock().unwrap();
        let _r = EnvGuard::unset("REQUIRE_SIGNED_ALLOWLIST");
        let sb = sandbox_with_inline_only(vec![ep("api.example.com", None)]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(res.endpoints.is_some(), "inline endpoints carried through");
        assert_eq!(res.endpoints.as_ref().unwrap().len(), 1);
        let auth = cond_by_type(&res.conditions, "AllowlistAuthoritative")
            .expect("AllowlistAuthoritative emitted");
        assert_eq!(auth.status, "False");
        assert_eq!(auth.reason, "Inline");
        // Slice 5c.2: inline-only now also surfaces an Unsigned
        // warning condition so operators see the gap.
        let verified = cond_by_type(&res.conditions, "AllowlistVerified")
            .expect("AllowlistVerified emitted with Unsigned reason");
        assert_eq!(verified.status, "False");
        assert_eq!(verified.reason, "Unsigned");
        assert!(cond_by_type(&res.conditions, "AllowlistDrift").is_none());
        assert!(!res.fail_closed_no_lkg);
    }

    /// Slice 5c.2: with `REQUIRE_SIGNED_ALLOWLIST=true`, inline-only
    /// allowlists fail-closed (endpoints=None, fail_closed_no_lkg=true,
    /// Authoritative=False/FailedClosed, Verified=False/Unsigned).
    #[tokio::test]
    async fn resolve_inline_only_with_require_signed_fails_closed() {
        let _g = env_lock().lock().unwrap();
        let _r = EnvGuard::set("REQUIRE_SIGNED_ALLOWLIST", "true");
        let sb = sandbox_with_inline_only(vec![ep("api.example.com", None)]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(
            res.endpoints.is_none(),
            "require-signed must drop inline endpoints"
        );
        assert!(res.fail_closed_no_lkg, "must signal fail-closed");
        let auth =
            cond_by_type(&res.conditions, "AllowlistAuthoritative").expect("Authoritative emitted");
        assert_eq!(auth.status, "False");
        assert_eq!(auth.reason, "FailedClosed");
        let verified =
            cond_by_type(&res.conditions, "AllowlistVerified").expect("Verified emitted");
        assert_eq!(verified.status, "False");
        assert_eq!(verified.reason, "Unsigned");
    }

    /// Slice 5c.2: `REQUIRE_SIGNED_ALLOWLIST=true` with an EMPTY inline
    /// list is a no-op (nothing to reject) — still no conditions, no
    /// fail-closed marker.
    #[tokio::test]
    async fn resolve_inline_empty_with_require_signed_is_noop() {
        let _g = env_lock().lock().unwrap();
        let _r = EnvGuard::set("REQUIRE_SIGNED_ALLOWLIST", "true");
        let sb = sandbox_with_inline_only(vec![]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(res.endpoints.is_none());
        assert!(res.conditions.is_empty());
        assert!(!res.fail_closed_no_lkg);
    }

    /// Slice 5c.2: `require_signed_allowlist()` parses common truthy
    /// values + treats anything else as `false`.
    #[test]
    fn require_signed_allowlist_parses_truthy_values() {
        let _g = env_lock().lock().unwrap();
        for v in ["true", "TRUE", "1", "yes", "on", "On"] {
            let _e = EnvGuard::set("REQUIRE_SIGNED_ALLOWLIST", v);
            assert!(require_signed_allowlist(), "expected truthy: {v}");
        }
        for v in ["false", "0", "no", "off", "", "garbage"] {
            let _e = EnvGuard::set("REQUIRE_SIGNED_ALLOWLIST", v);
            assert!(!require_signed_allowlist(), "expected falsy: {v}");
        }
        let _u = EnvGuard::unset("REQUIRE_SIGNED_ALLOWLIST");
        assert!(
            !require_signed_allowlist(),
            "unset env defaults to falsy / allow-with-warning"
        );
    }

    #[tokio::test]
    async fn resolve_inline_empty_no_ref_emits_no_conditions() {
        let _g = env_lock().lock().unwrap();
        let sb = sandbox_with_inline_only(vec![]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(res.endpoints.is_none());
        assert!(
            res.conditions.is_empty(),
            "no AllowlistAuthoritative when there's nothing to authorise"
        );
    }

    #[tokio::test]
    async fn resolve_with_ref_no_signer_no_lkg_fails_closed() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e2 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        lkg_clear();
        let sb = sandbox_with_ref_and_inline("noLkgSandbox", "azureclaw-fc", vec![]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(
            res.endpoints.is_none(),
            "no LKG → endpoints must be None (fail-closed)"
        );
        assert!(res.fail_closed_no_lkg);
        let v = cond_by_type(&res.conditions, "AllowlistVerified").expect("Verified emitted");
        assert_eq!(v.status, "False");
        assert_eq!(v.reason, "SignerPolicyMissing");
        let a =
            cond_by_type(&res.conditions, "AllowlistAuthoritative").expect("Authoritative emitted");
        assert_eq!(a.status, "False");
        assert_eq!(a.reason, "FailedClosed");
    }

    #[tokio::test]
    async fn resolve_with_ref_no_signer_with_lkg_uses_lkg() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e2 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        lkg_clear();
        lkg_put(
            "azureclaw-lkg",
            "lkgSandbox",
            LkgEntry {
                endpoints: Some(vec![ep("cached.example.com", Some(443))]),
                drift_clear_counter: 0,
                drift_active: false,
            },
        );
        let sb = sandbox_with_ref_and_inline("lkgSandbox", "azureclaw-lkg", vec![]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        let eps = res.endpoints.as_ref().expect("endpoints from LKG");
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].host, "cached.example.com");
        assert!(!res.fail_closed_no_lkg);
        let a =
            cond_by_type(&res.conditions, "AllowlistAuthoritative").expect("Authoritative emitted");
        assert_eq!(a.status, "False");
        assert_eq!(a.reason, "StaleLKG");
        lkg_clear();
    }

    #[tokio::test]
    async fn controller_restart_simulation_drops_lkg_fail_closed() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e2 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        lkg_put(
            "azureclaw-restart",
            "rs",
            LkgEntry {
                endpoints: Some(vec![ep("a.example", None)]),
                drift_clear_counter: 0,
                drift_active: false,
            },
        );
        lkg_clear();
        let sb = sandbox_with_ref_and_inline("rs", "azureclaw-restart", vec![]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(res.fail_closed_no_lkg, "restart must drop LKG");
        assert!(res.endpoints.is_none());
    }

    #[tokio::test]
    async fn resolve_with_ref_does_not_silently_use_inline_as_fallback() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e2 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        lkg_clear();
        let sb = sandbox_with_ref_and_inline(
            "evil",
            "azureclaw-evil",
            vec![ep("attacker.example.com", None)],
        );
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(res.endpoints.is_none());
        assert!(res.fail_closed_no_lkg);
    }

    #[test]
    fn endpoint_lists_equivalent_normalizes_case_and_default_port() {
        let a = vec![ep("API.Example.COM", None), ep("b.example", Some(443))];
        let b = vec![ep("b.example", None), ep("api.example.com", Some(443))];
        assert!(endpoint_lists_equivalent(&a, &b));
    }

    #[test]
    fn endpoint_lists_equivalent_distinguishes_ports() {
        let a = vec![ep("api.example", Some(443))];
        let b = vec![ep("api.example", Some(8443))];
        assert!(!endpoint_lists_equivalent(&a, &b));
    }

    #[test]
    fn endpoint_lists_equivalent_distinguishes_hosts() {
        let a = vec![ep("api.example", None)];
        let b = vec![ep("other.example", None)];
        assert!(!endpoint_lists_equivalent(&a, &b));
    }

    #[test]
    fn canonical_to_endpoint_config_round_trips_host_port() {
        let canonical = vec![
            CanonicalEndpoint {
                host: "api.example.com".into(),
                port: 443,
                protocol: None,
            },
            CanonicalEndpoint {
                host: "telemetry.example.com".into(),
                port: 8443,
                protocol: None,
            },
        ];
        let derived = canonical_to_endpoint_config(&canonical);
        assert_eq!(derived.len(), 2);
        assert_eq!(derived[0].host, "api.example.com");
        assert_eq!(derived[0].port, Some(443));
        assert_eq!(derived[1].port, Some(8443));
        assert!(
            derived
                .iter()
                .all(|e| !e.host.is_empty() && e.port.is_some())
        );
    }

    #[test]
    fn drift_summary_empty_when_lists_equivalent() {
        let a = vec![ep("API.Example.COM", None), ep("b.example", Some(443))];
        let b = vec![ep("b.example", None), ep("api.example.com", Some(443))];
        let s = compute_drift_summary(&a, &b);
        assert!(s.added.is_empty());
        assert!(s.removed.is_empty());
    }

    #[test]
    fn drift_summary_reports_added_and_removed_sorted() {
        let inline = vec![
            ep("z.added.example", None),
            ep("api.shared.example", None),
            ep("a.added.example", Some(8443)),
        ];
        let derived = vec![
            ep("api.shared.example", Some(443)),
            ep("only-in-artifact.example", None),
        ];
        let s = compute_drift_summary(&inline, &derived);
        assert_eq!(s.added, vec!["a.added.example:8443", "z.added.example:443"]);
        assert_eq!(s.removed, vec!["only-in-artifact.example:443"]);
    }

    #[test]
    fn drift_summary_port_normalization_matches_equivalence() {
        // ep("h", None) and ep("h", Some(443)) must NOT register as a
        // change (port-default normalization).
        let inline = vec![ep("api.example", None)];
        let derived = vec![ep("api.example", Some(443))];
        let s = compute_drift_summary(&inline, &derived);
        assert!(s.added.is_empty());
        assert!(s.removed.is_empty());
    }

    #[test]
    fn drift_summary_is_serde_round_trip_stable() {
        // Wire contract: headlamp plugin parses the JSON suffix of
        // the `AllowlistDrift` condition message. Pin the schema.
        let s = DriftSummary {
            added: vec!["a.example:443".into()],
            removed: vec!["b.example:8443".into()],
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(
            json,
            r#"{"added":["a.example:443"],"removed":["b.example:8443"]}"#
        );
        let back: DriftSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn lkg_round_trip_get_put_clear() {
        let _g = env_lock().lock().unwrap();
        lkg_clear();
        let entry = LkgEntry {
            endpoints: Some(vec![ep("x.example", None)]),
            drift_clear_counter: 1,
            drift_active: true,
        };
        lkg_put("ns", "name", entry.clone());
        let got = lkg_get("ns", "name");
        assert_eq!(got.endpoints.as_ref().unwrap().len(), 1);
        assert!(got.drift_active);
        assert_eq!(got.drift_clear_counter, 1);
        lkg_clear();
        let after = lkg_get("ns", "name");
        assert!(after.endpoints.is_none());
        assert!(!after.drift_active);
    }

    #[test]
    fn allowlist_resolution_default_is_no_op() {
        let r = AllowlistResolution::default();
        assert!(r.endpoints.is_none());
        assert!(r.conditions.is_empty());
        assert!(!r.fail_closed_no_lkg);
    }

    #[tokio::test]
    async fn resolve_inline_with_ref_does_not_emit_authoritative_inline() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e2 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        lkg_clear();
        let sb = sandbox_with_ref_and_inline(
            "withInline",
            "azureclaw-wi",
            vec![ep("inline.example", None)],
        );
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        let a =
            cond_by_type(&res.conditions, "AllowlistAuthoritative").expect("Authoritative emitted");
        assert_ne!(a.reason, "Inline");
    }

    #[tokio::test]
    async fn resolve_fail_closed_emits_two_conditions() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::unset(SIGNER_FULCIO_ISSUERS_ENV);
        let _e2 = EnvGuard::unset(SIGNER_SAN_PATTERNS_ENV);
        lkg_clear();
        let sb = sandbox_with_ref_and_inline("two", "azureclaw-two", vec![]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Absent);
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert_eq!(
            res.conditions.len(),
            2,
            "Verified + Authoritative on fail-closed; no Drift"
        );
    }

    #[tokio::test]
    async fn resolve_malformed_signer_policy_fails_closed_with_specific_reason() {
        let _g = env_lock().lock().unwrap();
        lkg_clear();
        let sb = sandbox_with_ref_and_inline("malformed", "azureclaw-mal", vec![]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Malformed("bad yaml".into()));
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(res.fail_closed_no_lkg);
        let v = cond_by_type(&res.conditions, "AllowlistVerified").expect("Verified emitted");
        assert_eq!(v.reason, "SignerPolicyMalformed");
    }
}
