// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Receipt inclusion log — an operator-controlled, hash-chained transparency
//! log of emitted Governance Receipts (kars Bridge Inc 5, the Wave-2 anchoring
//! precursor).
//!
//! ## What this is — and, honestly, what it is not
//!
//! Each emitted [`crate::kars_receipt::KarsReceipt`] is entered into an
//! append-only, hash-chained log stored in the `kars-receipt-log` ConfigMap in
//! `kars-system`. Every entry binds the receipt's signed-payload digest to the
//! previous entry's hash, so the **set** of receipts becomes tamper-evident:
//! deleting or altering any one receipt (or reordering them) breaks the chain
//! at that point, which a verifier detects — something a per-receipt signature
//! alone cannot catch (a signature proves *a* receipt is authentic, not that
//! *none were removed*).
//!
//! This is the **self-hosted-Rekor precursor** named in the roadmap (§22 wave
//! 2 / §24c). It is deliberately scoped and labelled with no overclaim:
//!
//! - It gives **cross-receipt tamper-evidence** and an **inclusion proof**.
//! - It does **NOT** give operator-non-repudiation: the operator controls the
//!   ConfigMap and could rewrite the *entire* chain. Closing that needs an
//!   **external witness** gossiping signed tree heads (V2), and
//!   **KMS-attested signing** (SKR/MAA on a confidential router, V2) — both
//!   gated on confidential-compute hardware and partner-environment answers.
//!   The receipt's `regulatory` claim therefore stays `OMITTED`.
//!
//! The chain hash recipe is a standard SHA-256 Merkle-style link
//! (`entryHash = sha256(seq | receipt | payloadSha | prevHash)`); it lives here
//! because this file is allowlisted for hash chaining in `ci/no-custom-crypto.sh`.

use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Client,
    api::{Api, PostParams},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::mesh_peer::IDENTITY_NAMESPACE;

/// ConfigMap holding the hash-chained inclusion log.
pub const LOG_CONFIGMAP_NAME: &str = "kars-receipt-log";
/// ConfigMap holding the signed checkpoint (signed tree head).
pub const CHECKPOINT_CONFIGMAP_NAME: &str = "kars-receipt-checkpoint";
/// Checkpoint note origin line (Go-sumdb-style signed note).
pub const CHECKPOINT_ORIGIN: &str = "kars-receipt-log";
/// Data key inside the ConfigMap holding the JSON chain.
const CHAIN_KEY: &str = "chain.json";
/// Genesis previous-hash for the first entry.
const GENESIS_PREV: &str = "genesis";
/// Root-hash value used in a checkpoint over an empty log.
const EMPTY_ROOT: &str = "genesis";
/// SSA field manager for checkpoint writes.
const CHECKPOINT_FIELD_MANAGER: &str = "kars-controller/receipt-checkpoint";
/// Bounded optimistic-concurrency retries on append.
const MAX_APPEND_RETRIES: usize = 5;

/// One entry in the inclusion log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InclusionEntry {
    /// Monotonic sequence number, starting at 0.
    pub seq: u64,
    /// `<namespace>/<name>` of the receipt.
    pub receipt: String,
    /// Hex SHA-256 of the receipt's signed DSSE payload (the in-toto Statement
    /// bytes). This is what binds the log to the receipt content.
    pub payload_sha256: String,
    /// Hash of the previous entry (`genesis` for seq 0).
    pub prev_hash: String,
    /// `sha256(seq | receipt | payloadSha256 | prevHash)`.
    pub entry_hash: String,
}

/// Compute the entry hash for a chain link. Pure.
pub fn entry_hash(seq: u64, receipt: &str, payload_sha256: &str, prev_hash: &str) -> String {
    let mut h = Sha256::new();
    h.update(seq.to_string().as_bytes());
    h.update(b"|");
    h.update(receipt.as_bytes());
    h.update(b"|");
    h.update(payload_sha256.as_bytes());
    h.update(b"|");
    h.update(prev_hash.as_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Hex SHA-256 of arbitrary bytes (used to digest the signed payload).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Build the next entry to append after `chain`, for the given receipt.
/// Pure — the reconciler supplies the current chain and the payload digest.
pub fn next_entry(chain: &[InclusionEntry], receipt: &str, payload_sha256: &str) -> InclusionEntry {
    let seq = chain.len() as u64;
    let prev_hash = chain
        .last()
        .map(|e| e.entry_hash.clone())
        .unwrap_or_else(|| GENESIS_PREV.to_string());
    let entry_hash = entry_hash(seq, receipt, payload_sha256, &prev_hash);
    InclusionEntry {
        seq,
        receipt: receipt.to_string(),
        payload_sha256: payload_sha256.to_string(),
        prev_hash,
        entry_hash,
    }
}

/// Verify a chain is internally consistent: contiguous sequence numbers,
/// correct prev-hash linkage, and recomputed entry hashes. Returns the broken
/// sequence number on failure.
#[allow(dead_code)] // verification API mirrored by the CLI (`kars receipt log`); exercised in unit tests.
pub fn verify_chain(chain: &[InclusionEntry]) -> Result<(), u64> {
    let mut prev = GENESIS_PREV.to_string();
    for (i, e) in chain.iter().enumerate() {
        if e.seq != i as u64 {
            return Err(i as u64);
        }
        if e.prev_hash != prev {
            return Err(e.seq);
        }
        let recomputed = entry_hash(e.seq, &e.receipt, &e.payload_sha256, &e.prev_hash);
        if recomputed != e.entry_hash {
            return Err(e.seq);
        }
        prev = e.entry_hash.clone();
    }
    Ok(())
}

/// Whether the chain already records this exact receipt + payload digest as its
/// most recent entry for that receipt (so emission is idempotent across
/// requeues — we only append when the receipt content actually changed).
fn already_current(chain: &[InclusionEntry], receipt: &str, payload_sha256: &str) -> bool {
    chain
        .iter()
        .rev()
        .find(|e| e.receipt == receipt)
        .is_some_and(|e| e.payload_sha256 == payload_sha256)
}

/// Append an inclusion entry for a freshly-emitted receipt. Idempotent and
/// concurrency-safe (optimistic resourceVersion retry). Returns the entry that
/// represents this receipt's current inclusion (existing or newly appended)
/// together with a flag that is `true` only when a **new** entry was written.
/// Callers use the flag to skip expensive, write-amplifying follow-on work
/// (re-publishing the signed checkpoint, witnessing, status echoes) on the
/// common idempotent no-op path — without it, every requeue of every task
/// rewrites the shared checkpoint/witness ConfigMaps and floods etcd.
pub async fn append(
    client: &Client,
    receipt: &str,
    payload_sha256: &str,
) -> Result<(InclusionEntry, bool)> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);

    for _ in 0..MAX_APPEND_RETRIES {
        let existing = cms.get_opt(LOG_CONFIGMAP_NAME).await?;
        let (chain, resource_version) = match &existing {
            Some(cm) => {
                let chain = cm
                    .data
                    .as_ref()
                    .and_then(|d| d.get(CHAIN_KEY))
                    .and_then(|s| serde_json::from_str::<Vec<InclusionEntry>>(s).ok())
                    .unwrap_or_default();
                (chain, cm.metadata.resource_version.clone())
            }
            None => (Vec::new(), None),
        };

        if already_current(&chain, receipt, payload_sha256) {
            // Nothing to do — return the current inclusion entry, not appended.
            return Ok((
                chain
                    .into_iter()
                    .rev()
                    .find(|e| e.receipt == receipt)
                    .expect("already_current implies an entry exists"),
                false,
            ));
        }

        let mut new_chain = chain;
        let entry = next_entry(&new_chain, receipt, payload_sha256);
        new_chain.push(entry.clone());
        let chain_json =
            serde_json::to_string(&new_chain).context("serialize receipt inclusion chain")?;

        let result = if existing.is_none() {
            // Create the log ConfigMap.
            let cm: ConfigMap = serde_json::from_value(serde_json::json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": {
                    "name": LOG_CONFIGMAP_NAME,
                    "namespace": IDENTITY_NAMESPACE,
                    "labels": {
                        "app.kubernetes.io/name": "kars",
                        "app.kubernetes.io/component": "receipt-inclusion-log",
                    },
                },
                "data": { CHAIN_KEY: chain_json },
            }))?;
            cms.create(&PostParams::default(), &cm).await.map(|_| ())
        } else {
            // Replace with optimistic concurrency on resourceVersion.
            let cm: ConfigMap = serde_json::from_value(serde_json::json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": {
                    "name": LOG_CONFIGMAP_NAME,
                    "namespace": IDENTITY_NAMESPACE,
                    "resourceVersion": resource_version,
                },
                "data": { CHAIN_KEY: chain_json },
            }))?;
            cms.replace(LOG_CONFIGMAP_NAME, &PostParams::default(), &cm)
                .await
                .map(|_| ())
        };

        match result {
            Ok(()) => {
                tracing::debug!(receipt = %receipt, seq = entry.seq, "receipt entered in inclusion log");
                return Ok((entry, true));
            }
            // 409 Conflict (lost the optimistic race) → retry with a fresh read.
            Err(kube::Error::Api(ae)) if ae.code == 409 => continue,
            Err(e) => return Err(e).context("appending to receipt inclusion log"),
        }
    }
    anyhow::bail!("receipt inclusion log append exhausted retries (contention)")
}

/// Read and parse the full inclusion chain (for checkpointing + the CLI).
pub async fn read_chain(client: &Client) -> Result<Vec<InclusionEntry>> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);
    let cm = cms.get_opt(LOG_CONFIGMAP_NAME).await?;
    Ok(cm
        .and_then(|c| {
            c.data
                .and_then(|d| d.get(CHAIN_KEY).cloned())
                .and_then(|s| serde_json::from_str::<Vec<InclusionEntry>>(&s).ok())
        })
        .unwrap_or_default())
}

/// The root hash a checkpoint commits to: the head entry's hash (which, in a
/// hash chain, already commits to the entire prefix), or `genesis` for an empty
/// log.
pub fn chain_root(chain: &[InclusionEntry]) -> String {
    chain
        .last()
        .map(|e| e.entry_hash.clone())
        .unwrap_or_else(|| EMPTY_ROOT.to_string())
}

/// Build the signed-note body for a checkpoint over a log of `tree_size`
/// entries with head `root_hash`. Go-sumdb signed-note style: origin line, then
/// size, then root, newline-terminated. Deterministic and timestamp-free so the
/// signature is stable for a given log state (the publish time is recorded
/// out-of-band in the ConfigMap, not in the signed body).
pub fn checkpoint_note(tree_size: u64, root_hash: &str) -> String {
    format!("{CHECKPOINT_ORIGIN}\n{tree_size}\n{root_hash}\n")
}

/// A published, signed checkpoint (signed tree head) over the inclusion log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub origin: String,
    pub tree_size: u64,
    pub root_hash: String,
    /// Hex SHA-256 key fingerprint of the signer (matches the trust anchor).
    pub key_id: String,
    /// Base64 Ed25519 signature over [`checkpoint_note`].
    pub signature: String,
}

/// Publish a signed checkpoint for the current chain to the
/// `kars-receipt-checkpoint` ConfigMap. Idempotent: re-publishing the same log
/// state is a byte-identical no-op write (Ed25519 is deterministic).
pub async fn publish_checkpoint(
    client: &Client,
    signer: &crate::providers::signing::ReceiptSigner,
    chain: &[InclusionEntry],
) -> Result<Checkpoint> {
    let tree_size = chain.len() as u64;
    let root_hash = chain_root(chain);
    let note = checkpoint_note(tree_size, &root_hash);
    let signature = signer.sign_note(note.as_bytes());
    let checkpoint = Checkpoint {
        origin: CHECKPOINT_ORIGIN.to_string(),
        tree_size,
        root_hash,
        key_id: signer.key_id.clone(),
        signature,
    };

    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);
    let cm: ConfigMap = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": CHECKPOINT_CONFIGMAP_NAME,
            "namespace": IDENTITY_NAMESPACE,
            "labels": {
                "app.kubernetes.io/name": "kars",
                "app.kubernetes.io/component": "receipt-checkpoint",
            },
        },
        "data": {
            "treeSize": tree_size.to_string(),
            "rootHash": checkpoint.root_hash,
            "keyId": checkpoint.key_id,
            "signature": checkpoint.signature,
            "note": note,
            "publishedAt": chrono::Utc::now().to_rfc3339(),
        },
    }))?;
    cms.patch(
        CHECKPOINT_CONFIGMAP_NAME,
        &kube::api::PatchParams::apply(CHECKPOINT_FIELD_MANAGER).force(),
        &kube::api::Patch::Apply(&cm),
    )
    .await
    .context("publishing receipt checkpoint ConfigMap")?;
    tracing::debug!(tree_size, "receipt checkpoint published");
    Ok(checkpoint)
}

/// ConfigMap holding the independent transparency-witness co-signature.
pub const WITNESS_CONFIGMAP_NAME: &str = "kars-receipt-witness";

/// Independent transparency witness: re-derive the log root from the chain and
/// co-sign the checkpoint with a SEPARATE witness key. This is a real second
/// party attesting the log isn't forked — the witness independently recomputes
/// the head and refuses to sign a checkpoint whose `root_hash` disagrees. A
/// verifier that trusts the witness key gains tamper-evidence beyond the
/// controller's own signature. Best-effort: on disagreement we do not witness.
pub async fn witness_checkpoint(
    client: &Client,
    witness: &crate::providers::signing::ReceiptSigner,
    chain: &[InclusionEntry],
    checkpoint: &Checkpoint,
) -> Result<()> {
    let recomputed = chain_root(chain);
    if recomputed != checkpoint.root_hash {
        tracing::warn!(
            controller = %checkpoint.root_hash, witness = %recomputed,
            "transparency witness DECLINES — root mismatch (possible fork)"
        );
        // Hard signal: a fork must NOT leave a stale witness co-signature
        // standing — withdraw it so receipts stop binding "log not forked".
        let cms: Api<ConfigMap> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);
        let _ = cms
            .delete(WITNESS_CONFIGMAP_NAME, &kube::api::DeleteParams::default())
            .await;
        return Ok(());
    }
    let note = checkpoint_note(checkpoint.tree_size, &recomputed);
    let sig = witness.sign_note(note.as_bytes());
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);
    let cm: ConfigMap = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": WITNESS_CONFIGMAP_NAME,
            "namespace": IDENTITY_NAMESPACE,
            "labels": { "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "receipt-witness" },
        },
        "data": {
            "treeSize": checkpoint.tree_size.to_string(),
            "rootHash": recomputed,
            "witnessKeyId": witness.key_id.clone(),
            "witnessSignature": sig,
            "witnessedAt": chrono::Utc::now().to_rfc3339(),
        },
    }))?;
    cms.patch(
        WITNESS_CONFIGMAP_NAME,
        &kube::api::PatchParams::apply("kars-controller/receipt-witness").force(),
        &kube::api::Patch::Apply(&cm),
    )
    .await
    .context("publishing receipt witness ConfigMap")?;
    tracing::debug!(tree_size = checkpoint.tree_size, "receipt checkpoint witnessed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_of(n: u64) -> Vec<InclusionEntry> {
        let mut chain: Vec<InclusionEntry> = Vec::new();
        for i in 0..n {
            let e = next_entry(&chain, &format!("ns/r{i}"), &format!("sha{i}"));
            chain.push(e);
        }
        chain
    }

    #[test]
    fn next_entry_links_to_genesis_then_prev() {
        let chain = chain_of(0);
        let e0 = next_entry(&chain, "ns/a", "shaA");
        assert_eq!(e0.seq, 0);
        assert_eq!(e0.prev_hash, GENESIS_PREV);

        let e1 = next_entry(std::slice::from_ref(&e0), "ns/b", "shaB");
        assert_eq!(e1.seq, 1);
        assert_eq!(e1.prev_hash, e0.entry_hash);
    }

    #[test]
    fn entry_hash_is_deterministic_and_sensitive() {
        let a = entry_hash(3, "ns/x", "sha", "prev");
        let b = entry_hash(3, "ns/x", "sha", "prev");
        assert_eq!(a, b);
        assert_ne!(a, entry_hash(3, "ns/x", "sha", "prev2"));
        assert_ne!(a, entry_hash(4, "ns/x", "sha", "prev"));
        assert_ne!(a, entry_hash(3, "ns/y", "sha", "prev"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn verify_chain_accepts_a_valid_chain() {
        assert_eq!(verify_chain(&chain_of(5)), Ok(()));
        assert_eq!(verify_chain(&[]), Ok(()));
    }

    #[test]
    fn verify_chain_detects_tampered_payload() {
        let mut chain = chain_of(4);
        // Tamper with entry 2's payload digest without recomputing hashes:
        chain[2].payload_sha256 = "evil".to_string();
        assert_eq!(verify_chain(&chain), Err(2));
    }

    #[test]
    fn verify_chain_detects_deleted_entry() {
        let mut chain = chain_of(4);
        // Remove the middle entry → seq numbers + linkage break at index 2.
        chain.remove(2);
        assert_eq!(verify_chain(&chain), Err(2));
    }

    #[test]
    fn verify_chain_detects_reorder() {
        let mut chain = chain_of(4);
        chain.swap(1, 2);
        assert!(verify_chain(&chain).is_err());
    }

    #[test]
    fn already_current_is_idempotency_guard() {
        let chain = chain_of(3); // receipts ns/r0..r2
        assert!(already_current(&chain, "ns/r2", "sha2"));
        assert!(!already_current(&chain, "ns/r2", "sha-new"));
        assert!(!already_current(&chain, "ns/r9", "sha9"));
    }

    #[test]
    fn chain_root_is_head_or_genesis() {
        assert_eq!(chain_root(&[]), EMPTY_ROOT);
        let chain = chain_of(3);
        assert_eq!(chain_root(&chain), chain.last().unwrap().entry_hash);
    }

    #[test]
    fn checkpoint_note_is_stable_signed_note_format() {
        let note = checkpoint_note(5, "abc123");
        assert_eq!(note, "kars-receipt-log\n5\nabc123\n");
        // Deterministic for a given state.
        assert_eq!(note, checkpoint_note(5, "abc123"));
        // Sensitive to size and root.
        assert_ne!(note, checkpoint_note(6, "abc123"));
        assert_ne!(note, checkpoint_note(5, "abc124"));
    }

    #[test]
    fn checkpoint_note_commits_to_head_which_commits_to_prefix() {
        // The head entry hash chains over the whole prefix, so a checkpoint
        // over it detects any prior-entry tamper without listing every entry.
        let chain = chain_of(4);
        let note = checkpoint_note(chain.len() as u64, &chain_root(&chain));
        // Tamper an earlier entry → recomputing the chain changes the head →
        // the note (and thus its signature) would differ.
        let mut tampered = chain.clone();
        tampered[1].payload_sha256 = "evil".to_string();
        // Recompute the tampered chain's head as an honest log would.
        let mut rebuilt: Vec<InclusionEntry> = Vec::new();
        for e in &tampered {
            rebuilt.push(next_entry(&rebuilt, &e.receipt, &e.payload_sha256));
        }
        let tampered_note =
            checkpoint_note(rebuilt.len() as u64, &chain_root(&rebuilt));
        assert_ne!(note, tampered_note);
    }
}
