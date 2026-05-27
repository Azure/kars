// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure-function compile step: `KarsMemorySpec` → binding JSON.
//!
//! Separated from the reconciler so it is unit-testable without a
//! `kube::Client`. The output JSON is consumed by the runtime path
//! (`cli/src/plugin.ts::ensureMemoryStore` + the router's existing
//! `/memory_stores/*` proxy in
//! `inference-router/src/routes/inference.rs`). S5 ships only the
//! producer side; S7 wires the runtime informer that reads from this
//! ConfigMap.
//!
//! ## What the compiler is NOT
//!
//! - **Not** a Foundry client. Foundry calls happen at runtime
//!   through the router's Workload Identity, not from the controller.
//!   The compiled binding describes intent; runtime executes.
//! - **Not** a retention enforcer. `retentionDays` flows verbatim;
//!   sweep execution happens via Foundry-side TTL or runtime-path
//!   `delete_scope`.
//! - **Not** a scope conflict resolver. Multiple `KarsMemory` CRs
//!   targeting the same sandbox+scope is a router-side concern (S7);
//!   admission CEL only checks shape.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::kars_memory::KarsMemorySpec;

/// Canonical filename the controller writes into the
/// `karsmemory-<name>-binding` ConfigMap. Kept in lockstep with
/// `inference-router::memory_binding_loader::MEMORY_BINDING_FILENAME`
/// — the byte layout of `canonical_bytes_for_digest` includes this
/// string, so any drift breaks the principles.md §3 "Ready ⇔ router
/// echo" contract.
pub const MEMORY_BINDING_FILENAME: &str = "binding.json";

/// Compile a `KarsMemorySpec` into the binding JSON the controller
/// publishes as a `ConfigMap`.
///
/// Shape:
///
/// ```json
/// {
///   "storeName": "...",
///   "sandboxRef": { "name": "..." },
///   "scope": "...",
///   "retentionDays": 30 | null,
///   "deleteOnSandboxDelete": true,
///   "displayName": "..." | null
/// }
/// ```
#[must_use]
pub fn compile_to_binding(spec: &KarsMemorySpec) -> Value {
    json!({
        "storeName": spec.store_name.clone().unwrap_or_default(),
        "sandboxRef": { "name": spec.sandbox_ref.name },
        "scope": spec.scope.clone().unwrap_or_default(),
        "retentionDays": spec.retention_days,
        "deleteOnSandboxDelete": spec.delete_on_sandbox_delete.unwrap_or(true),
        "displayName": spec.display_name,
    })
}

/// Stable SHA-256 over the canonicalised compiled binding, hex-encoded
/// (first 32 chars). Used as `versionHash` for change detection.
#[must_use]
pub fn version_hash(binding: &Value) -> String {
    let bytes = serde_json::to_vec(binding).expect("serde_json::Value always serialises");
    let digest = Sha256::digest(&bytes);
    hex::encode(&digest[..16])
}

/// Length-prefixed canonical bytes used by both controller and router
/// to compute the same `sha256:<hex>` digest for a single
/// `binding.json` file. Layout:
///
/// ```text
/// u64-BE(filename.len()) || filename || u64-BE(body.len()) || body
/// ```
///
/// Matches the router-side
/// `memory_binding_loader::canonical_bytes_for_digest`. Exposed so
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
/// [`canonical_bytes_for_digest`]) for the supplied compiled
/// `binding.json` body. This is the digest the `KarsMemory`
/// reconciler stamps in `status.compiledDigest` and the ConfigMap
/// annotation `kars.azure.com/claw-memory-digest`. The router
/// echoes the same value via `GET /internal/policy-status` once it
/// loads the file; matching values let `decide_enforcement_state`
/// promote the CR from `phase=Compiled` to `phase=Ready`.
///
/// **Wire contract — DO NOT CHANGE** without a coordinated router-
/// side update.
#[must_use]
pub fn kars_memory_digest(body: &[u8]) -> String {
    let canonical = canonical_bytes_for_digest(MEMORY_BINDING_FILENAME, body);
    let digest = Sha256::digest(&canonical);
    format!("sha256:{}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_memory::{KarsMemorySpec, SandboxRef};

    fn full_spec() -> KarsMemorySpec {
        KarsMemorySpec {
            store_name: Some("agent-x-mem".into()),
            sandbox_ref: SandboxRef {
                name: "agent-x".into(),
            },
            scope: Some("agent:agent-x".into()),
            retention_days: Some(30),
            delete_on_sandbox_delete: Some(true),
            display_name: Some("Agent X memory".into()),
            bundle_ref: None,
        }
    }

    #[test]
    fn compile_minimal_spec_round_trips() {
        let spec = KarsMemorySpec {
            store_name: Some("minimal".into()),
            sandbox_ref: SandboxRef {
                name: "agent".into(),
            },
            scope: Some("agent:agent".into()),
            ..KarsMemorySpec::default()
        };
        let binding = compile_to_binding(&spec);
        assert_eq!(binding["storeName"], "minimal");
        assert_eq!(binding["sandboxRef"]["name"], "agent");
        assert_eq!(binding["scope"], "agent:agent");
        assert!(binding["retentionDays"].is_null());
        // delete_on_sandbox_delete defaults to true at compile time
        // (preserving the prior bool-default semantics).
        assert_eq!(binding["deleteOnSandboxDelete"], true);
    }

    #[test]
    fn compile_full_spec_round_trips() {
        let spec = full_spec();
        let binding = compile_to_binding(&spec);
        assert_eq!(binding["storeName"], "agent-x-mem");
        assert_eq!(binding["sandboxRef"]["name"], "agent-x");
        assert_eq!(binding["scope"], "agent:agent-x");
        assert_eq!(binding["retentionDays"], 30);
        assert_eq!(binding["deleteOnSandboxDelete"], true);
        assert_eq!(binding["displayName"], "Agent X memory");
    }

    #[test]
    fn compile_is_deterministic() {
        let spec = full_spec();
        let a = compile_to_binding(&spec);
        let b = compile_to_binding(&spec);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn version_hash_changes_on_spec_change() {
        let mut a = full_spec();
        let mut b = full_spec();
        b.retention_days = Some(7);
        let h_a = version_hash(&compile_to_binding(&a));
        let h_b = version_hash(&compile_to_binding(&b));
        assert_ne!(h_a, h_b);

        a.display_name = Some("Agent X memory".into());
        let h_a2 = version_hash(&compile_to_binding(&a));
        assert_eq!(h_a, h_a2);
    }

    #[test]
    fn version_hash_is_stable_across_serde_round_trip() {
        let spec = full_spec();
        let binding_a = compile_to_binding(&spec);
        let s = serde_json::to_string(&binding_a).unwrap();
        let binding_b: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(version_hash(&binding_a), version_hash(&binding_b));
    }

    #[test]
    fn version_hash_is_hex_16_bytes() {
        let h = version_hash(&compile_to_binding(&full_spec()));
        assert_eq!(h.len(), 32);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn kars_memory_digest_uses_sha256_prefix_and_64_hex() {
        let body = br#"{"storeName":"x","scope":"agent:x"}"#;
        let d = kars_memory_digest(body);
        assert!(d.starts_with("sha256:"));
        assert_eq!(d.len(), "sha256:".len() + 64);
        assert!(d["sha256:".len()..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn kars_memory_digest_matches_canonical_layout() {
        let body = br#"{"storeName":"x"}"#;
        // Recompute the canonical bytes manually and verify the
        // sha256 matches: the contract is identical to the router
        // side `memory_binding_loader::canonical_bytes_for_digest`.
        let canonical = canonical_bytes_for_digest(MEMORY_BINDING_FILENAME, body);
        let expected = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
        assert_eq!(kars_memory_digest(body), expected);
    }

    #[test]
    fn canonical_bytes_length_prefixed_layout() {
        let body = b"hello";
        let bytes = canonical_bytes_for_digest("binding.json", body);
        // u64-BE("binding.json".len()=12) = 0x000000000000000c, then
        // "binding.json", then u64-BE(5) = 0x0000000000000005, then "hello".
        assert_eq!(&bytes[..8], &(12u64).to_be_bytes());
        assert_eq!(&bytes[8..20], b"binding.json");
        assert_eq!(&bytes[20..28], &(5u64).to_be_bytes());
        assert_eq!(&bytes[28..], b"hello");
    }

    #[test]
    fn memory_binding_filename_is_binding_json() {
        // Wire contract: keep in lockstep with the router-side
        // `memory_binding_loader::MEMORY_BINDING_FILENAME`.
        assert_eq!(MEMORY_BINDING_FILENAME, "binding.json");
    }
}
