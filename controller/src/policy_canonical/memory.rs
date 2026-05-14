// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ClawMemory canonical-form parser + `PolicyKind` impl.
//!
//! Slice 1c.4 — fourth per-kind implementation after egress (1c.1),
//! tools (1c.2), and inference (1c.3). A ClawMemory bundle carries
//! the **Foundry Memory Store binding content** the controller will
//! merge onto the per-CR `sandboxRef`:
//!
//! - `storeName`
//! - `scope`
//! - `retentionDays`
//! - `deleteOnSandboxDelete`
//! - `displayName`
//!
//! `sandboxRef` is intentionally NOT a bundle key. The sandbox a
//! binding applies to is owned by the `ClawMemory` CR — one signed
//! bundle can therefore be referenced by multiple CRs (e.g. fleet
//! rollout of an identical memory configuration to several sandboxes).
//! This mirrors the inference-policy and tool-policy patterns:
//! **selector stays in the CR, content lives in the bundle**.
//!
//! What this parser enforces (trait invariant #1, applied to memory):
//! - Valid UTF-8
//! - Parseable as a JSON object
//! - Top-level keys are policy-content keys only (no `sandboxRef`)
//! - `storeName` (when present) is a non-empty string
//! - `scope` (when present) is a non-empty string
//! - `retentionDays` (when present) is a non-negative integer
//! - `deleteOnSandboxDelete` (when present) is a boolean
//! - `displayName` (when present) is a string
//! - At least one policy-content key is present (empty bundles are
//!   rejected — the inline path is the supported way to express
//!   binding metadata without supply-chain verification)
//!
//! Semantic checks (DNS-label-style `storeName`, valid `scope`
//! prefixes, retention floor sanity) stay in the existing
//! `crd_validations::claw_memory_validations` admission layer and the
//! runtime path (`cli/src/plugin.ts::ensureMemoryStore`).

use super::{CachedValue, PolicyKind};
use crate::policy_fetcher::{CACHE_TTL, FetchError};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

/// OCI media type for the v1 memory-binding artifact. The pulled
/// `artifactType` MUST match this exactly; consumers reject any other
/// value (forward-compat: v2 bumps the suffix; v1 consumers MUST
/// refuse v2 artifacts).
pub const MEMORY_BINDING_V1_MEDIA_TYPE: &str = "application/vnd.azureclaw.memory-binding.v1+json";

/// Recognised top-level keys in a memory-binding bundle. `sandboxRef`
/// is intentionally NOT one of them: the binding targets a sandbox
/// owned by the `ClawMemory.spec`, not the signed artifact.
const POLICY_CONTENT_KEYS: &[&str] = &[
    "storeName",
    "scope",
    "retentionDays",
    "deleteOnSandboxDelete",
    "displayName",
];

/// A verified memory-binding bundle. Each `Option` field is extracted
/// at parse time so the reconciler does not re-parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedMemoryBinding {
    pub store_name: Option<String>,
    pub scope: Option<String>,
    pub retention_days: Option<u32>,
    pub delete_on_sandbox_delete: Option<bool>,
    pub display_name: Option<String>,
    /// Raw signed bytes. Kept for observability; the router echo
    /// digest is computed over the **recompiled** bytes (sandboxRef
    /// merged with content), not these.
    pub bytes: Vec<u8>,
    /// `sha256:...` matched against `OciArtifactRef.digest`.
    pub digest: String,
    pub fetched_at: SystemTime,
}

/// `PolicyKind` discriminator for memory-binding bundles.
pub struct MemoryKind;

impl PolicyKind for MemoryKind {
    const MEDIA_TYPE: &'static str = MEMORY_BINDING_V1_MEDIA_TYPE;
    const API_VERSION: &'static str = "azureclaw.azure.com/v1alpha1";
    const KIND: &'static str = "ClawMemory";
    type Output = VerifiedMemoryBinding;

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
            CachedValue::Memory(v) => Some(v.clone()),
            CachedValue::Egress(_) | CachedValue::Tools(_) | CachedValue::Inference(_) => None,
        }
    }

    fn cache_put(key: String, value: Self::Output) {
        if let Ok(mut guard) = cache().lock() {
            guard.insert(
                key,
                CacheEntry {
                    verified: CachedValue::Memory(value),
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

pub(crate) fn parse(bytes: &[u8]) -> Result<VerifiedMemoryBinding, FetchError> {
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
                "unrecognised top-level key `{key}`; allowed: storeName, scope, \
                 retentionDays, deleteOnSandboxDelete, displayName"
            )));
        }
    }

    let store_name = extract_optional_nonempty_string(map, "storeName")?;
    let scope = extract_optional_nonempty_string(map, "scope")?;
    let retention_days = extract_optional_u32(map, "retentionDays")?;
    let delete_on_sandbox_delete = extract_optional_bool(map, "deleteOnSandboxDelete")?;
    let display_name = extract_optional_string(map, "displayName")?;

    if store_name.is_none()
        && scope.is_none()
        && retention_days.is_none()
        && delete_on_sandbox_delete.is_none()
        && display_name.is_none()
    {
        return Err(FetchError::CanonicalFormViolation(
            "bundle has no policy content (all of storeName / scope / retentionDays / \
             deleteOnSandboxDelete / displayName are absent or null); use the inline \
             path for selector-only bindings"
                .into(),
        ));
    }

    Ok(VerifiedMemoryBinding {
        store_name,
        scope,
        retention_days,
        delete_on_sandbox_delete,
        display_name,
        bytes: bytes.to_vec(),
        digest: String::new(),
        fetched_at: SystemTime::UNIX_EPOCH,
    })
}

fn extract_optional_nonempty_string(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, FetchError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(FetchError::CanonicalFormViolation(format!(
            "`{key}` must be a non-empty string"
        ))),
        Some(_) => Err(FetchError::CanonicalFormViolation(format!(
            "`{key}` must be a non-empty string or null"
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

fn extract_optional_u32(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u32>, FetchError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let v = n.as_u64().ok_or_else(|| {
                FetchError::CanonicalFormViolation(format!(
                    "`{key}` must be a non-negative integer (got {n})"
                ))
            })?;
            u32::try_from(v).map(Some).map_err(|_| {
                FetchError::CanonicalFormViolation(format!("`{key}` must fit in u32 (got {v})"))
            })
        }
        Some(_) => Err(FetchError::CanonicalFormViolation(format!(
            "`{key}` must be a non-negative integer or null"
        ))),
    }
}

fn extract_optional_bool(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, FetchError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(FetchError::CanonicalFormViolation(format!(
            "`{key}` must be a boolean or null"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_bundle_ok() {
        let body = br#"{
            "storeName": "agent-x-mem",
            "scope": "agent:agent-x",
            "retentionDays": 30,
            "deleteOnSandboxDelete": true,
            "displayName": "Agent X memory"
        }"#;
        let v = parse(body).unwrap();
        assert_eq!(v.store_name.as_deref(), Some("agent-x-mem"));
        assert_eq!(v.scope.as_deref(), Some("agent:agent-x"));
        assert_eq!(v.retention_days, Some(30));
        assert_eq!(v.delete_on_sandbox_delete, Some(true));
        assert_eq!(v.display_name.as_deref(), Some("Agent X memory"));
    }

    #[test]
    fn parse_store_name_only_ok() {
        let body = br#"{"storeName":"agent-x-mem"}"#;
        let v = parse(body).unwrap();
        assert_eq!(v.store_name.as_deref(), Some("agent-x-mem"));
        assert!(v.scope.is_none());
    }

    #[test]
    fn parse_invalid_utf8_rejected() {
        let body = [0xFFu8, 0xFE];
        let err = parse(&body).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("utf-8")));
    }

    #[test]
    fn parse_invalid_json_rejected() {
        let err = parse(b"{not json").unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("json parse"))
        );
    }

    #[test]
    fn parse_non_object_top_level_rejected() {
        let err = parse(b"[]").unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("top-level"))
        );
    }

    #[test]
    fn parse_unknown_key_rejected() {
        let err = parse(br#"{"foo":1}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("unrecognised"))
        );
    }

    #[test]
    fn parse_sandboxref_in_bundle_rejected() {
        // sandboxRef is owned by the CR, not the bundle
        let err = parse(br#"{"storeName":"x","sandboxRef":{"name":"y"}}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("sandboxRef"))
        );
    }

    #[test]
    fn parse_empty_object_rejected() {
        let err = parse(b"{}").unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("no policy content"))
        );
    }

    #[test]
    fn parse_all_null_rejected() {
        let err = parse(
            br#"{"storeName":null,"scope":null,"retentionDays":null,"deleteOnSandboxDelete":null,"displayName":null}"#,
        )
        .unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("no policy content"))
        );
    }

    #[test]
    fn parse_empty_store_name_rejected() {
        let err = parse(br#"{"storeName":""}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-empty"))
        );
    }

    #[test]
    fn parse_store_name_wrong_type_rejected() {
        let err = parse(br#"{"storeName":42}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-empty"))
        );
    }

    #[test]
    fn parse_empty_scope_rejected() {
        let err = parse(br#"{"scope":""}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-empty"))
        );
    }

    #[test]
    fn parse_retention_days_negative_rejected() {
        let err = parse(br#"{"retentionDays":-1}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-negative"))
        );
    }

    #[test]
    fn parse_retention_days_wrong_type_rejected() {
        let err = parse(br#"{"retentionDays":"thirty"}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-negative"))
        );
    }

    #[test]
    fn parse_retention_days_overflow_rejected() {
        let err = parse(br#"{"retentionDays":4294967296}"#).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("u32")));
    }

    #[test]
    fn parse_delete_on_sandbox_delete_wrong_type_rejected() {
        let err = parse(br#"{"deleteOnSandboxDelete":"yes"}"#).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("boolean")));
    }

    #[test]
    fn parse_display_name_wrong_type_rejected() {
        let err = parse(br#"{"storeName":"x","displayName":42}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("displayName"))
        );
    }

    #[test]
    fn parse_retention_days_zero_ok() {
        // zero is a valid non-negative integer; semantic "retention
        // must be > 0" is enforced by admission CEL on the CR, not
        // here on the bundle.
        let v = parse(br#"{"retentionDays":0}"#).unwrap();
        assert_eq!(v.retention_days, Some(0));
    }
}
