// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure-function compile step: `ToolPolicySpec` → AGT-profile JSON.
//!
//! Separated from the reconciler so it is unit-testable without a
//! `kube::Client`. The output JSON shape slots into
//! [`crate::tool_policy::ToolPolicySpec`]'s router-side counterpart
//! `inference-router::policy_envelope::PolicyEntry::payload` — i.e. we
//! are not introducing a parallel data shape.
//!
//! ## Determinism
//!
//! `compile_to_profile` and `version_hash` are deterministic. Same input
//! spec ⇒ identical bytes (canonicalised key order, since
//! `serde_json::to_string` on a `serde_json::Value::Object` (which is
//! `BTreeMap` under the `preserve_order` opt-out we do **not** enable)
//! sorts keys lexicographically). Asserted by
//! `compile_is_deterministic` test.
//!
//! ## What the compiler is NOT
//!
//! - **Not** a currency parser. `daily_cap` / `monthly_cap` /
//!   `per_transfer_cap` strings flow through verbatim — admission CEL
//!   already validated them, and the router-side AP2 evaluator owns the
//!   parsing surface (see `inference-router/src/a2a/ap2.rs`). Adding a
//!   second parser here would duplicate the parse rules.
//! - **Not** a precedence resolver. Selector precedence between
//!   overlapping `ToolPolicy` CRs is a router-side concern (see
//!   `policy_envelope::PolicyEnvelopeSnapshot::select` and
//!   `docs/crd-precedence.md`).

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::tool_policy::ToolPolicySpec;

/// Compile a `ToolPolicySpec` into the JSON `payload` value the router
/// stores in `PolicyEntry.payload`.
///
/// Shape (stable contract — bumping requires a `policy_envelope.rs`
/// change too):
///
/// ```json
/// {
///   "appliesTo": { "tool": ..., "mcpServer": ..., "sandboxMatchLabels": {...} },
///   "commerce":   { "dailyCap": ..., "monthlyCap": ..., "perTransferCap": ..., "counterpartyAllowlist": [...] } | null,
///   "rateLimit":  { "rps": ..., "burst": ..., "window": "..." } | null,
///   "approval":   { "mode": "...", "threshold": "...", "channel": "..." } | null,
///   "displayName": "..." | null
/// }
/// ```
///
/// Optional sub-objects are emitted as JSON `null` rather than omitted —
/// makes router-side parsers simpler and the version_hash diff visible
/// when an operator removes a sub-policy.
#[must_use]
pub fn compile_to_profile(spec: &ToolPolicySpec) -> Value {
    let applies_to = json!({
        "tool": spec.applies_to.tool,
        "mcpServer": spec.applies_to.mcp_server,
        "sandboxMatchLabels": spec.applies_to.sandbox_match_labels,
    });

    let commerce = spec.commerce.as_ref().map(|c| {
        json!({
            "dailyCap": c.daily_cap,
            "monthlyCap": c.monthly_cap,
            "perTransferCap": c.per_transfer_cap,
            "counterpartyAllowlist": c.counterparty_allowlist,
        })
    });

    let rate_limit = spec.rate_limit.as_ref().map(|r| {
        json!({
            "rps": r.rps,
            "burst": r.burst,
            "window": r.window,
        })
    });

    let approval = spec.approval.as_ref().map(|a| {
        json!({
            "mode": a.mode,
            "threshold": a.threshold,
            "channel": a.channel,
        })
    });

    json!({
        "appliesTo": applies_to,
        "commerce": commerce,
        "rateLimit": rate_limit,
        "approval": approval,
        "displayName": spec.display_name,
    })
}

/// Stable SHA-256 over the canonicalised compiled profile, hex-encoded
/// (first 32 chars). Used as `PolicyEntry.version` so the router can
/// short-circuit redundant `replace_snapshot` calls
/// (`policy_envelope::apply_policy_change` already implements the
/// short-circuit when `(id, version)` matches).
#[must_use]
pub fn version_hash(profile: &Value) -> String {
    // serde_json sorts object keys lexicographically by default — gives
    // us canonicalisation for free as long as we stay on `Value::Object`.
    let bytes = serde_json::to_vec(profile).expect("serde_json::Value always serialises");
    let digest = Sha256::digest(&bytes);
    hex::encode(&digest[..16])
}

/// Filename the controller stamps into the compiled-profile ConfigMap
/// and the router echoes back via `GET /internal/policy-status`. The
/// digest in [`agt_profile_digest`] is computed over **this exact
/// filename string** — the byte layout is part of the wire contract
/// between controller and router and must not drift.
pub const AGT_PROFILE_FILENAME: &str = "agt-profile.yaml";

/// Length-prefixed sha256 over the inline AGT profile bytes,
/// formatted as `sha256:<hex>`.
///
/// Matches the aggregate canonical-bytes format used by the router's
/// `Governance::load_policies_from_dir` (slice-1a) for the
/// single-file case — that loader walks files sorted by path, and for
/// each file appends `u64-BE(name.len()) || name ||
/// u64-BE(body.len()) || body` before hashing the concatenated
/// result. With a single key `agt-profile.yaml`, the controller-side
/// emission is byte-identical to the router-side load — that is what
/// makes the `/internal/policy-status` echo a meaningful confirmation
/// rather than a parallel-implementation gamble.
///
/// **Wire contract — DO NOT CHANGE without a coordinated router-side
/// update** ([`inference-router/src/governance/mod.rs`] section
/// "Aggregate canonical bytes").
#[must_use]
pub fn agt_profile_digest(inline: &str) -> String {
    let name = AGT_PROFILE_FILENAME.as_bytes();
    let body = inline.as_bytes();
    let mut canonical: Vec<u8> = Vec::with_capacity(16 + name.len() + body.len());
    canonical.extend_from_slice(&(name.len() as u64).to_be_bytes());
    canonical.extend_from_slice(name);
    canonical.extend_from_slice(&(body.len() as u64).to_be_bytes());
    canonical.extend_from_slice(body);
    let digest = Sha256::digest(&canonical);
    format!("sha256:{}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_policy::{ApprovalPolicy, CommercePolicy, RateLimitPolicy, ToolPolicySpec};

    fn full_spec() -> ToolPolicySpec {
        let mut spec = ToolPolicySpec::default();
        spec.applies_to.tool = Some("pay".into());
        spec.applies_to.mcp_server = Some("commerce".into());
        spec.applies_to
            .sandbox_match_labels
            .insert("env".into(), "prod".into());
        spec.commerce = Some(CommercePolicy {
            daily_cap: Some("USD 100.00".into()),
            monthly_cap: Some("USD 1000.00".into()),
            counterparty_allowlist: vec!["did:web:bank.example".into()],
            per_transfer_cap: Some("USD 50.00".into()),
        });
        spec.rate_limit = Some(RateLimitPolicy {
            rps: Some(5),
            burst: Some(10),
            window: Some("1m".into()),
        });
        spec.approval = Some(ApprovalPolicy {
            mode: Some("aboveThreshold".into()),
            threshold: Some("USD 25.00".into()),
            channel: Some("telegram".into()),
        });
        spec.display_name = Some("Pay tool".into());
        spec
    }

    #[test]
    fn compile_empty_spec_yields_minimal_profile() {
        let spec = ToolPolicySpec::default();
        let profile = compile_to_profile(&spec);
        assert!(profile.is_object());
        assert!(profile.get("commerce").unwrap().is_null());
        assert!(profile.get("rateLimit").unwrap().is_null());
        assert!(profile.get("approval").unwrap().is_null());
        assert!(profile.get("appliesTo").unwrap().is_object());
    }

    #[test]
    fn compile_full_spec_round_trips() {
        let spec = full_spec();
        let profile = compile_to_profile(&spec);
        assert_eq!(profile["appliesTo"]["tool"], "pay");
        assert_eq!(profile["appliesTo"]["mcpServer"], "commerce");
        assert_eq!(profile["commerce"]["dailyCap"], "USD 100.00");
        assert_eq!(profile["commerce"]["monthlyCap"], "USD 1000.00");
        assert_eq!(profile["commerce"]["perTransferCap"], "USD 50.00");
        assert_eq!(profile["rateLimit"]["rps"], 5);
        assert_eq!(profile["rateLimit"]["burst"], 10);
        assert_eq!(profile["rateLimit"]["window"], "1m");
        assert_eq!(profile["approval"]["mode"], "aboveThreshold");
        assert_eq!(profile["approval"]["threshold"], "USD 25.00");
        assert_eq!(profile["approval"]["channel"], "telegram");
        assert_eq!(profile["displayName"], "Pay tool");
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
        b.commerce.as_mut().unwrap().daily_cap = Some("USD 999.99".into());
        let h_a = version_hash(&compile_to_profile(&a));
        let h_b = version_hash(&compile_to_profile(&b));
        assert_ne!(h_a, h_b);

        // No change ⇒ identical hash.
        a.display_name = Some("Pay tool".into());
        let h_a2 = version_hash(&compile_to_profile(&a));
        assert_eq!(h_a, h_a2);
    }

    #[test]
    fn version_hash_is_stable_across_serde_round_trip() {
        let spec = full_spec();
        let profile_a = compile_to_profile(&spec);
        // Round-trip through string to force any insertion-order ambiguity.
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
    fn agt_profile_digest_uses_sha256_prefix_and_64_hex() {
        let d = agt_profile_digest("policies: []\n");
        let rest = d.strip_prefix("sha256:").expect("sha256: prefix");
        assert_eq!(rest.len(), 64, "32 bytes = 64 hex chars");
        assert!(rest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn agt_profile_digest_matches_router_length_prefix_layout() {
        // Golden vector — re-implementing the exact length-prefixed
        // canonical-bytes layout the router uses in
        // `inference-router/src/governance/mod.rs` for a single
        // file. If this assertion ever fails, the wire contract
        // documented on `agt_profile_digest` has drifted; either the
        // controller or the router must be reverted before
        // promoting Compiled → Ready (principles.md §3).
        let body = "policies:\n  - id: deny-all\n    action: deny\n";
        let name = AGT_PROFILE_FILENAME.as_bytes();
        let mut canonical: Vec<u8> = Vec::new();
        canonical.extend_from_slice(&(name.len() as u64).to_be_bytes());
        canonical.extend_from_slice(name);
        canonical.extend_from_slice(&(body.len() as u64).to_be_bytes());
        canonical.extend_from_slice(body.as_bytes());
        let expected = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
        assert_eq!(agt_profile_digest(body), expected);
    }

    #[test]
    fn agt_profile_digest_changes_with_body() {
        let a = agt_profile_digest("policies: []\n");
        let b = agt_profile_digest("policies: [foo]\n");
        assert_ne!(a, b);
    }

    #[test]
    fn agt_profile_digest_is_deterministic() {
        let body = "policies:\n  - id: x\n";
        assert_eq!(agt_profile_digest(body), agt_profile_digest(body));
    }

    #[test]
    fn agt_profile_filename_constant_is_yaml() {
        // Wire-contract sentinel: the router's
        // `load_policies_from_dir` filters for `.yaml`/`.yml`. If
        // the filename ever drifts to `.json` or similar, the router
        // would silently ignore the file and never echo a digest.
        assert!(
            AGT_PROFILE_FILENAME.ends_with(".yaml") || AGT_PROFILE_FILENAME.ends_with(".yml"),
            "router AGT loader filters .yaml/.yml only"
        );
    }
}
