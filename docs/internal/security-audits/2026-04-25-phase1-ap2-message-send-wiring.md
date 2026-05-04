# Phase 1 — AP2-aware `message/send` wiring

**Date:** 2026-04-25
**Branch:** `phase1/ap2-message-send-wiring`
**Capability:** `handle_message_send_with_ap2` — AP2-aware A2A
JSON-RPC `message/send` handler that verifies the
`metadata.ap2 = {mandate, attempt}` extension before allowing the
underlying task-creation flow. New `A2aErrorCode::Ap2Denied` variant
(JSON-RPC `-32011`) carries the structured denial reason.

## Summary

Inbound A2A messages may carry an AP2 commerce extension authorising
the receiving agent to perform a billable transfer on behalf of the
mandate's principal. Until this PR, no router handler distinguished
such messages — they would have flowed through to task creation
without any signature, cap, or replay check.

`handle_message_send_with_ap2` is the production entry point that:

1. Inspects `params.message.metadata.ap2`.
   - **Absent** → delegates unchanged to
     `handle_message_send` (zero overhead for non-commerce traffic).
   - **Present but malformed** → JSON-RPC `InvalidParams`.
2. Verifies the mandate's detached-JWS Ed25519 signature against
   the `MandateTrustStore` (PR 38) using the snapshot's
   `as_verifier_keys(now)` projection. Fail-closed against unknown
   kid, expired anchor, tampered payload, or empty store.
3. Runs the AP2 policy validator
   (`validate_payment_attempt_signed`, PR 24) to enforce per-
   transfer / daily / monthly caps, counterparty allowlist,
   currency match, replay nonce, mandate `exp`, and timestamp
   bounds.
4. On success, appends the resulting `PaymentRecord` to the
   `MandateLedgerMut` *before* delegating to the underlying handler.
   Ledger remains untouched on any denial — the order of operations
   guarantees signature → policy → ledger-append → task-create with
   no partial commits.
5. Maps any `Ap2Denial` to `A2aErrorCode::Ap2Denied` (`-32011`)
   with `data.reason` (rendered denial) and `data.kind` (machine-
   readable enum tag) so audit consumers can attribute the
   rejection.

## Threat model delta

**Asset gaining new exposure:** the `MandateLedger` write path. Pre-
PR, ledger writes were only reachable from in-tree tests. Post-PR,
inbound A2A traffic with a verified mandate causes a ledger insert.
The verification chain (signature → policy) makes this safe; the
audit-doc enumerates each defence.

**STRIDE diff:**

| Category | Risk | Mitigation |
| --- | --- | --- |
| Spoofing | Forged mandate authorising a fraudulent transfer. | Detached-JWS Ed25519 verify against trust-store (PR 38). Empty store fails closed. |
| Tampering | Mandate body modified after signing (e.g. cap raised). | Canonical-payload signature covers every field; tamper test `tampered_payload_after_signing_rejected` confirms rejection. |
| Repudiation | Legitimate transfer denied later. | `data.reason` + `data.kind` in the JSON-RPC error envelope make every denial reason machine-classifiable; ledger append happens only after validation succeeds. |
| Info disclosure | Validator timing oracle leaking ledger state. | Signature check runs first (PR 24); unauthentic mandate cannot reach ledger queries. |
| DoS | Heavy malformed-extension traffic. | `serde(deny_unknown_fields)` rejects extras cheaply; signature verify only runs on a structurally valid extension. |
| Elevation of privilege | Agent re-issuing past mandates to drain caps. | Replay nonce + per-mandate ledger window queries; test `replayed_nonce_rejected` confirms. |

## OWASP mapping

- **OWASP LLM06 (Excessive Agency):** AP2 is the canonical
  excessive-agency vector for autonomous agents; this PR is the
  routing-layer enforcement.
- **OWASP LLM10 (Sensitive Information Disclosure):** denial reasons
  are surfaced as structured kinds, not raw error chains, to avoid
  leaking ledger internals.
- **OWASP A2 (Cryptographic Failures):** Ed25519 verify via
  `ed25519-dalek` (already a workspace dep); no custom crypto.
- **OWASP MCP/Tool Top 10 (T9 — Tool Authorisation):** mandate is
  the authorisation primitive; this PR is the gate.

## AuthN / AuthZ path

- **Caller identity:** outer A2A inbound caller is authenticated by
  `card_verifier::verify_inbound_card` upstream of this handler
  (per `jsonrpc_dispatch.rs` module docs). The mandate inside the
  message authenticates the *principal* whose money is being moved
  — distinct from the agent identity.
- **Authorisation:** signature verification + policy check, in that
  order, in `validate_payment_attempt_signed`.
- **Outage behaviour:** all checks are pure in-process; no external
  network calls. An empty trust store (e.g. during a Phase 2
  reconciler restart before snapshot rebuild) fails closed —
  consistent with `Strict` outage mode default per
  internal Phase 1 plan §1.3.

## Secret + key custody

- Public keys only. Same as PR 38 — `MandateTrustStore` holds only
  `VerifyingKey`s.
- No private mandate-issuer key material lives in the router.
- Ledger contents are not secrets but are tenant-scoped; current
  `InMemoryMandateLedger` is per-process. Production deployments
  will swap a Foundry-backed `MandateLedgerMut` impl behind the
  trait (no router-side secret involved).

## Egress surface delta

None. Pure in-process validation.

## Audit events emitted

This PR does **not** emit `AuditSink` events directly — that wiring
will land in the gateway daemon when it adopts this handler (the
gateway has the `AppState` reference; the pure handler does not).
The `data.reason` + `data.kind` shape on the JSON-RPC error
envelope is designed to map 1:1 to a future audit-event variant
without further parsing.

## Failure mode

| Scenario | Outcome |
| --- | --- |
| No `metadata.ap2` | Pass-through to underlying handler. |
| Malformed `metadata.ap2` | `InvalidParams` (-32602). |
| Unsigned mandate | `Ap2Denied` / `mandateUnauthentic`. Ledger untouched. |
| Unknown kid | `Ap2Denied` / `mandateUnauthentic`. |
| Tampered mandate | `Ap2Denied` / `mandateUnauthentic`. |
| Expired anchor | `Ap2Denied` / `mandateUnauthentic`. |
| Empty trust store | `Ap2Denied` / `mandateUnauthentic`. (Fail-closed default.) |
| Cap exceeded | `Ap2Denied` / `perTransferCapExceeded` (or daily/monthly variant). Ledger untouched. |
| Replayed nonce | `Ap2Denied` / `replayDetected`. |
| Counterparty not allowed | `Ap2Denied` / `counterpartyNotAllowed`. |
| Currency mismatch | `Ap2Denied` / `currencyMismatch`. |
| Validator success | Ledger append → task created in `submitted` state. |

Crucially, on any denial path the ledger is **not** mutated. Test
`cap_exceeded_rejected_after_signature_passes` asserts this
explicitly.

## Negative-test coverage

14 in-tree tests in `inference-router/src/a2a/message_send_ap2.rs`:

| Test | Asserts |
| --- | --- |
| `no_metadata_field_passes_through_to_underlying_handler` | AP2-free flow unchanged. |
| `metadata_without_ap2_key_passes_through` | Other metadata fields don't trigger the AP2 path. |
| `signed_valid_ap2_extension_records_to_ledger_and_creates_task` | Happy path: ledger gets one record + task created. |
| `unsigned_mandate_rejected_with_ap2_denied` | Signature gate. |
| `signed_by_unknown_kid_rejected` | Trust-store membership. |
| `tampered_payload_after_signing_rejected` | Canonical-payload integrity. |
| `empty_trust_store_fails_closed_for_signed_mandate` | Fail-closed default. |
| `expired_anchor_rejects_signed_mandate` | Strict-inequality expiry filter. |
| `cap_exceeded_rejected_after_signature_passes` | Policy enforcement + ledger non-mutation on denial. |
| `replayed_nonce_rejected` | Replay protection. |
| `malformed_ap2_extension_rejected_as_invalid_params` | Schema gate. |
| `ap2_with_extra_fields_rejected_due_to_deny_unknown_fields` | Unknown-field rejection. |
| `denial_kind_string_is_not_default_for_known_variants` | Kind-string mapping intact. |
| `base64_engine_alias_used_via_extraction` | Belt-and-braces import retention. |

All 14 negative cases assert ledger non-mutation where applicable.

## Vendored / third-party dependency delta

None. Reuses `ed25519-dalek`, `serde`, `serde_json`, `thiserror` —
all existing workspace deps.

## Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
