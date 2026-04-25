# 2026-04-25 — Phase 1: AP2 IntentMandate detached-JWS signing primitive

## Summary

Adds `inference-router/src/a2a/mandate_signing.rs` — a pure
sign/verify primitive that turns the `IntentMandate.signature` field
from an opaque pass-through string into a real, verifier-checkable
detached Ed25519 JWS (RFC 7515 §3.1, alg = "EdDSA").

Mirrors the established pattern from `card_signing.rs` (AgentCard
signing). Pure module, no I/O, deterministic. Public API:

- `sign_mandate(unsigned, &SigningKey, kid) -> IntentMandate`
- `verify_mandate(mandate, &TrustedKeys) -> Result<kid, MandateSignError>`

13 unit tests covering happy path + every reject variant. **No
existing call site is modified by this PR** — `validate_payment_attempt`
in `ap2.rs` continues to ignore the signature field. The wiring of
verification into the AP2 enforcement path is a follow-up that needs
a `SigningProvider`-backed trust store; this PR ships the primitive
so that follow-up is a one-liner.

## Threat model delta

Before: `IntentMandate.signature` was an opaque string copied
verbatim into `PaymentRecord` for audit. A hostile counterparty (or
a compromised intermediary) could mint an arbitrary `IntentMandate`
JSON blob, populate `signature` with anything, and the router would
have no way to detect tampering — the field's only consumer was the
audit log.

After: a verifier with a configured trust map can detect any
mutation of any non-`signature` field, any swap of the signing key,
any algorithm-confusion attempt (HS256-over-EdDSA-trust-anchor), any
malformed protected header, any wrong-length signature, any unknown
kid, and any unsigned mandate. Each is reported as a distinct
`MandateSignError` variant for audit emission.

STRIDE coverage:

- **Tampering** — `tampered_principal_rejected`,
  `tampered_caps_rejected` exercise field mutation post-sign.
- **Spoofing** — `unknown_kid_rejected`,
  `wrong_key_under_correct_kid_rejected` cover kid forgery and
  key-substitution.
- **Elevation of privilege** — `alg_other_than_eddsa_rejected`
  covers the algorithm-confusion class (no `none`, no HS256, no
  unexpected curve).
- **Information disclosure / DoS** — malformed inputs
  (`malformed_detached_form_rejected`,
  `empty_protected_segment_rejected`,
  `empty_signature_segment_rejected`,
  `signature_wrong_length_rejected`) are rejected without panic and
  without partial-trust state.

## OWASP mapping

- **OWASP LLM 2025 LLM02: Insecure Output Handling** — refusing to
  treat unverified mandate fields as authority is a direct mitigation.
- **OWASP A2A draft Top 10 A04: Mandate Forgery** — primary
  capability.
- **OWASP A2A draft Top 10 A07: Replay** — orthogonal; the existing
  `MandateLedger` rolling-window dedup handles replay separately.
  Mandate signing closes the *origin* gap; the ledger closes the
  *replay* gap.

## AuthN / AuthZ path

The verifier is pure synchronous Rust; no callers are added in this
PR. Its eventual call site is `validate_payment_attempt` (or a
wrapper above it) which the AP2 follow-up will land. The wiring will
read trust anchors from the existing `SigningProvider` /
`TrustStore`, so key custody stays at the provider boundary
(§1.2 of the implementation plan).

## Secret + key custody

The module accepts an `&SigningKey` from the caller. It does not
read environment variables, files, or the network. Tests use
deterministic seeds (`SigningKey::from_bytes(&[seed; 32])`) — these
are test-only fixtures and never reach production.

## Egress surface delta

None.

## Audit events emitted

None directly. Each `MandateSignError` variant is designed to map
1:1 to an audit reason code in the follow-up wiring PR. The variant
list (`Unsigned`, `MalformedDetached`, `MalformedHeader`,
`SignatureLength`, `SignatureInvalid`, `UnknownKid`, `MissingKid`)
gives downstream observers fine-grained classification without
forcing a single coarse "verification failed".

## Failure mode

Every failure path returns `Err(MandateSignError::*)`. There is no
panic, no `.unwrap()` on attacker-controlled bytes, no partial-trust
state. Default behaviour at the router-side wiring layer will be
fail-closed: a mandate that does not verify is treated identically
to a missing mandate.

## Negative-test coverage

13 unit tests under `a2a::mandate_signing::tests`:

- `round_trip_signs_and_verifies` (Allow)
- `unsigned_mandate_rejected`
- `malformed_detached_form_rejected`
- `empty_protected_segment_rejected`
- `empty_signature_segment_rejected`
- `unknown_kid_rejected`
- `tampered_principal_rejected`
- `tampered_caps_rejected`
- `wrong_key_under_correct_kid_rejected`
- `alg_other_than_eddsa_rejected`
- `missing_kid_in_header_rejected`
- `signature_wrong_length_rejected`
- `signature_field_overwritten_on_each_sign`

This complements the existing AP2 fixture corpus
(`tests/ap2_conformance.rs` from PR 25) which exercises mandate
*policy* (caps, allowlist, expiry). Together: PR 25 = "is the
transfer in policy?", this PR = "is the mandate authentic?".

## Vendored / third-party dependency delta

None. Reuses `ed25519_dalek`, `serde`, and the existing
`a2a::signature` JWS primitives — same dependency surface as
`card_signing.rs`. The crypto is delegated to the vendored
`ed25519-dalek` crate (already in tree via `card_signing.rs`).

## Sources consulted

- RFC 7515 §3.1 (detached JWS form) —
  <https://datatracker.ietf.org/doc/html/rfc7515#section-3.1>
- RFC 8725 §3.1 / §3.2 (JWT BCP, alg pinning) —
  <https://datatracker.ietf.org/doc/html/rfc8725>
- A2A AP2 extension draft (mandate model) —
  <https://a2a-protocol.org/extensions/ap2>
- `inference-router/src/a2a/card_signing.rs` for canonical-payload +
  detached-JWS pattern.
- `inference-router/src/a2a/ap2.rs` for `IntentMandate` shape and
  current opaque-signature behaviour.

## Verification

- `cargo test --package azureclaw-inference-router --lib a2a::mandate_signing`
  → 13 passed.
- `cargo test --package azureclaw-inference-router --lib`
  → **504 passed** (was 491; +13 from this PR).
- `cargo clippy --package azureclaw-inference-router --all-targets -- -D warnings`
  clean.

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
