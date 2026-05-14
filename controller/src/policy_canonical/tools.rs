// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! AGT policy profile canonical-form parser + `PolicyKind` impl.
//!
//! Slice 1c.2 — second per-kind implementation after egress (1c.1).
//! Unlike egress, AGT profiles are customer-authored YAML that operators
//! write by hand: rules are documented with `#` comments, key order is
//! cosmetic, and the file shape is a rich nested mapping rather than a
//! flat allowlist. Imposing the strict byte-canonical rules egress
//! enforces (no comments, sorted keys, deduplication) would break
//! day-1 ergonomics — operators would need a separate `azureclaw
//! agt-profile canonicalize` step before every sign.
//!
//! The supply-chain guarantee is preserved without strict byte-
//! canonicalization because the OCI digest already pins the *bytes*
//! that get loaded into the router. The semantic deduplication concern
//! that justifies egress canonicalization (two equivalent allowlists
//! producing the same digest) doesn't apply: AGT profiles are signed
//! and consumed as-is — the bytes themselves are the policy, not a
//! normalized projection of them.
//!
//! What we DO enforce (trait invariant #1, applied to AGT):
//! - Valid UTF-8
//! - Parseable as a YAML mapping
//! - Required top-level keys present: `version`, `agent`, `policies`
//! - `version` is a non-empty string
//! - `agent` is a non-empty string
//! - `policies` is a sequence (possibly empty)
//!
//! Anything beyond that is delegated to the router's AGT engine
//! (`Governance::load_policies_from_dir`), which catches per-rule
//! semantic errors and surfaces them via `GET /internal/policy-status`.
//! That deliberate split keeps the controller's responsibilities
//! narrow — *supply chain verification + structural sanity* — and lets
//! the producer-consumer loop close without the controller having to
//! re-implement the AGT rule engine.

use super::{CachedValue, PolicyKind};
use crate::policy_fetcher::{CACHE_TTL, FetchError};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

/// OCI media type for the v1 AGT profile artifact. The pulled
/// `artifactType` MUST match this exactly; consumers reject any other
/// value (forward-compat: v2 bumps the suffix; v1 consumers MUST refuse
/// v2 artifacts).
pub const AGT_PROFILE_V1_MEDIA_TYPE: &str = "application/vnd.azureclaw.agt-profile.v1+yaml";

/// A verified AGT policy profile bundle. The `bytes` field is the
/// signed payload as pulled from OCI; the router consumes it verbatim
/// via the existing `Governance::load_policies_from_dir` path. The
/// `version` / `agent` fields are extracted at parse time for
/// observability + admission cross-checks (the `agent` value MUST
/// equal `ToolPolicy.metadata.name` when bundleRef is in use — see
/// reconciler).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAgtProfile {
    /// AGT schema version declared by the producer. v1 accepts `"1.0"`
    /// (the only value upstream AGT ships today). Surfaced in
    /// controller status for forward-compat diagnostics.
    pub version: String,
    /// Agent name declared inside the profile. Used by the reconciler
    /// to cross-check that the producer's intent matches the consuming
    /// ToolPolicy. A mismatch is surfaced as a Ready=False condition
    /// rather than a fetch error so operators can diagnose it without
    /// re-pulling the artifact.
    pub agent: String,
    /// Number of `policies[]` entries — for log lines + status,
    /// not used in enforcement decisions.
    pub policies_count: usize,
    /// Raw signed bytes. The reconciler writes these verbatim into the
    /// compiled-profile ConfigMap under key `agt-profile.yaml`; the
    /// router echoes the [`crate::tool_policy_compile::agt_profile_digest`]
    /// computed over these exact bytes.
    pub bytes: Vec<u8>,
    /// `sha256:...` matched against `OciArtifactRef.digest`.
    pub digest: String,
    pub fetched_at: SystemTime,
}

/// `PolicyKind` discriminator for AGT policy profiles.
pub struct ToolsKind;

impl PolicyKind for ToolsKind {
    const MEDIA_TYPE: &'static str = AGT_PROFILE_V1_MEDIA_TYPE;
    const API_VERSION: &'static str = "agentmesh.io/v1";
    const KIND: &'static str = "AgtProfile";
    type Output = VerifiedAgtProfile;

    fn parse(bytes: &[u8]) -> Result<Self::Output, FetchError> {
        parse(bytes)
    }

    fn finalize(out: &mut Self::Output, digest: String, fetched_at: SystemTime) {
        out.digest = digest;
        out.fetched_at = fetched_at;
    }

    fn cache_get(key: &str, now: Instant) -> Option<Self::Output> {
        let guard = cache().lock().ok()?;
        let entry = guard.get(key)?;
        if now.duration_since(entry.inserted) > CACHE_TTL {
            return None;
        }
        match &entry.verified {
            CachedValue::Tools(v) => Some(v.clone()),
            CachedValue::Egress(_)
            | CachedValue::Inference(_)
            | CachedValue::McpServer(_)
            | CachedValue::Memory(_) => None,
        }
    }

    fn cache_put(key: String, value: Self::Output) {
        if let Ok(mut guard) = cache().lock() {
            guard.insert(
                key,
                CacheEntry {
                    verified: CachedValue::Tools(value),
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
}

// ─────────────────────────── cache ───────────────────────────

struct CacheEntry {
    verified: CachedValue,
    inserted: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// ─────────────────────────── parser ───────────────────────────

/// Parse + structural validate the AGT profile bytes. Returns
/// [`FetchError::CanonicalFormViolation`] for any structural issue.
/// The returned [`VerifiedAgtProfile::digest`] is empty; the caller
/// fills it via [`ToolsKind::finalize`].
pub(crate) fn parse(bytes: &[u8]) -> Result<VerifiedAgtProfile, FetchError> {
    // UTF-8 — required for both YAML parsing and the eventual ConfigMap
    // write (Kubernetes ConfigMap.data is map[string]string; binaryData
    // would be a different field).
    let s = std::str::from_utf8(bytes)
        .map_err(|e| FetchError::CanonicalFormViolation(format!("utf-8: {e}")))?;

    let doc: serde_yaml::Value = serde_yaml::from_str(s)
        .map_err(|e| FetchError::CanonicalFormViolation(format!("yaml parse: {e}")))?;

    let map = doc
        .as_mapping()
        .ok_or_else(|| FetchError::CanonicalFormViolation("top-level must be a mapping".into()))?;

    let version = map
        .get(serde_yaml::Value::String("version".into()))
        .ok_or_else(|| FetchError::CanonicalFormViolation("missing `version`".into()))?
        .as_str()
        .ok_or_else(|| FetchError::CanonicalFormViolation("`version` must be a string".into()))?
        .to_string();
    if version.is_empty() {
        return Err(FetchError::CanonicalFormViolation(
            "`version` must be non-empty".into(),
        ));
    }

    let agent = map
        .get(serde_yaml::Value::String("agent".into()))
        .ok_or_else(|| FetchError::CanonicalFormViolation("missing `agent`".into()))?
        .as_str()
        .ok_or_else(|| FetchError::CanonicalFormViolation("`agent` must be a string".into()))?
        .to_string();
    if agent.is_empty() {
        return Err(FetchError::CanonicalFormViolation(
            "`agent` must be non-empty".into(),
        ));
    }

    let policies = map
        .get(serde_yaml::Value::String("policies".into()))
        .ok_or_else(|| FetchError::CanonicalFormViolation("missing `policies`".into()))?;
    let policies_seq = policies.as_sequence().ok_or_else(|| {
        FetchError::CanonicalFormViolation("`policies` must be a sequence".into())
    })?;
    let policies_count = policies_seq.len();

    Ok(VerifiedAgtProfile {
        version,
        agent,
        policies_count,
        bytes: bytes.to_vec(),
        digest: String::new(),
        fetched_at: SystemTime::UNIX_EPOCH,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_ok() {
        let yaml = b"version: \"1.0\"\nagent: test\npolicies: []\n";
        let r = parse(yaml).unwrap();
        assert_eq!(r.version, "1.0");
        assert_eq!(r.agent, "test");
        assert_eq!(r.policies_count, 0);
        assert_eq!(r.bytes, yaml);
        assert_eq!(r.digest, "");
    }

    #[test]
    fn parse_with_one_policy_ok() {
        let yaml = br#"version: "1.0"
agent: my-agent
policies:
  - name: rule-1
    type: capability
    allowed_actions:
      - "shell:ls"
    priority: 10
"#;
        let r = parse(yaml).unwrap();
        assert_eq!(r.agent, "my-agent");
        assert_eq!(r.policies_count, 1);
    }

    #[test]
    fn parse_with_comments_ok() {
        // AGT profiles allow comments (unlike egress) — operators author by hand.
        let yaml = br#"# top-level comment
version: "1.0"  # inline comment
agent: with-comments
policies:
  # rule documentation
  - name: r
    type: capability
    allowed_actions: ["shell:ls"]
"#;
        let r = parse(yaml).unwrap();
        assert_eq!(r.agent, "with-comments");
        assert_eq!(r.policies_count, 1);
    }

    #[test]
    fn parse_rejects_non_utf8() {
        let bytes = b"\xff\xfeversion: 1.0\n";
        let err = parse(bytes).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("utf-8")));
    }

    #[test]
    fn parse_rejects_missing_version() {
        let yaml = b"agent: test\npolicies: []\n";
        let err = parse(yaml).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("version")),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn parse_rejects_empty_version() {
        let yaml = b"version: \"\"\nagent: test\npolicies: []\n";
        let err = parse(yaml).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-empty")),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn parse_rejects_missing_agent() {
        let yaml = b"version: \"1.0\"\npolicies: []\n";
        let err = parse(yaml).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("agent")));
    }

    #[test]
    fn parse_rejects_empty_agent() {
        let yaml = b"version: \"1.0\"\nagent: \"\"\npolicies: []\n";
        let err = parse(yaml).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-empty"))
        );
    }

    #[test]
    fn parse_rejects_missing_policies() {
        let yaml = b"version: \"1.0\"\nagent: test\n";
        let err = parse(yaml).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("policies")));
    }

    #[test]
    fn parse_rejects_policies_not_sequence() {
        let yaml = b"version: \"1.0\"\nagent: test\npolicies: not-a-list\n";
        let err = parse(yaml).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("sequence")));
    }

    #[test]
    fn parse_rejects_non_mapping_top_level() {
        let yaml = b"- not\n- a\n- mapping\n";
        let err = parse(yaml).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("mapping")));
    }

    #[test]
    fn parse_rejects_invalid_yaml() {
        let yaml = b"version: \"1.0\nagent: unclosed-quote\n";
        let err = parse(yaml).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("yaml")),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn parse_preserves_raw_bytes() {
        // Reconciler writes `bytes` verbatim into the ConfigMap; the
        // router echoes the digest computed over these exact bytes.
        // Verify any structural valid input round-trips bytes unchanged.
        let yaml = b"version: \"1.0\"\nagent: byte-preserve\npolicies: []\n";
        let r = parse(yaml).unwrap();
        assert_eq!(r.bytes.as_slice(), yaml);
    }

    #[test]
    fn finalize_stamps_digest_and_time() {
        let mut r = parse(b"version: \"1.0\"\nagent: f\npolicies: []\n").unwrap();
        let now = SystemTime::now();
        ToolsKind::finalize(&mut r, "sha256:abc".into(), now);
        assert_eq!(r.digest, "sha256:abc");
        assert_eq!(r.fetched_at, now);
    }

    #[test]
    fn cache_roundtrip() {
        ToolsKind::cache_clear();
        let mut v = parse(b"version: \"1.0\"\nagent: c\npolicies: []\n").unwrap();
        ToolsKind::finalize(&mut v, "sha256:cached".into(), SystemTime::now());
        ToolsKind::cache_put("k1".into(), v.clone());
        let hit = ToolsKind::cache_get("k1", Instant::now()).expect("cache hit");
        assert_eq!(hit.digest, "sha256:cached");
        assert_eq!(hit.agent, "c");
    }

    #[test]
    fn cache_isolated_per_kind() {
        // Sanity: a key inserted into the tools cache MUST NOT collide
        // with the egress cache (different OnceLock backing stores).
        ToolsKind::cache_clear();
        crate::policy_canonical::egress::EgressKind::cache_clear();
        let mut v = parse(b"version: \"1.0\"\nagent: iso\npolicies: []\n").unwrap();
        ToolsKind::finalize(&mut v, "sha256:tools-only".into(), SystemTime::now());
        ToolsKind::cache_put("shared-key".into(), v);
        // Egress cache MUST miss — different per-kind static slot.
        assert!(
            crate::policy_canonical::egress::EgressKind::cache_get("shared-key", Instant::now())
                .is_none()
        );
    }

    #[test]
    fn media_type_v1() {
        assert_eq!(
            ToolsKind::MEDIA_TYPE,
            "application/vnd.azureclaw.agt-profile.v1+yaml"
        );
    }
}
