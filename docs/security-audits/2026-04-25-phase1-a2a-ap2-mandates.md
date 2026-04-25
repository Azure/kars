# Security audit — Phase 1 A2A AP2 commerce mandate validation

**Date:** 2026-04-25
**Branch:** `phase1/a2a-ap2-mandates`
**Capability:** A2A 1.0.0 AP2 (Agent Payments) commerce-mandate types and
the pure-function validation kernel `validate_payment_attempt`. No live
HTTP route binding yet (route + AGT audit-emit wiring lands in a separate
cluster-validated PR).

## 1. Summary

Adds `inference-router/src/a2a/ap2.rs` implementing AP2 mandate evaluation:

- `IntentMandate` — signed authorisation envelope (mandate id, principal,
  currency, daily/monthly/per-transfer caps, counterparty allowlist,
  expiry, opaque signature blob).
- `PaymentAttempt` — proposed transfer.
- `PaymentRecord` — persisted ledger entry produced by a validated attempt.
- `MandateLedger` / `MandateLedgerMut` traits with reference
  `InMemoryMandateLedger`.
- `validate_payment_attempt` — pure function evaluating an attempt against
  a verified mandate + ledger snapshot.
- `Ap2Denial` — 12 distinct, structured denial variants (no string-typing).

Amounts are strictly `u64` minor units; floating-point is absent by
construction.

## 2. Threat model delta

This module is the **policy-evaluation kernel** for AP2 transfers. STRIDE
deltas, assuming subsequent route binding wires AGT signing + audit:

- **Spoofing:** mandate signature is verified upstream by the AGT signing
  provider before this kernel sees the mandate. The opaque `signature`
  field propagates through unchanged for audit projection.
- **Tampering:** all caps and allowlists are enforced inside this kernel
  with `checked_add` arithmetic; ledger sums use `saturating_add` to
  defend against accumulator overflow regardless of upstream input.
- **Repudiation:** `PaymentRecord::from_attempt` carries the
  caller-supplied `transfer_nonce` + timestamp into the audit record; the
  upstream route is responsible for emitting one `AuditSink::append`
  call per success. This module never logs.
- **Information disclosure:** N/A — this kernel emits no telemetry.
- **DoS:** validator is `O(n)` over recorded payments per mandate; AGT
  rate-limit upstream caps the call rate. `InMemoryMandateLedger` is
  reference-only; production deployments are expected to back the
  trait with an indexed AGT-side store.
- **Elevation of privilege:** principal is identified via the verified
  mandate; the kernel does not consult any other trust source.

## 3. OWASP mapping

- **OWASP LLM Top 10 — LLM06 (Excessive Agency):** AP2 mandates are the
  enforcement primitive that keeps an agent's commerce surface bounded.
  Daily/monthly/per-transfer caps + allowlist enforcement are the direct
  control.
- **OWASP API Top 10 — API4 Unrestricted Resource Consumption:** caps
  enforced rolling-window.
- **OWASP API Top 10 — API3 BOLA:** mandate id is a foreign key; the
  validator refuses any attempt whose `mandate_id` ≠ the supplied
  mandate's id (`Ap2Denial::MandateIdMismatch` is the first check),
  preventing cross-mandate attempt smuggling.

## 4. AuthN / AuthZ path

Caller is identified by the verified `IntentMandate.principal` string
delivered by the upstream `verify_inbound_card` pipeline (PR 21). This
kernel performs **no** authentication of its own; it consumes a
post-verification context.

## 5. Secret + key custody

None. No secrets, keys, or tokens flow through this module. The
`signature` field on `IntentMandate` is opaque base64 propagating into
the persisted record for downstream audit consumers.

## 6. Egress surface delta

None. Pure function; no I/O.

## 7. Audit events emitted

This module emits no audit events directly. The wrapping route is
responsible for emitting exactly one `AuditSink::append` per
successful `validate_payment_attempt`, capturing the returned
`PaymentRecord` (which carries `mandate_id`, `counterparty`, `amount`,
`currency`, `transfer_nonce`, `timestamp`). Denials should also be
audit-emitted at the route layer with the structured `Ap2Denial` variant
discriminant (no PII).

## 8. Failure mode

All paths fail-closed. Twelve distinct denial variants:

| Variant | Rationale |
|---|---|
| `MandateIdMismatch` | First-line check; foreign-key consistency. |
| `MandateExpired` | `now < exp` strictly required. |
| `CurrencyMismatch` | No silent FX. |
| `AmountZero` | Zero-amount transfers prohibited. |
| `PerTransferCapExceeded` | Single-attempt cap. |
| `CounterpartyNotAllowed` | Allowlist; `["*"]` is wildcard but only when sole entry. |
| `DailyCapExceeded` | Rolling 24 h sum + this attempt > cap. |
| `MonthlyCapExceeded` | Rolling 30 d sum + this attempt > cap. |
| `ReplayDetected` | `(mandate_id, transfer_nonce)` already on file. |
| `AttemptInFuture` | `attempt.timestamp > now`. |
| `AttemptTooOld` | `attempt.timestamp < now - 30d`. |
| `ArithmeticOverflow` | `u64` add overflow defensive guard. |

`u64::MAX` on any cap field disables that cap. No fail-open paths. No
`unwrap()` on caller data; all arithmetic uses `checked_add` /
`saturating_add` / `saturating_sub`.

## 9. Negative-test coverage

19 unit tests in `a2a::ap2::tests`:

- happy path → record returned with correct fields.
- `MandateIdMismatch` is the **first** check.
- expired mandate rejected.
- currency mismatch rejected.
- zero amount rejected.
- per-transfer cap exceeded rejected.
- counterparty not in allowlist rejected.
- wildcard `["*"]` permits arbitrary counterparty.
- wildcard must be sole entry — `["*", "acme"]` does not match arbitrary.
- daily cap exceeded rejects.
- daily cap **resets** after window (record older than 24 h ignored).
- monthly cap exceeded rejects.
- replay nonce rejected.
- attempt timestamp in future rejected.
- attempt timestamp older than monthly window rejected.
- `MandateLedgerMut::record` is idempotent on `(mandate_id, nonce)`.
- `u64::MAX` caps disable the corresponding limit.
- camelCase JSON round-trip (`mandateId`, `counterpartyAllowlist`,
  `perTransferCap`).
- `serde(deny_unknown_fields)` rejects forward-compat bait fields.

End-to-end protocol-conformance (signature-tampered mandate, route-
level audit emission, AGT outage interplay) lands with the route-binding
PR per plan §5.4.

## 10. Vendored / third-party dependency delta

None. Module uses only `serde`, `serde_json`, and `thiserror`, all
already in the workspace. No new crate; no AGT SDK touch; no
`vendor/agentmesh-*` change.

## 11. Sign-offs

Two independent reviews per principle 9.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
