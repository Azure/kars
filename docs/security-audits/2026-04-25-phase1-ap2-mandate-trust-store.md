# Phase 1 — AP2 mandate-issuer trust store

**Date:** 2026-04-25
**Branch:** `phase1/ap2-mandate-trust-store`
**Capability:** Type-safe wrapper `MandateTrustStore` around the
existing A2A `TrustStore`, dedicated to AP2 `IntentMandate`-issuer
keys. Exposes hot-reload (`replace_snapshot`) + reader projection
(`as_verifier_keys`) compatible with
`crate::a2a::mandate_signing::verify_mandate`.

## Summary

The router's AP2 path verifies `IntentMandate` signatures against
`HashMap<&str, &VerifyingKey>`. Until this PR, no router-side store
existed for *mandate-issuer* keys; tests passed an ad-hoc map per
call. This PR introduces `MandateTrustStore`, a newtype wrapper that
shares storage semantics with the A2A `TrustStore` (atomic
`ArcSwap`-backed snapshot, generation counter, strict expiry) but
is a **distinct Rust type**, so a future code path that accidentally
hands the A2A agent-card store to a mandate verifier (or vice versa)
fails compilation rather than authorising a privilege swap at run
time.

The wrapper exposes only the read + replace surface; the writer side
(`TrustAnchor`/`TrustStoreBuilder`) is reused unchanged from
`a2a::trust_store`. A future `MandateIssuer` CRD reconciler can
populate snapshots via `rebuild_snapshot` (PR 35) and publish them
through `replace_snapshot` with no additional plumbing.

## Threat model delta

**Asset gaining new exposure:** none in this PR — the AP2 verifier
already accepts a `TrustedKeys<'_>` map (PR 24); this PR only
introduces a typed wrapper around an existing snapshot type. No new
network surface, no new persisted state, no new deserialisation
target. The wrapper is internal to the crate.

**STRIDE diff vs `docs/threat-model.md`:**

| Category | Pre-PR risk | Post-PR | Notes |
| --- | --- | --- | --- |
| Spoofing | Single shared `TrustStore` instance could conceptually be wired to both A2A card verify and AP2 mandate verify, allowing a compromised A2A agent kid to sign mandates. | Distinct type makes the swap a compile-time error. | Defence-in-depth — no production wiring crossed paths today. |
| Tampering | n/a | n/a | Snapshot is immutable. |
| Repudiation | n/a | n/a | Audit emission unchanged. |
| Info disclosure | n/a | n/a | Only public verifying keys, no secrets. |
| DoS | A `replace_snapshot` runs in O(1) (Arc swap). | Unchanged. | |
| Elevation | See spoofing. | Mitigated as above. | |

## OWASP mapping

- **OWASP MCP Top 10 / LLM06 (Excessive Agency):** AP2 enables
  agents to authorise spending on a principal's behalf. Type-safe
  separation between A2A trust and mandate trust prevents an
  agent-layer compromise from being mechanically reusable as a
  commerce-issuer compromise. Control: distinct Rust type with
  zero conversion API across the boundary.
- **OWASP LLM03 (Training Data Poisoning) / LLM10 (Model Theft):**
  N/A — no model surface touched.
- **OWASP A2 (Cryptographic Failures):** `verify_mandate` itself is
  unchanged (PR 24); the Ed25519 verify path stays in
  `ed25519-dalek`.

## AuthN / AuthZ path

- **Caller:** the future AP2 message-handler (Phase 1 wiring; out of
  scope here). It obtains a snapshot via
  `app_state.mandate_trust_store.snapshot()`, projects to
  `as_verifier_keys(now)`, and passes the borrowed map to
  `verify_mandate`.
- **Identity proven:** detached-JWS Ed25519 signature over the
  canonical mandate payload, with `kid` resolved through the
  snapshot.
- **AGT policy gate:** `PolicyDecisionProvider::decide` is called
  *after* a successful signature verify (Phase 1 wiring will
  surface this seam); this PR is signature-verify infrastructure
  only.
- **Outage behaviour:** snapshot reads are pure in-process; no
  network dependency. An empty store fails closed (`UnknownKid`);
  no fallback to "any key accepted".

## Secret + key custody

- The store holds **only public** `VerifyingKey` material. No
  secrets cross this boundary. Agent UID 1000 has no path into
  this store (router runs as UID 1001 in a separate container in
  the AKS pod; the agent reaches the router solely over
  `127.0.0.1:8443`).
- Rotation: handled by `replace_snapshot` (Phase 2 reconciler);
  the existing snapshot is dropped when the last `Arc` holder
  releases it.

## Egress surface delta

None. Pure in-process data structure.

## Audit events emitted

This PR adds no new audit-event variants. The Phase 1 `message/send`
wiring will emit one mandate-verify event per call, leveraging the
existing `AuditSink` trait — that's a separate PR.

## Failure mode

- **Empty snapshot** → `verify_mandate` returns `UnknownKid` →
  caller rejects the mandate. Fail-closed.
- **Expired anchor** (`now >= not_after`) → filtered out of the
  projection map → same as above.
- **Stale `Arc` view** (caller held a snapshot across a
  `replace_snapshot`) → caller continues to verify against the
  pinned set; documented as an explicit feature for in-flight
  request consistency. New requests get the fresh snapshot.

## Negative-test coverage

7 in-tree tests in `inference-router/src/a2a/mandate_trust_store.rs`:

| Test | Asserts |
| --- | --- |
| `empty_store_yields_empty_snapshot` | Default-constructed store is empty + verifier map is empty. |
| `replace_snapshot_visible_on_next_snapshot_call` | Hot-reload contract. |
| `replace_with_empty_snapshot_revokes_all` | Atomic mass-revocation. |
| `arc_view_pins_pre_replace_snapshot` | Held `Arc` is *not* invalidated by a concurrent replace. |
| `expired_anchors_filtered_strictly` | Boundary at `now == not_after` (strict <) — both sides. |
| `generation_round_trips_through_wrapper` | Generation counter forwarded. |
| `as_verifier_keys_is_compatible_with_verify_mandate_signature` | Compile-time assertion that the projection type matches `mandate_signing::TrustedKeys<'_>`; signature drift on either side fails compilation here. |

Negative cases for the verifier itself (tampered signature,
malformed JWS, unknown kid) live in `mandate_signing.rs`'s in-tree
corpus (PR 24).

## Vendored / third-party dependency delta

None. Implementation reuses `ed25519-dalek` (already a workspace
dep) and the in-crate `a2a::trust_store` module.

## Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
