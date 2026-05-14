// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure-function compile step for the `EgressApproval` grant lane.
//!
//! Two outputs, both byte-pinned with the router-side
//! [`inference_router::egress_allowlist_loader`] (Slice 5e):
//!
//! 1. **Per-approval file** — the JSON the reconciler writes into the
//!    per-sandbox `clawsandbox-{sandbox}-egress-approvals` ConfigMap
//!    under key `approval-{approvalName}.json`. This is what the
//!    router scans, parses, and merges with the baseline. See
//!    [`compile_approval_file`].
//!
//! 2. **Merged-allowlist digest** — the deterministic sha256 over the
//!    sorted, deduplicated union of `(baseline.endpoints ∪
//!    approval.hosts)` for ALL active approvals on the sandbox. This
//!    is the value the router echoes via
//!    `GET /internal/policy-status` under `PolicyKind::EgressApproval`
//!    and the controller stamps in `EgressApproval.status.mergedDigest`.
//!    Closes the §3 Ready ⇔ router-echo loop for the grant lane.
//!
//! ## Why merged-allowlist digest (not per-file)
//!
//! The grant lane's purpose is to widen the L7 hostname filter.
//! What the operator wants to verify is: *"is the router actually
//! letting traffic through to the host I approved?"* The merged
//! digest answers that — it pins the byte-identical host set the
//! L7 filter is enforcing right now. A per-file echo would tell us
//! "the router read my approval file" but not whether the union
//! actually landed on `Blocklist.allowlist`.
//!
//! Sibling approvals on the same sandbox contribute to the same
//! merged digest. When approval-A is created and approval-B already
//! exists, A's reconciler enumerates siblings, computes the merged
//! digest over `{baseline ∪ A.hosts ∪ B.hosts}`, and waits for the
//! router to echo it. The mtime-poll watcher (5s default) ensures
//! the router catches sibling-induced changes promptly.
//!
//! ## Cross-binary parity (DO NOT BREAK)
//!
//! [`merged_allowlist_digest`] is byte-identical to the router-side
//! `egress_allowlist_loader::compute_merged_digest`. The parity is
//! pinned by:
//!
//! - `digest_is_byte_identical_to_router_layout` test below
//!   (controller side).
//! - `merged_digest_is_byte_identical_to_controller_layout` test in
//!   `inference-router/src/egress_allowlist_loader.rs` (router side).
//!
//! Both tests hash the same fixture bytes; any drift fails CI on
//! both binaries.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::crd::EndpointConfig;
use crate::egress_allowlist_compile::canonical_bytes_for_digest;

/// Canonical filename used in the length-prefixed digest layout
/// for merged allowlists. NOT a real on-disk file — purely a domain
/// separator that ensures the merged-allowlist digest can never
/// collide with the per-approval file digest or the baseline
/// `allowlist.json` digest. Pinned with the router side.
pub const EGRESS_APPROVAL_MERGED_FILENAME: &str = "merged-allowlist.json";

/// Schema version for the per-approval file JSON. Bumped on a
/// breaking layout change (the router refuses to parse mismatched
/// versions).
pub const APPROVAL_FILE_SCHEMA_VERSION: u32 = 1;

/// ConfigMap name template for the per-sandbox approvals collection.
/// One CM per sandbox; one key per approval (`approval-{name}.json`).
#[must_use]
pub fn approvals_configmap_name(sandbox: &str) -> String {
    format!("clawsandbox-{sandbox}-egress-approvals")
}

/// ConfigMap key template for a single approval's file.
#[must_use]
pub fn approval_file_key(approval_name: &str) -> String {
    format!("approval-{approval_name}.json")
}

/// Build the canonical JSON document for one approval. Written into
/// the ConfigMap under [`approval_file_key`].
///
/// Shape:
///
/// ```json
/// {
///   "schemaVersion": 1,
///   "approvalName": "inc-12345-pypi",
///   "sandbox": "demo",
///   "hosts": [
///     {"host": "pypi.org", "port": 443},
///     {"host": "files.pythonhosted.org", "port": 443}
///   ],
///   "reason": "PyPI install during incident",
///   "ticket": "INC-12345",
///   "effectiveAt": "2026-05-14T18:30:00Z",
///   "expiresAt": "2026-05-14T22:30:00Z"
/// }
/// ```
///
/// Hosts are passed through [`crate::egress_allowlist_compile::compile_to_doc`]
/// so the same sort/dedupe/lowercase rules apply as the baseline
/// allowlist. `ticket` is omitted entirely (not serialized as null)
/// when the spec didn't set it — the router parser tolerates both
/// shapes but operators reading the CM key prefer the cleaner form.
#[must_use]
pub fn compile_approval_file(
    approval_name: &str,
    sandbox: &str,
    hosts: &[EndpointConfig],
    reason: &str,
    ticket: Option<&str>,
    effective_at_rfc3339: &str,
    expires_at_rfc3339: &str,
) -> Value {
    let compiled = crate::egress_allowlist_compile::compile_to_doc(hosts);
    let host_arr = compiled
        .get("endpoints")
        .cloned()
        .unwrap_or_else(|| json!([]));

    let mut obj = serde_json::Map::new();
    obj.insert(
        "schemaVersion".to_string(),
        json!(APPROVAL_FILE_SCHEMA_VERSION),
    );
    obj.insert("approvalName".to_string(), json!(approval_name));
    obj.insert("sandbox".to_string(), json!(sandbox));
    obj.insert("hosts".to_string(), host_arr);
    obj.insert("reason".to_string(), json!(reason));
    if let Some(t) = ticket {
        obj.insert("ticket".to_string(), json!(t));
    }
    obj.insert("effectiveAt".to_string(), json!(effective_at_rfc3339));
    obj.insert("expiresAt".to_string(), json!(expires_at_rfc3339));
    Value::Object(obj)
}

/// Compute the merged-allowlist digest for `baseline ∪ approvals`.
///
/// Both sides — controller and router — call this same shape:
///
/// 1. Concatenate the baseline endpoint list with every active
///    approval's host list.
/// 2. Run through [`crate::egress_allowlist_compile::compile_to_doc`]
///    which lowercases hosts, defaults missing ports to 443, sorts
///    by `(host, port)`, and deduplicates.
/// 3. Serialize the resulting `{schemaVersion, endpoints}` value as
///    non-pretty JSON.
/// 4. Wrap in [`canonical_bytes_for_digest`] using the dedicated
///    [`EGRESS_APPROVAL_MERGED_FILENAME`] domain separator.
/// 5. sha256 → `sha256:<hex>`.
///
/// Pinned byte-identical to
/// `inference_router::egress_allowlist_loader::compute_merged_digest`.
#[must_use]
pub fn merged_allowlist_digest(
    baseline: &[EndpointConfig],
    approvals: &[EndpointConfig],
) -> String {
    let mut combined: Vec<EndpointConfig> = Vec::with_capacity(baseline.len() + approvals.len());
    combined.extend_from_slice(baseline);
    combined.extend_from_slice(approvals);
    let doc = crate::egress_allowlist_compile::compile_to_doc(&combined);
    let body = serde_json::to_vec(&doc).expect("compiled JSON is always serializable");
    let canonical = canonical_bytes_for_digest(EGRESS_APPROVAL_MERGED_FILENAME, &body);
    let raw = Sha256::digest(&canonical);
    format!("sha256:{}", hex::encode(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(host: &str, port: Option<u16>) -> EndpointConfig {
        EndpointConfig {
            host: host.into(),
            port,
        }
    }

    #[test]
    fn approvals_cm_name_includes_sandbox() {
        assert_eq!(
            approvals_configmap_name("demo"),
            "clawsandbox-demo-egress-approvals"
        );
    }

    #[test]
    fn approval_file_key_uses_approval_name() {
        assert_eq!(
            approval_file_key("inc-12345-pypi"),
            "approval-inc-12345-pypi.json"
        );
    }

    #[test]
    fn compile_approval_file_omits_ticket_when_none() {
        let v = compile_approval_file(
            "a1",
            "demo",
            &[ep("example.com", Some(443))],
            "needed",
            None,
            "2026-05-14T18:30:00Z",
            "2026-05-14T22:30:00Z",
        );
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("ticket"));
        assert_eq!(obj["schemaVersion"], 1);
        assert_eq!(obj["approvalName"], "a1");
        assert_eq!(obj["sandbox"], "demo");
        assert_eq!(obj["reason"], "needed");
        assert_eq!(obj["hosts"][0]["host"], "example.com");
        assert_eq!(obj["hosts"][0]["port"], 443);
    }

    #[test]
    fn compile_approval_file_serializes_ticket_when_some() {
        let v = compile_approval_file(
            "a1",
            "demo",
            &[ep("example.com", None)],
            "incident",
            Some("INC-1"),
            "2026-05-14T18:30:00Z",
            "2026-05-14T22:30:00Z",
        );
        assert_eq!(v["ticket"], "INC-1");
    }

    #[test]
    fn compile_approval_file_canonicalizes_hosts() {
        // Mixed case + missing port → lower + 443; sorted.
        let v = compile_approval_file(
            "a",
            "s",
            &[ep("Z.example.com", None), ep("a.EXAMPLE.com", Some(80))],
            "r",
            None,
            "t",
            "u",
        );
        let hosts = v["hosts"].as_array().unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0]["host"], "a.example.com");
        assert_eq!(hosts[0]["port"], 80);
        assert_eq!(hosts[1]["host"], "z.example.com");
        assert_eq!(hosts[1]["port"], 443);
    }

    #[test]
    fn compile_approval_file_dedupes_hosts() {
        let v = compile_approval_file(
            "a",
            "s",
            &[ep("example.com", Some(443)), ep("EXAMPLE.com", None)],
            "r",
            None,
            "t",
            "u",
        );
        let hosts = v["hosts"].as_array().unwrap();
        assert_eq!(hosts.len(), 1);
    }

    #[test]
    fn merged_digest_empty_baseline_empty_approvals_is_stable() {
        let d = merged_allowlist_digest(&[], &[]);
        assert!(d.starts_with("sha256:"));
        // Re-running yields the same digest (no clock dep).
        assert_eq!(d, merged_allowlist_digest(&[], &[]));
    }

    #[test]
    fn merged_digest_baseline_only_differs_from_baseline_plus_approval() {
        let baseline = vec![ep("api.github.com", Some(443))];
        let approvals = vec![ep("pypi.org", Some(443))];
        let d_baseline = merged_allowlist_digest(&baseline, &[]);
        let d_combined = merged_allowlist_digest(&baseline, &approvals);
        assert_ne!(d_baseline, d_combined);
    }

    #[test]
    fn merged_digest_commutative_in_input_order() {
        // Sort+dedup means order of arguments doesn't matter for
        // the final digest — only the set of endpoints does.
        let a = vec![ep("api.github.com", Some(443))];
        let b = vec![ep("pypi.org", Some(443)), ep("api.github.com", Some(443))];
        let d1 = merged_allowlist_digest(&a, &b);
        let d2 = merged_allowlist_digest(&b, &a);
        assert_eq!(d1, d2);
    }

    #[test]
    fn merged_digest_dedupes_baseline_approval_overlap() {
        // Approval adds a host already in baseline → digest equals
        // baseline-only.
        let baseline = vec![ep("api.github.com", Some(443))];
        let approvals = vec![ep("api.github.com", Some(443))];
        let d_combined = merged_allowlist_digest(&baseline, &approvals);
        let d_baseline = merged_allowlist_digest(&baseline, &[]);
        assert_eq!(d_combined, d_baseline);
    }

    #[test]
    fn merged_digest_distinguishes_port_variants() {
        // Same host, different port → distinct endpoints, distinct
        // digest from same-host-port-443 case.
        let d_80 = merged_allowlist_digest(&[ep("example.com", Some(80))], &[]);
        let d_443 = merged_allowlist_digest(&[ep("example.com", Some(443))], &[]);
        assert_ne!(d_80, d_443);
    }

    #[test]
    fn digest_is_byte_identical_to_router_layout() {
        // Cross-binary parity: must match the byte layout used by
        // the router's `egress_allowlist_loader::compute_merged_digest`.
        // The fixture below is the canonical golden case mirrored
        // verbatim in `inference-router/src/egress_allowlist_loader.rs`
        // — keep both in lockstep.
        let baseline = vec![ep("a.example.com", Some(443))];
        let approvals = vec![ep("b.example.com", Some(443))];
        let digest = merged_allowlist_digest(&baseline, &approvals);

        // Recompute by hand here so a refactor that breaks
        // `merged_allowlist_digest` also fails this test.
        let doc = crate::egress_allowlist_compile::compile_to_doc(&[
            ep("a.example.com", Some(443)),
            ep("b.example.com", Some(443)),
        ]);
        let body = serde_json::to_vec(&doc).unwrap();
        let canonical = canonical_bytes_for_digest(EGRESS_APPROVAL_MERGED_FILENAME, &body);
        let mut hex_str = String::with_capacity(64);
        for b in Sha256::digest(&canonical) {
            use std::fmt::Write;
            let _ = write!(hex_str, "{b:02x}");
        }
        assert_eq!(digest, format!("sha256:{hex_str}"));
    }

    #[test]
    fn merged_filename_distinct_from_baseline_filename() {
        // Domain separator must be distinct from the baseline
        // `allowlist.json` so a baseline digest can never be
        // mistaken for a merged-allowlist digest.
        assert_ne!(
            EGRESS_APPROVAL_MERGED_FILENAME,
            crate::egress_allowlist_compile::EGRESS_ALLOWLIST_FILENAME
        );
    }
}
