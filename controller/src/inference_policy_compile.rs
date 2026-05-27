// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure-function compile step: `InferencePolicySpec` → AGT-profile JSON.
//!
//! Separated from the reconciler so it is unit-testable without a
//! `kube::Client`. The output JSON shape slots into
//! `inference-router::policy_envelope::PolicyEntry::payload` — i.e. we
//! are not introducing a parallel data shape. The router's existing
//! `routes::inference_policy::check()` is the runtime gate; S7 wires
//! the informer that loads compiled profiles into `PolicyEnvelope`.
//!
//! ## Determinism
//!
//! `compile_to_profile` and `version_hash` are deterministic. Same
//! input spec ⇒ identical bytes (canonicalised key order, since
//! `serde_json::to_string` on a `serde_json::Value::Object` (which is
//! `BTreeMap` under the `preserve_order` opt-out we do **not** enable)
//! sorts keys lexicographically). Asserted by
//! `compile_is_deterministic` test.
//!
//! ## What the compiler is NOT
//!
//! - **Not** a Content Safety severity parser. Severity strings flow
//!   through verbatim — admission CEL already validated them, and the
//!   router-side parser owns the `prompt_filter_results` extraction
//!   path (Phase 1 substrate via Foundry-side Content Safety; AGT
//!   `BehaviorMonitor` receives flags via
//!   `safety::report_content_flags_to_agt`).
//! - **Not** a token-budget enforcer. The runtime tracker today is
//!   kars-owned: [`inference-router::budget::TokenBudgetTracker`]
//!   fed from env vars. **AGT does NOT currently expose a
//!   `TokenBudget` interface** (verified against `agentmesh` 3.1.0 on
//!   crates.io — only `PolicyEngine`, `TrustManager`, `RateLimiter`,
//!   `BehaviorMonitor`, `AuditLogger` are public). S7 will decide
//!   whether `budget.rs` consumes this CR's compiled cap or whether
//!   we contribute upstream.
//! - **Not** a model selector. Fallback resolution happens at the
//!   call site in `inference-router/src/routes/inference.rs`; we ship
//!   the *preferred order*, the router decides on health.
//!
//! ## Runtime consumers today (S4 ships none of these wire-ups)
//!
//! - `tokenBudget` → today: `inference-router::budget` (env-fed).
//!   Future (S7+): swap env source for `PolicyEntry.payload`.
//! - `contentSafety` → today: Foundry Content Safety via
//!   `safety::parse_prompt_filter_results` +
//!   `safety::report_content_flags_to_agt`; floors not yet enforced
//!   per-policy. Future (S7+): floor compare-and-block in
//!   `PolicyDecisionProvider::decide()`.
//! - `modelPreference` → today: not consumed. Future (S7+): consulted
//!   in `routes/inference.rs` provider selection.
//!
//! These "today vs future" lines must be removed once S7 wires the
//! `PolicyEnvelope` informer that feeds compiled profiles to the
//! call sites.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::inference_policy::InferencePolicySpec;

/// Compile an `InferencePolicySpec` into the JSON `payload` value the
/// router stores in `PolicyEntry.payload`.
///
/// Shape (stable contract — bumping requires a `policy_envelope.rs`
/// change too):
///
/// ```json
/// {
///   "appliesTo":       { "sandboxName": ..., "sandboxMatchLabels": {...}, "action": ... },
///   "tokenBudget":     { "perRequestTokens": ..., "dailyTokens": ..., "monthlyTokens": ... } | null,
///   "contentSafety":   { "hate": ..., "selfHarm": ..., "sexual": ..., "violence": ..., "requirePromptShields": ... } | null,
///   "modelPreference": { "primary": {provider, deployment}, "fallback": [...] } | null,
///   "displayName":     "..." | null
/// }
/// ```
///
/// Optional sub-objects emit JSON `null` rather than being omitted —
/// matches the ToolPolicy compile shape so the router-side loader has
/// a single null-handling rule across CRDs.
#[must_use]
pub fn compile_to_profile(spec: &InferencePolicySpec) -> Value {
    let applies_to = json!({
        "sandboxName": spec.applies_to.sandbox_name,
        "sandboxMatchLabels": spec.applies_to.sandbox_match_labels,
        "action": spec.applies_to.action,
    });

    let token_budget = spec.token_budget.as_ref().map(|t| {
        json!({
            "perRequestTokens": t.per_request_tokens,
            "dailyTokens": t.daily_tokens,
            "monthlyTokens": t.monthly_tokens,
        })
    });

    let content_safety = spec.content_safety.as_ref().map(|c| {
        json!({
            "hate": c.hate,
            "selfHarm": c.self_harm,
            "sexual": c.sexual,
            "violence": c.violence,
            "requirePromptShields": c.require_prompt_shields,
        })
    });

    let model_preference = spec.model_preference.as_ref().map(|m| {
        json!({
            "primary": {
                "provider": m.primary.provider,
                "deployment": m.primary.deployment,
            },
            "fallback": m.fallback.iter().map(|f| json!({
                "provider": f.provider,
                "deployment": f.deployment,
            })).collect::<Vec<_>>(),
        })
    });

    json!({
        "appliesTo": applies_to,
        "tokenBudget": token_budget,
        "contentSafety": content_safety,
        "modelPreference": model_preference,
        "displayName": spec.display_name,
    })
}

/// Stable SHA-256 over the canonicalised compiled profile, hex-encoded
/// (first 32 chars). Used as `PolicyEntry.version` so the router can
/// short-circuit redundant `replace_snapshot` calls.
#[must_use]
pub fn version_hash(profile: &Value) -> String {
    let bytes = serde_json::to_vec(profile).expect("serde_json::Value always serialises");
    let digest = Sha256::digest(&bytes);
    hex::encode(&digest[..16])
}

/// Canonical filename the controller writes into the
/// `inferencepolicy-<name>-profile` ConfigMap. Kept in lockstep with
/// `inference-router::inference_policy_loader::INFERENCE_POLICY_FILENAME`
/// — the byte layout of `canonical_bytes_for_digest` includes this
/// string, so any drift breaks the principles.md §3 "Ready ⇔ router
/// echo" contract.
pub const INFERENCE_POLICY_FILENAME: &str = "inference-policy.json";

/// Length-prefixed canonical bytes used by both controller and router
/// to compute the same `sha256:<hex>` digest for a single
/// `inference-policy.json` file. Layout:
///
/// ```text
/// u64-BE(filename.len()) || filename || u64-BE(body.len()) || body
/// ```
///
/// Matches the router-side
/// `inference_policy_loader::canonical_bytes_for_digest`. Exposed so
/// the reconciler can stamp the same digest on the ConfigMap
/// annotation as the router will echo back through
/// `GET /internal/policy-status`.
#[must_use]
pub fn canonical_bytes_for_digest(filename: &str, body: &[u8]) -> Vec<u8> {
    let name = filename.as_bytes();
    let mut canonical: Vec<u8> = Vec::with_capacity(16 + name.len() + body.len());
    canonical.extend_from_slice(&(name.len() as u64).to_be_bytes());
    canonical.extend_from_slice(name);
    canonical.extend_from_slice(&(body.len() as u64).to_be_bytes());
    canonical.extend_from_slice(body);
    canonical
}

/// `sha256:<full hex>` digest over the canonical bytes (see
/// `canonical_bytes_for_digest`) for the supplied compiled
/// `inference-policy.json` body. This is the digest the
/// `InferencePolicy` reconciler stamps in `status.compiledDigest`
/// and the ConfigMap annotation
/// `kars.azure.com/inference-policy-digest`. The router echoes
/// the same value via `GET /internal/policy-status` once it loads
/// the file; matching values let `decide_enforcement_state` promote
/// the CRD from `phase=Compiled` to `phase=Ready`.
///
/// **Wire contract — DO NOT CHANGE** without a coordinated router-
/// side update. Distinct from [`version_hash`] which keeps a short
/// 32-char identifier for `PolicyEntry.version` change-detection;
/// the two co-exist because `version_hash` is also recorded in
/// `status.versionHash` for backward compatibility.
#[must_use]
pub fn inference_policy_digest(body: &[u8]) -> String {
    let canonical = canonical_bytes_for_digest(INFERENCE_POLICY_FILENAME, body);
    let digest = Sha256::digest(&canonical);
    format!("sha256:{}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_policy::{
        ContentSafetyFloor, InferenceAppliesTo, InferencePolicySpec, ModelPreference, ModelRef,
        TokenBudget,
    };

    fn full_spec() -> InferencePolicySpec {
        InferencePolicySpec {
            applies_to: InferenceAppliesTo {
                sandbox_name: Some("agent-x".into()),
                sandbox_match_labels: [("env".to_string(), "prod".to_string())]
                    .into_iter()
                    .collect(),
                action: Some("chat".into()),
            },
            token_budget: Some(TokenBudget {
                per_request_tokens: Some(8_192),
                daily_tokens: Some(1_000_000),
                monthly_tokens: Some(20_000_000),
            }),
            content_safety: Some(ContentSafetyFloor {
                hate: Some("Medium".into()),
                self_harm: Some("Low".into()),
                sexual: Some("Medium".into()),
                violence: Some("High".into()),
                require_prompt_shields: Some(true),
            }),
            model_preference: Some(ModelPreference {
                primary: ModelRef {
                    provider: "azure-openai".into(),
                    deployment: "gpt-4o".into(),
                },
                fallback: vec![ModelRef {
                    provider: "anthropic".into(),
                    deployment: "claude-3-5-sonnet".into(),
                }],
            }),
            display_name: Some("Prod chat policy".into()),
            bundle_ref: None,
        }
    }

    #[test]
    fn compile_empty_spec_yields_minimal_profile() {
        let spec = InferencePolicySpec::default();
        let profile = compile_to_profile(&spec);
        assert!(profile.is_object());
        assert!(profile.get("tokenBudget").unwrap().is_null());
        assert!(profile.get("contentSafety").unwrap().is_null());
        assert!(profile.get("modelPreference").unwrap().is_null());
        assert!(profile.get("appliesTo").unwrap().is_object());
    }

    #[test]
    fn compile_full_spec_round_trips() {
        let spec = full_spec();
        let profile = compile_to_profile(&spec);
        assert_eq!(profile["appliesTo"]["sandboxName"], "agent-x");
        assert_eq!(profile["appliesTo"]["action"], "chat");
        assert_eq!(profile["tokenBudget"]["perRequestTokens"], 8_192);
        assert_eq!(profile["tokenBudget"]["dailyTokens"], 1_000_000);
        assert_eq!(profile["tokenBudget"]["monthlyTokens"], 20_000_000);
        assert_eq!(profile["contentSafety"]["hate"], "Medium");
        assert_eq!(profile["contentSafety"]["selfHarm"], "Low");
        assert_eq!(profile["contentSafety"]["violence"], "High");
        assert_eq!(profile["contentSafety"]["requirePromptShields"], true);
        assert_eq!(
            profile["modelPreference"]["primary"]["provider"],
            "azure-openai"
        );
        assert_eq!(
            profile["modelPreference"]["primary"]["deployment"],
            "gpt-4o"
        );
        assert_eq!(
            profile["modelPreference"]["fallback"][0]["provider"],
            "anthropic"
        );
        assert_eq!(profile["displayName"], "Prod chat policy");
    }

    #[test]
    fn compile_is_deterministic() {
        let spec = full_spec();
        let a = compile_to_profile(&spec);
        let b = compile_to_profile(&spec);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn version_hash_changes_on_spec_change() {
        let mut a = full_spec();
        let mut b = full_spec();
        b.token_budget.as_mut().unwrap().daily_tokens = Some(999_999);
        let h_a = version_hash(&compile_to_profile(&a));
        let h_b = version_hash(&compile_to_profile(&b));
        assert_ne!(h_a, h_b);

        // No change ⇒ identical hash.
        a.display_name = Some("Prod chat policy".into());
        let h_a2 = version_hash(&compile_to_profile(&a));
        assert_eq!(h_a, h_a2);
    }

    #[test]
    fn version_hash_is_stable_across_serde_round_trip() {
        let spec = full_spec();
        let profile_a = compile_to_profile(&spec);
        let s = serde_json::to_string(&profile_a).unwrap();
        let profile_b: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(version_hash(&profile_a), version_hash(&profile_b));
    }

    #[test]
    fn version_hash_is_hex_16_bytes() {
        let h = version_hash(&compile_to_profile(&full_spec()));
        assert_eq!(h.len(), 32, "16 bytes = 32 hex chars");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn inference_policy_digest_uses_sha256_prefix_and_64_hex() {
        let body = b"{}";
        let d = inference_policy_digest(body);
        let rest = d.strip_prefix("sha256:").expect("sha256: prefix");
        assert_eq!(rest.len(), 64, "32 bytes = 64 hex chars");
        assert!(rest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn inference_policy_digest_matches_canonical_layout() {
        // Belt-and-braces: the controller digest must equal sha256
        // over `u64-BE(name.len()) || name || u64-BE(body.len()) ||
        // body` for the *exact* filename string. Router-side
        // `inference_policy_loader::canonical_bytes_for_digest` uses
        // the same layout; if either side drifts, the §3 echo
        // contract silently breaks.
        let body = br#"{"tokenBudget":{"perRequestTokens":4096}}"#;
        let name = INFERENCE_POLICY_FILENAME.as_bytes();
        let mut canonical: Vec<u8> = Vec::new();
        canonical.extend_from_slice(&(name.len() as u64).to_be_bytes());
        canonical.extend_from_slice(name);
        canonical.extend_from_slice(&(body.len() as u64).to_be_bytes());
        canonical.extend_from_slice(body);
        let expected = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
        assert_eq!(inference_policy_digest(body), expected);
    }

    #[test]
    fn inference_policy_digest_changes_with_body() {
        let a = inference_policy_digest(b"{\"a\":1}");
        let b = inference_policy_digest(b"{\"a\":2}");
        assert_ne!(a, b);
    }

    #[test]
    fn inference_policy_digest_is_deterministic() {
        let body = b"{\"tokenBudget\":{\"perRequestTokens\":2048}}";
        assert_eq!(inference_policy_digest(body), inference_policy_digest(body));
    }

    #[test]
    fn canonical_bytes_for_digest_layout_is_length_prefixed() {
        let bytes = canonical_bytes_for_digest("a", b"bc");
        // u64-BE(1) || "a" || u64-BE(2) || "bc"
        assert_eq!(
            bytes,
            vec![
                0, 0, 0, 0, 0, 0, 0, 1, b'a', 0, 0, 0, 0, 0, 0, 0, 2, b'b', b'c'
            ]
        );
    }
}
