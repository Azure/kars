# Security Audit — `audit-merkle-signed`

**Date:** 2026-05-03
**PR:** #169 (target `dev`)
**Author:** @copilot-cli
**Independent reviewer:** TBD (router-data-plane)
**Capability scope:**
Adds a self-contained, signed-Merkle-anchor module
(`inference-router/src/audit/merkle.rs`) that builds RFC-6962-style
Merkle trees over arbitrary 32-byte leaves, produces inclusion proofs,
and seals the root inside an Ed25519-signed `SignedAnchor`. The module
is purely additive — it ships the data-plane primitives without
modifying the existing linear hash-chain audit sink in
`providers/audit_impl.rs`. A follow-up PR will wire the builder into
`Governance::append()` and decide the seal trigger semantics. No
external endpoints, CRDs, or kube objects are introduced in this PR.

---

## 1. Summary

The current audit sink (407 LOC, `providers/audit_impl.rs`) chains
entries via `previous_hash`. A linear chain detects a tampered prefix
only by replaying the entire log; it has no compact integrity witness
and the chain head is unsigned. This PR introduces the building block
for compact, signed integrity witnesses:

1. **`leaf_hash(bytes)`** — domain-separated SHA-256 (`0x00 || bytes`).
2. **`compute_root([leaf_hash, …])`** — RFC-6962 internal-node hash
   (`0x01 || left || right`) with **dangling-leaf promotion** (last odd
   node carried up unchanged; **no duplication**, which avoids the
   second-preimage attack pattern in the original Bitcoin layout).
3. **`build_proof` / `verify_proof`** — pure inclusion proofs.
4. **`AuditAnchorBuilder`** — accumulates leaves and `seal(now)`s a
   `SignedAnchor { seq, leaf_count, first_leaf, last_leaf, root,
   timestamp_ms, signature }` over a domain-separated pre-image
   (`b"azureclaw.audit.anchor.v1\0"`).
5. **`verify_anchor_signature`** — validator side; checks the
   Ed25519 signature over the same pre-image.

## 2. Threat model delta (STRIDE)

| Threat | Before | After |
|---|---|---|
| **Tampering** with audit history | Detected only by full replay; no compact witness. | A single `SignedAnchor` (96 B sig + small fixed metadata) commits to every entry in `[first_leaf, last_leaf]`. Any in-range tamper invalidates the root. |
| **Repudiation** of audit emission | Hash chain is unsigned; any party with write access could rewrite. | Anchor is Ed25519-signed; the controller's signing key is the only entity that can produce a valid anchor. |
| **Spoofing** an anchor (signature reuse from another context) | N/A | Domain-separation tag (`ANCHOR_DST = b"azureclaw.audit.anchor.v1\0"`) prepended to the signed pre-image; an Ed25519 signature created in any other context (chat completions, A2A cards, AGT KNOCKs, AP2 mandates) will not validate as an anchor. |
| **Information disclosure** | Audit content already non-secret. | Unchanged — only `leaf_hash` digests appear in proofs/anchors; raw entries are not exposed. |
| **Denial of service** | An attacker filling the chain can already exhaust storage. | A pathological caller seeding very large leaf vectors raises memory cost only on the seal path; `AuditAnchorBuilder` operates on `Vec<[u8; 32]>` (32 B per leaf). 1 M leaves = 32 MB; build is O(n). The wiring PR will choose a maximum window before forced seal. |
| **Elevation of privilege** | N/A — module performs no auth. | N/A. |

## 3. OWASP LLM-Top-10 mapping

- **LLM08 — Excessive Agency**: signed anchors give external auditors
  cryptographic evidence of what an agent did during a window,
  enabling after-the-fact audit-driven privilege scoping.
- **LLM06 — Sensitive Information Disclosure**: anchors expose only
  digests, not entry payloads.

Other items are not affected by this change.

## 4. Fail-closed semantics

- `seal()` on an empty buffer returns `Err(AnchorError::EmptyBuffer)`
  rather than emitting a signed empty-tree root that could be replayed.
- `seq` uses `saturating_add(1)` — at `u64::MAX` the builder refuses
  further seals (`Err(AnchorError::SeqOverflow)`); it does **not**
  silently wrap.
- `verify_anchor_signature` returns `Err(AnchorError::SignatureInvalid)`
  on any mismatch (tampered root, tampered timestamp, tampered seq,
  wrong key) — there is no soft-fail path.

## 5. Test coverage

`cargo test --package azureclaw-inference-router --lib audit::` —
**18 / 18 pass**. Coverage includes:

- empty-tree root equals `SHA-256("")`,
- single-leaf tree root equals the leaf itself,
- two- and three-leaf parent computations match hand-derived values,
- domain separation (leaf vs internal-node) actually differs,
- inclusion-proof round-trip for every index in trees of size 1..=8,
- tampered leaf, tampered root, tampered signature, swapped key all
  fail verification,
- `seq` strictly increments across consecutive `seal()`s,
- 1024-leaf root is stable across rebuilds,
- dangling-leaf promotion (no duplication) for odd counts.

## 6. Wiring deferred

This PR is intentionally **additive only**. The existing audit chain
in `providers/audit_impl.rs` is unchanged; no CRD or status field
emits anchors yet. The following are the open questions for the
follow-up wiring PR:

1. **Seal trigger** — count-based (every N entries), time-based
   (every T seconds), explicit admin call, or all three?
2. **Anchor publication surface** — `ClawSandbox.status.auditAnchor`
   field, dedicated `ClawAuditAnchor` CRD, or ConfigMap? (Per the
   "no public posting" constraint, no Sigstore Rekor / external
   transparency log will be used.)
3. **Signing-key provenance** — does the controller already have a
   suitable Ed25519 key, or will we provision a dedicated Secret
   (`azureclaw-audit-signing-key`) with rotation semantics?

Decisions will be captured in a follow-up audit doc.

## 7. Out-of-scope

- No changes to `providers/audit_impl.rs` linear hash-chain.
- No changes to `governance.rs`, no new fields on `Governance`.
- No new HTTP endpoints, CRDs, or admission policies.
- No external transparency-log emission; explicitly forbidden by
  user policy.
