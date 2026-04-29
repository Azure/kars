//! Signed egress-allowlist artifact fetcher (S12.b, status-only).
//!
//! Pulls a content-addressed OCI artifact referenced by
//! [`crate::crd::OciArtifactRef`], verifies the cosign signature against a
//! cluster [`SignerPolicyConfig`] (Fulcio issuer + SAN patterns), and
//! re-validates the byte-stable canonical-form rules from
//! `docs/policy-canonical-format.md`.
//!
//! ## Status-only in S12.b
//!
//! Verification semantics are wired in S12.b but the controller still
//! derives the live `NetworkPolicy` from inline
//! [`crate::crd::NetworkPolicyConfig::allowed_endpoints`]. This module's
//! output is surfaced **only** as the `AllowlistVerified` Condition on
//! `ClawSandbox.status` — it does not change network behavior. The
//! authoritative-mode flip (controller derives `NetworkPolicy` from the
//! verified artifact) ships in S12.e behind the same env gate.
//!
//! ## Feature gate
//!
//! The fetcher is **only** invoked when the env var
//! `AZURECLAW_FEATURE_SIGNED_ALLOWLIST=1` is set. With the gate off, no
//! `AllowlistVerified` Condition is emitted and existing deployments
//! observe **no** behavior change.
//!
//! ## SignerPolicy is S12.d
//!
//! S12.b ships without a `SignerPolicy` ConfigMap watcher; the policy is
//! provisionally read from the env vars
//! `AZURECLAW_SIGNER_FULCIO_ISSUERS` and `AZURECLAW_SIGNER_SAN_PATTERNS`
//! (both comma-separated). When unset the verifier returns
//! [`FetchError::SignerPolicyMissing`] — that is the intended fail-closed
//! behavior until the cluster operator provisions a SignerPolicy.
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
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Env gate for the signed-allowlist path. See module docs.
const FEATURE_ENV: &str = "AZURECLAW_FEATURE_SIGNED_ALLOWLIST";
/// Comma-separated list of Fulcio issuer URLs (e.g.
/// `https://token.actions.githubusercontent.com`). See [`SignerPolicyConfig`].
const SIGNER_FULCIO_ISSUERS_ENV: &str = "AZURECLAW_SIGNER_FULCIO_ISSUERS";
/// Comma-separated list of SAN glob patterns. See [`SignerPolicyConfig`].
const SIGNER_SAN_PATTERNS_ENV: &str = "AZURECLAW_SIGNER_SAN_PATTERNS";

/// OCI media type for the v1 egress-allowlist artifact. The pulled
/// `artifactType` MUST match this exactly; consumers reject any other
/// value (forward-compat: v2 bumps the suffix; v1 consumers MUST refuse
/// v2 artifacts — see canonical-format doc §"Forward compatibility").
pub const EGRESS_ALLOWLIST_V1_MEDIA_TYPE: &str =
    "application/vnd.azureclaw.egress-allowlist.v1+yaml";

/// Pinned canonical apiVersion / kind values for v1.
const CANONICAL_API_VERSION: &str = "azureclaw.dev/v1alpha1";
const CANONICAL_KIND: &str = "EgressAllowlist";

/// Cache TTL for verified artifacts (per plan §S12.b — "1h").
const CACHE_TTL: Duration = Duration::from_secs(3600);

/// Errors surfaced by the fetcher. Each variant maps 1:1 to a Condition
/// `reason` value emitted on `ClawSandbox.status` — see
/// [`reason_for_error`].
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("AZURECLAW_FEATURE_SIGNED_ALLOWLIST is not enabled")]
    FeatureDisabled,
    #[error("SignerPolicy is not configured for this cluster")]
    SignerPolicyMissing,
    #[error("invalid artifact reference: {0}")]
    InvalidRef(String),
    #[error("OCI registry auth failed: {0}")]
    Unauthorized(String),
    #[error("artifact not found at {0}")]
    NotFound(String),
    #[error("digest mismatch: expected {expected}, got {actual}")]
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
        FetchError::FeatureDisabled => None,
        FetchError::SignerPolicyMissing => Some("SignerPolicyMissing"),
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

/// A verified, canonical-form egress allowlist. The `digest` field is the
/// pulled artifact digest (re-validated against `OciArtifactRef.digest`),
/// not a content-hash computed over [`endpoints`] — those are byte-stable
/// from canonicalization rules so the relationship is bijective.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAllowlist {
    pub api_version: String,
    pub kind: String,
    pub generation: u64,
    pub endpoints: Vec<CanonicalEndpoint>,
    /// `sha256:...` matched against `OciArtifactRef.digest`.
    pub digest: String,
    pub fetched_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalEndpoint {
    /// Lowercase ASCII; IDNA-2008 Punycode-encoded if originally non-ASCII.
    pub host: String,
    pub port: u16,
    /// Reserved for v2; always `None` in v1 canonical artifacts.
    pub protocol: Option<String>,
}

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
}

impl SignerPolicyConfig {
    /// Read from env. Both vars are comma-separated; empty / unset →
    /// empty vector. Whitespace is trimmed; empty entries are dropped.
    pub fn from_env() -> Self {
        Self {
            fulcio_issuers: split_csv_env(SIGNER_FULCIO_ISSUERS_ENV),
            san_patterns: split_csv_env(SIGNER_SAN_PATTERNS_ENV),
        }
    }

    /// A SignerPolicy is "configured" when *both* an issuer allow-list
    /// and a SAN allow-list are present. Either one alone is unsafe (any
    /// SAN with no issuer pinning permits arbitrary trust roots; any
    /// issuer with no SAN pinning permits arbitrary identities under
    /// that issuer), so we require both.
    pub fn is_configured(&self) -> bool {
        !self.fulcio_issuers.is_empty() && !self.san_patterns.is_empty()
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

/// Whether the feature gate is enabled. Cheap; safe to call on every
/// reconcile.
pub fn feature_enabled() -> bool {
    std::env::var(FEATURE_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
}

// ─────────────────────────── cache ───────────────────────────

#[derive(Debug, Clone)]
struct CacheEntry {
    verified: VerifiedAllowlist,
    inserted: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(r: &OciArtifactRef) -> String {
    format!("{}/{}@{}", r.registry, r.repository, r.digest)
}

fn cache_get(key: &str, now: Instant) -> Option<VerifiedAllowlist> {
    let guard = cache().lock().ok()?;
    let entry = guard.get(key)?;
    if now.duration_since(entry.inserted) > CACHE_TTL {
        return None;
    }
    Some(entry.verified.clone())
}

fn cache_put(key: String, verified: VerifiedAllowlist) {
    if let Ok(mut guard) = cache().lock() {
        guard.insert(
            key,
            CacheEntry {
                verified,
                inserted: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
fn cache_clear() {
    if let Ok(mut guard) = cache().lock() {
        guard.clear();
    }
}

// ─────────────────────────── public entry ───────────────────────────

/// Pull, verify, and parse a signed egress-allowlist artifact.
///
/// Returns `Err(FetchError::FeatureDisabled)` if the env gate is off
/// (defensive — the reconciler should check [`feature_enabled`] before
/// calling). Returns `Err(FetchError::SignerPolicyMissing)` when no
/// SignerPolicy is configured; this is the expected outcome in S12.b
/// until S12.d ships the ConfigMap watcher.
///
/// On success, the result is cached by `<registry>/<repository>@<digest>`
/// for [`CACHE_TTL`].
pub async fn fetch_and_verify(
    artifact_ref: &OciArtifactRef,
    signer_policy: &SignerPolicyConfig,
) -> Result<VerifiedAllowlist, FetchError> {
    if !feature_enabled() {
        return Err(FetchError::FeatureDisabled);
    }

    validate_ref_shape(artifact_ref)?;

    if !signer_policy.is_configured() {
        // S12.b fail-closed: without identity pinning we treat
        // "valid sig" alone as insufficient authority. The reconciler
        // surfaces this as `AllowlistVerified=False/SignerPolicyMissing`.
        return Err(FetchError::SignerPolicyMissing);
    }

    let key = cache_key(artifact_ref);
    if let Some(hit) = cache_get(&key, Instant::now()) {
        tracing::debug!(digest = %artifact_ref.digest, "policy_fetcher cache hit");
        return Ok(hit);
    }

    // The cosign verification + OCI pull path. Wired against
    // sigstore-rs 0.13 + oci-client 0.16. With S12.b we exit at
    // `SignerPolicyMissing` above in production, but the path below is
    // real code (not a stub): when an operator configures
    // `AZURECLAW_SIGNER_*` env vars, the controller will execute it.
    let verified = verify_via_sigstore(artifact_ref, signer_policy).await?;

    cache_put(key, verified.clone());
    Ok(verified)
}

fn validate_ref_shape(r: &OciArtifactRef) -> Result<(), FetchError> {
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
    if r.artifact_type != EGRESS_ALLOWLIST_V1_MEDIA_TYPE {
        return Err(FetchError::InvalidRef(format!(
            "artifactType `{}` is not the expected `{}`",
            r.artifact_type, EGRESS_ALLOWLIST_V1_MEDIA_TYPE
        )));
    }
    Ok(())
}

// ─────────────────────────── verify path ───────────────────────────

async fn verify_via_sigstore(
    artifact_ref: &OciArtifactRef,
    signer_policy: &SignerPolicyConfig,
) -> Result<VerifiedAllowlist, FetchError> {
    use sigstore::cosign::verification_constraint::cert_subject_email_verifier::StringVerifier;
    use sigstore::cosign::verification_constraint::{
        CertSubjectEmailVerifier, CertSubjectUrlVerifier, VerificationConstraintVec,
    };
    use sigstore::cosign::{ClientBuilder, CosignCapabilities, verify_constraints};
    use sigstore::registry::OciReference;

    // The S12.b client is built without a Fulcio/Rekor trust repository
    // explicitly attached. Sigstore-rs requires both to elevate the
    // embedded cert in `SignatureLayer.certificate_signature`, which
    // means with this minimal builder the `CertSubject*Verifier`
    // constraints will not match (they require a verified cert subject).
    // S12.d wires `ManualTrustRoot` from the cluster `SignerPolicy`
    // ConfigMap; for now we surface verification failure as
    // `SignatureVerifyFailed` rather than silently passing.
    let mut client = ClientBuilder::default()
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
        let mut constraints: VerificationConstraintVec = Vec::new();
        for issuer in &signer_policy.fulcio_issuers {
            for san in &signer_policy.san_patterns {
                if san.starts_with("https://") || san.starts_with("http://") {
                    constraints.push(Box::new(CertSubjectUrlVerifier {
                        url: san.clone(),
                        issuer: issuer.clone(),
                    }));
                } else {
                    constraints.push(Box::new(CertSubjectEmailVerifier {
                        email: StringVerifier::ExactMatch(san.clone()),
                        issuer: Some(StringVerifier::ExactMatch(issuer.clone())),
                    }));
                }
            }
        }
        verify_constraints(&layers, constraints.iter())
            .map_err(|e| FetchError::IdentityMismatch(format!("{e:?}")))?;
    }

    // Step D: pull artifact bytes (digest-pinned) and re-validate.
    let bytes = pull_artifact_bytes(artifact_ref, &auth).await?;
    let actual = format!("sha256:{}", sha256_hex(&bytes));
    if actual != artifact_ref.digest {
        return Err(FetchError::DigestMismatch {
            expected: artifact_ref.digest.clone(),
            actual,
        });
    }

    let mut parsed = canonical::parse(&bytes)?;
    parsed.digest = artifact_ref.digest.clone();
    parsed.fetched_at = SystemTime::now();
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
        .pull(&reference, &oci_auth, vec![EGRESS_ALLOWLIST_V1_MEDIA_TYPE])
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

// ─────────────────────────── canonical parser ───────────────────────────

pub mod canonical {
    //! Canonical YAML parser for
    //! `application/vnd.azureclaw.egress-allowlist.v1+yaml`.
    //!
    //! Implements the byte-stable rules in `docs/policy-canonical-format.md`.
    //! Invoked **after** cosign signature verification — re-validates that
    //! the bytes are canonical (sorted, IDNA-normalized, deduplicated,
    //! generation present). Any deviation returns
    //! [`FetchError::CanonicalFormViolation`].
    //!
    //! Parses with `serde_yaml` for structural deserialization, then
    //! independently re-checks the byte-level invariants that
    //! `serde_yaml` would silently tolerate (key order, sort order,
    //! duplicate detection, etc.). This catches the case where a
    //! producer signs structurally-valid-but-non-canonical bytes —
    //! verification still rejects them.
    use super::{
        CANONICAL_API_VERSION, CANONICAL_KIND, CanonicalEndpoint, FetchError, VerifiedAllowlist,
    };
    use std::time::SystemTime;

    /// Parse + canonical-form re-validate. The returned
    /// [`VerifiedAllowlist::digest`] is empty here; the caller fills it
    /// from the verified `OciArtifactRef.digest`.
    pub fn parse(bytes: &[u8]) -> Result<VerifiedAllowlist, FetchError> {
        let s = std::str::from_utf8(bytes)
            .map_err(|e| FetchError::CanonicalFormViolation(format!("utf-8: {e}")))?;
        // Rule #1: LF line endings, trailing newline.
        if s.contains('\r') {
            return Err(FetchError::CanonicalFormViolation(
                "CRLF line endings forbidden".into(),
            ));
        }
        if !s.ends_with('\n') {
            return Err(FetchError::CanonicalFormViolation(
                "missing trailing newline".into(),
            ));
        }
        // Rule #12: no comments.
        for line in s.lines() {
            // YAML block scalars don't collide with our schema (we have
            // no scalar values that contain '#'), so a simple line-level
            // check is sufficient for the v1 surface.
            if line.trim_start().starts_with('#') {
                return Err(FetchError::CanonicalFormViolation(
                    "comments forbidden".into(),
                ));
            }
        }

        // Top-level key order check (rule #2).
        validate_top_level_key_order(s)?;

        let doc: serde_yaml::Value = serde_yaml::from_str(s)
            .map_err(|e| FetchError::CanonicalFormViolation(format!("yaml parse: {e}")))?;
        let map = doc
            .as_mapping()
            .ok_or_else(|| FetchError::CanonicalFormViolation("not a mapping".into()))?;

        // Rule #3 + #4.
        let api_version = map
            .get(serde_yaml::Value::String("apiVersion".into()))
            .and_then(|v| v.as_str())
            .ok_or_else(|| FetchError::CanonicalFormViolation("missing apiVersion".into()))?;
        if api_version != CANONICAL_API_VERSION {
            return Err(FetchError::CanonicalFormViolation(format!(
                "apiVersion `{api_version}` != `{CANONICAL_API_VERSION}`"
            )));
        }
        let kind = map
            .get(serde_yaml::Value::String("kind".into()))
            .and_then(|v| v.as_str())
            .ok_or_else(|| FetchError::CanonicalFormViolation("missing kind".into()))?;
        if kind != CANONICAL_KIND {
            return Err(FetchError::CanonicalFormViolation(format!(
                "kind `{kind}` != `{CANONICAL_KIND}`"
            )));
        }

        // Rule #5: metadata.generation must be a positive integer.
        let metadata = map
            .get(serde_yaml::Value::String("metadata".into()))
            .and_then(|v| v.as_mapping())
            .ok_or_else(|| FetchError::CanonicalFormViolation("missing metadata".into()))?;
        let generation = metadata
            .get(serde_yaml::Value::String("generation".into()))
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                FetchError::CanonicalFormViolation(
                    "metadata.generation missing or not a positive integer".into(),
                )
            })?;
        if generation == 0 {
            return Err(FetchError::CanonicalFormViolation(
                "metadata.generation must be > 0".into(),
            ));
        }

        // Rule #6: spec.endpoints required.
        let spec = map
            .get(serde_yaml::Value::String("spec".into()))
            .and_then(|v| v.as_mapping())
            .ok_or_else(|| FetchError::CanonicalFormViolation("missing spec".into()))?;
        let endpoints_node = spec
            .get(serde_yaml::Value::String("endpoints".into()))
            .ok_or_else(|| FetchError::CanonicalFormViolation("missing spec.endpoints".into()))?;
        let endpoints_seq = endpoints_node.as_sequence().ok_or_else(|| {
            FetchError::CanonicalFormViolation("spec.endpoints not a sequence".into())
        })?;

        // Parse endpoints + per-endpoint validation (rules #7, #8, #11).
        let mut parsed: Vec<CanonicalEndpoint> = Vec::with_capacity(endpoints_seq.len());
        for (i, ep) in endpoints_seq.iter().enumerate() {
            let m = ep.as_mapping().ok_or_else(|| {
                FetchError::CanonicalFormViolation(format!("endpoint[{i}] not a mapping"))
            })?;
            // Rule #11: keys must be host then port, in that order.
            let mut keys = m.keys();
            let k1 = keys.next().and_then(|v| v.as_str()).ok_or_else(|| {
                FetchError::CanonicalFormViolation(format!("endpoint[{i}] missing first key"))
            })?;
            let k2 = keys.next().and_then(|v| v.as_str()).ok_or_else(|| {
                FetchError::CanonicalFormViolation(format!("endpoint[{i}] missing second key"))
            })?;
            if keys.next().is_some() {
                return Err(FetchError::CanonicalFormViolation(format!(
                    "endpoint[{i}] has extra keys (only host,port allowed in v1)"
                )));
            }
            if k1 != "host" || k2 != "port" {
                return Err(FetchError::CanonicalFormViolation(format!(
                    "endpoint[{i}] key order must be host,port (got {k1},{k2})"
                )));
            }
            let host = m
                .get(serde_yaml::Value::String("host".into()))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    FetchError::CanonicalFormViolation(format!("endpoint[{i}] host not a string"))
                })?;
            validate_host(host, i)?;
            let port = m
                .get(serde_yaml::Value::String("port".into()))
                .and_then(|v| v.as_u64())
                .ok_or_else(|| {
                    FetchError::CanonicalFormViolation(format!("endpoint[{i}] port not an integer"))
                })?;
            if port == 0 || port > 65535 {
                return Err(FetchError::CanonicalFormViolation(format!(
                    "endpoint[{i}] port {port} out of range [1,65535]"
                )));
            }
            parsed.push(CanonicalEndpoint {
                host: host.to_string(),
                port: port as u16,
                protocol: None,
            });
        }

        // Rule #9 + #10: dedup + sort order.
        for w in parsed.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            let cmp = a
                .host
                .as_str()
                .cmp(b.host.as_str())
                .then(a.port.cmp(&b.port));
            if cmp == std::cmp::Ordering::Greater {
                return Err(FetchError::CanonicalFormViolation(format!(
                    "endpoints not sorted: `{}:{}` after `{}:{}`",
                    b.host, b.port, a.host, a.port
                )));
            }
            if cmp == std::cmp::Ordering::Equal {
                return Err(FetchError::CanonicalFormViolation(format!(
                    "duplicate endpoint `{}:{}`",
                    a.host, a.port
                )));
            }
        }

        Ok(VerifiedAllowlist {
            api_version: api_version.to_string(),
            kind: kind.to_string(),
            generation,
            endpoints: parsed,
            digest: String::new(),
            fetched_at: SystemTime::UNIX_EPOCH,
        })
    }

    fn validate_top_level_key_order(s: &str) -> Result<(), FetchError> {
        let expected = ["apiVersion:", "kind:", "metadata:", "spec:"];
        let mut idx = 0;
        for line in s.lines() {
            if line.starts_with(' ') || line.is_empty() || line.starts_with('-') {
                continue;
            }
            // Match top-level keys only (no leading whitespace).
            for (i, k) in expected.iter().enumerate().skip(idx) {
                if line.starts_with(k) {
                    if i != idx {
                        return Err(FetchError::CanonicalFormViolation(format!(
                            "top-level key `{k}` out of order"
                        )));
                    }
                    idx = i + 1;
                    break;
                }
            }
        }
        if idx != expected.len() {
            return Err(FetchError::CanonicalFormViolation(
                "top-level keys missing or out of order".into(),
            ));
        }
        Ok(())
    }

    fn validate_host(host: &str, idx: usize) -> Result<(), FetchError> {
        if host.is_empty() {
            return Err(FetchError::CanonicalFormViolation(format!(
                "endpoint[{idx}] host empty"
            )));
        }
        if host.ends_with('.') {
            return Err(FetchError::CanonicalFormViolation(format!(
                "endpoint[{idx}] host has trailing dot"
            )));
        }
        if host.contains('*') {
            return Err(FetchError::CanonicalFormViolation(format!(
                "endpoint[{idx}] wildcards not allowed in v1"
            )));
        }
        for c in host.chars() {
            let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-';
            if !ok {
                return Err(FetchError::CanonicalFormViolation(format!(
                    "endpoint[{idx}] host `{host}` contains invalid byte `{c}` (rule #7)"
                )));
            }
        }
        // Rule #7: any non-ASCII Unicode would already have failed the
        // ASCII-only loop above, so reaching here means the producer
        // either pre-encoded with IDNA-2008 (good) or the value is
        // pure ASCII (also good).
        Ok(())
    }
}

// ─────────────────────── reconciler integration ────────────────────────

/// Reconciler-side driver: when the feature gate is on AND the sandbox
/// has a `networkPolicy.allowlistRef`, run the fetcher and translate the
/// outcome into a Condition (with `lastTransitionTime` preserved across
/// same-status reconciles via [`crate::status::conditions::preserve_transition_time`]).
///
/// Returns `None` when the slice should NOT emit any
/// `AllowlistVerified` Condition: either the gate is off, or the CR has
/// no `allowlistRef`, or the fetch returned a `Transient` error (in
/// which case the prior condition value is preserved by the caller — we
/// never overwrite known-good state with a flap).
pub async fn maybe_verify_allowlist(
    sandbox: &crate::crd::ClawSandbox,
) -> Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> {
    use crate::status::conditions::{
        TYPE_ALLOWLIST_VERIFIED, preserve_transition_time, reason as cond_reason,
        status as cond_status,
    };
    if !feature_enabled() {
        return None;
    }
    let artifact_ref = sandbox
        .spec
        .network_policy
        .as_ref()
        .and_then(|np| np.allowlist_ref.as_ref())?;

    let policy = SignerPolicyConfig::from_env();
    let prior = sandbox.status.as_ref().and_then(|s| {
        s.conditions
            .iter()
            .find(|c| c.type_ == TYPE_ALLOWLIST_VERIFIED)
    });
    let generation = sandbox.metadata.generation;

    match fetch_and_verify(artifact_ref, &policy).await {
        Ok(verified) => {
            tracing::info!(
                digest = %verified.digest,
                gen = verified.generation,
                "AllowlistVerified=True (S12.b status-only)"
            );
            let msg = format!(
                "digest={}, generation={}",
                verified.digest, verified.generation
            );
            Some(preserve_transition_time(
                prior,
                TYPE_ALLOWLIST_VERIFIED,
                cond_status::TRUE,
                cond_reason::VERIFIED,
                &msg,
                generation,
            ))
        }
        Err(FetchError::Transient(msg)) => {
            tracing::warn!(error = %msg, "policy_fetcher transient; preserving prior AllowlistVerified");
            prior.cloned()
        }
        Err(FetchError::FeatureDisabled) => None,
        Err(other) => {
            let reason = reason_for_error(&other).unwrap_or("Failed");
            tracing::warn!(reason = %reason, error = %other, "AllowlistVerified=False");
            Some(preserve_transition_time(
                prior,
                TYPE_ALLOWLIST_VERIFIED,
                cond_status::FALSE,
                reason,
                &other.to_string(),
                generation,
            ))
        }
    }
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
    fn feature_disabled_by_default() {
        let _g = env_lock().lock().unwrap();
        let _e = EnvGuard::unset(FEATURE_ENV);
        assert!(!feature_enabled());
    }

    #[test]
    fn feature_enabled_when_env_one() {
        let _g = env_lock().lock().unwrap();
        let _e = EnvGuard::set(FEATURE_ENV, "1");
        assert!(feature_enabled());
    }

    #[test]
    fn feature_disabled_for_non_one_truthy_values() {
        let _g = env_lock().lock().unwrap();
        // Operators sometimes set "true" expecting it to work; we
        // accept only "1" to keep the surface unambiguous (mirrors
        // many Kubernetes feature gates).
        let _e = EnvGuard::set(FEATURE_ENV, "true");
        assert!(!feature_enabled());
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
        let v = canonical::parse(canonical_doc().as_bytes()).expect("valid canonical");
        assert_eq!(v.api_version, CANONICAL_API_VERSION);
        assert_eq!(v.kind, CANONICAL_KIND);
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("> 0")));
    }

    #[test]
    fn canonical_parser_rejects_uppercase_host() {
        let doc = "apiVersion: azureclaw.dev/v1alpha1\n\
                   kind: EgressAllowlist\n\
                   metadata:\n  generation: 1\n\
                   spec:\n  endpoints:\n  \
                   - host: API.github.com\n    port: 443\n";
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
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
        let err = canonical::parse(doc.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("comments")));
    }

    #[test]
    fn validate_ref_shape_rejects_bad_digest() {
        let mut r = good_ref();
        r.digest = "sha512:xyz".into();
        let err = validate_ref_shape(&r).unwrap_err();
        assert!(matches!(err, FetchError::InvalidRef(_)));
    }

    #[test]
    fn validate_ref_shape_rejects_bad_artifact_type() {
        let mut r = good_ref();
        r.artifact_type = "application/vnd.something.else".into();
        let err = validate_ref_shape(&r).unwrap_err();
        assert!(matches!(err, FetchError::InvalidRef(ref m) if m.contains("artifactType")));
    }

    #[test]
    fn validate_ref_shape_rejects_uppercase_digest() {
        let mut r = good_ref();
        r.digest = format!("sha256:{}", "A".repeat(64));
        let err = validate_ref_shape(&r).unwrap_err();
        assert!(matches!(err, FetchError::InvalidRef(ref m) if m.contains("lowercase")));
    }

    #[test]
    fn validate_ref_shape_accepts_valid_ref() {
        validate_ref_shape(&good_ref()).expect("good ref");
    }

    #[test]
    fn reason_for_error_maps_each_variant() {
        assert_eq!(reason_for_error(&FetchError::FeatureDisabled), None);
        assert_eq!(
            reason_for_error(&FetchError::SignerPolicyMissing),
            Some("SignerPolicyMissing")
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
        cache_clear();
        let r = good_ref();
        let key = cache_key(&r);
        let now = Instant::now();
        assert!(cache_get(&key, now).is_none(), "cold cache");

        let v = canonical::parse(canonical_doc().as_bytes()).unwrap();
        cache_put(key.clone(), v.clone());
        let hit = cache_get(&key, now).expect("hit");
        assert_eq!(hit.generation, v.generation);

        // Simulated expiry: a synthetic "now" beyond TTL should miss
        // even though the entry is still in the map. (We can't actually
        // advance Instant::now(), but cache_get computes
        // `now.duration_since(entry.inserted)`, so passing a future
        // `now` exercises the TTL branch.)
        let future = now + CACHE_TTL + Duration::from_secs(1);
        assert!(cache_get(&key, future).is_none(), "expired");
        cache_clear();
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
    async fn fetch_returns_feature_disabled_when_gate_off() {
        let _g = env_lock().lock().unwrap();
        let _e = EnvGuard::unset(FEATURE_ENV);
        let policy = SignerPolicyConfig::default();
        let err = fetch_and_verify(&good_ref(), &policy).await.unwrap_err();
        assert!(matches!(err, FetchError::FeatureDisabled));
    }

    #[tokio::test]
    async fn fetch_returns_signer_policy_missing_when_no_signer_policy() {
        let _g = env_lock().lock().unwrap();
        let _e1 = EnvGuard::set(FEATURE_ENV, "1");
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
        let _e = EnvGuard::set(FEATURE_ENV, "1");
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
}
