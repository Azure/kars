// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Egress-allowlist canonical-form parser + `PolicyKind` impl.
//!
//! This is the **reference implementation** of the per-kind
//! canonical-form contract — egress shipped first (Slice S12.b),
//! its bytes are frozen at v1, and every other policy kind added in
//! Slices 1c.2 - 1c.5 follows the rules established here.
//!
//! Slice 1c.1 extracted this module from the previous inline
//! `controller/src/policy_fetcher.rs::canonical` sub-module. The
//! parser logic is byte-identical to the pre-1c.1 implementation;
//! the egress-specific verified-output types
//! ([`VerifiedAllowlist`], [`CanonicalEndpoint`]) moved with it. The
//! pre-1c.1 path remains accessible via the `pub use` re-exports in
//! [`crate::policy_fetcher`] so existing reconciler call sites
//! continue to work unchanged.
//!
//! See `docs/internal/policy-canonical-format.md` §1 for the
//! authoritative byte-stable rules this parser enforces.

use super::{CachedValue, PolicyKind};
use crate::policy_fetcher::{CACHE_TTL, FetchError};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

/// OCI media type for the v1 egress-allowlist artifact. The pulled
/// `artifactType` MUST match this exactly; consumers reject any other
/// value (forward-compat: v2 bumps the suffix; v1 consumers MUST refuse
/// v2 artifacts — see canonical-format doc §"Forward compatibility").
pub const EGRESS_ALLOWLIST_V1_MEDIA_TYPE: &str =
    "application/vnd.azureclaw.egress-allowlist.v1+yaml";

/// Pinned canonical apiVersion / kind values for v1.
const CANONICAL_API_VERSION: &str = "azureclaw.dev/v1alpha1";
const CANONICAL_KIND: &str = "EgressAllowlist";

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

/// `PolicyKind` discriminator for the egress allowlist.
pub struct EgressKind;

impl PolicyKind for EgressKind {
    const MEDIA_TYPE: &'static str = EGRESS_ALLOWLIST_V1_MEDIA_TYPE;
    const API_VERSION: &'static str = CANONICAL_API_VERSION;
    const KIND: &'static str = CANONICAL_KIND;
    type Output = VerifiedAllowlist;

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
            CachedValue::Egress(v) => Some(v.clone()),
            CachedValue::Inference(_) | CachedValue::Tools(_) => None,
        }
    }

    fn cache_put(key: String, value: Self::Output) {
        if let Ok(mut guard) = cache().lock() {
            guard.insert(
                key,
                CacheEntry {
                    verified: CachedValue::Egress(value),
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
//
// Implements the byte-stable rules in `docs/internal/policy-canonical-format.md` §1.
// Invoked **after** cosign signature verification — re-validates that
// the bytes are canonical (sorted, IDNA-normalized, deduplicated,
// generation present). Any deviation returns
// [`FetchError::CanonicalFormViolation`].
//
// Parses with `serde_yaml` for structural deserialization, then
// independently re-checks the byte-level invariants that
// `serde_yaml` would silently tolerate (key order, sort order,
// duplicate detection, etc.). This catches the case where a
// producer signs structurally-valid-but-non-canonical bytes —
// verification still rejects them.

/// Parse + canonical-form re-validate. The returned
/// [`VerifiedAllowlist::digest`] is empty here; the caller fills it
/// from the verified `OciArtifactRef.digest`.
pub(crate) fn parse(bytes: &[u8]) -> Result<VerifiedAllowlist, FetchError> {
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

#[cfg(test)]
mod tests {
    //! 1c.1 invariant tests: byte-identical parser behavior. These
    //! tests assert the egress canonical bytes accepted/rejected
    //! pre-1c.1 are still accepted/rejected post-1c.1 after the
    //! module extraction.

    use super::*;

    fn good_doc() -> &'static str {
        "apiVersion: azureclaw.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - host: example.com\n      port: 443\n"
    }

    #[test]
    fn parse_accepts_canonical_bytes() {
        let v = parse(good_doc().as_bytes()).expect("canonical parse should succeed");
        assert_eq!(v.api_version, CANONICAL_API_VERSION);
        assert_eq!(v.kind, CANONICAL_KIND);
        assert_eq!(v.generation, 1);
        assert_eq!(v.endpoints.len(), 1);
        assert_eq!(v.endpoints[0].host, "example.com");
        assert_eq!(v.endpoints[0].port, 443);
        assert!(v.digest.is_empty());
        assert_eq!(v.fetched_at, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn finalize_stamps_digest_and_time() {
        let mut v = parse(good_doc().as_bytes()).unwrap();
        let t = SystemTime::now();
        EgressKind::finalize(
            &mut v,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            t,
        );
        assert!(v.digest.starts_with("sha256:"));
        assert_eq!(v.fetched_at, t);
    }

    #[test]
    fn rejects_crlf() {
        let bytes = good_doc().replace('\n', "\r\n");
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_missing_trailing_newline() {
        let bytes = good_doc().trim_end_matches('\n').to_string();
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_comments() {
        let bytes = format!("# hi\n{}", good_doc());
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_wrong_api_version() {
        let bytes = good_doc().replace("v1alpha1", "v9");
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_wrong_kind() {
        let bytes = good_doc().replace("EgressAllowlist", "ToolPolicy");
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_zero_generation() {
        let bytes = good_doc().replace("generation: 1", "generation: 0");
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_unsorted_endpoints() {
        let bytes = "apiVersion: azureclaw.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - host: z.example.com\n      port: 443\n    - host: a.example.com\n      port: 443\n";
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_duplicate_endpoints() {
        let bytes = "apiVersion: azureclaw.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - host: a.example.com\n      port: 443\n    - host: a.example.com\n      port: 443\n";
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_wildcard_host() {
        let bytes = good_doc().replace("example.com", "*.example.com");
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_uppercase_host() {
        let bytes = good_doc().replace("example.com", "Example.com");
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_port_out_of_range() {
        let bytes = good_doc().replace("port: 443", "port: 99999");
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_extra_endpoint_key() {
        let bytes = "apiVersion: azureclaw.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - host: a.example.com\n      port: 443\n      protocol: tcp\n";
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn rejects_endpoint_key_wrong_order() {
        let bytes = "apiVersion: azureclaw.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - port: 443\n      host: a.example.com\n";
        let err = parse(bytes.as_bytes()).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }

    #[test]
    fn cache_roundtrip() {
        EgressKind::cache_clear();
        let v = parse(good_doc().as_bytes()).unwrap();
        EgressKind::cache_put("k1".into(), v.clone());
        let hit = EgressKind::cache_get("k1", Instant::now()).expect("cache hit");
        assert_eq!(hit.endpoints, v.endpoints);
        EgressKind::cache_clear();
        assert!(EgressKind::cache_get("k1", Instant::now()).is_none());
    }
}
