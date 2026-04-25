# Security audit — AP2 mandate signature verification wired into payment-attempt validation

**Date:** 2026-04-25
**PR branch:** `phase1/ap2-mandate-verify-wired`
**Capability owner:** AzureClaw Phase 1 — A2A / AP2

## 1. Summary

Adds `validate_payment_attempt_signed`, a new public entry point on the AP2
policy engine that verifies a mandate's detached-JWS `signature` against a
trust map BEFORE running any of the existing cap / allowlist / replay /
expiry checks. The pre-existing `validate_payment_attempt` continues to
exist unchanged for unsigned-mandate code paths (tests, migration, dev
clusters running the `Null*` providers).

A new `Ap2Denial::MandateUnauthentic(String)` variant is added for the
failure mode. The string carries the rendered
`MandateSignError` so the audit-emission path can classify the underlying
reason (Unsigned / MalformedDetached / SignatureInvalid / UnknownKid /
MissingKid / SignatureLength / MalformedHeader / Serialise /
ProtectedSerialise) without exposing that internal type across the AP2
public surface.

This PR ships the wiring of the PR 29 primitive
(`a2a::mandate_signing::{sign_mandate, verify_mandate}`) into the payment-
authorization decision. The router enforcement hook (calling
`validate_payment_attempt_signed` from `routes/a2a.rs`) and the
`SigningProvider`-backed `TrustedKeys` materialisation are still pending —
those land alongside the OAuth 2.1 tower layer.

## 2. Threat model delta

### Asset gaining new exposure
None. This PR is pure-Rust unit-level wiring; no new I/O, no new
network endpoint, no new persisted state, no new K8s resource type.

### STRIDE diff against `docs/threat-model.md`
- **Tampering** (T1 — mandate body altered after signing): now detected at
  `validate_payment_attempt_signed`. Tampered caps, allowlist additions, or
  expiry extension all change the canonical payload and produce
  `MandateUnauthentic(SignatureInvalid)`.
- **Spoofing** (S1 — mandate forged by an untrusted issuer): now detected
  at `validate_payment_attempt_signed`. A mandate signed by an unknown kid
  produces `MandateUnauthentic(UnknownKid)`; one signed by a kid present
  in the trust map but with a different key produces
  `MandateUnauthentic(SignatureInvalid)`.
- **Repudiation** (R1 — payor denies authorising a mandate): unchanged in
  this PR — the audit-receipt landing happens at the route boundary.
  Wiring will reference this audit doc.

## 3. OWASP mapping

- **OWASP LLM03 — Sensitive Information Disclosure (mandate caps):** caps
  cannot be silently raised after signing → unauthenticated tamper now
  produces a deny verdict before the ledger is consulted, so timing-side-
  channel exposure of the ledger's per-mandate bucket totals is also
  closed for unauthentic mandates.
- **OWASP LLM06 — Insecure Output Handling:** AP2 denials are well-typed;
  the new variant carries a rendered string (not the raw error object), so
  audit serialisers cannot accidentally leak the verifier's private state.
- **OWASP MCP05 — Authentication and Authorization Bypass:** mandate
  signature is now part of the AP2 authorization path. Bypass requires
  forging an Ed25519 signature on the canonical payload OR compromising a
  `kid` in the trust map. Both are out of scope for this layer (handled
  by `SigningProvider`).

## 4. AuthN / AuthZ path

- **Caller:** `validate_payment_attempt_signed` is internal-router-only.
  No new public surface, no auth at this layer.
- **Trust map:** `TrustedKeys<'a>` is a borrowed view; the caller (the
  eventual route handler) materialises it from `SigningProvider` + the
  router's mandate-issuer trust store snapshot. Stale entries are
  caller-side concern — verifier doesn't cache.
- **Outage behaviour:** if the trust map is empty, every signed mandate
  fails `UnknownKid`; every unsigned mandate fails `Unsigned`. There is
  no fail-open path. This is `Strict` by construction.

## 5. Secret + key custody

No keys are read or written in this PR. The verifier only consumes
public `VerifyingKey` references provided by the caller. Private keys
remain owned by `SigningProvider`. Agent (UID 1000) cannot read either —
there is no on-disk secret introduced.

## 6. Egress surface delta

None. Pure in-process function.

## 7. Audit events emitted

None directly emitted by this PR. The route handler that calls
`validate_payment_attempt_signed` will emit one of:
- `ap2.mandate.deny.unauthentic` (new, this PR enables it) — receipt id
  only, with the inner classification string copied into the AGT event
  attributes; no mandate body / signature / payload bytes.
- `ap2.mandate.deny.<existing-policy-variant>` — unchanged.
- `ap2.mandate.allow` — unchanged.

## 8. Failure mode

- All five negative test cases land **fail-closed**: signature missing /
  malformed / wrong-key / tampered-payload / unknown-kid → returns
  `Ap2Denial::MandateUnauthentic`. Ledger is **not** consulted; cap
  windows are **not** queried; expiry is **not** evaluated. This means
  the unauthentic-mandate branch has zero ledger / cap timing-side-
  channel observability.
- The allow path still runs the full cap / allowlist / replay / expiry
  set, so a signed-but-out-of-policy mandate is denied with the
  underlying policy variant (proven by `signed_policy_denials_still_propagate`).

## 9. Negative-test coverage

In `inference-router/src/a2a/ap2.rs` (5 new in-tree tests):

- `signed_happy_path_accepted` — signed + in-policy → record returned.
- `signed_unsigned_mandate_rejected_before_policy` — empty signature →
  `MandateUnauthentic`.
- `signed_tampered_mandate_rejected_before_policy` — caps raised after
  signing → `MandateUnauthentic` (specifically not `PerTransferCapExceeded`).
- `signed_unknown_kid_rejected` — kid not in trust map → `MandateUnauthentic`.
- `signed_policy_denials_still_propagate` — signed mandate over per-
  transfer cap → `PerTransferCapExceeded` (not `MandateUnauthentic`).

The `tests/ap2_conformance.rs::denial_kind` switch has been extended with
the new `MandateUnauthentic` arm so the AP2 conformance corpus continues
to compile against the larger error enum (no new fixtures added — fixtures
exercise the unsigned `validate_payment_attempt` entry point).

## 10. Vendored / third-party dependency delta

None. Reuses the previously audited primitives in
`a2a::mandate_signing` (PR 29) and the existing `ed25519-dalek` /
`base64` / `serde` stack used by `card_signing.rs`.

Sources consulted while authoring this PR:
- RFC 7515 §3.1 (JWS Detached Form).
- `inference-router/src/a2a/mandate_signing.rs` (PR 29 audit doc:
  `docs/security-audits/2026-04-25-phase1-ap2-mandate-signing.md`).

## 11. Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
