// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! S12.d — `SignerPolicy` ConfigMap watcher.
//!
//! Replaces the env-var path that S12.b used (`AZURECLAW_SIGNER_FULCIO_ISSUERS`,
//! `AZURECLAW_SIGNER_SAN_PATTERNS`) with a cluster-scoped, watched
//! `ConfigMap` named [`SIGNER_POLICY_CM_NAME`] in the controller
//! namespace. The env-var path is preserved as an emergency-override
//! fallback when the ConfigMap is **absent**; a *malformed* ConfigMap
//! does **not** silently fall back — it surfaces as
//! [`crate::policy_fetcher::FetchError::SignerPolicyMalformed`] on the
//! affected `ClawSandbox` resources.
//!
//! ## Wire shape
//!
//! ```yaml
//! apiVersion: v1
//! kind: ConfigMap
//! metadata:
//!   name: azureclaw-signer-policy
//!   namespace: azureclaw-system
//! data:
//!   fulcioIssuers: |
//!     https://token.actions.githubusercontent.com
//!     https://login.microsoftonline.com/<tenant>/v2.0
//!   sanPatterns: |
//!     https://github.com/Azure/azureclaw/.github/workflows/*.yml@*
//!     signer@example.com
//!   ed25519Keys: |
//!     [
//!       {
//!         "id": "grant-signer-2026-q2",
//!         "publicKeyBase64": "MCowBQYDK2VwAyEA...",
//!         "allowedSubjects": ["egress-approval"]
//!       }
//!     ]
//! ```
//!
//! `fulcioIssuers` + `sanPatterns` are **required** and govern the
//! data-plane policy lane (egress allowlist / tool policy /
//! inference / memory / mcp-server bundles, all signed via cosign +
//! Fulcio). `ed25519Keys` is **optional** forward-compat for the
//! grant lane that lands in Slice 5e+ (`EgressApproval`) — parsed
//! and surfaced here, but **not yet enforced**. Empty + absent are
//! treated identically.
//!
//! ## Trust model
//!
//! - Cluster-scoped object → only cluster-admins can mutate.
//! - Watched only in the controller namespace → blast radius is the
//!   controller pod itself.
//! - RBAC: namespace-scoped `get/list/watch` on `configmaps` (the
//!   existing controller `ClusterRole` already grants this; no
//!   broadening needed).
//! - Fail-closed: malformed → `SignerPolicyMalformed`. Absent +
//!   no-env → `SignerPolicyMissing`. Malformed `ed25519Keys` blocks
//!   the **whole** policy parse — the grant lane defers to 5e+ but
//!   broken JSON is never silently ignored.

use std::sync::{Arc, RwLock};

use k8s_openapi::api::core::v1::ConfigMap;

/// Singleton ConfigMap name. Cluster-scoped because there is one
/// authoritative SignerPolicy per cluster — multi-tenant slicing of
/// trust roots is out of scope until a future slice introduces tenant
/// CRDs.
pub const SIGNER_POLICY_CM_NAME: &str = "azureclaw-signer-policy";

/// `data.fulcioIssuers` key — newline-separated list of Fulcio issuer
/// URLs (`https://...`). Comment lines starting with `#` are stripped.
pub const KEY_FULCIO_ISSUERS: &str = "fulcioIssuers";

/// `data.sanPatterns` key — newline-separated list of SAN patterns.
/// Each pattern is matched against the signer cert's `Subject.email` or
/// `Subject.uri` per `policy_fetcher::verify_via_sigstore`. Glob syntax
/// (`*`, `?`) is the same as the env-var path.
pub const KEY_SAN_PATTERNS: &str = "sanPatterns";

/// `data.ed25519Keys` key — JSON array of [`Ed25519Key`] entries.
///
/// **Optional** ConfigMap key. Absent or `[]` means "no grant-lane
/// signers registered" — equivalent for forward-compat purposes.
/// Parsed + surfaced here for Slice 5e+ consumption; the data-plane
/// policy lane (1c.1–1c.5) does **not** consult this list.
pub const KEY_ED25519_KEYS: &str = "ed25519Keys";

/// A registered Ed25519 public key for the grant lane (Slice 5e+).
///
/// Wire shape (a single element of the `ed25519Keys` JSON array):
///
/// ```json
/// {
///   "id": "grant-signer-2026-q2",
///   "publicKeyBase64": "MCowBQYDK2VwAyEA...",
///   "allowedSubjects": ["egress-approval"]
/// }
/// ```
///
/// **Field semantics:**
///
/// - `id` — operator-chosen stable identifier. Must be non-empty,
///   DNS-label-safe (1-63 chars, `[a-z0-9-]`, no leading/trailing
///   dash) so it can flow through K8s labels + ConfigMap names in
///   5e+. Duplicate ids across entries are a parse error.
/// - `public_key_b64` — standard base64-encoded raw Ed25519 public
///   key (32 bytes raw → 44 chars base64). Both raw + DER/PKIX
///   forms are accepted to ease cosign-issued material; the 5e+
///   verifier normalises before use.
/// - `allowed_subjects` — list of grant-artifact subject types this
///   key is authorised to sign. Initial vocabulary:
///   `egress-approval`. Empty list means "the key is registered
///   but cannot authorise anything yet" — explicit and intentional.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ed25519Key {
    pub id: String,
    pub public_key_base64: String,
    #[serde(default)]
    pub allowed_subjects: Vec<String>,
}

/// Parsed signer policy. The wire shape mirrors
/// [`crate::policy_fetcher::SignerPolicyConfig`] for a zero-cost
/// conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignerPolicy {
    /// Allowed Fulcio issuer URLs. Empty → reject all.
    pub fulcio_issuers: Vec<String>,
    /// Allowed SAN glob patterns. Empty → reject all.
    pub san_patterns: Vec<String>,
    /// Registered Ed25519 keys for the grant lane (Slice 5e+).
    /// Parsed + surfaced; **not enforced** in this slice. Empty +
    /// absent are equivalent.
    pub ed25519_keys: Vec<Ed25519Key>,
}

impl SignerPolicy {
    /// Both lists must be non-empty for a configured policy.
    #[allow(dead_code)] // Test-only assertion helper; mirrors `SignerPolicyConfig::is_configured`.
    pub fn is_configured(&self) -> bool {
        !self.fulcio_issuers.is_empty() && !self.san_patterns.is_empty()
    }
}

/// Errors surfaced by [`parse_configmap`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SignerPolicyError {
    #[error("ConfigMap has no `data` block")]
    NoData,
    #[error("ConfigMap missing required key `{0}`")]
    MissingKey(&'static str),
    #[error("ConfigMap key `{key}` is empty after trimming")]
    EmptyKey { key: &'static str },
    #[error("ConfigMap key `{key}` contains invalid entry `{entry}`: {reason}")]
    InvalidEntry {
        key: &'static str,
        entry: String,
        reason: &'static str,
    },
    #[error("ConfigMap key `ed25519Keys` is not valid JSON: {0}")]
    Ed25519JsonParse(String),
    #[error("ConfigMap key `ed25519Keys` must be a JSON array")]
    Ed25519NotArray,
    #[error("ed25519Keys[{index}].{field} is invalid: {reason}")]
    Ed25519InvalidEntry {
        index: usize,
        field: &'static str,
        reason: String,
    },
    #[error("ed25519Keys contains duplicate id `{0}`")]
    Ed25519DuplicateId(String),
}

/// Parse a `ConfigMap` into a [`SignerPolicy`]. Strict — see module docs
/// for the wire shape; both keys MUST be present and non-empty.
pub fn parse_configmap(cm: &ConfigMap) -> Result<SignerPolicy, SignerPolicyError> {
    let data = cm.data.as_ref().ok_or(SignerPolicyError::NoData)?;
    let issuers_raw = data
        .get(KEY_FULCIO_ISSUERS)
        .ok_or(SignerPolicyError::MissingKey(KEY_FULCIO_ISSUERS))?;
    let sans_raw = data
        .get(KEY_SAN_PATTERNS)
        .ok_or(SignerPolicyError::MissingKey(KEY_SAN_PATTERNS))?;

    let fulcio_issuers = parse_list(issuers_raw, KEY_FULCIO_ISSUERS, validate_issuer)?;
    if fulcio_issuers.is_empty() {
        return Err(SignerPolicyError::EmptyKey {
            key: KEY_FULCIO_ISSUERS,
        });
    }
    let san_patterns = parse_list(sans_raw, KEY_SAN_PATTERNS, validate_san)?;
    if san_patterns.is_empty() {
        return Err(SignerPolicyError::EmptyKey {
            key: KEY_SAN_PATTERNS,
        });
    }

    Ok(SignerPolicy {
        fulcio_issuers,
        san_patterns,
        ed25519_keys: match data.get(KEY_ED25519_KEYS) {
            Some(raw) => parse_ed25519_keys(raw)?,
            None => Vec::new(),
        },
    })
}

/// Parse the `ed25519Keys` ConfigMap value (a JSON array of
/// [`Ed25519Key`] objects) into a deduplicated, validated `Vec`.
///
/// Strict: empty whitespace and `[]` both parse to an empty `Vec`
/// (forward-compat — operators wire the data key in early without
/// having keys yet). Any other malformed shape is an error.
pub fn parse_ed25519_keys(raw: &str) -> Result<Vec<Ed25519Key>, SignerPolicyError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| SignerPolicyError::Ed25519JsonParse(e.to_string()))?;
    let arr = value
        .as_array()
        .ok_or(SignerPolicyError::Ed25519NotArray)?
        .clone();

    let mut out: Vec<Ed25519Key> = Vec::with_capacity(arr.len());
    let mut seen_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (index, entry) in arr.into_iter().enumerate() {
        let key: Ed25519Key =
            serde_json::from_value(entry).map_err(|e| SignerPolicyError::Ed25519InvalidEntry {
                index,
                field: "<entry>",
                reason: e.to_string(),
            })?;
        validate_ed25519_key(index, &key)?;
        if !seen_ids.insert(key.id.clone()) {
            return Err(SignerPolicyError::Ed25519DuplicateId(key.id));
        }
        out.push(key);
    }
    Ok(out)
}

fn validate_ed25519_key(index: usize, key: &Ed25519Key) -> Result<(), SignerPolicyError> {
    if key.id.is_empty() {
        return Err(SignerPolicyError::Ed25519InvalidEntry {
            index,
            field: "id",
            reason: "must be non-empty".to_string(),
        });
    }
    if key.id.len() > 63 {
        return Err(SignerPolicyError::Ed25519InvalidEntry {
            index,
            field: "id",
            reason: format!("must be ≤63 chars (got {})", key.id.len()),
        });
    }
    if !is_dns_label(&key.id) {
        return Err(SignerPolicyError::Ed25519InvalidEntry {
            index,
            field: "id",
            reason: "must be a DNS label ([a-z0-9-], no leading/trailing dash)".to_string(),
        });
    }
    let b64 = key.public_key_base64.trim();
    if b64.is_empty() {
        return Err(SignerPolicyError::Ed25519InvalidEntry {
            index,
            field: "publicKeyBase64",
            reason: "must be non-empty".to_string(),
        });
    }
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| SignerPolicyError::Ed25519InvalidEntry {
            index,
            field: "publicKeyBase64",
            reason: format!("invalid base64: {e}"),
        })?;
    // Accept raw 32-byte Ed25519 pubkeys + the 44-byte DER/PKIX SPKI
    // form cosign emits. Sub-44-byte payloads with `MCowBQYDK2VwAyEA`
    // prefix are not valid PKIX — reject.
    let len = decoded.len();
    if !matches!(len, 32 | 44) {
        return Err(SignerPolicyError::Ed25519InvalidEntry {
            index,
            field: "publicKeyBase64",
            reason: format!("must decode to 32 (raw) or 44 (DER) bytes, got {len}"),
        });
    }
    for (s_idx, subject) in key.allowed_subjects.iter().enumerate() {
        if subject.trim().is_empty() {
            return Err(SignerPolicyError::Ed25519InvalidEntry {
                index,
                field: "allowedSubjects",
                reason: format!("entry [{s_idx}] is empty"),
            });
        }
    }
    Ok(())
}

fn is_dns_label(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let bytes = s.as_bytes();
    if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn parse_list(
    raw: &str,
    key: &'static str,
    validate: fn(&str) -> Result<(), &'static str>,
) -> Result<Vec<String>, SignerPolicyError> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        validate(trimmed).map_err(|reason| SignerPolicyError::InvalidEntry {
            key,
            entry: trimmed.to_string(),
            reason,
        })?;
        out.push(trimmed.to_string());
    }
    Ok(out)
}

fn validate_issuer(s: &str) -> Result<(), &'static str> {
    if !(s.starts_with("https://") || s.starts_with("http://")) {
        return Err("issuer must be an http(s) URL");
    }
    if s.contains(char::is_whitespace) {
        return Err("issuer must not contain whitespace");
    }
    Ok(())
}

fn validate_san(s: &str) -> Result<(), &'static str> {
    // Accept either an http(s) URL (CertSubjectUrlVerifier), an
    // `re:<pattern>` regex (CertSubjectEmailVerifier::Regex), or a
    // generic email-like literal (CertSubjectEmailVerifier::ExactMatch).
    // The only hard rule is "no embedded whitespace" — matches the
    // env-var-path's CSV split semantics.
    if s.contains(char::is_whitespace) {
        return Err("SAN must not contain whitespace");
    }
    if let Some(re) = s.strip_prefix("re:") {
        if re.is_empty() {
            return Err("re: prefix requires a regex pattern");
        }
        if regex::Regex::new(re).is_err() {
            return Err("re: pattern is not a valid regular expression");
        }
    }
    Ok(())
}

/// Current state of the signer-policy holder.
#[derive(Debug, Clone, Default)]
pub enum SignerPolicyState {
    /// ConfigMap parsed cleanly; this policy is authoritative.
    FromConfigMap(SignerPolicy),
    /// ConfigMap present but invalid. We do **not** fall back silently
    /// — the message is surfaced as `SignerPolicyMalformed`.
    Malformed(String),
    /// No ConfigMap observed (cleanly absent). Fall back to env vars.
    #[default]
    Absent,
}

/// Process-shared signer-policy holder. Cheap to clone (refcount
/// bump). Reads take a brief read-lock; writes only happen on watcher
/// events.
#[derive(Debug, Clone, Default)]
pub struct SharedSignerPolicy {
    inner: Arc<RwLock<SignerPolicyState>>,
}

impl SharedSignerPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test-only constructor. Production code starts at
    /// [`SignerPolicyState::Absent`] and gets driven by the watcher.
    #[cfg(test)]
    pub fn from_state(state: SignerPolicyState) -> Self {
        Self {
            inner: Arc::new(RwLock::new(state)),
        }
    }

    /// Apply a freshly-observed `ConfigMap`. Sets state to
    /// [`SignerPolicyState::FromConfigMap`] on parse-success or
    /// [`SignerPolicyState::Malformed`] on parse-error.
    pub fn apply_configmap(&self, cm: &ConfigMap) {
        let next = match parse_configmap(cm) {
            Ok(p) => {
                tracing::info!(
                    fulcio_issuers = p.fulcio_issuers.len(),
                    san_patterns = p.san_patterns.len(),
                    ed25519_keys = p.ed25519_keys.len(),
                    ed25519_key_ids = ?p.ed25519_keys.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
                    "SignerPolicy ConfigMap applied",
                );
                SignerPolicyState::FromConfigMap(p)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "SignerPolicy ConfigMap is malformed; surfacing SignerPolicyMalformed (no env fallback)",
                );
                SignerPolicyState::Malformed(e.to_string())
            }
        };
        if let Ok(mut w) = self.inner.write() {
            *w = next;
        }
    }

    /// Snapshot of the registered Ed25519 keys (grant lane, Slice 5e+).
    ///
    /// Returns an empty `Vec` when the SignerPolicy ConfigMap is
    /// absent, malformed, or omits the `ed25519Keys` data key. Cheap
    /// to call — clones the inner `Vec<Ed25519Key>` (which is
    /// typically 0–4 entries in practice).
    ///
    /// **Forward-compat surface.** The data-plane policy verifier
    /// (egress / tools / inference / memory / mcp-server) does not
    /// call this method; Slice 5e+ grant-lane verification will.
    #[allow(dead_code)] // Slice 5e+ grant-lane verifier consumer.
    pub fn ed25519_keys(&self) -> Vec<Ed25519Key> {
        self.inner
            .read()
            .map(|g| match &*g {
                SignerPolicyState::FromConfigMap(p) => p.ed25519_keys.clone(),
                _ => Vec::new(),
            })
            .unwrap_or_default()
    }

    /// Reset to [`SignerPolicyState::Absent`]. Used on watcher delete
    /// events and when the initial-list completes without observing
    /// the singleton.
    pub fn clear(&self) {
        if let Ok(mut w) = self.inner.write() {
            *w = SignerPolicyState::Absent;
        }
    }

    /// Snapshot the current state. Cheap; safe to call on every
    /// reconcile.
    pub fn snapshot(&self) -> SignerPolicyState {
        self.inner
            .read()
            .map(|g| g.clone())
            .unwrap_or(SignerPolicyState::Absent)
    }
}

// ─────────────────────────── process-global handle ───────────────────────────

use std::sync::OnceLock;

static GLOBAL: OnceLock<SharedSignerPolicy> = OnceLock::new();

/// Install the process-global handle. Idempotent — only the first
/// install wins; subsequent calls are no-ops (kept loose for tests).
pub fn install_global(handle: SharedSignerPolicy) {
    let _ = GLOBAL.set(handle);
}

/// Read the process-global handle. Returns a freshly-default Absent
/// handle if `install_global` was never called (defensive — keeps
/// `policy_fetcher::maybe_verify_allowlist` callable from unit tests
/// that don't spin up the watcher).
pub fn global() -> SharedSignerPolicy {
    GLOBAL.get().cloned().unwrap_or_default()
}

// ─────────────────────────── watcher ───────────────────────────

/// Watcher loop. Runs until the cluster connection drops. Filters by
/// `metadata.name == SIGNER_POLICY_CM_NAME` so we don't pull every
/// ConfigMap in the controller namespace. Updates [`SharedSignerPolicy`]
/// on each apply / delete; on watch-restart (`Init` → `InitDone`) the
/// state is rebuilt atomically (cleared if no matching object is
/// observed).
pub async fn run(
    client: kube::Client,
    namespace: String,
    shared: SharedSignerPolicy,
) -> anyhow::Result<()> {
    use futures::TryStreamExt;
    use kube::Api;
    use kube::runtime::watcher::{self, Config, Event};

    let api: Api<ConfigMap> = Api::namespaced(client, &namespace);
    let cfg = Config::default().fields(&format!("metadata.name={SIGNER_POLICY_CM_NAME}"));

    tracing::info!(
        namespace = %namespace,
        name = %SIGNER_POLICY_CM_NAME,
        "SignerPolicy watcher: starting",
    );

    let mut stream = std::pin::pin!(watcher::watcher(api, cfg));

    // During an initial-list pass we buffer "did we see the singleton?"
    // and only commit at InitDone — that way a controller restart
    // observing an empty cluster atomically transitions to Absent
    // without a brief Malformed-from-stale-state flap.
    let mut init_saw_object = false;
    let mut in_init = false;

    while let Some(ev) = stream.try_next().await? {
        match ev {
            Event::Init => {
                in_init = true;
                init_saw_object = false;
            }
            Event::InitApply(cm) => {
                init_saw_object = true;
                shared.apply_configmap(&cm);
            }
            Event::InitDone => {
                if in_init && !init_saw_object {
                    shared.clear();
                }
                in_init = false;
            }
            Event::Apply(cm) => {
                shared.apply_configmap(&cm);
            }
            Event::Delete(_) => {
                tracing::info!(
                    namespace = %namespace,
                    name = %SIGNER_POLICY_CM_NAME,
                    "SignerPolicy ConfigMap deleted; falling back to env-var emergency override",
                );
                shared.clear();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::api::ObjectMeta;
    use std::collections::BTreeMap;

    fn cm_with(data: Option<BTreeMap<String, String>>) -> ConfigMap {
        ConfigMap {
            metadata: ObjectMeta {
                name: Some(SIGNER_POLICY_CM_NAME.into()),
                namespace: Some("azureclaw-system".into()),
                ..Default::default()
            },
            data,
            ..Default::default()
        }
    }

    fn good_data() -> BTreeMap<String, String> {
        let mut d = BTreeMap::new();
        d.insert(
            KEY_FULCIO_ISSUERS.into(),
            "https://token.actions.githubusercontent.com\n\
             https://login.microsoftonline.com/abc/v2.0\n"
                .into(),
        );
        d.insert(
            KEY_SAN_PATTERNS.into(),
            "https://github.com/Azure/azureclaw/.github/workflows/*.yml@*\n\
             signer@example.com\n"
                .into(),
        );
        d
    }

    #[test]
    fn parse_valid_configmap() {
        let p = parse_configmap(&cm_with(Some(good_data()))).expect("parses");
        assert_eq!(p.fulcio_issuers.len(), 2);
        assert_eq!(p.san_patterns.len(), 2);
        assert!(p.is_configured());
    }

    #[test]
    fn parse_strips_comments_and_blank_lines() {
        let mut d = BTreeMap::new();
        d.insert(
            KEY_FULCIO_ISSUERS.into(),
            "# comment line\n\nhttps://issuer.example\n   \n".into(),
        );
        d.insert(KEY_SAN_PATTERNS.into(), "signer@example.com\n".into());
        let p = parse_configmap(&cm_with(Some(d))).expect("parses");
        assert_eq!(p.fulcio_issuers, vec!["https://issuer.example".to_string()]);
    }

    #[test]
    fn parse_rejects_missing_data_block() {
        let err = parse_configmap(&cm_with(None)).unwrap_err();
        assert_eq!(err, SignerPolicyError::NoData);
    }

    #[test]
    fn parse_rejects_missing_fulcio_issuers_key() {
        let mut d = BTreeMap::new();
        d.insert(KEY_SAN_PATTERNS.into(), "signer@example.com".into());
        let err = parse_configmap(&cm_with(Some(d))).unwrap_err();
        assert_eq!(err, SignerPolicyError::MissingKey(KEY_FULCIO_ISSUERS));
    }

    #[test]
    fn parse_rejects_missing_san_patterns_key() {
        let mut d = BTreeMap::new();
        d.insert(KEY_FULCIO_ISSUERS.into(), "https://issuer.example".into());
        let err = parse_configmap(&cm_with(Some(d))).unwrap_err();
        assert_eq!(err, SignerPolicyError::MissingKey(KEY_SAN_PATTERNS));
    }

    #[test]
    fn parse_rejects_empty_issuer_list_after_trimming() {
        let mut d = BTreeMap::new();
        d.insert(KEY_FULCIO_ISSUERS.into(), "# only comments\n\n".into());
        d.insert(KEY_SAN_PATTERNS.into(), "signer@example.com".into());
        let err = parse_configmap(&cm_with(Some(d))).unwrap_err();
        assert_eq!(
            err,
            SignerPolicyError::EmptyKey {
                key: KEY_FULCIO_ISSUERS
            }
        );
    }

    #[test]
    fn parse_rejects_non_url_issuer() {
        let mut d = BTreeMap::new();
        d.insert(KEY_FULCIO_ISSUERS.into(), "not-a-url".into());
        d.insert(KEY_SAN_PATTERNS.into(), "signer@example.com".into());
        let err = parse_configmap(&cm_with(Some(d))).unwrap_err();
        assert!(
            matches!(err, SignerPolicyError::InvalidEntry { key, .. } if key == KEY_FULCIO_ISSUERS)
        );
    }

    #[test]
    fn parse_rejects_san_with_whitespace() {
        let mut d = BTreeMap::new();
        d.insert(KEY_FULCIO_ISSUERS.into(), "https://issuer.example".into());
        d.insert(KEY_SAN_PATTERNS.into(), "has space@example.com".into());
        let err = parse_configmap(&cm_with(Some(d))).unwrap_err();
        // Whitespace inside an entry: the line is the trimmed form
        // "has space@example.com"; `validate_san` rejects embedded
        // whitespace. (Surrounding whitespace is trimmed by `parse_list`.)
        assert!(
            matches!(err, SignerPolicyError::InvalidEntry { key, .. } if key == KEY_SAN_PATTERNS)
        );
    }

    #[test]
    fn shared_apply_then_snapshot_reflects_configmap() {
        let s = SharedSignerPolicy::new();
        s.apply_configmap(&cm_with(Some(good_data())));
        match s.snapshot() {
            SignerPolicyState::FromConfigMap(p) => {
                assert!(p.is_configured());
            }
            other => panic!("expected FromConfigMap, got {other:?}"),
        }
    }

    #[test]
    fn shared_apply_with_malformed_sets_malformed_state() {
        let s = SharedSignerPolicy::new();
        s.apply_configmap(&cm_with(None));
        assert!(matches!(s.snapshot(), SignerPolicyState::Malformed(_)));
    }

    #[test]
    fn shared_clear_returns_to_absent() {
        let s = SharedSignerPolicy::new();
        s.apply_configmap(&cm_with(Some(good_data())));
        s.clear();
        assert!(matches!(s.snapshot(), SignerPolicyState::Absent));
    }

    #[test]
    fn shared_default_is_absent() {
        let s = SharedSignerPolicy::default();
        assert!(matches!(s.snapshot(), SignerPolicyState::Absent));
    }

    #[test]
    fn shared_clone_shares_state() {
        let s = SharedSignerPolicy::new();
        let s2 = s.clone();
        s.apply_configmap(&cm_with(Some(good_data())));
        assert!(matches!(s2.snapshot(), SignerPolicyState::FromConfigMap(_)));
    }

    // ─── Slice 1c.6: ed25519 grant-lane forward-compat ───

    /// Raw 32-byte all-zero Ed25519 pubkey base64 (44 chars + padding).
    const RAW_PUBKEY_B64: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    /// DER/PKIX SPKI-wrapped Ed25519 pubkey (44 raw bytes → 60 base64
    /// chars + padding). Header `30 2a 30 05 06 03 2b 65 70 03 21 00`
    /// is the standard `id-Ed25519` SubjectPublicKeyInfo prefix.
    const DER_PUBKEY_B64: &str = "MCowBQYDK2VwAyEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn good_data_with_ed25519(ed: &str) -> BTreeMap<String, String> {
        let mut d = good_data();
        d.insert(KEY_ED25519_KEYS.into(), ed.into());
        d
    }

    #[test]
    fn ed25519_absent_yields_empty_vec() {
        let p = parse_configmap(&cm_with(Some(good_data()))).expect("parses");
        assert!(p.ed25519_keys.is_empty());
    }

    #[test]
    fn ed25519_empty_string_yields_empty_vec() {
        let p = parse_configmap(&cm_with(Some(good_data_with_ed25519("   \n  ")))).expect("parses");
        assert!(p.ed25519_keys.is_empty());
    }

    #[test]
    fn ed25519_empty_array_yields_empty_vec() {
        let p = parse_configmap(&cm_with(Some(good_data_with_ed25519("[]")))).expect("parses");
        assert!(p.ed25519_keys.is_empty());
    }

    #[test]
    fn ed25519_parses_single_raw_key() {
        let json = format!(
            r#"[{{"id":"signer-a","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":["egress-approval"]}}]"#
        );
        let p = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).expect("parses");
        assert_eq!(p.ed25519_keys.len(), 1);
        assert_eq!(p.ed25519_keys[0].id, "signer-a");
        assert_eq!(p.ed25519_keys[0].allowed_subjects, vec!["egress-approval"]);
    }

    #[test]
    fn ed25519_parses_der_key() {
        let json = format!(
            r#"[{{"id":"signer-b","publicKeyBase64":"{DER_PUBKEY_B64}","allowedSubjects":["egress-approval"]}}]"#
        );
        parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).expect("parses DER form");
    }

    #[test]
    fn ed25519_parses_multiple_keys() {
        let json = format!(
            r#"[
                {{"id":"signer-a","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":["egress-approval"]}},
                {{"id":"signer-b","publicKeyBase64":"{DER_PUBKEY_B64}","allowedSubjects":["egress-approval","future-grant"]}}
            ]"#
        );
        let p = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).expect("parses");
        assert_eq!(p.ed25519_keys.len(), 2);
    }

    #[test]
    fn ed25519_rejects_malformed_json() {
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519("[{]")))).unwrap_err();
        assert!(matches!(err, SignerPolicyError::Ed25519JsonParse(_)));
    }

    #[test]
    fn ed25519_rejects_non_array_json() {
        let json = format!(
            r#"{{"id":"signer-a","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":[]}}"#
        );
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).unwrap_err();
        assert!(matches!(err, SignerPolicyError::Ed25519NotArray));
    }

    #[test]
    fn ed25519_rejects_empty_id() {
        let json =
            format!(r#"[{{"id":"","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":[]}}]"#);
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).unwrap_err();
        match err {
            SignerPolicyError::Ed25519InvalidEntry { field: "id", .. } => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn ed25519_rejects_non_dns_label_id() {
        let json = format!(
            r#"[{{"id":"Signer_A","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":[]}}]"#
        );
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).unwrap_err();
        match err {
            SignerPolicyError::Ed25519InvalidEntry { field: "id", .. } => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn ed25519_rejects_long_id() {
        let long_id = "a".repeat(64);
        let json = format!(
            r#"[{{"id":"{long_id}","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":[]}}]"#
        );
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).unwrap_err();
        match err {
            SignerPolicyError::Ed25519InvalidEntry { field: "id", .. } => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn ed25519_rejects_wrong_pubkey_length() {
        // 33-byte payload (32-byte raw key + extra byte) → length 33 ≠ 32 ≠ 44.
        let bad_pk = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB";
        let json =
            format!(r#"[{{"id":"signer-a","publicKeyBase64":"{bad_pk}","allowedSubjects":[]}}]"#);
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).unwrap_err();
        match err {
            SignerPolicyError::Ed25519InvalidEntry {
                field: "publicKeyBase64",
                ..
            } => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn ed25519_rejects_invalid_base64() {
        let json =
            r#"[{"id":"signer-a","publicKeyBase64":"!!!not-base64!!!","allowedSubjects":[]}]"#;
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(json)))).unwrap_err();
        match err {
            SignerPolicyError::Ed25519InvalidEntry {
                field: "publicKeyBase64",
                ..
            } => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn ed25519_rejects_empty_subject_string() {
        let json = format!(
            r#"[{{"id":"signer-a","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":["egress-approval", "  "]}}]"#
        );
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).unwrap_err();
        match err {
            SignerPolicyError::Ed25519InvalidEntry {
                field: "allowedSubjects",
                ..
            } => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn ed25519_rejects_duplicate_ids() {
        let json = format!(
            r#"[
                {{"id":"signer-a","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":[]}},
                {{"id":"signer-a","publicKeyBase64":"{DER_PUBKEY_B64}","allowedSubjects":[]}}
            ]"#
        );
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).unwrap_err();
        assert!(matches!(err, SignerPolicyError::Ed25519DuplicateId(id) if id == "signer-a"));
    }

    #[test]
    fn ed25519_rejects_unknown_field() {
        let json = format!(
            r#"[{{"id":"signer-a","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":[],"foo":"bar"}}]"#
        );
        let err = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).unwrap_err();
        // serde_json's deny_unknown_fields surfaces as Ed25519InvalidEntry with the parse error message.
        assert!(matches!(
            err,
            SignerPolicyError::Ed25519InvalidEntry {
                field: "<entry>",
                ..
            }
        ));
    }

    #[test]
    fn shared_ed25519_keys_accessor_round_trips() {
        let s = SharedSignerPolicy::new();
        let json = format!(
            r#"[{{"id":"signer-a","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":["egress-approval"]}}]"#
        );
        s.apply_configmap(&cm_with(Some(good_data_with_ed25519(&json))));
        let keys = s.ed25519_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].id, "signer-a");
    }

    #[test]
    fn shared_ed25519_keys_empty_on_absent() {
        let s = SharedSignerPolicy::default();
        assert!(s.ed25519_keys().is_empty());
    }

    #[test]
    fn shared_ed25519_keys_empty_on_malformed() {
        let s = SharedSignerPolicy::new();
        s.apply_configmap(&cm_with(Some(good_data_with_ed25519("[{]"))));
        assert!(s.ed25519_keys().is_empty());
        assert!(matches!(s.snapshot(), SignerPolicyState::Malformed(_)));
    }

    #[test]
    fn signer_policy_config_from_surfaces_ed25519() {
        let json = format!(
            r#"[{{"id":"signer-a","publicKeyBase64":"{RAW_PUBKEY_B64}","allowedSubjects":["egress-approval"]}}]"#
        );
        let p = parse_configmap(&cm_with(Some(good_data_with_ed25519(&json)))).expect("parses");
        let cfg: crate::policy_fetcher::SignerPolicyConfig = p.into();
        assert_eq!(cfg.ed25519_keys.len(), 1);
        assert_eq!(cfg.ed25519_keys[0].id, "signer-a");
    }
}
