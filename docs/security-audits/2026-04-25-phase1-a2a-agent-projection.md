# 2026-04-25 — Phase 1: A2AAgent CRD spec → trust-anchor projection

## Summary

Adds `inference-router/src/a2a/agent_projection.rs` — a pure
synchronous projection function that turns an `A2AAgent` CRD spec
into a `Vec<TrustAnchor>` ready to feed
[`TrustStoreBuilder`](../../inference-router/src/a2a/trust_store.rs).

The `A2AAgent` CRD itself lands in Phase 2 (implementation-plan
§8 item 2). Shipping the projection now locks in the schema
contract and gives the eventual controller-side informer a
one-line integration:

```rust
let anchors = project_anchors(&cr.spec, "a2a-agent-cr")?;
for a in anchors { builder.add(a)?; }
let snap = builder.build();
trust_store.replace_snapshot(snap);
```

13 unit tests cover happy path + every reject variant + a
cross-CR kid-collision scenario that documents how cluster-wide
duplicates are caught at the builder level (not the projection
level).

## Threat model delta

The projection is the only place where bytes from an
operator-authored CR become `ed25519_dalek::VerifyingKey`s
installed in the runtime trust store. If the projection accepts
any malformed input — short keys, alg-confusion strings, empty
kids, duplicate kids — the trust store silently inherits the
ambiguity.

This module rejects every such case explicitly with a typed
`ProjectionError` variant. STRIDE coverage:

- **Spoofing** — `empty_kid_rejected`, `duplicate_kid_rejected`
  (per-CR), and `cross_cr_kid_collision_caught_by_builder_not_projection`
  (cross-CR via the builder) ensure no two anchors with the same
  kid coexist.
- **Tampering / EoP via algorithm confusion** —
  `unsupported_alg_rejected`, `algorithm_confusion_hs256_rejected`,
  `alg_none_rejected` enforce the EdDSA-only allow-list at admission.
- **Information disclosure / DoS** — `invalid_base64_rejected`,
  `wrong_key_length_rejected` ensure malformed bytes never land
  in `VerifyingKey::from_bytes` panickably (they short-circuit
  before with a typed error).

## OWASP mapping

- **OWASP A2A draft Top 10 A05: Trust Anchor Misconfiguration** —
  primary mitigation. Pairs with the existing
  `empty_trust_anchors` rejection in
  `tests/a2a_card_verifier_conformance.rs` (PR 28) which covers
  the runtime side; this PR covers the *config* side.
- **OWASP MCP Top 10 (in progress) — Cryptographic Misconfig** —
  alg pinning at projection time prevents CR-author error from
  becoming a runtime "accept HS256" footgun.

## AuthN / AuthZ path

Not applicable — the function is synchronous and pure. Its
runtime caller will be the controller-side informer, which itself
is reached only by reconcile-loop code triggered by K8s API
admission events. CRD-level RBAC governs who can write
`A2AAgent` objects (configured in the Helm chart per the §7
phase plan).

## Secret + key custody

Verifying keys are *public* by definition. No secret material
is read by this module. The spec carries the public key as
base64url; tests use deterministic seeds and never exercise
production keys.

## Egress surface delta

None.

## Audit events emitted

None directly. The eventual reconciler will emit a structured
event per snapshot rebuild containing the projection-error
(if any), the resulting anchor count, and the generation
counter — all Phase 2 work.

## Failure mode

Each projection error is fail-closed: if any single
`signingKey` is malformed, the entire spec rejects (no partial
projection). The reconciler's contract will be: "either install
a complete snapshot or stamp `Degraded=True` on the CR and keep
serving the previous snapshot." Tested negative paths:

- Empty kid → `EmptyKid`
- Duplicate kid within a CR → `DuplicateKid`
- Cross-CR duplicate kid (caught by builder) →
  `TrustStoreBuildError::DuplicateKid` (existing test in this PR
  documents the expected handoff)
- alg ≠ EdDSA → `UnsupportedAlg`
- Specifically `alg = "none"` → `UnsupportedAlg`
- Specifically `alg = "HS256"` (algorithm confusion) →
  `UnsupportedAlg`
- Bad base64url → `InvalidBase64`
- Wrong key length → `WrongKeyLength`

## Negative-test coverage

13 unit tests under `a2a::agent_projection::tests`:

- `happy_path_single_key`
- `happy_path_multiple_keys_preserves_order`
- `empty_kid_rejected`
- `duplicate_kid_rejected`
- `unsupported_alg_rejected`
- `algorithm_confusion_hs256_rejected`
- `alg_none_rejected`
- `invalid_base64_rejected`
- `wrong_key_length_rejected`
- `empty_signing_keys_yields_empty_anchor_list`
- `source_prefix_propagates_namespace_and_name`
- `anchor_round_trips_into_trust_store_builder`
- `cross_cr_kid_collision_caught_by_builder_not_projection`

## Vendored / third-party dependency delta

None. Reuses `ed25519_dalek::VerifyingKey`, `serde`, and the
existing `a2a::signature::base64url_decode` helper. No new
crates.

## Sources consulted

- `inference-router/src/a2a/trust_store.rs` (target shape:
  `TrustAnchor`, `TrustStoreBuilder::add` semantics).
- `inference-router/src/a2a/signature.rs` (`base64url_decode`
  + alg-pinning convention).
- A2A 1.0.0 spec §4.4.7 (signature key model) —
  <https://a2a-protocol.org/v1.0.0/specification#447-agentcardsignature>
- RFC 8725 §3.1 (alg pinning, no `none`) —
  <https://datatracker.ietf.org/doc/html/rfc8725>

## Verification

- `cargo test --package azureclaw-inference-router --lib a2a::agent_projection`
  → 13 passed.
- `cargo test --package azureclaw-inference-router --lib`
  → **517 passed** (was 504; +13 from this PR).
- `cargo clippy --package azureclaw-inference-router --all-targets -- -D warnings`
  clean.

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
