// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Controller configuration hash.
//!
//! P2 #12: Computes a deterministic 16-hex-char SHA-256 digest over
//! the controller's runtime configuration (image refs, Foundry
//! endpoints, leader-election toggles, …). The hash is logged at
//! startup and surfaced as a Prometheus info-metric so operators
//! can:
//!
//! - See whether two controller pods are running the same config
//!   (lease handover sanity check).
//! - Correlate a config rollout with a behaviour change (the hash
//!   changes iff one of the inputs changes).
//! - Use it as one input to a future per-CR
//!   `status.observedHash` skip-cache (P0 item 3 in §14.6's
//!   roadmap), so bumping the controller invalidates every CR's
//!   skip-cache exactly once on next reconcile.
//!
//! Determinism guarantees:
//! - The list of input env vars is hard-coded in this module
//!   (`CONFIG_HASH_INPUTS`), so the hash domain is stable across
//!   restarts.
//! - Inputs are joined with a NUL byte separator + key-sorted, so
//!   the hash is independent of process env-var iteration order.
//! - Missing env vars are recorded as the empty string so
//!   "unset" and "set to empty" are equivalent (operationally
//!   identical for this controller).

use prometheus::{IntGaugeVec, opts, register_int_gauge_vec};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Env var names that contribute to the controller config hash.
///
/// Adding/removing entries from this list is itself a config-hash
/// change and should be called out in the audit trail.
pub const CONFIG_HASH_INPUTS: &[&str] = &[
    "AZURECLAW_DISABLE_ENTRA_AUTH",
    "AZURE_AUTHORITY_HOST",
    "AZURE_OPENAI_ENDPOINT",
    "AZURE_SUBSCRIPTION_ID",
    "AZURE_TENANT_ID",
    "BYO_STRICT_MODE",
    "CLUSTER_NAME",
    "CONTENT_SAFETY_ENDPOINT",
    "CONTROLLER_METRICS_ADDR",
    "FEDCRED_REAPER_INTERVAL_SECS",
    "FOUNDRY_DEPLOYMENTS",
    "FOUNDRY_ENDPOINT",
    "FOUNDRY_PROJECT_ENDPOINT",
    "IDENTITY_NAME",
    "IDENTITY_RESOURCE_GROUP",
    "IMDS_CLIENT_ID",
    "INFERENCE_ROUTER_IMAGE",
    "LEADER_ELECTION_ENABLED",
    "LEADER_ELECTION_LEASE_NAME",
    "MAF_RUNTIME_IMAGE",
    "MESH_PEER_ENABLED",
    "MESH_REGISTRY_URL",
    "MESH_RELAY_URL",
    "OIDC_ISSUER_URL",
    "OPENAI_AGENTS_RUNTIME_IMAGE",
    "SANDBOX_IMAGE",
];

/// Compute the controller config hash from the supplied lookup
/// function. Pure for testability — production code calls
/// [`compute_from_env`].
pub fn compute_with<F>(inputs: &[&str], lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let mut keys: Vec<&str> = inputs.to_vec();
    keys.sort_unstable();
    keys.dedup();

    let mut hasher = Sha256::new();
    for k in keys {
        let v = lookup(k).unwrap_or_default();
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    // 16 hex chars (8 bytes) is plenty for change-detection while
    // staying small enough to log/label cheaply.
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// Compute the controller config hash from `std::env`.
pub fn compute_from_env() -> String {
    compute_with(CONFIG_HASH_INPUTS, |k| std::env::var(k).ok())
}

/// Prometheus info-metric exposing the current controller config
/// hash. Always set to `1`; the hash is a label so PromQL queries
/// can join on it.
///
/// Cardinality: exactly one series per controller pod. On a config
/// rollover the *new* hash gets a new series; the old one stops
/// being touched and falls out after the scrape-staleness window.
pub static CONTROLLER_CONFIG_INFO: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        opts!(
            "azureclaw_controller_config_info",
            "Controller config hash info-metric (always 1; hash is a label)"
        ),
        &["config_hash"]
    )
    .expect("failed to register azureclaw_controller_config_info")
});

/// Stamp the gauge with the current config hash. Idempotent.
pub fn record_config_hash(config_hash: &str) {
    CONTROLLER_CONFIG_INFO
        .with_label_values(&[config_hash])
        .set(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_fn(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |k: &str| map.get(k).map(|s| s.to_string())
    }

    #[test]
    fn hash_is_deterministic() {
        let inputs = &["A", "B", "C"];
        let l = lookup_fn(HashMap::from([("A", "1"), ("B", "2"), ("C", "3")]));
        let h1 = compute_with(inputs, &l);
        let h2 = compute_with(inputs, &l);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn hash_is_input_order_independent() {
        let l = lookup_fn(HashMap::from([("A", "1"), ("B", "2"), ("C", "3")]));
        let h_sorted = compute_with(&["A", "B", "C"], &l);
        let h_shuffled = compute_with(&["C", "A", "B"], &l);
        assert_eq!(h_sorted, h_shuffled);
    }

    #[test]
    fn hash_changes_when_value_changes() {
        let inputs = &["A"];
        let l1 = lookup_fn(HashMap::from([("A", "v1")]));
        let l2 = lookup_fn(HashMap::from([("A", "v2")]));
        assert_ne!(compute_with(inputs, &l1), compute_with(inputs, &l2));
    }

    #[test]
    fn hash_treats_unset_and_empty_as_equivalent() {
        let inputs = &["A", "B"];
        let l_unset = lookup_fn(HashMap::from([("A", "v")]));
        let l_empty = lookup_fn(HashMap::from([("A", "v"), ("B", "")]));
        assert_eq!(
            compute_with(inputs, &l_unset),
            compute_with(inputs, &l_empty)
        );
    }

    #[test]
    fn hash_changes_when_key_added_to_input_set() {
        let l = lookup_fn(HashMap::from([("A", "1"), ("B", "2")]));
        // "B" missing from inputs vs included with value "2" must
        // differ — guards against accidental input-set drift.
        let h_a_only = compute_with(&["A"], &l);
        let h_a_b = compute_with(&["A", "B"], &l);
        assert_ne!(h_a_only, h_a_b);
    }

    #[test]
    fn duplicate_inputs_do_not_affect_hash() {
        let l = lookup_fn(HashMap::from([("A", "1"), ("B", "2")]));
        let h1 = compute_with(&["A", "B"], &l);
        let h2 = compute_with(&["A", "B", "A"], &l);
        assert_eq!(h1, h2);
    }

    #[test]
    fn record_config_hash_renders_info_metric() {
        record_config_hash("deadbeefcafebabe");
        let mut buf = Vec::new();
        let encoder = prometheus::TextEncoder::new();
        let families = prometheus::gather();
        prometheus::Encoder::encode(&encoder, &families, &mut buf).unwrap();
        let rendered = String::from_utf8(buf).unwrap();
        assert!(rendered.contains("azureclaw_controller_config_info"));
        assert!(rendered.contains("deadbeefcafebabe"));
    }

    #[test]
    fn production_input_list_is_non_empty_and_unique() {
        assert!(!CONFIG_HASH_INPUTS.is_empty());
        let mut seen = std::collections::HashSet::new();
        for k in CONFIG_HASH_INPUTS {
            assert!(
                seen.insert(*k),
                "duplicate input key in CONFIG_HASH_INPUTS: {k}"
            );
        }
    }
}
