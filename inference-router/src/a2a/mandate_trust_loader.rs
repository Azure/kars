// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! AP2 mandate-trust file loader.
//!
//! Production-grade alternative to the in-memory-only test fixtures
//! used by the `mandate_trust_store` tests. Loads a static JSON
//! document from disk into a fresh [`MandateTrustStoreSnapshot`] so
//! the inference router can verify AP2 [`crate::a2a::ap2::IntentMandate`]
//! signatures at startup without depending on a live K8s informer.
//!
//! ## File format
//!
//! Either an A2A-projection-shaped object:
//!
//! ```json
//! {
//!   "namespace": "azureclaw-system",
//!   "name": "mandate-issuer-bootstrap",
//!   "signingKeys": [
//!     {
//!       "kid": "issuer-2026-01",
//!       "alg": "EdDSA",
//!       "publicKeyB64u": "<32-byte ed25519 verifying key, base64url, no padding>",
//!       "notAfter": 1830000000
//!     }
//!   ]
//! }
//! ```
//!
//! …or an array of such objects to permit a single file to advertise
//! multiple issuers (e.g., bootstrap + per-tenant). The shape mirrors
//! [`crate::a2a::agent_projection::A2aAgentSpec`] verbatim — same
//! field names, same EdDSA-only algorithm pin, same 32-byte
//! base64url-no-padding key encoding — so when the future
//! `MandateIssuer` CRD lands the controller-side write path is a
//! one-liner: serialise the CR's spec to JSON and write it to the
//! mounted file.
//!
//! ## Why static-file load and not env-only?
//!
//! - Public keys are larger than env-var hygiene tolerates (44+
//!   chars, base64-encoded).
//! - Multiple anchors → array of objects fits naturally in a file,
//!   not in a single env var.
//! - K8s ConfigMap mount → file is the canonical pattern in this
//!   codebase (see `A2A_CARD_DIR` in `routes/a2a.rs::A2aRouteState::from_card_dir`).
//!
//! ## Failure semantics
//!
//! Any parse / projection error returns a typed
//! [`MandateTrustLoadError`]. The caller (router boot in `main.rs`)
//! is expected to **log** the error and proceed with an empty trust
//! store. An empty trust store is *fail-closed* by design: every
//! AP2-bearing message is rejected with `Ap2Denied`. Operators see
//! the rejection in audit, see the warning at boot, and fix the
//! mount.
//!
//! Note that an empty trust store has zero impact on AP2-free
//! traffic — the AP2 path is only entered when `metadata.ap2` is
//! present on the inbound message.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use crate::a2a::agent_projection::{A2aAgentSpec, ProjectionError, project_anchors};
use crate::a2a::mandate_trust_store::MandateTrustStoreSnapshot;
use crate::a2a::trust_store::{TrustStoreBuildError, TrustStoreBuilder};

/// Errors raised by [`load_mandate_trust_snapshot`].
#[derive(thiserror::Error, Debug)]
pub enum MandateTrustLoadError {
    /// The file could not be read.
    #[error("read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file contents were not valid UTF-8 / JSON.
    #[error("parse {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// One of the projected entries was rejected by
    /// [`project_anchors`] (bad kid / alg / key bytes).
    #[error("project {path}: entry {entry}: {source}")]
    Projection {
        path: PathBuf,
        entry: usize,
        #[source]
        source: ProjectionError,
    },
    /// Two entries (across all CRs in the file) collided on `kid`.
    #[error("trust store build {path}: {source}")]
    Build {
        path: PathBuf,
        #[source]
        source: TrustStoreBuildError,
    },
}

/// Load a [`MandateTrustStoreSnapshot`] from a JSON file.
///
/// `path` must exist and contain either a single object matching
/// [`A2aAgentSpec`] (with `namespace`, `name`, `signingKeys[*]`) or
/// an array of such objects. The result is a snapshot ready to feed
/// to [`crate::a2a::mandate_trust_store::MandateTrustStore::replace_snapshot`].
///
/// Determinism: anchors are inserted in file order, with each spec's
/// keys appended in array order. The underlying
/// [`TrustStoreBuilder`] enforces kid uniqueness across the whole
/// file.
pub fn load_mandate_trust_snapshot(
    path: &Path,
) -> Result<MandateTrustStoreSnapshot, MandateTrustLoadError> {
    let bytes = std::fs::read(path).map_err(|e| MandateTrustLoadError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let specs = parse_specs(&bytes).map_err(|e| MandateTrustLoadError::Json {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Generation 1 — every (re)load bumps to 1, mirroring how the
    // tests in `mandate_trust_store::tests` seed the store.
    let mut builder = TrustStoreBuilder::new().generation(1);
    for (entry, spec) in specs.iter().enumerate() {
        let anchors = project_anchors(spec, "mandate-issuer-file").map_err(|e| {
            MandateTrustLoadError::Projection {
                path: path.to_path_buf(),
                entry,
                source: e,
            }
        })?;
        for anchor in anchors {
            builder
                .add(anchor)
                .map_err(|e| MandateTrustLoadError::Build {
                    path: path.to_path_buf(),
                    source: e,
                })?;
        }
    }
    Ok(MandateTrustStoreSnapshot::from_inner(builder.build()))
}

/// Parse a JSON document holding one or more [`A2aAgentSpec`]s.
fn parse_specs(bytes: &[u8]) -> Result<Vec<A2aAgentSpec>, serde_json::Error> {
    // Try array first; fall back to single object. Doing it in this
    // order avoids the cost of speculatively allocating a `Vec` for
    // the single-object path that the K8s CR-mirror reconciler will
    // emit by default.
    match serde_json::from_slice::<Vec<A2aAgentSpec>>(bytes) {
        Ok(v) => Ok(v),
        Err(arr_err) => match serde_json::from_slice::<A2aAgentSpec>(bytes) {
            Ok(s) => Ok(vec![s]),
            // Surface the *array* error if neither shape parses — it
            // carries the most informative diagnostic for the
            // common case (controller mirrors an array).
            Err(_) => Err(arr_err),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    fn vk_b64u(seed: u8) -> String {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sk.verifying_key().as_bytes())
    }

    fn write(dir: &Path, body: &serde_json::Value) -> PathBuf {
        let path = dir.join("trust.json");
        std::fs::write(&path, serde_json::to_vec(body).unwrap()).unwrap();
        path
    }

    #[test]
    fn loads_single_spec() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            &json!({
                "namespace": "azureclaw-system",
                "name": "mandate-issuer-bootstrap",
                "signingKeys": [
                    {"kid": "issuer-1", "alg": "EdDSA", "publicKeyB64u": vk_b64u(1)}
                ]
            }),
        );
        let snap = load_mandate_trust_snapshot(&path).expect("load");
        assert_eq!(snap.generation(), 1);
        let view = snap.as_verifier_keys(0);
        assert!(view.contains_key("issuer-1"));
    }

    #[test]
    fn loads_array_of_specs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            &json!([
                {
                    "namespace": "team-a",
                    "name": "issuer-a",
                    "signingKeys": [
                        {"kid": "team-a-1", "alg": "EdDSA", "publicKeyB64u": vk_b64u(1)}
                    ]
                },
                {
                    "namespace": "team-b",
                    "name": "issuer-b",
                    "signingKeys": [
                        {"kid": "team-b-1", "alg": "EdDSA", "publicKeyB64u": vk_b64u(2)}
                    ]
                }
            ]),
        );
        let snap = load_mandate_trust_snapshot(&path).expect("load");
        let view = snap.as_verifier_keys(0);
        assert!(view.contains_key("team-a-1"));
        assert!(view.contains_key("team-b-1"));
    }

    #[test]
    fn rejects_duplicate_kid_across_specs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            &json!([
                {
                    "namespace": "team-a",
                    "name": "issuer-a",
                    "signingKeys": [
                        {"kid": "shared", "alg": "EdDSA", "publicKeyB64u": vk_b64u(1)}
                    ]
                },
                {
                    "namespace": "team-b",
                    "name": "issuer-b",
                    "signingKeys": [
                        {"kid": "shared", "alg": "EdDSA", "publicKeyB64u": vk_b64u(2)}
                    ]
                }
            ]),
        );
        let err = load_mandate_trust_snapshot(&path).expect_err("must fail");
        assert!(
            matches!(err, MandateTrustLoadError::Build { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_unsupported_alg() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            &json!({
                "namespace": "ns",
                "name": "n",
                "signingKeys": [
                    {"kid": "k", "alg": "RS256", "publicKeyB64u": vk_b64u(1)}
                ]
            }),
        );
        let err = load_mandate_trust_snapshot(&path).expect_err("must fail");
        assert!(
            matches!(err, MandateTrustLoadError::Projection { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn missing_file_is_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load_mandate_trust_snapshot(&tmp.path().join("does-not-exist.json"))
            .expect_err("must fail");
        assert!(
            matches!(err, MandateTrustLoadError::Io { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn malformed_json_is_json_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trust.json");
        std::fs::write(&path, b"not json").unwrap();
        let err = load_mandate_trust_snapshot(&path).expect_err("must fail");
        assert!(
            matches!(err, MandateTrustLoadError::Json { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn empty_array_yields_empty_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), &json!([]));
        let snap = load_mandate_trust_snapshot(&path).expect("empty");
        let view = snap.as_verifier_keys(0);
        assert!(view.is_empty());
    }
}
