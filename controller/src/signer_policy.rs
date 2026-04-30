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
//! ```
//!
//! Both keys are **required**; both lists must be non-empty after
//! trimming and `#` comment stripping. Either being empty is a parse
//! error (`SignerPolicyError::EmptyKey`) — empty issuers + empty SANs
//! together is *not* an implicit accept-all (per S12.d trust model).
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
//!   no-env → `SignerPolicyMissing`.

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

/// Parsed signer policy. The wire shape mirrors
/// [`crate::policy_fetcher::SignerPolicyConfig`] for a zero-cost
/// conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignerPolicy {
    /// Allowed Fulcio issuer URLs. Empty → reject all.
    pub fulcio_issuers: Vec<String>,
    /// Allowed SAN glob patterns. Empty → reject all.
    pub san_patterns: Vec<String>,
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
    })
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
    // Accept either an http(s) URL (CertSubjectUrlVerifier) or a
    // generic email-like / glob string (CertSubjectEmailVerifier). The
    // only hard rule is "no embedded whitespace" — matches the
    // env-var-path's CSV split semantics.
    if s.contains(char::is_whitespace) {
        return Err("SAN must not contain whitespace");
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
            Ok(p) => SignerPolicyState::FromConfigMap(p),
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
}
