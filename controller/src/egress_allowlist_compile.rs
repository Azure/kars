// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure-function compile step: resolved egress endpoints → `allowlist.json`.
//!
//! Separated from the reconciler so the canonical-bytes layout is
//! unit-testable without a `kube::Client`. The output JSON is consumed
//! by the router's [`inference_router::egress_allowlist_loader`] which
//! seeds `Blocklist.allowlist` and echoes the digest via
//! `GET /internal/policy-status` under `PolicyKind::EgressAllowlist`.
//!
//! ## Why this is Slice 5c.1
//!
//! Pre-5c.1, the router's L7 hostname filter
//! (`forward_proxy::handle_connect → blocklist.check_egress`)
//! consulted an in-memory `HashSet` populated solely by the
//! `POST /egress/approve` admin endpoint. The signed allowlist
//! (`spec.networkPolicy.allowlistRef` + cosign verify in
//! [`crate::policy_fetcher::resolve_allowlist`]) was wired into the
//! K8s `NetworkPolicy` egress rules (L4 port only) — never reaching
//! the router's L7 check.
//!
//! Result: the signed bundle had **no runtime teeth**. Operators who
//! signed a 5-host allowlist still saw their agents reach arbitrary
//! `:443` hosts because the K8s rule was `0.0.0.0/0 except RFC1918`.
//!
//! Slice 5c.1 closes that gap. The reconciler now:
//! 1. Resolves the allowlist via the existing
//!    [`crate::policy_fetcher::resolve_allowlist`] (same verify path).
//! 2. Compiles it to `allowlist.json` via [`compile_to_doc`] here.
//! 3. Publishes as `ConfigMap` `clawsandbox-{name}-egress-allowlist`
//!    with annotation `azureclaw.azure.com/egress-allowlist-digest`.
//! 4. Mounts into the inference-router container at
//!    `/etc/azureclaw/egress/allowlist.json`.
//!
//! The router-side loader (`egress_allowlist_loader.rs`) reads the
//! mount, replaces its in-memory allowlist atomically, and registers
//! the digest under `PolicyKind::EgressAllowlist`.
//!
//! ## What this compiler is NOT
//!
//! - **Not** a verifier. cosign verification happens upstream in
//!   `policy_fetcher::resolve_allowlist`. By the time endpoints reach
//!   `compile_to_doc`, they are either (a) inline (unsigned, no
//!   `allowlistRef`), (b) verified-from-artifact, or (c) LKG cache.
//!   The consumer (router) doesn't re-verify — it trusts the
//!   controller's k8s-mediated channel + the per-container mount.
//! - **Not** an approval merger. `EgressApproval` CRD (Slice 5e) lives
//!   in a separate mount (`approvals/*.json`); the router merges
//!   `bundle ∪ approvals` at load time in `egress_allowlist_loader`,
//!   not in this bundle JSON.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::crd::EndpointConfig;

/// Canonical filename the controller writes into the
/// `clawsandbox-{name}-egress-allowlist` ConfigMap. Kept in lockstep
/// with the router-side
/// `inference_router::egress_allowlist_loader::EGRESS_ALLOWLIST_FILENAME`
/// — the byte layout of [`canonical_bytes_for_digest`] includes this
/// string, so any drift breaks the principles.md §3 "Ready ⇔ router
/// echo" contract.
pub const EGRESS_ALLOWLIST_FILENAME: &str = "allowlist.json";

/// Compile resolved endpoints into the `allowlist.json` document the
/// controller publishes as a `ConfigMap`.
///
/// Shape:
///
/// ```json
/// {
///   "schemaVersion": 1,
///   "endpoints": [
///     {"host": "api.github.com", "port": 443},
///     {"host": "objects.githubusercontent.com", "port": 443}
///   ]
/// }
/// ```
///
/// Endpoints are sorted lexicographically by `(host, port)` and
/// host-lowercased so the byte layout is deterministic regardless of
/// the order the operator listed them in the source artifact /
/// inline.
///
/// `port` defaults to 443 when unset (matching the canonical-format
/// doc — `docs/internal/policy-canonical-format.md`).
#[must_use]
pub fn compile_to_doc(endpoints: &[EndpointConfig]) -> Value {
    let mut normalized: Vec<(String, u16)> = endpoints
        .iter()
        .map(|e| (e.host.trim().to_ascii_lowercase(), e.port.unwrap_or(443)))
        .filter(|(h, _)| !h.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();

    let arr: Vec<Value> = normalized
        .into_iter()
        .map(|(h, p)| json!({ "host": h, "port": p }))
        .collect();

    json!({
        "schemaVersion": 1,
        "endpoints": arr,
    })
}

/// Length-prefixed canonical bytes used by both controller and router
/// to compute the same `sha256:<hex>` digest for a single
/// `allowlist.json` file. Layout:
///
/// ```text
/// u64-BE(filename.len()) || filename || u64-BE(body.len()) || body
/// ```
///
/// Matches the router-side
/// `egress_allowlist_loader::canonical_bytes_for_digest`. Identical
/// helper-by-helper to `claw_memory_compile::canonical_bytes_for_digest`
/// — the layout is a project-wide convention for `/etc/azureclaw/*`
/// mounts that closes the §3 echo loop.
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
/// `allowlist.json` body. This is the digest the sandbox
/// reconciler stamps in the ConfigMap annotation
/// `azureclaw.azure.com/egress-allowlist-digest`. The router echoes
/// the same value via `GET /internal/policy-status` once it loads
/// the file.
///
/// **Wire contract — DO NOT CHANGE** without a coordinated router-
/// side update.
#[must_use]
pub fn egress_allowlist_digest(body: &[u8]) -> String {
    let canonical = canonical_bytes_for_digest(EGRESS_ALLOWLIST_FILENAME, body);
    let digest = Sha256::digest(&canonical);
    format!("sha256:{}", hex::encode(digest))
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
    fn compile_empty_endpoints_produces_empty_array() {
        let doc = compile_to_doc(&[]);
        assert_eq!(doc["schemaVersion"], 1);
        assert_eq!(doc["endpoints"], json!([]));
    }

    #[test]
    fn compile_normalizes_hosts_lowercase_and_defaults_port_443() {
        let doc = compile_to_doc(&[
            ep("API.GitHub.com", None),
            ep("Objects.GithubUserContent.com", Some(443)),
        ]);
        assert_eq!(
            doc["endpoints"],
            json!([
                {"host": "api.github.com", "port": 443},
                {"host": "objects.githubusercontent.com", "port": 443},
            ])
        );
    }

    #[test]
    fn compile_sorts_lexicographically_by_host_then_port() {
        let doc = compile_to_doc(&[
            ep("z.example.com", None),
            ep("a.example.com", Some(80)),
            ep("a.example.com", Some(443)),
        ]);
        assert_eq!(
            doc["endpoints"],
            json!([
                {"host": "a.example.com", "port": 80},
                {"host": "a.example.com", "port": 443},
                {"host": "z.example.com", "port": 443},
            ])
        );
    }

    #[test]
    fn compile_deduplicates_repeated_host_port_pairs() {
        let doc = compile_to_doc(&[
            ep("api.github.com", None),
            ep("API.GITHUB.COM", Some(443)),
            ep("api.github.com", Some(443)),
        ]);
        assert_eq!(doc["endpoints"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn compile_drops_blank_hosts() {
        let doc = compile_to_doc(&[ep("", None), ep("   ", Some(443)), ep("real.example", None)]);
        assert_eq!(
            doc["endpoints"],
            json!([{"host": "real.example", "port": 443}])
        );
    }

    #[test]
    fn compile_is_deterministic_across_input_order() {
        let a = compile_to_doc(&[ep("b.example", None), ep("a.example", None)]);
        let b = compile_to_doc(&[ep("a.example", None), ep("b.example", None)]);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn digest_starts_with_sha256_and_has_64_hex() {
        let doc = compile_to_doc(&[ep("api.github.com", None)]);
        let bytes = serde_json::to_vec(&doc).unwrap();
        let d = egress_allowlist_digest(&bytes);
        assert!(d.starts_with("sha256:"));
        assert_eq!(d.len(), "sha256:".len() + 64);
        assert!(d["sha256:".len()..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn digest_matches_canonical_layout() {
        let body = br#"{"schemaVersion":1,"endpoints":[]}"#;
        let canonical = canonical_bytes_for_digest(EGRESS_ALLOWLIST_FILENAME, body);
        let expected = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
        assert_eq!(egress_allowlist_digest(body), expected);
    }

    #[test]
    fn canonical_bytes_length_prefixed_layout() {
        let body = b"hello";
        let bytes = canonical_bytes_for_digest("allowlist.json", body);
        // u64-BE("allowlist.json".len()=14) prefix.
        assert_eq!(&bytes[..8], &(14u64).to_be_bytes());
        assert_eq!(&bytes[8..22], b"allowlist.json");
        assert_eq!(&bytes[22..30], &(5u64).to_be_bytes());
        assert_eq!(&bytes[30..], b"hello");
    }

    #[test]
    fn egress_allowlist_filename_is_allowlist_json() {
        // Wire contract: keep in lockstep with the router-side
        // `egress_allowlist_loader::EGRESS_ALLOWLIST_FILENAME`.
        assert_eq!(EGRESS_ALLOWLIST_FILENAME, "allowlist.json");
    }

    #[test]
    fn different_endpoint_sets_produce_different_digests() {
        let a = serde_json::to_vec(&compile_to_doc(&[ep("a.example", None)])).unwrap();
        let b = serde_json::to_vec(&compile_to_doc(&[ep("b.example", None)])).unwrap();
        assert_ne!(egress_allowlist_digest(&a), egress_allowlist_digest(&b));
    }

    #[test]
    fn equivalent_endpoint_sets_produce_equal_digests() {
        let a = serde_json::to_vec(&compile_to_doc(&[
            ep("A.example", Some(443)),
            ep("b.example", None),
        ]))
        .unwrap();
        let b = serde_json::to_vec(&compile_to_doc(&[
            ep("b.example", Some(443)),
            ep("a.example", None),
        ]))
        .unwrap();
        assert_eq!(egress_allowlist_digest(&a), egress_allowlist_digest(&b));
    }
}
