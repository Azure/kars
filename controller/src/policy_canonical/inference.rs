// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! InferencePolicy canonical-form parser + `PolicyKind` impl.
//!
//! Slice 1c.3 — third per-kind implementation after egress (1c.1) and
//! tools (1c.2). Unlike AGT profiles (which are operator-authored YAML
//! with comments), inference-policy bundles are content artifacts the
//! producer compiles before signing: the canonical bytes the controller
//! consumes from the OCI artifact are the **same byte-frozen JSON
//! shape** the controller's own inline path produces via
//! [`crate::inference_policy_compile`].
//!
//! That symmetry is intentional: the router's
//! [`inference-router::inference_policy_loader`] consumes a single
//! `inference-policy.json` file regardless of whether it was produced
//! from inline spec fields or pulled from a signed OCI artifact. The
//! length-prefixed sha256 digest (see
//! [`crate::inference_policy_compile::inference_policy_digest`]) closes
//! the §3 echo loop either way.
//!
//! What we DO enforce (trait invariant #1, applied to inference policy):
//! - Valid UTF-8
//! - Parseable as a JSON object
//! - Top-level object with policy-content keys only:
//!   `tokenBudget` / `contentSafety` / `modelPreference` / `displayName`
//! - Each present sub-object has the expected shape (sub-object or null)
//! - At least one policy-content key is present (empty bundles are
//!   rejected — the inline path is the supported way to express
//!   "selector-only" policies; a signed bundle MUST carry policy
//!   content)
//!
//! Anything beyond that is delegated to the router's enforcement layer
//! (`inference_policy_loader::reload`, per-axis enforcement gates), which
//! catches semantic errors (e.g. unknown severity strings, negative
//! budgets) and surfaces them via `GET /internal/policy-status`. This
//! split mirrors 1c.2 (tools): the controller's responsibility is
//! supply-chain verification + structural sanity, not policy semantics.

use super::{CachedValue, PolicyKind};
use crate::policy_fetcher::{CACHE_TTL, FetchError};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

/// OCI media type for the v1 inference-policy artifact. The pulled
/// `artifactType` MUST match this exactly; consumers reject any other
/// value (forward-compat: v2 bumps the suffix; v1 consumers MUST refuse
/// v2 artifacts).
pub const INFERENCE_POLICY_V1_MEDIA_TYPE: &str =
    "application/vnd.azureclaw.inference-policy.v1+json";

/// Recognised top-level keys in an inference-policy bundle. The
/// selector (`appliesTo`) is intentionally NOT one of them: selector
/// is owned by the `InferencePolicy.spec`, not the signed artifact, so
/// one signed bundle can be referenced by multiple CRs with different
/// selectors. See `docs/internal/policy-canonical-format.md`.
const POLICY_CONTENT_KEYS: &[&str] = &[
    "tokenBudget",
    "contentSafety",
    "modelPreference",
    "displayName",
];

/// A verified inference-policy bundle. The `bytes` field is the
/// signed payload as pulled from OCI; the reconciler merges the parsed
/// content into the compile shape before writing the ConfigMap. The
/// individual `Option<Value>` fields are extracted at parse time so
/// the reconciler does not re-parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedInferencePolicy {
    /// Parsed `tokenBudget` sub-object, if present (and non-null).
    pub token_budget: Option<Value>,
    /// Parsed `contentSafety` sub-object, if present (and non-null).
    pub content_safety: Option<Value>,
    /// Parsed `modelPreference` sub-object, if present (and non-null).
    pub model_preference: Option<Value>,
    /// Parsed `displayName` string, if present (and non-null).
    pub display_name: Option<String>,
    /// Raw signed bytes. Kept for observability / status diagnostics;
    /// the router echo digest is computed over the **recompiled**
    /// bytes (selector + content merged), not these.
    pub bytes: Vec<u8>,
    /// `sha256:...` matched against `OciArtifactRef.digest`.
    pub digest: String,
    pub fetched_at: SystemTime,
}

/// `PolicyKind` discriminator for inference-policy bundles.
pub struct InferenceKind;

impl PolicyKind for InferenceKind {
    const MEDIA_TYPE: &'static str = INFERENCE_POLICY_V1_MEDIA_TYPE;
    const API_VERSION: &'static str = "azureclaw.azure.com/v1alpha1";
    const KIND: &'static str = "InferencePolicy";
    type Output = VerifiedInferencePolicy;

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
            CachedValue::Inference(v) => Some(v.clone()),
            CachedValue::Egress(_) | CachedValue::Tools(_) => None,
        }
    }

    fn cache_put(key: String, value: Self::Output) {
        if let Ok(mut guard) = cache().lock() {
            guard.insert(
                key,
                CacheEntry {
                    verified: CachedValue::Inference(value),
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

/// Parse + structural validate the inference-policy bundle bytes.
/// Returns [`FetchError::CanonicalFormViolation`] for any structural
/// issue. The returned [`VerifiedInferencePolicy::digest`] is empty;
/// the caller fills it via [`InferenceKind::finalize`].
pub(crate) fn parse(bytes: &[u8]) -> Result<VerifiedInferencePolicy, FetchError> {
    let s = std::str::from_utf8(bytes)
        .map_err(|e| FetchError::CanonicalFormViolation(format!("utf-8: {e}")))?;

    let doc: Value = serde_json::from_str(s)
        .map_err(|e| FetchError::CanonicalFormViolation(format!("json parse: {e}")))?;

    let map = doc.as_object().ok_or_else(|| {
        FetchError::CanonicalFormViolation("top-level must be a JSON object".into())
    })?;

    for key in map.keys() {
        if !POLICY_CONTENT_KEYS.contains(&key.as_str()) {
            return Err(FetchError::CanonicalFormViolation(format!(
                "unrecognised top-level key `{key}`; allowed: tokenBudget, \
                 contentSafety, modelPreference, displayName"
            )));
        }
    }

    let token_budget = extract_optional_object(map, "tokenBudget")?;
    let content_safety = extract_optional_object(map, "contentSafety")?;
    let model_preference = extract_optional_object(map, "modelPreference")?;
    let display_name = extract_optional_string(map, "displayName")?;

    if token_budget.is_none()
        && content_safety.is_none()
        && model_preference.is_none()
        && display_name.is_none()
    {
        return Err(FetchError::CanonicalFormViolation(
            "bundle has no policy content (all of tokenBudget / contentSafety / \
             modelPreference / displayName are absent or null); use the inline \
             path for selector-only policies"
                .into(),
        ));
    }

    if let Some(ref tb) = token_budget {
        validate_token_budget(tb)?;
    }
    if let Some(ref cs) = content_safety {
        validate_content_safety(cs)?;
    }
    if let Some(ref mp) = model_preference {
        validate_model_preference(mp)?;
    }

    Ok(VerifiedInferencePolicy {
        token_budget,
        content_safety,
        model_preference,
        display_name,
        bytes: bytes.to_vec(),
        digest: String::new(),
        fetched_at: SystemTime::UNIX_EPOCH,
    })
}

fn extract_optional_object(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<Value>, FetchError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(_)) => Ok(Some(map[key].clone())),
        Some(_) => Err(FetchError::CanonicalFormViolation(format!(
            "`{key}` must be a JSON object or null"
        ))),
    }
}

fn extract_optional_string(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, FetchError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(FetchError::CanonicalFormViolation(format!(
            "`{key}` must be a string or null"
        ))),
    }
}

fn validate_token_budget(v: &Value) -> Result<(), FetchError> {
    let obj = v.as_object().expect("checked by caller");
    for k in ["perRequestTokens", "dailyTokens", "monthlyTokens"] {
        if let Some(val) = obj.get(k)
            && !val.is_null()
            && !val.is_u64()
        {
            return Err(FetchError::CanonicalFormViolation(format!(
                "tokenBudget.{k} must be a non-negative integer or null"
            )));
        }
    }
    Ok(())
}

fn validate_content_safety(v: &Value) -> Result<(), FetchError> {
    let obj = v.as_object().expect("checked by caller");
    for k in ["hate", "selfHarm", "sexual", "violence"] {
        if let Some(val) = obj.get(k)
            && !val.is_null()
            && !val.is_string()
        {
            return Err(FetchError::CanonicalFormViolation(format!(
                "contentSafety.{k} must be a string or null"
            )));
        }
    }
    if let Some(val) = obj.get("requirePromptShields")
        && !val.is_null()
        && !val.is_boolean()
    {
        return Err(FetchError::CanonicalFormViolation(
            "contentSafety.requirePromptShields must be a boolean or null".into(),
        ));
    }
    Ok(())
}

fn validate_model_preference(v: &Value) -> Result<(), FetchError> {
    let obj = v.as_object().expect("checked by caller");
    let primary = obj.get("primary").ok_or_else(|| {
        FetchError::CanonicalFormViolation("modelPreference.primary required".into())
    })?;
    validate_model_ref(primary, "modelPreference.primary")?;

    if let Some(fallback) = obj.get("fallback") {
        let seq = fallback.as_array().ok_or_else(|| {
            FetchError::CanonicalFormViolation("modelPreference.fallback must be an array".into())
        })?;
        for (i, entry) in seq.iter().enumerate() {
            validate_model_ref(entry, &format!("modelPreference.fallback[{i}]"))?;
        }
    }
    Ok(())
}

fn validate_model_ref(v: &Value, path: &str) -> Result<(), FetchError> {
    let obj = v
        .as_object()
        .ok_or_else(|| FetchError::CanonicalFormViolation(format!("{path} must be an object")))?;
    for k in ["provider", "deployment"] {
        let val = obj
            .get(k)
            .ok_or_else(|| FetchError::CanonicalFormViolation(format!("{path}.{k} required")))?;
        let s = val.as_str().ok_or_else(|| {
            FetchError::CanonicalFormViolation(format!("{path}.{k} must be a string"))
        })?;
        if s.is_empty() {
            return Err(FetchError::CanonicalFormViolation(format!(
                "{path}.{k} must be non-empty"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_token_budget_ok() {
        let body = br#"{"tokenBudget":{"perRequestTokens":8192}}"#;
        let r = parse(body).unwrap();
        assert!(r.token_budget.is_some());
        assert!(r.content_safety.is_none());
        assert!(r.model_preference.is_none());
        assert!(r.display_name.is_none());
        assert_eq!(r.bytes, body);
        assert_eq!(r.digest, "");
    }

    #[test]
    fn parse_full_bundle_ok() {
        let body = br#"{
            "tokenBudget":{"perRequestTokens":8192,"dailyTokens":1000000,"monthlyTokens":20000000},
            "contentSafety":{"hate":"Medium","selfHarm":"Low","sexual":"Medium","violence":"High","requirePromptShields":true},
            "modelPreference":{"primary":{"provider":"azure-openai","deployment":"gpt-4o"},"fallback":[{"provider":"anthropic","deployment":"claude-3-5-sonnet"}]},
            "displayName":"strict-policy"
        }"#;
        let r = parse(body).unwrap();
        assert!(r.token_budget.is_some());
        assert!(r.content_safety.is_some());
        assert!(r.model_preference.is_some());
        assert_eq!(r.display_name.as_deref(), Some("strict-policy"));
    }

    #[test]
    fn parse_invalid_utf8_rejected() {
        let body = b"\xFF\xFE not utf-8";
        let err = parse(body).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("utf-8")));
    }

    #[test]
    fn parse_invalid_json_rejected() {
        let body = b"not json";
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("json parse"))
        );
    }

    #[test]
    fn parse_non_object_top_level_rejected() {
        let body = b"[1, 2, 3]";
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("JSON object"))
        );
    }

    #[test]
    fn parse_unknown_key_rejected() {
        let body = br#"{"tokenBudget":{"perRequestTokens":1},"unknownKey":"x"}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("unrecognised"))
        );
    }

    #[test]
    fn parse_appliesto_in_bundle_rejected() {
        // appliesTo is selector — must come from CR, never bundle.
        let body = br#"{"appliesTo":{"sandboxName":"x"},"tokenBudget":{"perRequestTokens":1}}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("unrecognised"))
        );
    }

    #[test]
    fn parse_empty_object_rejected() {
        let body = b"{}";
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("no policy content"))
        );
    }

    #[test]
    fn parse_all_null_rejected() {
        let body = br#"{"tokenBudget":null,"contentSafety":null,"modelPreference":null,"displayName":null}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("no policy content"))
        );
    }

    #[test]
    fn parse_token_budget_wrong_type_rejected() {
        let body = br#"{"tokenBudget":{"perRequestTokens":"big"}}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("perRequestTokens"))
        );
    }

    #[test]
    fn parse_token_budget_negative_rejected() {
        let body = br#"{"tokenBudget":{"perRequestTokens":-1}}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("perRequestTokens"))
        );
    }

    #[test]
    fn parse_content_safety_wrong_severity_type_rejected() {
        let body = br#"{"contentSafety":{"hate":1}}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("contentSafety.hate"))
        );
    }

    #[test]
    fn parse_content_safety_wrong_shields_type_rejected() {
        let body = br#"{"contentSafety":{"requirePromptShields":"yes"}}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("requirePromptShields"))
        );
    }

    #[test]
    fn parse_model_preference_missing_primary_rejected() {
        let body = br#"{"modelPreference":{"fallback":[]}}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("primary required"))
        );
    }

    #[test]
    fn parse_model_preference_empty_deployment_rejected() {
        let body =
            br#"{"modelPreference":{"primary":{"provider":"azure-openai","deployment":""}}}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("deployment"))
        );
    }

    #[test]
    fn parse_model_preference_fallback_bad_entry_rejected() {
        let body = br#"{"modelPreference":{"primary":{"provider":"a","deployment":"b"},"fallback":[{"provider":"c"}]}}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("fallback[0].deployment"))
        );
    }

    #[test]
    fn parse_display_name_wrong_type_rejected() {
        let body = br#"{"displayName":42}"#;
        let err = parse(body).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("displayName"))
        );
    }
}
