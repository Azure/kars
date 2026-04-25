# Security audit — A2A trust-store hot-reload integration test

**Date:** 2026-04-25
**PR branch:** `phase1/trust-store-hot-reload-integration`
**Capability owner:** AzureClaw Phase 1 — A2A trust path

## 1. Summary

Adds `inference-router/tests/a2a_trust_store_hot_reload.rs`, a six-
scenario integration test that stitches together the four trust-path
modules currently sitting independently green:

1. `a2a::agent_projection::project_anchors` (PR 30)
2. `a2a::trust_store::TrustStoreBuilder::add` + `build` (PR 24)
3. `a2a::trust_store::TrustStoreSnapshot::as_verifier_keys` (PR 24)
4. `a2a::card_verifier::verify_inbound_card` (PR 22)

The test asserts the **hot-reload contract**: a card signed by a
previously-untrusted `kid` becomes verifiable within one
`TrustStore::replace_snapshot` call, with no router restart and no
re-establishment of in-flight HTTP connections. This is the
informer-driven update path that the Phase 2 A2AAgent CRD reconciler
will run; locking it down at the data-plane integration level today
prevents schema or type drift between the four modules.

No production code changes — this PR is **tests only**.

## 2. Threat model delta

### Asset gaining new exposure
None. Tests only.

### STRIDE diff against `docs/threat-model.md`
The integration test exercises (without changing) three already-modelled
threats:

- **Spoofing** (S3 — adversary signs a card with a kid not yet known to
  the verifier): `pre_reload_card_signed_by_unknown_kid_is_rejected`
  asserts the verifier rejects.
- **Tampering** (T2 — kid-old card replayed after admin
  revoked it): `snapshot_replace_can_revoke_previously_trusted_kid`
  asserts revocation is observable on the next snapshot.
- **Information disclosure** (I3 — expired anchors silently still
  trusted): `anchor_expiry_filtered_by_as_verifier_keys` asserts
  `as_verifier_keys` filters strictly with `now < not_after`.

## 3. OWASP mapping

- **OWASP MCP05 — Authentication and Authorization Bypass:** the test
  closes the "informer-applied-but-verifier-stale" race by asserting
  that the `Arc<TrustStoreSnapshot>` view consumed by the verifier
  reflects the most recent `replace_snapshot` call, not a stale arc.
- **OWASP LLM07 — Insecure Plugin Design:** revocation is observable
  end-to-end; the test asserts that removing a `kid` from the spec
  causes the corresponding card to fail verification on the very next
  snapshot — no caching layer can re-allow it.

## 4. AuthN / AuthZ path

Not applicable — pure in-process integration test, no network.

The test does, however, lock the contract that future production
wiring depends on:

- `informer → project_anchors → TrustStoreBuilder → replace_snapshot`
- `verifier handler → TrustStore::snapshot() → as_verifier_keys(now)
  → CardVerifierConfig::trusted_keys`

Any drift between these two pipelines (field rename, enum-variant rename,
clock-source mismatch, base64 dialect mismatch) breaks the test.

## 5. Secret + key custody

No keys are generated, persisted, or transmitted. The test uses
deterministic Ed25519 keys derived from single-byte seeds for
reproducibility.

## 6. Egress surface delta

None.

## 7. Audit events emitted

None — tests do not emit audit events.

## 8. Failure mode

Each scenario asserts fail-closed:

| Scenario | Asserted outcome |
|---|---|
| `pre_reload_card_signed_by_unknown_kid_is_rejected` | verifier returns `Err` |
| `hot_reload_makes_new_kid_verify_within_one_replace` | verifier returns `Ok` after replace; generation counter advances |
| `snapshot_replace_can_revoke_previously_trusted_kid` | first verify `Ok`; verify after revocation `Err` |
| `anchor_expiry_filtered_by_as_verifier_keys` | strict-`<` filter; expired anchor → verify `Err` |
| `anchor_with_future_expiry_still_verifies` | future expiry → verify `Ok` |
| `empty_snapshot_rejects_every_card` | empty store → verify `Err` |

## 9. Negative-test coverage

This **is** the negative-test coverage for the trust-path integration.
It complements:

- `a2a_card_verifier_conformance.rs` (PR 28, 15 tests) — tests the
  verifier under fixed `HashMap<&str, &VerifyingKey>` trust maps.
- `agent_projection.rs::tests` (PR 30, 13 tests) — tests projection
  in isolation.
- `trust_store.rs::tests` (PR 24, multiple tests) — tests builder /
  snapshot / replace in isolation.

This new corpus is the only place where all four modules are tested
together in a single test binary, and is the first test that exercises
`as_verifier_keys` against `verify_inbound_card`.

## 10. Vendored / third-party dependency delta

None. Reuses workspace dependencies (`ed25519-dalek`, `serde_json`,
`base64`).

Sources consulted:
- `inference-router/src/a2a/trust_store.rs:136-145` for the
  `as_verifier_keys` strict-`<` filter behaviour.
- PR 30 audit doc (`2026-04-25-phase1-a2a-agent-projection.md`) for
  the projection contract.
- PR 28 audit doc (`2026-04-25-phase1-a2a-card-verifier-conformance.md`)
  for the verifier-corpus pattern this test extends.

## 11. Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
