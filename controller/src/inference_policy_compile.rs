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
//!   AzureClaw-owned: [`inference-router::budget::TokenBudgetTracker`]
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
}
