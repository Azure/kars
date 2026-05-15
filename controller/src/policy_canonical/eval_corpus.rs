// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `ClawEval` corpus canonical-form `PolicyKind` impl.
//!
//! Wires the shared eval-corpus parser (the
//! [`azureclaw_eval_corpus`] crate) into the kind-agnostic
//! [`crate::policy_fetcher::fetch_and_verify_generic`] pipeline so
//! operator-supplied signed corpora (`ClawEval.spec.corpora[].bundleRef`)
//! pull through the same OCI + cosign trust root as every other policy
//! kind. The reconciler consumer lands in slice 6.3.
//!
//! Built-in corpora ([`azureclaw_eval_corpus::BUILTIN_NAMES`]) **do not**
//! travel this path — they are compiled into the runner image and
//! verified against the AzureClaw release public key by the runner
//! itself. This `PolicyKind` impl exists exclusively for the
//! `bundleRef` lane.

// Slice 6.1 shipped the parser, PolicyKind impl, and cache for the
// eval-corpus signing pipeline. The bundleRef consumer (the `ClawEval`
// reconciler that calls `fetch_and_verify_generic::<EvalCorpusKind>`)
// lands in slice 6.3. Note: the `Corpus`/`parse`/`judge` parts of the
// library are NOT dead — they are consumed by the `conformance-runner`
// workspace crate (slice 6.2) and the `claw_eval_reconciler` (slice 6.3).

use super::{CachedValue, PolicyKind};
use crate::policy_fetcher::{CACHE_TTL, FetchError};
use azureclaw_eval_corpus::{Corpus, ParseError, parse};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

/// OCI media type for the v1 eval-corpus artifact. v2 corpora MUST
/// bump the suffix; v1 consumers MUST refuse v2 artifacts so a
/// schema-incompatible bundle cannot be silently downgraded.
pub const EVAL_CORPUS_V1_MEDIA_TYPE: &str = "application/vnd.azureclaw.eval-corpus.v1+json";

/// A verified eval-corpus bundle. Wraps [`Corpus`] with the signing-
/// pipeline accounting that every kind carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEvalCorpus {
    /// The parsed corpus content. Identical bytes pulled from OCI.
    pub corpus: Corpus,
    /// Raw signed bytes. Kept for echo to the router (the corpus
    /// digest published on `/internal/policy-status` is computed over
    /// these bytes verbatim).
    pub bytes: Vec<u8>,
    /// `sha256:...` matched against `OciArtifactRef.digest`.
    pub digest: String,
    pub fetched_at: SystemTime,
}

/// `PolicyKind` discriminator for eval-corpus bundles.
pub struct EvalCorpusKind;

impl PolicyKind for EvalCorpusKind {
    const MEDIA_TYPE: &'static str = EVAL_CORPUS_V1_MEDIA_TYPE;
    const API_VERSION: &'static str = "azureclaw.azure.com/v1alpha1";
    const KIND: &'static str = "ClawEval";
    type Output = VerifiedEvalCorpus;

    fn parse(bytes: &[u8]) -> Result<Self::Output, FetchError> {
        let corpus = parse(bytes)
            .map_err(|ParseError::Invalid(msg)| FetchError::CanonicalFormViolation(msg))?;
        Ok(VerifiedEvalCorpus {
            corpus,
            bytes: bytes.to_vec(),
            digest: String::new(),
            fetched_at: SystemTime::UNIX_EPOCH,
        })
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
            CachedValue::EvalCorpus(v) => Some(v.clone()),
            CachedValue::Egress(_)
            | CachedValue::Inference(_)
            | CachedValue::McpServer(_)
            | CachedValue::Memory(_)
            | CachedValue::Tools(_) => None,
        }
    }

    fn cache_put(key: String, value: Self::Output) {
        if let Ok(mut guard) = cache().lock() {
            guard.insert(
                key,
                CacheEntry {
                    verified: CachedValue::EvalCorpus(value),
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

// ─────────────────────────── tests ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use azureclaw_eval_corpus::Decision;

    fn minimal_bytes() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": "v1",
            "name": "min",
            "cases": [{
                "id": "c1", "tags": [],
                "scenario": { "kind": "EgressConnect", "host": "h.example.test", "port": 443 },
                "expect": { "decision": "Blocked" }
            }]
        }))
        .unwrap()
    }

    #[test]
    fn media_type_is_v1_json() {
        assert_eq!(
            EvalCorpusKind::MEDIA_TYPE,
            "application/vnd.azureclaw.eval-corpus.v1+json"
        );
    }

    #[test]
    fn parse_wraps_corpus_with_empty_signing_metadata() {
        let bytes = minimal_bytes();
        let out = EvalCorpusKind::parse(&bytes).expect("parse");
        assert_eq!(out.corpus.name, "min");
        assert_eq!(out.corpus.cases.len(), 1);
        assert_eq!(out.corpus.cases[0].expect.decision, Decision::Blocked);
        assert_eq!(out.bytes, bytes);
        assert!(out.digest.is_empty());
        assert_eq!(out.fetched_at, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn finalize_stamps_digest_and_fetched_at() {
        let bytes = minimal_bytes();
        let mut out = EvalCorpusKind::parse(&bytes).unwrap();
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(123_456);
        EvalCorpusKind::finalize(&mut out, "sha256:deadbeef".into(), when);
        assert_eq!(out.digest, "sha256:deadbeef");
        assert_eq!(out.fetched_at, when);
    }

    // Cache tests use a process-wide singleton; serialize via a local
    // mutex AND use unique keys per test so cache_clear() in one test
    // can never race a cache_put() in another.
    fn cache_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn cache_roundtrip_returns_within_ttl() {
        let _g = cache_test_lock();
        EvalCorpusKind::cache_clear();
        let bytes = minimal_bytes();
        let mut out = EvalCorpusKind::parse(&bytes).unwrap();
        EvalCorpusKind::finalize(&mut out, "sha256:abc".into(), SystemTime::now());
        EvalCorpusKind::cache_put("k_roundtrip".into(), out.clone());
        let hit = EvalCorpusKind::cache_get("k_roundtrip", Instant::now()).expect("cache hit");
        assert_eq!(hit, out);
    }

    #[test]
    fn cache_miss_for_unknown_key() {
        let _g = cache_test_lock();
        EvalCorpusKind::cache_clear();
        assert!(EvalCorpusKind::cache_get("not-there", Instant::now()).is_none());
    }

    #[test]
    fn cache_clear_drops_entries() {
        let _g = cache_test_lock();
        EvalCorpusKind::cache_clear();
        let bytes = minimal_bytes();
        let mut out = EvalCorpusKind::parse(&bytes).unwrap();
        EvalCorpusKind::finalize(&mut out, "sha256:abc".into(), SystemTime::now());
        EvalCorpusKind::cache_put("k_drop".into(), out);
        assert!(EvalCorpusKind::cache_get("k_drop", Instant::now()).is_some());
        EvalCorpusKind::cache_clear();
        assert!(EvalCorpusKind::cache_get("k_drop", Instant::now()).is_none());
    }

    #[test]
    fn parse_propagates_canonical_form_violations() {
        let bytes = b"{}";
        let err = EvalCorpusKind::parse(bytes).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(_)));
    }
}
