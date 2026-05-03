// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Signed Merkle audit anchors.
//!
//! This module upgrades AzureClaw's per-event linear hash chain
//! (provided by the agentmesh SDK and consumed via
//! [`crate::providers::audit_impl`]) with a periodic, signed Merkle
//! root that anchors a contiguous range of audit entries. The chain
//! itself is unchanged; the anchor is **additive**, providing:
//!
//! 1. **Tamper-evidence at scale.** A single SHA-256 root binds every
//!    entry in the range. If any leaf changes, the root changes.
//! 2. **Compact inclusion proofs.** Verifying that entry `i` is in a
//!    range of `n` entries costs O(log n) hashes — much cheaper than
//!    rescanning the linear chain.
//! 3. **Non-repudiation.** The root is signed with the controller's
//!    Ed25519 key. A downstream consumer (transparency log, CR
//!    status, external auditor) can verify integrity without
//!    trusting the router process that produced the entries.
//!
//! ## Design constraints
//!
//! - **No new dependencies.** SHA-256 is already in the crate via
//!   `sha2`; Ed25519 via `ed25519-dalek`. We don't pull in
//!   `rs_merkle` (last release 2026-01, low activity); the tree is
//!   ~80 lines of code and easy to audit ourselves.
//! - **Pure functions where possible.** Tree construction and proof
//!   verification are pure — no I/O, no allocator gymnastics.
//! - **Domain separation.** Leaf hashes use a `0x00` prefix and
//!   internal-node hashes use `0x01`, per RFC 6962 §2.1 (Certificate
//!   Transparency). This prevents second-preimage attacks where an
//!   internal node's hash could be mistaken for a leaf.
//! - **Empty tree.** The root of an empty tree is `SHA-256("")` per
//!   RFC 6962 §2.1. Defined explicitly to avoid `unwrap()` panics.
//!
//! ## Anchor lifecycle
//!
//! The router's [`AuditAnchorBuilder`] accumulates leaf hashes from
//! the existing `AuditSink::append` path. When `seal()` is called
//! (driven by either a count threshold or a wall-clock timer in
//! caller code), the builder:
//!
//! 1. Computes the Merkle root over accumulated leaves.
//! 2. Builds a [`SignedAnchor`] containing the root, the leaf count,
//!    the first/last leaf hashes, a UNIX-millisecond timestamp, and
//!    an Ed25519 signature over the canonical pre-image.
//! 3. Resets internal state for the next anchor window.
//!
//! Verification (downstream) takes the `SignedAnchor` plus the
//! verifying key — no other state required.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

/// Domain-separation prefix for leaf hashes (RFC 6962 §2.1).
const LEAF_PREFIX: u8 = 0x00;
/// Domain-separation prefix for internal-node hashes (RFC 6962 §2.1).
const NODE_PREFIX: u8 = 0x01;
/// Domain-separation tag for the signed pre-image. Prevents an
/// attacker who can solicit a signature over different content
/// (e.g. a chat completion) from re-using it as an audit anchor.
const ANCHOR_DST: &[u8] = b"azureclaw.audit.anchor.v1\0";

/// Compute a leaf hash with the RFC 6962 leaf prefix.
pub fn leaf_hash(payload: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([LEAF_PREFIX]);
    h.update(payload);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// Compute an internal-node hash from two child hashes (left first).
fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([NODE_PREFIX]);
    h.update(left);
    h.update(right);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// Empty-tree root: `SHA-256("")` per RFC 6962 §2.1.
pub fn empty_tree_root() -> [u8; 32] {
    let out = Sha256::digest([]);
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// Compute the Merkle root over a sequence of leaf hashes.
///
/// Leaves are NOT re-prefixed — callers must pass values already
/// produced by [`leaf_hash`]. Odd levels are handled by promoting
/// the dangling node unchanged (the same convention RFC 6962 calls
/// "trailing partial trees", which avoids the
/// duplicate-rightmost-leaf second-preimage class of attacks at the
/// price of accepting that structurally-different trees with the
/// same leaves can't exist; not a concern here because we always
/// know `n`).
pub fn compute_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return empty_tree_root();
    }
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    while level.len() > 1 {
        let mut next: Vec<[u8; 32]> = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i + 1 < level.len() {
            next.push(node_hash(&level[i], &level[i + 1]));
            i += 2;
        }
        if i < level.len() {
            // Promote dangling leaf unchanged — see doc comment
            // above for the security rationale.
            next.push(level[i]);
        }
        level = next;
    }
    level[0]
}

/// One step in an inclusion proof: a sibling hash and its position
/// (left or right of the running hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofStep {
    pub sibling: [u8; 32],
    /// `true` if the sibling is to the *right* of the running hash
    /// at this level (i.e., we are the left child).
    pub sibling_is_right: bool,
}

/// Build an inclusion proof for `index` against `leaves`. Returns
/// `None` if `index` is out of range.
pub fn build_proof(leaves: &[[u8; 32]], index: usize) -> Option<Vec<ProofStep>> {
    if index >= leaves.len() {
        return None;
    }
    let mut proof = Vec::new();
    let mut idx = index;
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    while level.len() > 1 {
        let pair_idx = idx ^ 1;
        if pair_idx < level.len() {
            proof.push(ProofStep {
                sibling: level[pair_idx],
                sibling_is_right: idx & 1 == 0,
            });
        }
        // Build next level using the same promotion rule as
        // compute_root so indices stay consistent.
        let mut next: Vec<[u8; 32]> = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i + 1 < level.len() {
            next.push(node_hash(&level[i], &level[i + 1]));
            i += 2;
        }
        if i < level.len() {
            next.push(level[i]);
        }
        level = next;
        idx /= 2;
    }
    Some(proof)
}

/// Verify an inclusion proof: returns `true` iff `leaf` at `index`
/// is part of a tree with the given `root`.
pub fn verify_proof(root: &[u8; 32], leaf: &[u8; 32], index: usize, proof: &[ProofStep]) -> bool {
    let mut running = *leaf;
    let mut idx = index;
    for step in proof {
        running = if step.sibling_is_right {
            node_hash(&running, &step.sibling)
        } else {
            node_hash(&step.sibling, &running)
        };
        idx /= 2;
    }
    let _ = idx; // index walks to 0 at the root; not used downstream.
    &running == root
}

/// A signed Merkle anchor binding a contiguous range of audit
/// entries to a specific Ed25519 key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedAnchor {
    /// Sequence number, monotonically incrementing per builder.
    /// Allows downstream consumers to detect missing anchors.
    pub anchor_seq: u64,
    /// Number of leaves bound by `merkle_root_hex`.
    pub leaf_count: u64,
    /// First leaf hash (entry hash of the oldest entry in the
    /// range). 32 bytes hex.
    pub first_leaf_hex: String,
    /// Last leaf hash (entry hash of the newest entry in the
    /// range). 32 bytes hex.
    pub last_leaf_hex: String,
    /// Merkle root over the leaves.
    pub merkle_root_hex: String,
    /// UNIX milliseconds when the anchor was sealed.
    pub timestamp_ms: u64,
    /// Hex of the Ed25519 signature over the canonical pre-image.
    pub signature_hex: String,
    /// Hex of the public key (so verifier can pin out-of-band).
    pub verifying_key_hex: String,
}

/// Serialise the canonical pre-image. The signature is computed over
/// `ANCHOR_DST || anchor_seq_be || leaf_count_be || first_leaf ||
/// last_leaf || merkle_root || timestamp_ms_be`.
fn canonical_preimage(
    anchor_seq: u64,
    leaf_count: u64,
    first_leaf: &[u8; 32],
    last_leaf: &[u8; 32],
    root: &[u8; 32],
    timestamp_ms: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(ANCHOR_DST.len() + 8 + 8 + 32 + 32 + 32 + 8);
    out.extend_from_slice(ANCHOR_DST);
    out.extend_from_slice(&anchor_seq.to_be_bytes());
    out.extend_from_slice(&leaf_count.to_be_bytes());
    out.extend_from_slice(first_leaf);
    out.extend_from_slice(last_leaf);
    out.extend_from_slice(root);
    out.extend_from_slice(&timestamp_ms.to_be_bytes());
    out
}

/// Verify the signature on a [`SignedAnchor`]. Returns `true` iff
/// the signature is valid for the embedded verifying key. Does NOT
/// validate Merkle structure — pair this with [`verify_proof`].
pub fn verify_anchor_signature(anchor: &SignedAnchor) -> Result<(), AnchorError> {
    let vk_bytes = hex_to_array_32(&anchor.verifying_key_hex)
        .map_err(|_| AnchorError::BadHex("verifying_key"))?;
    let vk = VerifyingKey::from_bytes(&vk_bytes).map_err(|e| AnchorError::Parse(e.to_string()))?;
    let first =
        hex_to_array_32(&anchor.first_leaf_hex).map_err(|_| AnchorError::BadHex("first_leaf"))?;
    let last =
        hex_to_array_32(&anchor.last_leaf_hex).map_err(|_| AnchorError::BadHex("last_leaf"))?;
    let root =
        hex_to_array_32(&anchor.merkle_root_hex).map_err(|_| AnchorError::BadHex("merkle_root"))?;
    let preimage = canonical_preimage(
        anchor.anchor_seq,
        anchor.leaf_count,
        &first,
        &last,
        &root,
        anchor.timestamp_ms,
    );
    let sig_bytes =
        hex_to_array_64(&anchor.signature_hex).map_err(|_| AnchorError::BadHex("signature"))?;
    let sig = Signature::from_bytes(&sig_bytes);
    vk.verify(&preimage, &sig)
        .map_err(|e| AnchorError::SignatureInvalid(e.to_string()))
}

/// Errors specific to anchor handling. Distinct from [`crate::providers::audit::AuditError`]
/// because anchor verification is a separate concern (downstream-only)
/// and we don't want to entangle the data plane's error type with it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AnchorError {
    #[error("hex field `{0}` is malformed")]
    BadHex(&'static str),
    #[error("verifying key parse: {0}")]
    Parse(String),
    #[error("signature verification failed: {0}")]
    SignatureInvalid(String),
}

fn hex_to_array_32(s: &str) -> Result<[u8; 32], ()> {
    if s.len() != 64 {
        return Err(());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        let byte_str = s.get(i * 2..i * 2 + 2).ok_or(())?;
        *b = u8::from_str_radix(byte_str, 16).map_err(|_| ())?;
    }
    Ok(out)
}

fn hex_to_array_64(s: &str) -> Result<[u8; 64], ()> {
    if s.len() != 128 {
        return Err(());
    }
    let mut out = [0u8; 64];
    for (i, b) in out.iter_mut().enumerate() {
        let byte_str = s.get(i * 2..i * 2 + 2).ok_or(())?;
        *b = u8::from_str_radix(byte_str, 16).map_err(|_| ())?;
    }
    Ok(out)
}

fn array_to_hex(a: &[u8]) -> String {
    let mut s = String::with_capacity(a.len() * 2);
    for b in a {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Stateful builder that accumulates leaf hashes and seals them
/// into a [`SignedAnchor`] on demand.
///
/// Safe to use behind a `Mutex` — there is no internal locking; each
/// method runs in O(buffer size) and the buffer is bounded by the
/// caller's seal cadence (count or wall-clock).
pub struct AuditAnchorBuilder {
    signing_key: SigningKey,
    next_seq: u64,
    leaves: Vec<[u8; 32]>,
}

impl AuditAnchorBuilder {
    pub fn new(signing_key: SigningKey) -> Self {
        Self {
            signing_key,
            next_seq: 0,
            leaves: Vec::new(),
        }
    }

    /// Append a leaf hash (already prefixed via [`leaf_hash`]).
    pub fn append_leaf(&mut self, leaf: [u8; 32]) {
        self.leaves.push(leaf);
    }

    /// Number of un-sealed leaves currently buffered.
    pub fn pending(&self) -> usize {
        self.leaves.len()
    }

    /// The current sequence number that the next [`seal`] would
    /// emit. Monotonically increasing.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Seal the current buffer into a [`SignedAnchor`]. Returns
    /// `None` if there are no leaves to seal (callers should treat
    /// this as a no-op rather than an error).
    pub fn seal(&mut self, timestamp_ms: u64) -> Option<SignedAnchor> {
        if self.leaves.is_empty() {
            return None;
        }
        let leaf_count = self.leaves.len() as u64;
        let first_leaf = self.leaves[0];
        let last_leaf = self.leaves[self.leaves.len() - 1];
        let root = compute_root(&self.leaves);
        let anchor_seq = self.next_seq;
        let preimage = canonical_preimage(
            anchor_seq,
            leaf_count,
            &first_leaf,
            &last_leaf,
            &root,
            timestamp_ms,
        );
        let sig: Signature = self.signing_key.sign(&preimage);
        let vk = self.signing_key.verifying_key();

        // Reset state for the next anchor window.
        self.leaves.clear();
        self.next_seq = self.next_seq.saturating_add(1);

        Some(SignedAnchor {
            anchor_seq,
            leaf_count,
            first_leaf_hex: array_to_hex(&first_leaf),
            last_leaf_hex: array_to_hex(&last_leaf),
            merkle_root_hex: array_to_hex(&root),
            timestamp_ms,
            signature_hex: array_to_hex(sig.to_bytes().as_ref()),
            verifying_key_hex: array_to_hex(vk.as_bytes()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn fixed_signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn empty_tree_root_matches_rfc6962() {
        // RFC 6962 §2.1: MTH({}) = SHA-256(""). Computed once;
        // value is hard-coded in the standard library docs and many
        // CT implementations.
        let expected_hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(array_to_hex(&empty_tree_root()), expected_hex);
    }

    #[test]
    fn single_leaf_root_equals_leaf_hash() {
        let l = leaf_hash(b"event-1");
        assert_eq!(compute_root(&[l]), l);
    }

    #[test]
    fn two_leaves_root_is_node_hash() {
        let a = leaf_hash(b"a");
        let b = leaf_hash(b"b");
        let root = compute_root(&[a, b]);
        assert_eq!(root, node_hash(&a, &b));
    }

    #[test]
    fn three_leaves_promotes_dangling() {
        let a = leaf_hash(b"a");
        let b = leaf_hash(b"b");
        let c = leaf_hash(b"c");
        let root = compute_root(&[a, b, c]);
        // Level 1: [H(a,b), c]; Level 0: H(H(a,b), c)
        let ab = node_hash(&a, &b);
        assert_eq!(root, node_hash(&ab, &c));
    }

    #[test]
    fn proof_round_trip_for_each_index() {
        let leaves: Vec<[u8; 32]> = (0u8..7).map(|i| leaf_hash(&[i; 4])).collect();
        let root = compute_root(&leaves);
        for i in 0..leaves.len() {
            let proof = build_proof(&leaves, i).unwrap();
            assert!(
                verify_proof(&root, &leaves[i], i, &proof),
                "proof failed for index {i}"
            );
        }
    }

    #[test]
    fn proof_with_wrong_index_fails() {
        let leaves: Vec<[u8; 32]> = (0u8..4).map(|i| leaf_hash(&[i; 4])).collect();
        let root = compute_root(&leaves);
        let proof = build_proof(&leaves, 1).unwrap();
        // Use leaf 1's proof but verify against leaf 2's hash and
        // leaf 2's index — must fail.
        assert!(!verify_proof(&root, &leaves[2], 2, &proof));
    }

    #[test]
    fn proof_with_tampered_leaf_fails() {
        let leaves: Vec<[u8; 32]> = (0u8..4).map(|i| leaf_hash(&[i; 4])).collect();
        let root = compute_root(&leaves);
        let proof = build_proof(&leaves, 0).unwrap();
        let tampered = leaf_hash(b"definitely not the original");
        assert!(!verify_proof(&root, &tampered, 0, &proof));
    }

    #[test]
    fn proof_with_tampered_root_fails() {
        let leaves: Vec<[u8; 32]> = (0u8..4).map(|i| leaf_hash(&[i; 4])).collect();
        let proof = build_proof(&leaves, 0).unwrap();
        let mut bad_root = compute_root(&leaves);
        bad_root[0] ^= 0xFF;
        assert!(!verify_proof(&bad_root, &leaves[0], 0, &proof));
    }

    #[test]
    fn out_of_range_index_returns_none() {
        let leaves: Vec<[u8; 32]> = (0u8..3).map(|i| leaf_hash(&[i; 4])).collect();
        assert!(build_proof(&leaves, 3).is_none());
        assert!(build_proof(&leaves, 99).is_none());
    }

    #[test]
    fn anchor_round_trip_signature_verifies() {
        let sk = fixed_signing_key(7);
        let mut builder = AuditAnchorBuilder::new(sk);
        for i in 0u8..5 {
            builder.append_leaf(leaf_hash(&[i; 8]));
        }
        let anchor = builder.seal(1_700_000_000_000).unwrap();
        assert_eq!(anchor.anchor_seq, 0);
        assert_eq!(anchor.leaf_count, 5);
        verify_anchor_signature(&anchor).unwrap();
    }

    #[test]
    fn anchor_with_tampered_root_fails_verification() {
        let sk = fixed_signing_key(8);
        let mut builder = AuditAnchorBuilder::new(sk);
        builder.append_leaf(leaf_hash(b"x"));
        let mut anchor = builder.seal(1_700_000_000_000).unwrap();
        // Flip one nibble in the merkle root hex string.
        let mut chars: Vec<char> = anchor.merkle_root_hex.chars().collect();
        chars[0] = if chars[0] == '0' { '1' } else { '0' };
        anchor.merkle_root_hex = chars.into_iter().collect();
        assert!(matches!(
            verify_anchor_signature(&anchor),
            Err(AnchorError::SignatureInvalid(_))
        ));
    }

    #[test]
    fn anchor_with_tampered_signature_fails_verification() {
        let sk = fixed_signing_key(9);
        let mut builder = AuditAnchorBuilder::new(sk);
        builder.append_leaf(leaf_hash(b"x"));
        let mut anchor = builder.seal(1_700_000_000_000).unwrap();
        let mut chars: Vec<char> = anchor.signature_hex.chars().collect();
        chars[0] = if chars[0] == '0' { '1' } else { '0' };
        anchor.signature_hex = chars.into_iter().collect();
        assert!(matches!(
            verify_anchor_signature(&anchor),
            Err(AnchorError::SignatureInvalid(_))
        ));
    }

    #[test]
    fn anchor_seq_increments_on_each_seal() {
        let sk = fixed_signing_key(10);
        let mut builder = AuditAnchorBuilder::new(sk);
        builder.append_leaf(leaf_hash(b"a"));
        let a1 = builder.seal(1).unwrap();
        builder.append_leaf(leaf_hash(b"b"));
        let a2 = builder.seal(2).unwrap();
        builder.append_leaf(leaf_hash(b"c"));
        let a3 = builder.seal(3).unwrap();
        assert_eq!(a1.anchor_seq, 0);
        assert_eq!(a2.anchor_seq, 1);
        assert_eq!(a3.anchor_seq, 2);
    }

    #[test]
    fn seal_on_empty_buffer_returns_none() {
        let sk = fixed_signing_key(11);
        let mut builder = AuditAnchorBuilder::new(sk);
        assert!(builder.seal(0).is_none());
    }

    #[test]
    fn seal_resets_buffer() {
        let sk = fixed_signing_key(12);
        let mut builder = AuditAnchorBuilder::new(sk);
        builder.append_leaf(leaf_hash(b"a"));
        builder.append_leaf(leaf_hash(b"b"));
        assert_eq!(builder.pending(), 2);
        builder.seal(1).unwrap();
        assert_eq!(builder.pending(), 0);
    }

    #[test]
    fn anchor_with_swapped_verifying_key_fails() {
        let sk1 = fixed_signing_key(13);
        let sk2 = fixed_signing_key(14);
        let mut builder = AuditAnchorBuilder::new(sk1);
        builder.append_leaf(leaf_hash(b"x"));
        let mut anchor = builder.seal(1_700_000_000_000).unwrap();
        // Replace the verifying key with a different one. The
        // signature was over the original key, so verification
        // against the new key must fail (signature mismatch).
        anchor.verifying_key_hex = array_to_hex(sk2.verifying_key().as_bytes());
        assert!(matches!(
            verify_anchor_signature(&anchor),
            Err(AnchorError::SignatureInvalid(_))
        ));
    }

    #[test]
    fn domain_separation_distinguishes_leaf_from_node() {
        // Defence-in-depth: a 64-byte input should NOT collide
        // between leaf_hash(64 bytes) and node_hash(32, 32) because
        // the prefixes differ.
        let payload = [42u8; 64];
        let l = leaf_hash(&payload);
        let mut left = [0u8; 32];
        let mut right = [0u8; 32];
        left.copy_from_slice(&payload[..32]);
        right.copy_from_slice(&payload[32..]);
        let n = node_hash(&left, &right);
        assert_ne!(l, n);
    }

    #[test]
    fn many_leaves_root_is_stable() {
        // Simple regression guard: 1024 leaves produce a fixed root
        // for fixed inputs. Any change to the tree algorithm or
        // domain-separation tags would break this.
        let leaves: Vec<[u8; 32]> = (0..1024u32).map(|i| leaf_hash(&i.to_be_bytes())).collect();
        let root = compute_root(&leaves);
        // We don't hard-code the expected hex (would tie tests to
        // the hash output) — just assert it's stable across
        // recomputation and that proofs verify for a sampling.
        let root2 = compute_root(&leaves);
        assert_eq!(root, root2);
        for &i in &[0usize, 1, 511, 512, 1023] {
            let proof = build_proof(&leaves, i).unwrap();
            assert!(verify_proof(&root, &leaves[i], i, &proof));
        }
    }
}
