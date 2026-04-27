//! Pure-function compile step: `ClawMemorySpec` → binding JSON.
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
//! - **Not** a scope conflict resolver. Multiple `ClawMemory` CRs
//!   targeting the same sandbox+scope is a router-side concern (S7);
//!   admission CEL only checks shape.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::claw_memory::ClawMemorySpec;

/// Compile a `ClawMemorySpec` into the binding JSON the controller
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
pub fn compile_to_binding(spec: &ClawMemorySpec) -> Value {
    json!({
        "storeName": spec.store_name,
        "sandboxRef": { "name": spec.sandbox_ref.name },
        "scope": spec.scope,
        "retentionDays": spec.retention_days,
        "deleteOnSandboxDelete": spec.delete_on_sandbox_delete,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claw_memory::{ClawMemorySpec, SandboxRef};

    fn full_spec() -> ClawMemorySpec {
        ClawMemorySpec {
            store_name: "agent-x-mem".into(),
            sandbox_ref: SandboxRef {
                name: "agent-x".into(),
            },
            scope: "agent:agent-x".into(),
            retention_days: Some(30),
            delete_on_sandbox_delete: true,
            display_name: Some("Agent X memory".into()),
        }
    }

    #[test]
    fn compile_minimal_spec_round_trips() {
        let spec = ClawMemorySpec {
            store_name: "minimal".into(),
            sandbox_ref: SandboxRef {
                name: "agent".into(),
            },
            scope: "agent:agent".into(),
            ..ClawMemorySpec::default()
        };
        let binding = compile_to_binding(&spec);
        assert_eq!(binding["storeName"], "minimal");
        assert_eq!(binding["sandboxRef"]["name"], "agent");
        assert_eq!(binding["scope"], "agent:agent");
        assert!(binding["retentionDays"].is_null());
        // delete_on_sandbox_delete defaults to false on Default ⇒ false.
        assert_eq!(binding["deleteOnSandboxDelete"], false);
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
}
