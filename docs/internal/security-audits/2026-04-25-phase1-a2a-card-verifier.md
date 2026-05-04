# Phase 1 — A2A inbound AgentCard verification pipeline

Date: 2026-04-25
Branch: `phase1/a2a-card-verifier`
Capability touched: `inference-router/src/a2a/card_verifier.rs` (new), `mod.rs`

## What landed

Real **inbound** A2A AgentCard verification — symmetric counterpart to
`a2a::card_server::build_signed_card`. Pure function. Caller fetches
the bytes (the forthcoming `azureclaw-a2a-gateway` will do the I/O),
this function returns either a `VerifiedCallerIdentity` or a
structured `CardVerifyError`.

Public surface:

- `verify_inbound_card(raw, &CardVerifierConfig) → Result<VerifiedCallerIdentity, CardVerifyError>`
- `CardVerifierConfig { trusted_keys, expected_url_prefix, now }`
- `VerifiedCallerIdentity { kid, name, version, provider_url }`
- `CardVerifyError` — 8 distinct variants for downstream audit + HTTP
  status mapping

## Threat model

| Threat | Mitigation | Test |
|---|---|---|
| Empty trust store accepts everything | Explicit early return when `trusted_keys.is_empty()` (defence-in-depth — `card_signing::verify_card` already returns NoTrustedSignatureValid, but we surface it with a clearer code path) | `empty_trust_store_rejects_even_validly_signed_card` |
| Unknown `kid` accepted | `card_signing::verify_card` skips signatures with unknown kid; if all signatures are skipped, returns NoTrustedSignatureValid | `unknown_kid_rejected_as_no_trusted_signature` |
| Unsigned card accepted as authenticated | Reject when `signatures` field is absent | `unsigned_card_rejected` |
| Empty `signatures: []` array bypasses signed-card requirement | Reject when array is empty | `empty_signatures_array_rejected_as_unsigned` |
| Protocol downgrade — caller declares `protocolVersion` we no longer support | Strict equality check against pinned `A2A_PROTOCOL_VERSION` | `protocol_version_mismatch_rejected` |
| Verified-but-misbinding — card validly signed by a trusted key but for a different origin | Caller-supplied `expected_url_prefix` MUST be a prefix of `provider.url` | `url_prefix_mismatch_rejected`, `url_prefix_set_but_card_has_no_provider_rejected` |
| Empty required field (`name`, `version` whitespace-only) | Trim + non-empty check after parse | `empty_name_rejected`, `whitespace_only_version_rejected` |
| Replay of stale card (revoked agent's old card resurfaced) | Optional `validUntil` enforced against caller's `now` | `valid_until_in_past_rejected` |
| Premature use (card signed for future activation) | Optional `validFrom` enforced | `valid_from_in_future_rejected` |
| Malformed timestamp causes silent acceptance | Strict RFC 3339 parser (Z or `±HH:MM` offset only); rejects anything that doesn't match | `malformed_valid_from_rejected`, `rfc3339_parser_rejects_bad_input` |
| Body tampering after signing | `card_signing::verify_card` re-canonicalises payload with `signatures` stripped → any body change fails verification | `signature_tamper_after_signing_rejected` |
| Multi-signer card with one trusted + many untrusted signers should still pass | `card_signing::verify_card` returns first valid kid match | `multi_signer_cards_pass_when_any_kid_trusted` |
| Malformed JSON | `serde_json::from_slice` returns Err → `CardVerifyError::Parse` (no panic, no exposed raw bytes in error message) | `malformed_json_returns_parse_error` |
| Out-of-range clock provider crashes | All `SystemTime` arithmetic uses checked `duration_since`; far-future clock with no freshness fields is a no-op | `far_future_now_doesnt_panic_without_freshness_field` |

## Crypto

This module performs **no direct cryptographic operations**. All
signature work delegates to `card_signing::verify_card`, which itself
uses the workspace `ed25519-dalek` dep. No new crypto-primitive
imports → `no-custom-crypto` gate clean.

## RFC 3339 parser

A bounded, hand-written parser is included rather than pulling
`chrono`. Rationale:

1. The format we parse is fixed (`YYYY-MM-DDTHH:MM:SS[.fraction][Z|±HH:MM]`).
2. `chrono` has a track record of silently accepting near-misses
   that this module's threat model treats as adversarial input.
3. The parser uses Howard Hinnant's date algorithm
   (https://howardhinnant.github.io/date_algorithms.html#civil_from_days)
   for correctness across any year ±9999 with no leap-year bug.
4. Out-of-range components (month 13, day 32, hour 25, etc.) are
   explicitly rejected — `chrono` rejects most of these too, but the
   pinned cases here are tested directly.
5. No floating point arithmetic.

The parser is `pub(crate)`-scoped via being inside the module; not
exposed in `mod.rs`.

## What this PR is NOT

- Not the `/.well-known/agent.json` HTTP fetcher. That belongs to the
  forthcoming `azureclaw-a2a-gateway` daemon — needs cluster validation
  (real DNS / TLS / NetworkPolicy).
- Not the JSON-RPC method dispatcher (`message/send`, `tasks/get`,
  `tasks/cancel`). Lands in `phase1/a2a-jsonrpc-dispatch`.
- Not the AP2 commerce mandate validator. Lands in
  `phase1/a2a-ap2-mandates`.
- Not the trust-store reconciler (which gathers `kid → VerifyingKey`
  bindings from `A2AAgent` CRs into a hot-reload cache). Lands in
  `phase1/a2a-trust-store`.

## Test coverage

`a2a::card_verifier::tests` — **25 tests** covering every variant of
`CardVerifyError` plus all positive paths. Total router lib tests
384 → 410 passing.

## Pipeline integration

Once the gateway daemon lands, the inbound path is:

```
TLS terminate → fetch caller's agent.json (or extract from request) →
verify_inbound_card(raw, config) → bind kid+name to JSON-RPC session →
dispatch message/send|tasks/get|tasks/cancel
```

This PR ships the third step end-to-end. The other steps land in
the gateway/dispatch PRs.

## CI gates

- `ci/no-stubs.sh` — pass
- `ci/no-custom-crypto.sh` — pass (delegates to existing `card_signing`)
- `ci/check-loc.sh` — pass (card_verifier.rs 595 LOC ≤ 1500 cap)
- `ci/no-null-provider-prod.sh` — pass
- `ci/a2a-module-isolation.sh` — pass (no `auth::*` import; `forbid(unsafe_code)` honoured)
- `ci/vendored-patch-audit.sh` — pass (no SDK pin change)
- `ci/security-audit-required.sh` — this doc

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
