// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! McpServer canonical-form parser + `PolicyKind` impl.
//!
//! Slice 1c.5 — fifth per-kind implementation after egress (1c.1),
//! tools (1c.2), inference (1c.3), and memory (1c.4). An McpServer
//! bundle carries the **server identity + OAuth + tool-allowlist
//! content** the controller will merge onto the per-CR
//! `allowedSandboxes` selector:
//!
//! - `url`
//! - `oauth`  (object: `issuer`, `audience?`, `resource?`, `pkce?`)
//! - `productionMode`
//! - `scopes`
//! - `allowedTools`
//! - `displayName`
//!
//! `allowedSandboxes` is intentionally NOT a bundle key. The set of
//! sandboxes permitted to reach a server is a deployment-time
//! decision owned by the `McpServer` CR — one signed bundle can
//! therefore be referenced by multiple CRs (e.g. fleet rollout of the
//! same MCP server scoped to different sandbox-label selectors).
//! This mirrors the inference-policy and memory-binding patterns:
//! **selector stays in the CR, content lives in the bundle**.
//!
//! What this parser enforces (trait invariant #1, applied to mcp-server):
//! - Valid UTF-8
//! - Parseable as a JSON object
//! - Top-level keys are policy-content keys only (no `allowedSandboxes`)
//! - `url` (when present) is a non-empty string
//! - `oauth` (when present) is an object whose `issuer` is a non-empty
//!   string, and whose `audience`/`resource`/`pkce` (when present) are
//!   strings
//! - `productionMode` (when present) is a boolean
//! - `scopes` / `allowedTools` (when present) are arrays of non-empty
//!   strings
//! - `displayName` (when present) is a string
//! - At least one policy-content key is present (empty bundles are
//!   rejected — the inline path is the supported way to express server
//!   metadata without supply-chain verification)
//!
//! Semantic checks (HTTPS-required-when-productionMode, valid issuer
//! URL shape, registered-tool-name format) stay in the existing
//! `crd_validations::mcp_server_validations` admission layer and the
//! reconciler's productionMode-vs-issuer guard.

use super::{CachedValue, PolicyKind};
use crate::policy_fetcher::{CACHE_TTL, FetchError};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

/// OCI media type for the v1 mcp-server-bundle artifact. The pulled
/// `artifactType` MUST match this exactly; consumers reject any other
/// value (forward-compat: v2 bumps the suffix; v1 consumers MUST
/// refuse v2 artifacts).
pub const MCP_SERVER_BUNDLE_V1_MEDIA_TYPE: &str =
    "application/vnd.azureclaw.mcp-server-bundle.v1+json";

/// Recognised top-level keys in an mcp-server bundle. `allowedSandboxes`
/// is intentionally NOT one of them: the selector targets sandboxes
/// owned by the `McpServer.spec`, not the signed artifact.
const POLICY_CONTENT_KEYS: &[&str] = &[
    "url",
    "oauth",
    "productionMode",
    "scopes",
    "allowedTools",
    "displayName",
];

/// Parsed OAuth sub-object from the bundle. Mirrors
/// [`crate::mcp_server::McpOAuthConfig`] except all fields are owned
/// strings extracted at parse time — keeps the merge step
/// allocation-free at reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOAuthConfig {
    pub issuer: String,
    pub audience: Option<String>,
    pub resource: Option<String>,
    pub pkce: Option<String>,
}

/// A verified mcp-server bundle. Each `Option` field is extracted at
/// parse time so the reconciler does not re-parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedMcpServerBundle {
    pub url: Option<String>,
    pub oauth: Option<VerifiedOAuthConfig>,
    pub production_mode: Option<bool>,
    pub scopes: Option<Vec<String>>,
    pub allowed_tools: Option<Vec<String>>,
    pub display_name: Option<String>,
    /// Raw signed bytes. Kept for observability; the operator-visible
    /// `bundleRefDigest` is the OCI manifest digest, not a hash of
    /// these bytes (mirrors the memory + inference modules).
    pub bytes: Vec<u8>,
    /// `sha256:...` matched against `OciArtifactRef.digest`.
    pub digest: String,
    pub fetched_at: SystemTime,
}

/// `PolicyKind` discriminator for mcp-server bundles.
pub struct McpServerKind;

impl PolicyKind for McpServerKind {
    const MEDIA_TYPE: &'static str = MCP_SERVER_BUNDLE_V1_MEDIA_TYPE;
    const API_VERSION: &'static str = "azureclaw.azure.com/v1alpha1";
    const KIND: &'static str = "McpServer";
    type Output = VerifiedMcpServerBundle;

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
            CachedValue::McpServer(v) => Some(v.clone()),
            CachedValue::Egress(_)
            | CachedValue::EvalCorpus(_)
            | CachedValue::Tools(_)
            | CachedValue::Inference(_)
            | CachedValue::Memory(_) => None,
        }
    }

    fn cache_put(key: String, value: Self::Output) {
        if let Ok(mut guard) = cache().lock() {
            guard.insert(
                key,
                CacheEntry {
                    verified: CachedValue::McpServer(value),
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

pub(crate) fn parse(bytes: &[u8]) -> Result<VerifiedMcpServerBundle, FetchError> {
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
                "unrecognised top-level key `{key}`; allowed: url, oauth, productionMode, \
                 scopes, allowedTools, displayName"
            )));
        }
    }

    let url = extract_optional_nonempty_string(map, "url")?;
    let oauth = extract_optional_oauth(map, "oauth")?;
    let production_mode = extract_optional_bool(map, "productionMode")?;
    let scopes = extract_optional_string_array(map, "scopes")?;
    let allowed_tools = extract_optional_string_array(map, "allowedTools")?;
    let display_name = extract_optional_string(map, "displayName")?;

    if url.is_none()
        && oauth.is_none()
        && production_mode.is_none()
        && scopes.is_none()
        && allowed_tools.is_none()
        && display_name.is_none()
    {
        return Err(FetchError::CanonicalFormViolation(
            "bundle has no policy content (all of url / oauth / productionMode / scopes / \
             allowedTools / displayName are absent or null); use the inline path for \
             selector-only McpServer CRs"
                .into(),
        ));
    }

    Ok(VerifiedMcpServerBundle {
        url,
        oauth,
        production_mode,
        scopes,
        allowed_tools,
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

fn extract_optional_string_array(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, FetchError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, v) in arr.iter().enumerate() {
                match v {
                    Value::String(s) if !s.is_empty() => out.push(s.clone()),
                    Value::String(_) => {
                        return Err(FetchError::CanonicalFormViolation(format!(
                            "`{key}[{i}]` must be a non-empty string"
                        )));
                    }
                    _ => {
                        return Err(FetchError::CanonicalFormViolation(format!(
                            "`{key}[{i}]` must be a non-empty string"
                        )));
                    }
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(FetchError::CanonicalFormViolation(format!(
            "`{key}` must be an array of strings or null"
        ))),
    }
}

fn extract_optional_oauth(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<VerifiedOAuthConfig>, FetchError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(obj)) => {
            const OAUTH_KEYS: &[&str] = &["issuer", "audience", "resource", "pkce"];
            for k in obj.keys() {
                if !OAUTH_KEYS.contains(&k.as_str()) {
                    return Err(FetchError::CanonicalFormViolation(format!(
                        "unrecognised key `{key}.{k}`; allowed: issuer, audience, \
                         resource, pkce"
                    )));
                }
            }
            let issuer = match obj.get("issuer") {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                _ => {
                    return Err(FetchError::CanonicalFormViolation(format!(
                        "`{key}.issuer` is required and must be a non-empty string"
                    )));
                }
            };
            let audience = extract_oauth_optional_string(obj, "audience", key)?;
            let resource = extract_oauth_optional_string(obj, "resource", key)?;
            let pkce = extract_oauth_optional_string(obj, "pkce", key)?;
            Ok(Some(VerifiedOAuthConfig {
                issuer,
                audience,
                resource,
                pkce,
            }))
        }
        Some(_) => Err(FetchError::CanonicalFormViolation(format!(
            "`{key}` must be a JSON object or null"
        ))),
    }
}

fn extract_oauth_optional_string(
    obj: &serde_json::Map<String, Value>,
    child: &str,
    parent: &str,
) -> Result<Option<String>, FetchError> {
    match obj.get(child) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(FetchError::CanonicalFormViolation(format!(
            "`{parent}.{child}` must be a string or null"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_bundle_ok() {
        let body = br#"{
            "url": "https://mcp.example.com",
            "oauth": {
                "issuer": "https://issuer.example.com",
                "audience": "mcp-api",
                "resource": "https://mcp.example.com",
                "pkce": "S256"
            },
            "productionMode": true,
            "scopes": ["read", "write"],
            "allowedTools": ["search", "fetch"],
            "displayName": "Example MCP"
        }"#;
        let v = parse(body).unwrap();
        assert_eq!(v.url.as_deref(), Some("https://mcp.example.com"));
        let oauth = v.oauth.unwrap();
        assert_eq!(oauth.issuer, "https://issuer.example.com");
        assert_eq!(oauth.audience.as_deref(), Some("mcp-api"));
        assert_eq!(oauth.resource.as_deref(), Some("https://mcp.example.com"));
        assert_eq!(oauth.pkce.as_deref(), Some("S256"));
        assert_eq!(v.production_mode, Some(true));
        assert_eq!(
            v.scopes.as_deref(),
            Some(["read".to_string(), "write".to_string()].as_slice())
        );
        assert_eq!(
            v.allowed_tools.as_deref(),
            Some(["search".to_string(), "fetch".to_string()].as_slice())
        );
        assert_eq!(v.display_name.as_deref(), Some("Example MCP"));
    }

    #[test]
    fn parse_url_only_ok() {
        let body = br#"{"url":"https://mcp.example.com"}"#;
        let v = parse(body).unwrap();
        assert_eq!(v.url.as_deref(), Some("https://mcp.example.com"));
        assert!(v.oauth.is_none());
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
    fn parse_allowed_sandboxes_in_bundle_rejected() {
        // allowedSandboxes is owned by the CR, not the bundle
        let err = parse(br#"{"url":"https://x","allowedSandboxes":{"matchLabels":{"a":"b"}}}"#)
            .unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("allowedSandboxes"))
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
            br#"{"url":null,"oauth":null,"productionMode":null,"scopes":null,"allowedTools":null,"displayName":null}"#,
        )
        .unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("no policy content"))
        );
    }

    #[test]
    fn parse_empty_url_rejected() {
        let err = parse(br#"{"url":""}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-empty"))
        );
    }

    #[test]
    fn parse_url_wrong_type_rejected() {
        let err = parse(br#"{"url":42}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("non-empty"))
        );
    }

    #[test]
    fn parse_production_mode_wrong_type_rejected() {
        let err = parse(br#"{"productionMode":"yes"}"#).unwrap_err();
        assert!(matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("boolean")));
    }

    #[test]
    fn parse_oauth_missing_issuer_rejected() {
        let err = parse(br#"{"oauth":{"audience":"a"}}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("oauth.issuer"))
        );
    }

    #[test]
    fn parse_oauth_empty_issuer_rejected() {
        let err = parse(br#"{"oauth":{"issuer":""}}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("oauth.issuer"))
        );
    }

    #[test]
    fn parse_oauth_wrong_type_rejected() {
        let err = parse(br#"{"oauth":"https://example.com"}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("must be a JSON object"))
        );
    }

    #[test]
    fn parse_oauth_unknown_subkey_rejected() {
        let err = parse(br#"{"oauth":{"issuer":"https://x","clientId":"y"}}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("unrecognised key `oauth.clientId`"))
        );
    }

    #[test]
    fn parse_scopes_wrong_type_rejected() {
        let err = parse(br#"{"scopes":"read"}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("must be an array of strings"))
        );
    }

    #[test]
    fn parse_scopes_with_empty_string_rejected() {
        let err = parse(br#"{"scopes":["read",""]}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("scopes[1]"))
        );
    }

    #[test]
    fn parse_scopes_with_non_string_rejected() {
        let err = parse(br#"{"scopes":["read",42]}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("scopes[1]"))
        );
    }

    #[test]
    fn parse_allowed_tools_wrong_type_rejected() {
        let err = parse(br#"{"allowedTools":{"a":"b"}}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("must be an array of strings"))
        );
    }

    #[test]
    fn parse_display_name_wrong_type_rejected() {
        let err = parse(br#"{"url":"https://x","displayName":42}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("displayName"))
        );
    }

    #[test]
    fn parse_empty_scopes_array_ok() {
        // Empty array means "no scopes requested" — semantically valid
        // (e.g. the CR-side selector handles per-sandbox auth).
        let v = parse(br#"{"url":"https://x","scopes":[]}"#).unwrap();
        assert_eq!(v.scopes.as_deref(), Some([].as_slice()));
    }

    #[test]
    fn parse_oauth_audience_wrong_type_rejected() {
        let err = parse(br#"{"oauth":{"issuer":"https://x","audience":42}}"#).unwrap_err();
        assert!(
            matches!(err, FetchError::CanonicalFormViolation(ref m) if m.contains("oauth.audience"))
        );
    }
}
