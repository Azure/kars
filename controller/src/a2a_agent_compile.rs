// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure-function compile step: `A2AAgentSpec` → A2A 1.2 AgentCard JSON.
//!
//! Separated from the reconciler so it is unit-testable without a
//! `kube::Client`. The output JSON is the **wire-format AgentCard** —
//! the router serves the bytes verbatim from `/.well-known/agent.json`
//! once S7 wires the mount.
//!
//! ## Determinism
//!
//! Same input spec ⇒ identical bytes. `serde_json::Value::Object` is a
//! `BTreeMap` (we do not enable `preserve_order`) so all keys are
//! emitted in lexicographic order. The `version_hash` test asserts
//! this round-trips through serde stably.
//!
//! ## What the compiler is NOT
//!
//! - **Not a signer.** AgentCard signing (JWS detached, RFC 7515 §A.5)
//!   happens in the router (S7) using the signing-key Secret. The
//!   compiled body is the canonical input to the JWS, not a JWS
//!   itself.
//! - **Not a federation resolver.** `federation.peers[]` is rendered
//!   verbatim; cross-CR `agentRef` resolution is a router-side
//!   concern (S7 informer + trust-store rebuild orchestrator,
//!   already pure-functional in `inference-router::a2a::snapshot_rebuild`).
//! - **Not a trust evaluator.** `trust` thresholds flow through to the
//!   card; the router-side verifier owns the actual evaluation.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::a2a_agent::A2AAgentSpec;

/// Compile an `A2AAgentSpec` into the JSON body the router will serve
/// as `/.well-known/agent.json`.
///
/// Shape (stable contract — bumping requires the router-side serve
/// path in `inference-router::routes::a2a` to update too):
///
/// ```json
/// {
///   "protocolVersion": "1.2",
///   "namespace": "<from CR>",
///   "name": "<from CR>",
///   "displayName": "..." | null,
///   "description": "..." | null,
///   "endpointUrl": "https://...",
///   "productionMode": true | false,
///   "capabilities": ["tasks", "streaming", ...],
///   "signingKeys": [
///     { "kid": "...", "alg": "EdDSA", "publicKeyB64u": "...", "notAfter": ... | null }
///   ],
///   "trust": { ... } | null,
///   "federation": { "peers": [ ... ] },
///   "policyRefs": { "toolPolicy": "..." | null } | null
/// }
/// ```
///
/// `namespace` / `name` come from the CR's `metadata` and are passed in
/// rather than read from the spec — the spec is the **declarative
/// surface**, not a place to mirror metadata.
#[must_use]
pub fn compile_agent_card(spec: &A2AAgentSpec, namespace: &str, name: &str) -> Value {
    let signing_keys: Vec<Value> = spec
        .signing_keys
        .iter()
        .map(|k| {
            json!({
                "kid": k.kid,
                "alg": k.alg,
                "publicKeyB64u": k.public_key_b64u,
                "notAfter": k.not_after,
            })
        })
        .collect();

    let trust = spec.trust.as_ref().map(|t| {
        json!({
            "requireSignedRequests": t.require_signed_requests,
            "minSignaturesRequired": t.min_signatures_required,
            "maxClockSkewSeconds": t.max_clock_skew_seconds,
        })
    });

    let federation_peers: Vec<Value> = spec
        .federation
        .iter()
        .map(|p| {
            json!({
                "label": p.label,
                "kind": p.kind,
                "agentRef": p.agent_ref,
                "endpointUrl": p.endpoint_url,
                "pinnedKid": p.pinned_kid,
            })
        })
        .collect();

    let policy_refs = spec.policy_refs.as_ref().map(|p| {
        json!({
            "toolPolicy": p.tool_policy,
        })
    });

    json!({
        "protocolVersion": "1.2",
        "namespace": namespace,
        "name": name,
        "displayName": spec.display_name,
        "description": spec.description,
        "endpointUrl": spec.endpoint_url,
        "productionMode": spec.production_mode,
        "capabilities": spec.capabilities,
        "signingKeys": signing_keys,
        "trust": trust,
        "federation": json!({ "peers": federation_peers }),
        "policyRefs": policy_refs,
    })
}

/// Stable SHA-256 over the canonicalised compiled AgentCard, hex-encoded
/// (first 32 chars). Same shape as
/// [`crate::tool_policy_compile::version_hash`] — used as the ConfigMap
/// annotation `azureclaw.azure.com/a2aagent-version-hash`.
#[must_use]
pub fn version_hash(card: &Value) -> String {
    let bytes = serde_json::to_vec(card).expect("serde_json::Value always serialises");
    let digest = Sha256::digest(&bytes);
    hex::encode(&digest[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a_agent::{
        A2AAgentSpec, A2aSigningKey, FederationPeer, PolicyRefs, TrustThresholds,
    };

    fn full_spec() -> A2AAgentSpec {
        A2AAgentSpec {
            endpoint_url: "https://agent.example.com".into(),
            signing_keys: vec![A2aSigningKey {
                kid: "k1".into(),
                alg: "EdDSA".into(),
                public_key_b64u: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                not_after: Some(2_000_000_000),
            }],
            production_mode: true,
            capabilities: vec!["tasks".into(), "streaming".into()],
            trust: Some(TrustThresholds {
                require_signed_requests: true,
                min_signatures_required: Some(1),
                max_clock_skew_seconds: Some(60),
            }),
            federation: vec![FederationPeer {
                label: "buddy".into(),
                kind: "in-cluster".into(),
                agent_ref: Some("buddy-agent".into()),
                endpoint_url: None,
                pinned_kid: None,
            }],
            policy_refs: Some(PolicyRefs {
                tool_policy: Some("pay-cap".into()),
            }),
            display_name: Some("Acme Agent".into()),
            description: Some("Pays bills.".into()),
        }
    }

    #[test]
    fn compile_minimal_spec_yields_protocol_version_and_endpoint() {
        let spec = A2AAgentSpec {
            endpoint_url: "https://x.example".into(),
            signing_keys: vec![A2aSigningKey {
                kid: "k0".into(),
                alg: "EdDSA".into(),
                public_key_b64u: "AAAA".into(),
                not_after: None,
            }],
            ..Default::default()
        };
        let card = compile_agent_card(&spec, "ns", "agent-x");
        assert_eq!(card["protocolVersion"], "1.2");
        assert_eq!(card["namespace"], "ns");
        assert_eq!(card["name"], "agent-x");
        assert_eq!(card["endpointUrl"], "https://x.example");
        assert_eq!(card["signingKeys"].as_array().unwrap().len(), 1);
        // Optional sub-objects are emitted as null on the empty path.
        assert!(card["trust"].is_null());
        assert!(card["policyRefs"].is_null());
        // Federation always emits the peers array (possibly empty).
        assert_eq!(card["federation"]["peers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn compile_full_spec_round_trips_every_field() {
        let card = compile_agent_card(&full_spec(), "default", "acme");
        assert_eq!(card["protocolVersion"], "1.2");
        assert_eq!(card["productionMode"], true);
        assert_eq!(card["displayName"], "Acme Agent");
        assert_eq!(card["description"], "Pays bills.");
        assert_eq!(card["capabilities"][0], "tasks");
        assert_eq!(card["capabilities"][1], "streaming");
        assert_eq!(card["signingKeys"][0]["kid"], "k1");
        assert_eq!(card["signingKeys"][0]["alg"], "EdDSA");
        assert_eq!(card["signingKeys"][0]["notAfter"], 2_000_000_000_i64);
        assert_eq!(card["trust"]["requireSignedRequests"], true);
        assert_eq!(card["trust"]["minSignaturesRequired"], 1);
        assert_eq!(card["trust"]["maxClockSkewSeconds"], 60);
        assert_eq!(card["federation"]["peers"][0]["label"], "buddy");
        assert_eq!(card["federation"]["peers"][0]["kind"], "in-cluster");
        assert_eq!(card["federation"]["peers"][0]["agentRef"], "buddy-agent");
        assert_eq!(card["policyRefs"]["toolPolicy"], "pay-cap");
    }

    #[test]
    fn compile_is_deterministic() {
        let spec = full_spec();
        let a = compile_agent_card(&spec, "default", "acme");
        let b = compile_agent_card(&spec, "default", "acme");
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn version_hash_changes_on_spec_change() {
        let spec_a = full_spec();
        let mut spec_b = full_spec();
        spec_b.signing_keys[0].kid = "rotated".into();
        let h_a = version_hash(&compile_agent_card(&spec_a, "default", "acme"));
        let h_b = version_hash(&compile_agent_card(&spec_b, "default", "acme"));
        assert_ne!(h_a, h_b);
    }

    #[test]
    fn version_hash_changes_when_namespace_or_name_changes() {
        // Cross-namespace federation refs distinguish CRs by
        // (namespace, name); both must enter the hash.
        let spec = full_spec();
        let h_a = version_hash(&compile_agent_card(&spec, "ns-a", "acme"));
        let h_b = version_hash(&compile_agent_card(&spec, "ns-b", "acme"));
        let h_c = version_hash(&compile_agent_card(&spec, "ns-a", "acme2"));
        assert_ne!(h_a, h_b);
        assert_ne!(h_a, h_c);
    }

    #[test]
    fn version_hash_is_hex_16_bytes() {
        let h = version_hash(&compile_agent_card(&full_spec(), "default", "acme"));
        assert_eq!(h.len(), 32, "16 bytes = 32 hex chars");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn version_hash_is_stable_across_serde_round_trip() {
        let spec = full_spec();
        let card_a = compile_agent_card(&spec, "default", "acme");
        let s = serde_json::to_string(&card_a).unwrap();
        let card_b: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(version_hash(&card_a), version_hash(&card_b));
    }
}
