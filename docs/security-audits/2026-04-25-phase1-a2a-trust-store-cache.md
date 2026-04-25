# Security audit — Phase 1 A2A trust-store cache (snapshot hot-reload)

**Date:** 2026-04-25
**Branch:** `phase1/a2a-trust-store-cache`
**Capability:** Hot-reloadable `kid → VerifyingKey` trust-anchor cache
backing `verify_inbound_card`. In-tree, lock-free reads via
`Arc<Snapshot>` semantics over `RwLock<Arc<TrustStoreSnapshot>>`. No new
HTTP route or external dependency.

## 1. Summary

Adds `inference-router/src/a2a/trust_store.rs`:

- `TrustAnchor` — kid + Ed25519 `VerifyingKey` + optional `not_after`
  expiry + `AnchorSource` provenance string.
- `TrustStoreSnapshot` — immutable map; `lookup(kid, now)` applies expiry;
  `as_verifier_keys(now)` projects to the `HashMap<&str, &VerifyingKey>`
  borrow shape `card_verifier::CardVerifierConfig` consumes; carries a
  monotonic `generation` counter.
- `TrustStoreBuilder` — accumulator that rejects conflicting duplicate
  `kid` and is idempotent on identical re-add.
- `TrustStore` — `RwLock<Arc<TrustStoreSnapshot>>` with `snapshot()`
  (cheap Arc clone), `replace_snapshot(next)` (atomic install), and
  `compact(now)` (rebuild dropping already-expired anchors).
- `TrustStoreBuildError::DuplicateKid` for conflict telemetry.

This is the seam K8s informer adapters call to publish anchors from
`A2AAgent` CRs without disrupting the verification hot path.

## 2. Threat model delta

- **Spoofing:** trust anchors are the root of the A2A AgentCard
  signature-verification chain. A poisoned anchor would forge any
  caller. Two defences here: (a) `DuplicateKid` rejects two informer
  adapters silently shadowing each other; (b) `not_after` enforces
  per-anchor expiry so a rotated key falls out of the trust set even
  before the next informer event.
- **Tampering:** snapshot is replaced atomically via `Arc<Snapshot>`
  swap — readers never see a half-installed map. Builder produces a
  fully-populated snapshot before publication.
- **Repudiation:** every anchor carries an `AnchorSource` provenance
  string used by the audit projection so a verification decision can be
  attributed to the K8s object that published the key.
- **Information disclosure:** `VerifyingKey` (public) only; no private
  material in this module.
- **DoS:** `O(1)` lookup; reader path acquires the read lock for a
  single `Arc::clone` (microseconds) then drops it. `compact` is a
  bounded write-lock window proportional to anchor count.
- **Elevation of privilege:** anchor source provenance is opaque to
  this module and is enforced upstream by the controller-side admission
  policy that decides which CRs may publish anchors.

## 3. OWASP mapping

- **OWASP MCP Top 10 — M02 Broken Authentication:** trust-store hot
  reload prevents a stale revoked key from continuing to authenticate
  callers.
- **OWASP API Top 10 — API2 Broken Authentication:** snapshot
  generation counter exposes drift to operators (metrics + healthz).
- **OWASP API Top 10 — API8 Security Misconfiguration:** `DuplicateKid`
  fail-closed means a misconfigured second adapter does not silently
  override the first.

## 4. AuthN / AuthZ path

This module is a passive cache. Authority over *which* anchors land in
the snapshot belongs to the publishing path (informer adapter +
admission policy controlling which CRs may carry signing keys). The
cache enforces structural integrity (no conflicting duplicate, expiry
applied) but never decides whether a CR is allowed to publish.

## 5. Secret + key custody

Public `VerifyingKey` material only. No private keys in this module by
construction; the Ed25519 type API used here is verifying-only.

## 6. Egress surface delta

None. In-process structure.

## 7. Audit events emitted

None directly. Each `replace_snapshot` and `compact` invocation is
expected to be wrapped by the caller (informer adapter) with an audit
event carrying `previous_generation`, `next_generation`,
`anchors_added`, `anchors_removed` so operators can correlate trust
changes with subsequent verification decisions.

## 8. Failure mode

All paths fail-closed:

- Empty snapshot → every lookup returns `None` → `card_verifier` denies
  every signed card. (This is the safe boot state until the informer
  publishes.)
- Expired anchor → lookup returns `None` immediately even before
  `compact` runs; `card_verifier` denies.
- Conflicting duplicate `kid` at build time → `DuplicateKid` error;
  caller is expected to abort the snapshot publication and log.
- Lock poisoning → `expect("trust-store … lock poisoned")` panics the
  router process; this is the correct behaviour: a poisoned lock means
  another thread crashed mid-mutation and the snapshot may be in an
  inconsistent state. The container restart re-bootstraps from the
  informer.

No fail-open paths. No `unsafe`.

## 9. Negative-test coverage

15 unit tests in `a2a::trust_store::tests`:

- empty snapshot returns `None`.
- builder roundtrip → 1 anchor → looked up.
- expiry boundary (`now < not_after` returns; `now == not_after`
  rejects; `now > not_after` rejects).
- never-expiring anchor returned even at `i64::MAX`.
- duplicate `kid` with same metadata is idempotent.
- duplicate `kid` with different key rejected.
- duplicate `kid` with different expiry rejected.
- `as_verifier_keys` filters expired.
- new `TrustStore` is empty and generation 0.
- `replace_snapshot` publishes new generation.
- pinned `Arc<Snapshot>` clone is decoupled from subsequent
  `replace_snapshot` calls (lock-free read semantics).
- `compact` drops expired and bumps generation by 1.
- multi-thread fuzz: 8 threads × 100 iterations of `snapshot()` +
  `lookup` succeed with no data race (Rust's borrow checker plus
  `Arc<Snapshot>` semantics prove the absence of a UB; the test
  exercises observable correctness).
- `kids()` returns all kids including expired (for diagnostics).
- card-verifier borrow-shape integration: `as_verifier_keys` produces
  the exact `HashMap<&str, &VerifyingKey>` shape `verify_inbound_card`
  consumes.

## 10. Vendored / third-party dependency delta

None. Module uses only `std`, `ed25519-dalek` (already in workspace),
and `thiserror` (already in workspace). No new crate; no `arc-swap`
dependency (RwLock<Arc<…>> gives equivalent semantics for our
acceptable-cost workload). No `vendor/` change.

## 11. Sign-offs

Two independent reviews per principle 9.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
