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
//! When `spec.networkPolicy.allowlistRef` is set on a `KarsSandbox`, the
//! verified canonical artifact is the **authoritative** source of egress
//! endpoints — inline `allowedEndpoints` is ignored (a non-empty inline
//! that differs from the artifact surfaces as `AllowlistDrift=True`).
//! When verification fails the controller fails closed: it preserves the
//! last-known-good (LKG) endpoint set from the prior successful reconcile
//! if available, and otherwise stamps `Degraded` without writing a
//! NetworkPolicy that opens egress beyond the always-allowed defaults.
//! See [`resolve_allowlist`].
//!
//! The S12.b `KARS_FEATURE_SIGNED_ALLOWLIST` env gate was lifted in
//! S12.e — the verification + authoritative resolution path is always-on
//! when `allowlistRef` is set; existing deployments without `allowlistRef`
//! observe no behavior change.
//!
//! ## SignerPolicy — S12.d (landed)
//!
//! The cluster `SignerPolicy` is now sourced from the watched
//! `kars-signer-policy` ConfigMap in the controller namespace
//! (see [`crate::signer_policy`]). The env vars
//! `KARS_SIGNER_FULCIO_ISSUERS` / `KARS_SIGNER_SAN_PATTERNS`
//! remain as an emergency-override fallback that activates only when
//! the ConfigMap is absent. A *malformed* ConfigMap surfaces as
//! [`FetchError::SignerPolicyMalformed`] and does **not** silently
//! fall back — operators get a hard signal on every affected
//! `KarsSandbox`.
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
const SIGNER_FULCIO_ISSUERS_ENV: &str = "KARS_SIGNER_FULCIO_ISSUERS";
/// Comma-separated list of SAN glob patterns. See [`SignerPolicyConfig`].
const SIGNER_SAN_PATTERNS_ENV: &str = "KARS_SIGNER_SAN_PATTERNS";

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
/// `reason` value emitted on `KarsSandbox.status` — see
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
    let trust_root = trust_root_cache().await?;
    verify_via_sigstore_with_root::<K, _>(artifact_ref, signer_policy, trust_root.as_ref(), false)
        .await
}

/// Bind each candidate signature to its trusted certificate identity and Rekor record.
///
/// Sigstore intentionally accepts an image when any signature layer satisfies
/// the constraints. Invalid Rekor bindings therefore disqualify only their
/// corresponding candidate and must not poison another valid signature.
fn verify_signer_identity(
    layers: &[sigstore::cosign::SignatureLayer],
    signer_policy: &SignerPolicyConfig,
) -> Result<(), FetchError> {
    use sigstore::cosign::verification_constraint::cert_subject_email_verifier::StringVerifier;
    use sigstore::cosign::verification_constraint::{
        CertSubjectEmailVerifier, CertSubjectUrlVerifier, VerificationConstraint,
        VerificationConstraintVec,
    };
    use sigstore::cosign::verify_constraints;
    use sigstore::errors::Result as SigstoreResult;

    let mut binding_failure = None;

    #[derive(Debug)]
    struct AnyOf(Vec<Box<dyn VerificationConstraint>>);
    impl VerificationConstraint for AnyOf {
        fn verify(&self, layer: &sigstore::cosign::SignatureLayer) -> SigstoreResult<bool> {
            for constraint in &self.0 {
                if constraint.verify(layer)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }

    #[derive(Debug)]
    struct SignedIdentity(Box<dyn VerificationConstraint>);
    impl VerificationConstraint for SignedIdentity {
        fn verify(&self, layer: &sigstore::cosign::SignatureLayer) -> SigstoreResult<bool> {
            use sigstore::crypto::Signature;

            if !self.0.verify(layer)? {
                return Ok(false);
            }
            let Some(certificate_signature) = &layer.certificate_signature else {
                return Ok(false);
            };
            let Some(signature) = &layer.signature else {
                return Ok(false);
            };
            Ok(certificate_signature
                .verification_key
                .verify_signature(
                    Signature::Base64Encoded(signature.as_bytes()),
                    &layer.raw_data,
                )
                .is_ok())
        }
    }

    let mut identities: Vec<Box<dyn VerificationConstraint>> = Vec::new();
    for issuer in &signer_policy.fulcio_issuers {
        for san in &signer_policy.san_patterns {
            if san.starts_with("https://") || san.starts_with("http://") {
                identities.push(Box::new(SignedIdentity(Box::new(CertSubjectUrlVerifier {
                    url: san.clone(),
                    issuer: issuer.clone(),
                }))));
            } else {
                let email = if let Some(re) = san.strip_prefix("re:") {
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
                identities.push(Box::new(SignedIdentity(Box::new(
                    CertSubjectEmailVerifier {
                        email,
                        issuer: Some(StringVerifier::ExactMatch(issuer.clone())),
                    },
                ))));
            }
        }
    }

    let constraints: VerificationConstraintVec = vec![Box::new(AnyOf(identities))];
    let mut identity_failure = None;
    for layer in layers {
        if let Some(sigstore::cosign::bundle_content::BundleContent::RekorBundle(bundle)) =
            &layer.bundle
            && let Err(error) = verify_legacy_rekor_binding(layer, bundle)
        {
            binding_failure.get_or_insert(error);
            continue;
        }
        match verify_constraints(std::slice::from_ref(layer), constraints.iter()) {
            Ok(()) => return Ok(()),
            Err(error) => {
                identity_failure
                    .get_or_insert_with(|| FetchError::IdentityMismatch(format!("{error:?}")));
            }
        }
    }

    if let Some(error) = identity_failure {
        return Err(error);
    }
    if let Some(error) = binding_failure {
        return Err(error);
    }
    Err(FetchError::IdentityMismatch(
        "no signature candidates were provided".into(),
    ))
}

async fn verify_via_sigstore_with_root<K: PolicyKind, T: sigstore::trust::TrustRoot + Sync>(
    artifact_ref: &OciArtifactRef,
    signer_policy: &SignerPolicyConfig,
    trust_root: &T,
    use_http_registry: bool,
) -> Result<K::Output, FetchError> {
    use sigstore::cosign::{ClientBuilder, CosignCapabilities};
    use sigstore::registry::{ClientConfig, ClientProtocol, OciReference};

    // Lazily initialize a Sigstore Public Good trust root (Fulcio CA +
    // Rekor + CTfe keys) from the embedded TUF root and remote metadata.
    // Without this, sigstore-rs builds clients with `Fulcio integration
    // disabled` and `signature_layer.certificate_signature` is `None`,
    // causing every `CertSubject*Verifier` constraint to fail closed.
    // Cached process-wide; refreshed on controller restart (TUF metadata
    // is checked-in via the crate's embedded snapshot, with TUF refresh
    // performed once on first reconcile).
    let mut registry_config = ClientConfig::default();
    if use_http_registry {
        registry_config.protocol = ClientProtocol::Http;
    }

    let mut client = ClientBuilder::default()
        .with_oci_client_config(registry_config)
        .with_trust_repository(trust_root)
        .map_err(|e| FetchError::Transient(format!("cosign client (trust repo): {e}")))?
        .build()
        .map_err(|e| FetchError::Transient(format!("cosign client: {e}")))?;

    let auth = pick_registry_auth(&artifact_ref.registry, &artifact_ref.repository).await?;

    let image = OciReference::with_digest(
        artifact_ref.registry.clone(),
        artifact_ref.repository.clone(),
        artifact_ref.digest.clone(),
    );

    // Fetch candidate layers and validate Sigstore trust material. Legacy
    // Rekor-backed artifact signatures are verified by the SignedIdentity
    // constraint after their Rekor binding below; Sigstore verifies referrer
    // DSSE signatures while constructing those bundle candidates.
    let layers = client
        .trusted_signature_layers(&auth, &image)
        .await
        .map_err(map_oci_error)?;

    // Keep the non-Send verification constraints out of the reconcile future
    // before its next `.await` (kube-rs requires the future to be `Send`).
    verify_signer_identity(&layers, signer_policy)?;

    // Step D: pull artifact bytes (digest-pinned).
    //
    // The pull is content-addressed by manifest digest (`@sha256:...`), so the
    // registry cannot substitute a different manifest. `oci_client::pull`
    // verifies each layer descriptor against the manifest as part of the pull,
    // which authenticates the bytes we get back. We deliberately do NOT
    // recompute `sha256(layer_bytes)` here and compare it to `artifact_ref.digest`:
    // those are different things (layer digest vs. manifest digest) and would
    // never match for any non-trivial artifact.
    let bytes = pull_artifact_bytes(artifact_ref, &auth, K::MEDIA_TYPE, use_http_registry).await?;

    let mut parsed = K::parse(&bytes)?;
    K::finalize(&mut parsed, artifact_ref.digest.clone(), SystemTime::now());
    Ok(parsed)
}

fn verify_legacy_rekor_binding(
    layer: &sigstore::cosign::SignatureLayer,
    bundle: &sigstore::cosign::bundle::Bundle,
) -> Result<(), FetchError> {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    use sigstore::cosign::signature_layers::CertificateSubject;
    use sigstore::crypto::{CosignVerificationKey, Signature};
    use x509_cert::der::{Decode, pem::PemLabel};

    let fail = |message: &str| FetchError::SignatureVerifyFailed(message.to_owned());
    let body_bytes = base64::engine::general_purpose::STANDARD
        .decode(&bundle.payload.body)
        .map_err(|_| fail("invalid base64 Rekor body"))?;
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes).map_err(|_| fail("invalid Rekor body JSON"))?;
    let kind = body.get("kind").and_then(serde_json::Value::as_str);
    if !matches!(kind, Some("hashedrekord" | "rekord"))
        || body.get("apiVersion").and_then(serde_json::Value::as_str) != Some("0.0.1")
    {
        return Err(fail("unsupported Rekor body kind or apiVersion"));
    }

    let spec = body
        .get("spec")
        .ok_or_else(|| fail("Rekor hashedrekord body is missing spec"))?;
    let hash = spec
        .get("data")
        .and_then(|data| data.get("hash"))
        .ok_or_else(|| fail("Rekor hashedrekord body is missing data.hash"))?;
    let expected_hash = match (
        kind,
        hash.get("algorithm").and_then(serde_json::Value::as_str),
    ) {
        (Some("hashedrekord"), Some("sha256")) | (Some("rekord"), Some("sha256")) => {
            Sha256::digest(&layer.raw_data).to_vec()
        }
        (Some("hashedrekord"), Some("sha384")) => sha2::Sha384::digest(&layer.raw_data).to_vec(),
        (Some("hashedrekord"), Some("sha512")) => sha2::Sha512::digest(&layer.raw_data).to_vec(),
        _ => return Err(fail("unsupported Rekor hashedrekord hash algorithm")),
    };
    let logged_hash = hash
        .get("value")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| fail("Rekor hash value is missing"))?;
    let logged_hash =
        hex::decode(logged_hash).map_err(|_| fail("Rekor hash value is not valid hexadecimal"))?;
    if logged_hash != expected_hash {
        return Err(fail(
            "Rekor hashedrekord payload hash does not match the signed layer",
        ));
    }

    let signature = layer
        .signature
        .as_deref()
        .ok_or_else(|| fail("signed layer is missing signature"))?;
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature)
        .map_err(|_| fail("invalid base64 layer signature"))?;
    let logged_signature = spec
        .get("signature")
        .and_then(|signature| signature.get("content"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| fail("Rekor hashedrekord body is missing signature.content"))?;
    if kind == Some("rekord")
        && spec
            .get("signature")
            .and_then(|signature| signature.get("format"))
            .and_then(serde_json::Value::as_str)
            != Some("x509")
    {
        return Err(fail("Rekor rekord signature format must be x509"));
    }
    let logged_signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(logged_signature)
        .map_err(|_| fail("invalid base64 Rekor signature"))?;
    if logged_signature_bytes != signature_bytes {
        return Err(fail(
            "Rekor hashedrekord signature does not match the signed layer",
        ));
    }

    let logged_certificate = spec
        .get("signature")
        .and_then(|signature| signature.get("publicKey"))
        .and_then(|key| key.get("content"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| fail("Rekor hashedrekord body is missing publicKey.content"))?;
    let logged_certificate = base64::engine::general_purpose::STANDARD
        .decode(logged_certificate)
        .map_err(|_| fail("invalid base64 Rekor certificate"))?;
    let (certificate_label, logged_certificate) =
        x509_cert::der::pem::decode_vec(&logged_certificate)
            .map_err(|_| fail("invalid PEM certificate in Rekor hashedrekord body"))?;
    if certificate_label != x509_cert::Certificate::PEM_LABEL {
        return Err(fail(
            "Rekor hashedrekord public key is not a certificate PEM",
        ));
    }
    let logged_certificate = x509_cert::Certificate::from_der(&logged_certificate)
        .map_err(|_| fail("invalid DER certificate in Rekor hashedrekord body"))?;
    let accepted_identity = layer
        .certificate_signature
        .as_ref()
        .ok_or_else(|| fail("legacy signature has no Sigstore-validated certificate"))?;
    let logged_subject = CertificateSubject::from_certificate(&logged_certificate)
        .map_err(|_| fail("Rekor certificate has no supported signer identity"))?;
    if logged_subject != accepted_identity.subject {
        return Err(fail(
            "Rekor certificate subject does not match the Sigstore-validated identity",
        ));
    }
    const SIGSTORE_ISSUER_OID: &str = "1.3.6.1.4.1.57264.1.1";
    let logged_issuer = logged_certificate
        .tbs_certificate()
        .extensions()
        .ok_or_else(|| fail("Rekor certificate has no extensions"))?
        .iter()
        .find(|extension| extension.extn_id.to_string() == SIGSTORE_ISSUER_OID)
        .map(|extension| {
            String::from_utf8(extension.extn_value.as_bytes().to_vec())
                .map_err(|_| fail("Rekor certificate issuer is not UTF-8"))
        })
        .transpose()?;
    if logged_issuer != accepted_identity.issuer {
        return Err(fail(
            "Rekor certificate issuer does not match the Sigstore-validated identity",
        ));
    }
    let logged_key = CosignVerificationKey::try_from(
        logged_certificate
            .tbs_certificate()
            .subject_public_key_info(),
    )
    .map_err(|_| fail("Rekor certificate has an unsupported public key"))?;
    fn key_material(key: &CosignVerificationKey) -> (&'static str, &[u8]) {
        match key {
            CosignVerificationKey::RSA_PSS_SHA256(bytes) => ("rsa-pss-sha256", bytes),
            CosignVerificationKey::RSA_PKCS1_SHA256(bytes) => ("rsa-pkcs1-sha256", bytes),
            CosignVerificationKey::ECDSA_P256_SHA256_ASN1(bytes) => ("ecdsa-p256-sha256", bytes),
            CosignVerificationKey::ECDSA_P384_SHA384_ASN1(bytes) => ("ecdsa-p384-sha384", bytes),
            CosignVerificationKey::ECDSA_P521_SHA512_ASN1(bytes) => ("ecdsa-p521-sha512", bytes),
            CosignVerificationKey::ED25519(bytes) => ("ed25519", bytes),
        }
    }
    if key_material(&logged_key) != key_material(&accepted_identity.verification_key) {
        return Err(fail(
            "Rekor certificate key does not match the Sigstore-validated certificate",
        ));
    }
    logged_key
        .verify_signature(
            Signature::Base64Encoded(signature.as_bytes()),
            &layer.raw_data,
        )
        .map_err(|_| fail("Rekor certificate key does not verify the signed layer"))?;

    Ok(())
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
    use_http_registry: bool,
) -> Result<Vec<u8>, FetchError> {
    use oci_client::Reference;
    use oci_client::client::{Client, ClientConfig, ClientProtocol};
    use oci_client::secrets::RegistryAuth;

    let mut client_config = ClientConfig::default();
    if use_http_registry {
        client_config.protocol = ClientProtocol::Http;
    }
    let client = Client::new(client_config);
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
// reconciler calls per reconcile of a `KarsSandbox`. It encapsulates the
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

/// Resolve the effective egress allowlist for a `KarsSandbox`.
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
    sandbox: &crate::crd::KarsSandbox,
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
pub async fn resolve_allowlist(sandbox: &crate::crd::KarsSandbox) -> AllowlistResolution {
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
    sandbox: &crate::crd::KarsSandbox,
) -> Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> {
    maybe_verify_allowlist_with_handle(sandbox, &crate::signer_policy::global()).await
}

/// Test-friendly variant: callers can inject a
/// [`crate::signer_policy::SharedSignerPolicy`] in any state without
/// spinning up the watcher. Production code uses
/// [`maybe_verify_allowlist`], which reads the process-global handle.
#[allow(dead_code)]
pub async fn maybe_verify_allowlist_with_handle(
    sandbox: &crate::crd::KarsSandbox,
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
            repository: "kars/policies/sandbox-foo".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            artifact_type: EGRESS_ALLOWLIST_V1_MEDIA_TYPE.to_string(),
        }
    }

    fn canonical_doc() -> String {
        // Two endpoints, sorted, deduped, lowercase, explicit ports.
        "apiVersion: kars.dev/v1alpha1\n\
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
            "https://github.com/Azure/kars/.github/workflows/*.yml@*,signer@example.com",
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
        let doc = "apiVersion: kars.dev/v1alpha1\n\
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
        let doc = "apiVersion: kars.dev/v1alpha1\n\
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
        let doc = "apiVersion: kars.dev/v1alpha1\n\
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
        let doc = "apiVersion: kars.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 0\n\
                   spec:\n  endpoints: []\n";
        let err = crate::policy_canonical::egress::parse(doc.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("> 0")));
    }

    #[test]
    fn canonical_parser_rejects_uppercase_host() {
        let doc = "apiVersion: kars.dev/v1alpha1\n\
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
        let doc = "apiVersion: kars.dev/v1alpha1\n\
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
        let doc = "apiVersion: kars.dev/v1alpha1\n\
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
        let doc = "apiVersion: kars.dev/v1alpha1\n\
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
        let doc = "apiVersion: kars.dev/v1alpha1\n\
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
                   apiVersion: kars.dev/v1alpha1\n\
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
                   apiVersion: kars.dev/v1alpha1\n\
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
    // case constructs a `KarsSandbox` carrying an `allowlistRef` + an
    // injected handle in a specific [`SignerPolicyState`], asserts on
    // the resulting `Condition.reason`. Network IO never executes
    // because we either short-circuit on policy state (Malformed) or
    // we use an obviously-fake registry that the cosign client will
    // refuse before any DNS lookup; the assertion only inspects the
    // mapped reason.

    use crate::crd::{KarsSandbox, KarsSandboxSpec, NetworkPolicyConfig};
    use crate::signer_policy::{
        SharedSignerPolicy, SignerPolicy as ParsedSignerPolicy, SignerPolicyState,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    fn sandbox_with_allowlist_ref() -> KarsSandbox {
        KarsSandbox {
            metadata: ObjectMeta {
                name: Some("demo".into()),
                namespace: Some("kars-demo".into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: KarsSandboxSpec {
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

    fn sandbox_with_inline_only(eps: Vec<crate::crd::EndpointConfig>) -> KarsSandbox {
        KarsSandbox {
            metadata: ObjectMeta {
                name: Some("inline".into()),
                namespace: Some("kars-inline".into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: KarsSandboxSpec {
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
    ) -> KarsSandbox {
        KarsSandbox {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(ns.into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: KarsSandboxSpec {
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
        let sb = KarsSandbox {
            metadata: ObjectMeta {
                name: Some("nope".into()),
                namespace: Some("kars-nope".into()),
                generation: Some(1),
                ..Default::default()
            },
            spec: KarsSandboxSpec {
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
        let sb = sandbox_with_ref_and_inline("noLkgSandbox", "kars-fc", vec![]);
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
            "kars-lkg",
            "lkgSandbox",
            LkgEntry {
                endpoints: Some(vec![ep("cached.example.com", Some(443))]),
                drift_clear_counter: 0,
                drift_active: false,
            },
        );
        let sb = sandbox_with_ref_and_inline("lkgSandbox", "kars-lkg", vec![]);
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
            "kars-restart",
            "rs",
            LkgEntry {
                endpoints: Some(vec![ep("a.example", None)]),
                drift_clear_counter: 0,
                drift_active: false,
            },
        );
        lkg_clear();
        let sb = sandbox_with_ref_and_inline("rs", "kars-restart", vec![]);
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
            "kars-evil",
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
        let sb =
            sandbox_with_ref_and_inline("withInline", "kars-wi", vec![ep("inline.example", None)]);
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
        let sb = sandbox_with_ref_and_inline("two", "kars-two", vec![]);
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
        let sb = sandbox_with_ref_and_inline("malformed", "kars-mal", vec![]);
        let h = SharedSignerPolicy::from_state(SignerPolicyState::Malformed("bad yaml".into()));
        let res = resolve_allowlist_with_handle(&sb, &h).await;
        assert!(res.fail_closed_no_lkg);
        let v = cond_by_type(&res.conditions, "AllowlistVerified").expect("Verified emitted");
        assert_eq!(v.reason, "SignerPolicyMalformed");
    }

    fn signed_test_layer(subject: &str, issuer: &str) -> sigstore::cosign::SignatureLayer {
        use sigstore::cosign::SignatureLayer;
        use sigstore::cosign::constraint::{Constraint, PrivateKeySigner};
        use sigstore::cosign::signature_layers::{CertificateSignature, CertificateSubject};
        use sigstore::crypto::{CosignVerificationKey, SigningScheme};

        let signer = SigningScheme::ECDSA_P256_SHA256_ASN1
            .create_signer()
            .expect("create test signer");
        let public_key = signer
            .to_sigstore_keypair()
            .expect("derive test keypair")
            .public_key_to_pem()
            .expect("encode test public key");
        let verification_key = CosignVerificationKey::try_from_pem(public_key.as_bytes())
            .expect("parse test verification key");
        let image = "localhost/policies:fixture"
            .parse()
            .expect("parse image ref");
        let mut layer = SignatureLayer::new_unsigned(&image, &format!("sha256:{}", "a".repeat(64)))
            .expect("create unsigned signature layer");
        assert!(
            PrivateKeySigner::new_with_signer(signer)
                .add_constraint(&mut layer)
                .expect("sign test layer")
        );
        layer.certificate_signature = Some(CertificateSignature {
            verification_key,
            subject: CertificateSubject::Uri(subject.into()),
            issuer: Some(issuer.into()),
            github_workflow_trigger: None,
            github_workflow_sha: None,
            github_workflow_name: None,
            github_workflow_repository: None,
            github_workflow_ref: None,
        });
        layer
    }

    fn test_signer_policy(issuer: &str, subject: &str) -> SignerPolicyConfig {
        SignerPolicyConfig {
            fulcio_issuers: vec![issuer.into()],
            san_patterns: vec![subject.into()],
            ed25519_keys: Vec::new(),
        }
    }

    #[test]
    fn signer_identity_requires_matching_identity_and_valid_signature() {
        let issuer = "https://issuer.example";
        let subject = "https://example.test/workflow";
        let layer = signed_test_layer(subject, issuer);

        verify_signer_identity(
            std::slice::from_ref(&layer),
            &test_signer_policy(issuer, subject),
        )
        .expect("valid signed identity");

        assert!(
            verify_signer_identity(
                std::slice::from_ref(&layer),
                &test_signer_policy("https://wrong-issuer.example", subject),
            )
            .is_err(),
            "wrong issuer must fail"
        );
        assert!(
            verify_signer_identity(
                std::slice::from_ref(&layer),
                &test_signer_policy(issuer, "https://wrong-subject.example"),
            )
            .is_err(),
            "wrong subject must fail"
        );

        let mut tampered_signature = layer.clone();
        tampered_signature.signature = Some("AAAA".into());
        assert!(
            verify_signer_identity(&[tampered_signature], &test_signer_policy(issuer, subject))
                .is_err(),
            "tampered signature must fail"
        );

        let mut tampered_payload = layer;
        tampered_payload.raw_data.push(b' ');
        assert!(
            verify_signer_identity(&[tampered_payload], &test_signer_policy(issuer, subject))
                .is_err(),
            "tampered signed payload must fail"
        );
    }

    const TEST_SIGNER_CERT: &[u8] = include_bytes!("../tests/fixtures/sigstore/signer.pem");
    const TEST_SIGNER_KEY: &[u8] = include_bytes!("../tests/fixtures/sigstore/signer-key.pem");
    const TEST_FULCIO_ROOT: &[u8] = include_bytes!("../tests/fixtures/sigstore/fulcio-root.pem");
    const TEST_REKOR_KEY: &[u8] = include_bytes!("../tests/fixtures/sigstore/rekor-key.pem");
    const TEST_REKOR_PUBLIC_KEY: &[u8] =
        include_bytes!("../tests/fixtures/sigstore/rekor-public.der");
    const TEST_ISSUER: &str = "https://issuer.example";
    const TEST_SUBJECT: &str = "https://example.test/workflow";
    const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
    const OCI_EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
    const COSIGN_LAYER_MEDIA_TYPE: &str = "application/vnd.dev.cosign.simplesigning.v1+json";
    const SIGSTORE_BUNDLE_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FixtureMutation {
        None,
        Signature,
        Artifact,
        RekorSignature,
        RekorCertificate,
        RekorPayloadHash,
        RekorKind,
        RekorSha384,
        RekorSha512,
        RekorUppercaseHash,
        RekorHashedrekordWithoutFormat,
        RekorWrongFormat,
        RekorCertificateSnapshot,
    }

    struct LocalRegistryFixture {
        manifests: std::collections::HashMap<String, (String, Vec<u8>)>,
        manifest_snapshots: std::collections::HashMap<String, Vec<(String, Vec<u8>)>>,
        manifest_reads: std::sync::Mutex<std::collections::HashMap<String, usize>>,
        blobs: std::collections::HashMap<String, Vec<u8>>,
        referrers: std::collections::HashMap<String, Vec<u8>>,
    }

    struct LocalRegistry {
        address: String,
        task: tokio::task::JoinHandle<()>,
        state: std::sync::Arc<LocalRegistryFixture>,
    }

    impl LocalRegistry {
        async fn start(fixture: LocalRegistryFixture) -> Self {
            use axum::routing::any;
            use std::sync::Arc;
            use tokio::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local OCI registry");
            let address = listener.local_addr().expect("local OCI registry address");
            let state = Arc::new(fixture);
            let app = axum::Router::new()
                .route("/{*path}", any(local_registry_request))
                .with_state(state.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve local OCI registry");
            });
            Self {
                address: address.to_string(),
                task,
                state,
            }
        }

        fn manifest_reads(&self, reference: &str) -> usize {
            self.state
                .manifest_reads
                .lock()
                .expect("manifest read lock")
                .get(reference)
                .copied()
                .unwrap_or_default()
        }
    }

    impl Drop for LocalRegistry {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn local_registry_request(
        axum::extract::State(fixture): axum::extract::State<std::sync::Arc<LocalRegistryFixture>>,
        method: axum::http::Method,
        uri: axum::http::Uri,
    ) -> axum::response::Response {
        use axum::body::Body;
        use axum::http::{HeaderName, HeaderValue, Response, StatusCode};

        let path = uri.path().trim_start_matches('/');
        if path == "v2/" {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::empty())
                .expect("build OCI version response");
        }
        if path.starts_with("v2/policy/referrers/") {
            let digest = path.trim_start_matches("v2/policy/referrers/");
            let body = fixture.referrers.get(digest).cloned().unwrap_or_else(|| {
                br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#
                    .to_vec()
            });
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/vnd.oci.image.index.v1+json")
                .body(Body::from(body))
                .expect("build OCI referrers response");
        }
        if let Some(reference) = path.strip_prefix("v2/policy/manifests/") {
            let snapshot = fixture
                .manifest_snapshots
                .get(reference)
                .and_then(|snapshots| {
                    if method != axum::http::Method::GET {
                        return snapshots.first();
                    }
                    let mut reads = fixture.manifest_reads.lock().expect("manifest read lock");
                    let index = reads.entry(reference.to_owned()).or_default();
                    let snapshot = snapshots.get((*index).min(snapshots.len().saturating_sub(1)));
                    *index = index.saturating_add(1);
                    snapshot
                });
            let manifest = snapshot.or_else(|| fixture.manifests.get(reference));
            if let Some((digest, bytes)) = manifest {
                let builder = Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", OCI_MANIFEST_MEDIA_TYPE)
                    .header(
                        HeaderName::from_static("docker-content-digest"),
                        HeaderValue::from_str(digest).expect("valid manifest digest"),
                    );
                let body = if method == axum::http::Method::HEAD {
                    Body::empty()
                } else {
                    Body::from(bytes.clone())
                };
                return builder.body(body).expect("build OCI manifest response");
            }
        }
        if let Some(digest) = path.strip_prefix("v2/policy/blobs/")
            && let Some(bytes) = fixture.blobs.get(digest)
        {
            let body = if method == axum::http::Method::HEAD {
                Body::empty()
            } else {
                Body::from(bytes.clone())
            };
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/octet-stream")
                .body(body)
                .expect("build OCI blob response");
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("build OCI not-found response")
    }

    fn pem_contents(pem_bytes: &[u8]) -> Vec<u8> {
        use base64::Engine as _;

        let pem = std::str::from_utf8(pem_bytes).expect("PEM fixture is UTF-8");
        let encoded = pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect::<String>();
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("decode PEM fixture")
    }

    fn test_trust_root() -> sigstore::trust::ManualTrustRoot<'static> {
        let rekor_key_id = sha256_hex(TEST_REKOR_PUBLIC_KEY);
        sigstore::trust::ManualTrustRoot {
            fulcio_certs: vec![pem_contents(TEST_FULCIO_ROOT).into()],
            rekor_keys: std::collections::BTreeMap::from([(
                rekor_key_id,
                TEST_REKOR_PUBLIC_KEY.to_vec(),
            )]),
            ctfe_keys: std::collections::BTreeMap::new(),
        }
    }

    fn build_local_registry_fixture(
        policy_bytes: &[u8],
        mutation: FixtureMutation,
    ) -> (LocalRegistryFixture, String) {
        use base64::Engine as _;
        use serde_json::json;
        use sha2::{Digest, Sha256};
        use sigstore::cosign::constraint::{Constraint, PrivateKeySigner};
        use sigstore::cosign::signature_layers::SignatureLayer;
        use sigstore::crypto::SigningScheme;
        use sigstore::crypto::signing_key::SigStoreKeyPair;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn digest(bytes: &[u8]) -> String {
            format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
        }

        fn descriptor(bytes: &[u8], media_type: &str) -> serde_json::Value {
            json!({
                "mediaType": media_type,
                "digest": digest(bytes),
                "size": bytes.len(),
            })
        }

        let empty_config = [];
        let policy_manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "artifactType": EGRESS_ALLOWLIST_V1_MEDIA_TYPE,
            "config": descriptor(&empty_config, OCI_EMPTY_CONFIG_MEDIA_TYPE),
            "layers": [descriptor(policy_bytes, EGRESS_ALLOWLIST_V1_MEDIA_TYPE)],
        }))
        .expect("serialize policy manifest");
        let artifact_digest = digest(&policy_manifest);
        let image = "localhost/policy:fixture"
            .parse()
            .expect("parse fixture image reference");
        let mut signature_layer =
            SignatureLayer::new_unsigned(&image, &artifact_digest).expect("create signed payload");
        let signer_key = SigStoreKeyPair::from_pem(TEST_SIGNER_KEY).expect("parse test signer key");
        let signing_scheme = SigningScheme::ECDSA_P256_SHA256_ASN1;
        assert!(
            PrivateKeySigner::new_with_signer(
                signer_key
                    .to_sigstore_signer(&signing_scheme)
                    .expect("build test artifact signer"),
            )
            .add_constraint(&mut signature_layer)
            .expect("sign artifact manifest digest")
        );
        let valid_signature = signature_layer
            .signature
            .clone()
            .expect("signature generated");
        let simple_signing_bytes = serde_json::to_vec(&signature_layer.simple_signing)
            .expect("serialize cosign simple-signing payload");

        let rekor_key = SigStoreKeyPair::from_pem(TEST_REKOR_KEY).expect("parse test Rekor key");
        let rekor_signer = rekor_key
            .to_sigstore_signer(&signing_scheme)
            .expect("build test Rekor signer");
        let rekor_key_id = sha256_hex(TEST_REKOR_PUBLIC_KEY);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_secs() as i64;
        let rekor_signature = if mutation == FixtureMutation::RekorSignature {
            base64::engine::general_purpose::STANDARD.encode(b"mismatched signature")
        } else {
            valid_signature.clone()
        };
        let rekor_certificate = if matches!(
            mutation,
            FixtureMutation::RekorCertificate | FixtureMutation::RekorCertificateSnapshot
        ) {
            TEST_FULCIO_ROOT
        } else {
            TEST_SIGNER_CERT
        };
        let hash_algorithm = match mutation {
            FixtureMutation::RekorSha384 => "sha384",
            FixtureMutation::RekorSha512 => "sha512",
            _ => "sha256",
        };
        let rekor_payload_hash = if mutation == FixtureMutation::RekorPayloadHash {
            "0".repeat(64)
        } else {
            let hash = match hash_algorithm {
                "sha384" => hex::encode(sha2::Sha384::digest(&simple_signing_bytes)),
                "sha512" => hex::encode(sha2::Sha512::digest(&simple_signing_bytes)),
                _ => hex::encode(Sha256::digest(&simple_signing_bytes)),
            };
            if mutation == FixtureMutation::RekorUppercaseHash {
                hash.to_ascii_uppercase()
            } else {
                hash
            }
        };
        let rekor_kind = if matches!(
            mutation,
            FixtureMutation::RekorKind | FixtureMutation::RekorWrongFormat
        ) {
            "rekord"
        } else {
            "hashedrekord"
        };
        let mut rekor_signature_entry = json!({
            "content": rekor_signature,
            "publicKey": {
                "content": base64::engine::general_purpose::STANDARD.encode(rekor_certificate),
            }
        });
        if rekor_kind == "rekord" {
            rekor_signature_entry["format"] =
                json!(if mutation == FixtureMutation::RekorWrongFormat {
                    "pgp"
                } else {
                    "x509"
                });
        }
        let rekor_body = json!({
            "apiVersion": "0.0.1",
            "kind": rekor_kind,
            "spec": {
                "data": {
                    "hash": {
                        "algorithm": hash_algorithm,
                        "value": rekor_payload_hash,
                    }
                },
                "signature": rekor_signature_entry
            }
        });
        let rekor_body =
            serde_json_canonicalizer::to_vec(&rekor_body).expect("canonicalize Rekor entry");
        let payload = json!({
            "body": base64::engine::general_purpose::STANDARD.encode(&rekor_body),
            "integratedTime": timestamp,
            "logID": rekor_key_id,
            "logIndex": 1,
        });
        let set_payload = serde_json_canonicalizer::to_vec(&payload).expect("canonicalize SET");
        let set_signature = rekor_signer.sign(&set_payload).expect("sign Rekor SET");
        let rekor_bundle = serde_json::to_string(&json!({
            "SignedEntryTimestamp": base64::engine::general_purpose::STANDARD.encode(set_signature),
            "Payload": payload,
        }))
        .expect("serialize test Rekor bundle");

        let signature_annotations = json!({
            "dev.cosignproject.cosign/signature": if mutation == FixtureMutation::Signature {
                "AAAA"
            } else {
                valid_signature.as_str()
            },
            "dev.sigstore.cosign/certificate": std::str::from_utf8(TEST_SIGNER_CERT)
                .expect("test signer cert is UTF-8"),
            "dev.sigstore.cosign/bundle": rekor_bundle,
        });
        let signature_manifest_value = json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "config": descriptor(&empty_config, OCI_EMPTY_CONFIG_MEDIA_TYPE),
            "layers": [{
                "mediaType": COSIGN_LAYER_MEDIA_TYPE,
                "digest": digest(&simple_signing_bytes),
                "size": simple_signing_bytes.len(),
                "annotations": signature_annotations,
            }],
        });
        let signature_manifest = serde_json::to_vec(&signature_manifest_value)
            .expect("serialize cosign signature manifest");
        let signature_manifest_digest = digest(&signature_manifest);
        let signature_tag = artifact_digest.replace(':', "-") + ".sig";
        let mut manifest_snapshots = std::collections::HashMap::new();
        if mutation == FixtureMutation::RekorCertificateSnapshot {
            let mut changed_snapshot = signature_manifest_value.clone();
            changed_snapshot["layers"][0]["annotations"]["dev.sigstore.cosign/certificate"] =
                serde_json::Value::String(
                    std::str::from_utf8(TEST_FULCIO_ROOT)
                        .expect("test Fulcio root is UTF-8")
                        .to_owned(),
                );
            let changed_snapshot = serde_json::to_vec(&changed_snapshot)
                .expect("serialize changed signature tag snapshot");
            manifest_snapshots.insert(
                signature_tag.clone(),
                vec![
                    (
                        signature_manifest_digest.clone(),
                        signature_manifest.clone(),
                    ),
                    (
                        signature_manifest_digest.clone(),
                        signature_manifest.clone(),
                    ),
                    (digest(&changed_snapshot), changed_snapshot),
                ],
            );
        }

        let mut blobs = std::collections::HashMap::from([
            (digest(&empty_config), empty_config.to_vec()),
            (digest(policy_bytes), policy_bytes.to_vec()),
            (digest(&simple_signing_bytes), simple_signing_bytes),
        ]);
        if mutation == FixtureMutation::Artifact {
            blobs.insert(
                digest(policy_bytes),
                b"tampered signed policy bytes".to_vec(),
            );
        }
        let manifests = std::collections::HashMap::from([
            (
                artifact_digest.clone(),
                (artifact_digest.clone(), policy_manifest),
            ),
            (
                signature_tag,
                (signature_manifest_digest, signature_manifest),
            ),
        ]);
        (
            LocalRegistryFixture {
                manifests,
                manifest_snapshots,
                manifest_reads: std::sync::Mutex::new(std::collections::HashMap::new()),
                blobs,
                referrers: std::collections::HashMap::new(),
            },
            artifact_digest,
        )
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum BundleFixtureMutation {
        None,
        WrongSubject,
        BadSignature,
        Artifact,
    }

    #[derive(Clone, Copy)]
    enum AdditionalLegacyCandidate {
        MismatchedPayloadHash,
        MalformedCertificate,
    }

    fn build_local_registry_mixed_legacy_fixture(
        policy_bytes: &[u8],
        additional: AdditionalLegacyCandidate,
    ) -> (LocalRegistryFixture, String) {
        use serde_json::json;
        use sha2::{Digest, Sha256};

        fn digest(bytes: &[u8]) -> String {
            format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
        }

        let (mut valid_fixture, artifact_digest) =
            build_local_registry_fixture(policy_bytes, FixtureMutation::None);
        let additional_mutation = match additional {
            AdditionalLegacyCandidate::MismatchedPayloadHash => FixtureMutation::RekorPayloadHash,
            AdditionalLegacyCandidate::MalformedCertificate => FixtureMutation::None,
        };
        let (additional_fixture, additional_digest) =
            build_local_registry_fixture(policy_bytes, additional_mutation);
        assert_eq!(artifact_digest, additional_digest);
        let signature_tag = artifact_digest.replace(':', "-") + ".sig";
        let valid_manifest = valid_fixture
            .manifests
            .get(&signature_tag)
            .expect("valid legacy signature manifest")
            .1
            .clone();
        let additional_manifest = additional_fixture
            .manifests
            .get(&signature_tag)
            .expect("additional legacy signature manifest")
            .1
            .clone();
        let mut valid_manifest: serde_json::Value =
            serde_json::from_slice(&valid_manifest).expect("parse valid signature manifest");
        let additional_manifest: serde_json::Value = serde_json::from_slice(&additional_manifest)
            .expect("parse additional signature manifest");
        let mut additional_layer = additional_manifest["layers"][0].clone();
        if matches!(additional, AdditionalLegacyCandidate::MalformedCertificate) {
            additional_layer["annotations"]["dev.sigstore.cosign/certificate"] =
                json!("not a PEM certificate");
        }
        valid_manifest["layers"]
            .as_array_mut()
            .expect("signature manifest layers")
            .push(additional_layer);
        let manifest_bytes =
            serde_json::to_vec(&valid_manifest).expect("serialize multi-signature manifest");
        let manifest_digest = digest(&manifest_bytes);
        valid_fixture
            .manifests
            .insert(signature_tag, (manifest_digest, manifest_bytes));
        valid_fixture.blobs.extend(additional_fixture.blobs);
        (valid_fixture, artifact_digest)
    }

    fn build_local_registry_referrer_with_bad_legacy_fixture(
        policy_bytes: &[u8],
    ) -> (LocalRegistryFixture, String) {
        let (mut referrer_fixture, artifact_digest) =
            build_local_registry_bundle_fixture(policy_bytes, BundleFixtureMutation::None);
        let (legacy_fixture, legacy_digest) =
            build_local_registry_fixture(policy_bytes, FixtureMutation::RekorCertificateSnapshot);
        assert_eq!(artifact_digest, legacy_digest);
        let signature_tag = artifact_digest.replace(':', "-") + ".sig";
        let legacy_manifest = legacy_fixture
            .manifests
            .get(&signature_tag)
            .expect("invalid legacy signature manifest")
            .clone();
        referrer_fixture
            .manifests
            .insert(signature_tag, legacy_manifest);
        referrer_fixture
            .manifest_snapshots
            .extend(legacy_fixture.manifest_snapshots);
        referrer_fixture.blobs.extend(legacy_fixture.blobs);
        (referrer_fixture, artifact_digest)
    }

    fn build_local_registry_bundle_fixture(
        policy_bytes: &[u8],
        mutation: BundleFixtureMutation,
    ) -> (LocalRegistryFixture, String) {
        use base64::{Engine as _, engine::general_purpose::STANDARD as base64};
        use serde_json::json;
        use sha2::{Digest, Sha256};
        use sigstore::crypto::SigningScheme;
        use sigstore::crypto::signing_key::SigStoreKeyPair;
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{
            Bundle as ProtoBundle, VerificationMaterial, bundle::Content as BundleContent,
            verification_material::Content as MaterialContent,
        };
        use sigstore_protobuf_specs::dev::sigstore::common::v1::{LogId, X509Certificate};
        use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{
            Checkpoint, InclusionPromise, InclusionProof, KindVersion, TransparencyLogEntry,
        };
        use sigstore_protobuf_specs::io::intoto::{Envelope, Signature};
        use std::time::{SystemTime, UNIX_EPOCH};

        fn digest(bytes: &[u8]) -> String {
            format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
        }

        fn descriptor(bytes: &[u8], media_type: &str) -> serde_json::Value {
            json!({
                "mediaType": media_type,
                "digest": digest(bytes),
                "size": bytes.len(),
            })
        }

        fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
            let header = format!(
                "DSSEv1 {} {} {} ",
                payload_type.len(),
                payload_type,
                payload.len()
            );
            [header.as_bytes(), payload].concat()
        }

        let empty_config = [];
        let policy_manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "artifactType": EGRESS_ALLOWLIST_V1_MEDIA_TYPE,
            "config": descriptor(&empty_config, OCI_EMPTY_CONFIG_MEDIA_TYPE),
            "layers": [descriptor(policy_bytes, EGRESS_ALLOWLIST_V1_MEDIA_TYPE)],
        }))
        .expect("serialize policy manifest");
        let artifact_digest = digest(&policy_manifest);
        let subject_digest = artifact_digest.trim_start_matches("sha256:");
        let statement_digest = if mutation == BundleFixtureMutation::WrongSubject {
            "0".repeat(64)
        } else {
            subject_digest.to_owned()
        };
        let statement = serde_json::to_vec(&json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{
                "name": "policy",
                "digest": {"sha256": statement_digest},
            }],
            "predicateType": "https://sigstore.dev/cosign/sign/v1",
        }))
        .expect("serialize in-toto statement");
        let payload_type = "application/vnd.in-toto+json";
        let pae = pae(payload_type, &statement);

        let signing_scheme = SigningScheme::ECDSA_P256_SHA256_ASN1;
        let signer_key = SigStoreKeyPair::from_pem(TEST_SIGNER_KEY).expect("parse test signer key");
        let signer = signer_key
            .to_sigstore_signer(&signing_scheme)
            .expect("build test artifact signer");
        let mut signature = signer.sign(&pae).expect("sign DSSE PAE");
        if mutation == BundleFixtureMutation::BadSignature {
            let last = signature.last_mut().expect("ECDSA signature is nonempty");
            *last ^= 1;
        }
        let envelope = Envelope {
            payload: statement.clone(),
            payload_type: payload_type.to_owned(),
            signatures: vec![Signature {
                sig: signature.clone(),
                keyid: String::new(),
            }],
        };
        let envelope_json = serde_json::to_vec(&envelope).expect("serialize DSSE envelope");

        let certificate_der = pem_contents(TEST_SIGNER_CERT);
        let rekor_key = SigStoreKeyPair::from_pem(TEST_REKOR_KEY).expect("parse test Rekor key");
        let rekor_signer = rekor_key
            .to_sigstore_signer(&signing_scheme)
            .expect("build test Rekor signer");
        let rekor_key_id = sha256_hex(TEST_REKOR_PUBLIC_KEY);
        let rekor_key_id_bytes = Sha256::digest(TEST_REKOR_PUBLIC_KEY).to_vec();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_secs() as i64;
        let body = json!({
            "apiVersion": "0.0.1",
            "kind": "dsse",
            "spec": {
                "envelopeHash": {
                    "algorithm": "sha256",
                    "value": hex::encode(Sha256::digest(&envelope_json)),
                },
                "payloadHash": {
                    "algorithm": "sha256",
                    "value": hex::encode(Sha256::digest(&statement)),
                },
                "signatures": [{
                    "signature": base64.encode(&signature),
                    "verifier": base64.encode(TEST_SIGNER_CERT),
                }],
            },
        });
        let canonicalized_body =
            serde_json_canonicalizer::to_vec(&body).expect("canonicalize DSSE Rekor entry");
        let set_payload = json!({
            "body": base64.encode(&canonicalized_body),
            "integratedTime": timestamp,
            "logID": rekor_key_id,
            "logIndex": 0,
        });
        let set_payload =
            serde_json_canonicalizer::to_vec(&set_payload).expect("canonicalize DSSE SET payload");
        let signed_entry_timestamp = rekor_signer
            .sign(&set_payload)
            .expect("sign DSSE Rekor SET");

        let leaf_hash = Sha256::new()
            .chain_update([0])
            .chain_update(&canonicalized_body)
            .finalize()
            .to_vec();
        let root_hash = base64.encode(&leaf_hash);
        let checkpoint_note = format!("test-rekor\n1\n{root_hash}\n");
        let checkpoint_signature = rekor_signer
            .sign(checkpoint_note.as_bytes())
            .expect("sign Rekor checkpoint");
        let key_fingerprint = &Sha256::digest(TEST_REKOR_PUBLIC_KEY)[..4];
        let checkpoint_signature = [key_fingerprint, checkpoint_signature.as_slice()].concat();
        let checkpoint = format!(
            "{checkpoint_note}\n— test-rekor {}",
            base64.encode(checkpoint_signature)
        );

        let tlog_entry = TransparencyLogEntry {
            log_index: 0,
            log_id: Some(LogId {
                key_id: rekor_key_id_bytes,
            }),
            kind_version: Some(KindVersion {
                kind: "dsse".into(),
                version: "0.0.1".into(),
            }),
            integrated_time: timestamp,
            inclusion_promise: Some(InclusionPromise {
                signed_entry_timestamp,
            }),
            inclusion_proof: Some(InclusionProof {
                log_index: 0,
                root_hash: leaf_hash,
                tree_size: 1,
                hashes: Vec::new(),
                checkpoint: Some(Checkpoint {
                    envelope: checkpoint,
                }),
            }),
            canonicalized_body,
        };
        let bundle = ProtoBundle {
            media_type: SIGSTORE_BUNDLE_MEDIA_TYPE.into(),
            verification_material: Some(VerificationMaterial {
                tlog_entries: vec![tlog_entry],
                timestamp_verification_data: None,
                content: Some(MaterialContent::Certificate(X509Certificate {
                    raw_bytes: certificate_der,
                })),
            }),
            content: Some(BundleContent::DsseEnvelope(envelope)),
        };
        let bundle_bytes = serde_json::to_vec(&bundle).expect("serialize Sigstore bundle");
        let bundle_digest = digest(&bundle_bytes);
        let referrer_manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "artifactType": SIGSTORE_BUNDLE_MEDIA_TYPE,
            "config": descriptor(&empty_config, OCI_EMPTY_CONFIG_MEDIA_TYPE),
            "layers": [descriptor(&bundle_bytes, SIGSTORE_BUNDLE_MEDIA_TYPE)],
            "subject": descriptor(&policy_manifest, OCI_MANIFEST_MEDIA_TYPE),
        }))
        .expect("serialize Sigstore referrer manifest");
        let referrer_digest = digest(&referrer_manifest);
        let referrers_index = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": OCI_MANIFEST_MEDIA_TYPE,
                "digest": referrer_digest,
                "size": referrer_manifest.len(),
                "artifactType": SIGSTORE_BUNDLE_MEDIA_TYPE,
            }],
        }))
        .expect("serialize OCI referrers index");

        let mut blobs = std::collections::HashMap::from([
            (digest(&empty_config), empty_config.to_vec()),
            (digest(policy_bytes), policy_bytes.to_vec()),
            (bundle_digest, bundle_bytes),
        ]);
        if mutation == BundleFixtureMutation::Artifact {
            blobs.insert(
                digest(policy_bytes),
                b"tampered signed policy bytes".to_vec(),
            );
        }
        let manifests = std::collections::HashMap::from([
            (
                artifact_digest.clone(),
                (artifact_digest.clone(), policy_manifest),
            ),
            (
                referrer_digest.clone(),
                (referrer_digest.clone(), referrer_manifest),
            ),
        ]);
        let referrers =
            std::collections::HashMap::from([(artifact_digest.clone(), referrers_index)]);
        (
            LocalRegistryFixture {
                manifests,
                manifest_snapshots: std::collections::HashMap::new(),
                manifest_reads: std::sync::Mutex::new(std::collections::HashMap::new()),
                blobs,
                referrers,
            },
            artifact_digest,
        )
    }

    async fn verify_local_signed_policy(
        policy_bytes: &[u8],
        mutation: FixtureMutation,
        signer_policy: &SignerPolicyConfig,
    ) -> Result<VerifiedAllowlist, FetchError> {
        let (fixture, digest) = build_local_registry_fixture(policy_bytes, mutation);
        verify_local_fixture(fixture, digest, signer_policy).await
    }

    async fn verify_local_fixture(
        fixture: LocalRegistryFixture,
        digest: String,
        signer_policy: &SignerPolicyConfig,
    ) -> Result<VerifiedAllowlist, FetchError> {
        let registry = LocalRegistry::start(fixture).await;
        verify_local_fixture_at_registry(&registry, digest, signer_policy).await
    }

    async fn verify_local_fixture_at_registry(
        registry: &LocalRegistry,
        digest: String,
        signer_policy: &SignerPolicyConfig,
    ) -> Result<VerifiedAllowlist, FetchError> {
        let artifact_ref = OciArtifactRef {
            registry: registry.address.clone(),
            repository: "policy".into(),
            digest,
            artifact_type: EGRESS_ALLOWLIST_V1_MEDIA_TYPE.into(),
        };
        verify_via_sigstore_with_root::<EgressKind, _>(
            &artifact_ref,
            signer_policy,
            &test_trust_root(),
            true,
        )
        .await
    }

    async fn verify_local_bundle_signed_policy(
        policy_bytes: &[u8],
        mutation: BundleFixtureMutation,
        signer_policy: &SignerPolicyConfig,
    ) -> Result<VerifiedAllowlist, FetchError> {
        let (fixture, digest) = build_local_registry_bundle_fixture(policy_bytes, mutation);
        verify_local_fixture(fixture, digest, signer_policy).await
    }

    #[tokio::test]
    async fn signed_oci_policy_verification_checks_signature_identity_and_canonical_bytes() {
        let matching_policy = test_signer_policy(TEST_ISSUER, TEST_SUBJECT);
        let verified = verify_local_signed_policy(
            canonical_doc().as_bytes(),
            FixtureMutation::None,
            &matching_policy,
        )
        .await
        .expect("real signed OCI policy should verify");
        assert_eq!(verified.endpoints.len(), 2);

        assert!(
            verify_local_signed_policy(
                canonical_doc().as_bytes(),
                FixtureMutation::None,
                &test_signer_policy("https://wrong-issuer.example", TEST_SUBJECT),
            )
            .await
            .is_err(),
            "wrong Fulcio issuer must fail"
        );
        assert!(
            verify_local_signed_policy(
                canonical_doc().as_bytes(),
                FixtureMutation::None,
                &test_signer_policy(TEST_ISSUER, "https://wrong.example/workflow"),
            )
            .await
            .is_err(),
            "wrong certificate SAN must fail"
        );
        assert!(
            verify_local_signed_policy(
                canonical_doc().as_bytes(),
                FixtureMutation::Signature,
                &matching_policy,
            )
            .await
            .is_err(),
            "tampered signature must fail"
        );
        assert!(
            verify_local_signed_policy(
                canonical_doc().as_bytes(),
                FixtureMutation::Artifact,
                &matching_policy,
            )
            .await
            .is_err(),
            "tampered artifact bytes must fail"
        );
        for (mutation, description) in [
            (
                FixtureMutation::RekorSignature,
                "mismatched logged signature",
            ),
            (
                FixtureMutation::RekorCertificate,
                "mismatched logged certificate",
            ),
            (
                FixtureMutation::RekorPayloadHash,
                "mismatched logged payload hash",
            ),
        ] {
            assert!(
                matches!(
                    verify_local_signed_policy(
                        canonical_doc().as_bytes(),
                        mutation,
                        &matching_policy,
                    )
                    .await,
                    Err(FetchError::SignatureVerifyFailed(_))
                ),
                "{description} in a validly signed SET must fail as a signature error"
            );
        }

        let noncanonical = format!("# signed but noncanonical\n{}", canonical_doc());
        assert!(
            matches!(
                verify_local_signed_policy(
                    noncanonical.as_bytes(),
                    FixtureMutation::None,
                    &matching_policy,
                )
                .await,
                Err(FetchError::CanonicalFormViolation(_))
            ),
            "signed noncanonical policy bytes must be rejected"
        );

        for mutation in [
            FixtureMutation::RekorKind,
            FixtureMutation::RekorSha384,
            FixtureMutation::RekorSha512,
            FixtureMutation::RekorUppercaseHash,
            FixtureMutation::RekorHashedrekordWithoutFormat,
        ] {
            verify_local_signed_policy(canonical_doc().as_bytes(), mutation, &matching_policy)
                .await
                .expect("supported SET-signed Rekor format should verify");
        }
        assert!(
            verify_local_signed_policy(
                canonical_doc().as_bytes(),
                FixtureMutation::RekorWrongFormat,
                &matching_policy,
            )
            .await
            .is_err(),
            "non-x509 Rekor signature formats must fail"
        );
        let (snapshot_fixture, digest) = build_local_registry_fixture(
            canonical_doc().as_bytes(),
            FixtureMutation::RekorCertificateSnapshot,
        );
        let signature_tag = digest.replace(':', "-") + ".sig";
        let registry = LocalRegistry::start(snapshot_fixture).await;
        assert!(
            verify_local_fixture_at_registry(&registry, digest, &matching_policy)
                .await
                .is_err(),
            "a later mutable .sig tag snapshot must not substitute a different certificate"
        );
        assert_eq!(
            registry.manifest_reads(&signature_tag),
            2,
            "Sigstore reads its signature manifest twice; Kars must not make a third mutable-tag read"
        );
    }

    #[tokio::test]
    async fn signed_oci_verification_keeps_any_valid_candidate_semantics() {
        let matching_policy = test_signer_policy(TEST_ISSUER, TEST_SUBJECT);
        for additional in [
            AdditionalLegacyCandidate::MismatchedPayloadHash,
            AdditionalLegacyCandidate::MalformedCertificate,
        ] {
            let (fixture, digest) =
                build_local_registry_mixed_legacy_fixture(canonical_doc().as_bytes(), additional);
            verify_local_fixture(fixture, digest, &matching_policy)
                .await
                .expect("an unrelated invalid legacy signature must not reject a valid signature");
        }

        let (fixture, digest) =
            build_local_registry_referrer_with_bad_legacy_fixture(canonical_doc().as_bytes());
        let signature_tag = digest.replace(':', "-") + ".sig";
        let registry = LocalRegistry::start(fixture).await;
        verify_local_fixture_at_registry(&registry, digest, &matching_policy)
            .await
            .expect("a valid OCI referrer must survive an invalid legacy candidate");
        assert_eq!(
            registry.manifest_reads(&signature_tag),
            2,
            "mixed verification must not refetch the changed legacy tag"
        );
    }

    #[tokio::test]
    async fn signed_oci_referrer_bundle_verification_checks_signature_identity_and_subject() {
        let matching_policy = test_signer_policy(TEST_ISSUER, TEST_SUBJECT);
        let verified = verify_local_bundle_signed_policy(
            canonical_doc().as_bytes(),
            BundleFixtureMutation::None,
            &matching_policy,
        )
        .await
        .expect("cryptographically signed OCI referrer bundle should verify");
        assert_eq!(verified.endpoints.len(), 2);

        assert!(
            verify_local_bundle_signed_policy(
                canonical_doc().as_bytes(),
                BundleFixtureMutation::None,
                &test_signer_policy("https://wrong-issuer.example", TEST_SUBJECT),
            )
            .await
            .is_err(),
            "wrong Fulcio issuer must fail for OCI referrer bundles"
        );
        assert!(
            verify_local_bundle_signed_policy(
                canonical_doc().as_bytes(),
                BundleFixtureMutation::None,
                &test_signer_policy(TEST_ISSUER, "https://wrong.example/workflow"),
            )
            .await
            .is_err(),
            "wrong certificate SAN must fail for OCI referrer bundles"
        );
        assert!(
            verify_local_bundle_signed_policy(
                canonical_doc().as_bytes(),
                BundleFixtureMutation::WrongSubject,
                &matching_policy,
            )
            .await
            .is_err(),
            "validly signed bundle for a different manifest must fail"
        );
        assert!(
            verify_local_bundle_signed_policy(
                canonical_doc().as_bytes(),
                BundleFixtureMutation::BadSignature,
                &matching_policy,
            )
            .await
            .is_err(),
            "invalid DSSE signature must fail"
        );
        assert!(
            verify_local_bundle_signed_policy(
                canonical_doc().as_bytes(),
                BundleFixtureMutation::Artifact,
                &matching_policy,
            )
            .await
            .is_err(),
            "tampered artifact bytes must fail for OCI referrer bundles"
        );
    }
}
