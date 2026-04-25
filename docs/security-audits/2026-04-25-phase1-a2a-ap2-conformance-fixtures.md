# Security audit — Phase 1 A2A AP2 conformance fixture corpus

**Date:** 2026-04-25
**Branch:** `phase1/a2a-ap2-conformance-fixtures`
**Capability:** Fixture-driven conformance corpus for the AP2 mandate
validator (`a2a::validate_payment_attempt`). Pure-function integration
test backed by 14 wire-format JSON fixtures plus a coverage-floor
guard.

## 1. Summary

Adds:

- 14 fixtures under `inference-router/tests/fixtures/ap2_conformance/`
  (`001-happy-path.json` through `014-mandate-id-mismatch.json`) plus
  `_common.json` (excluded from the loader by leading `_`).
- `inference-router/tests/ap2_conformance.rs` — integration test that
  loads every fixture, drives the validator, and asserts the documented
  verdict; plus `ap2_conformance_corpus_has_minimum_coverage` which
  enforces that the corpus contains at least one Allow + every reachable
  denial kind (11 of 12; `ArithmeticOverflow` requires synthetic input).

Fixtures use the canonical A2A camelCase wire format
(`mandateId`, `counterpartyAllowlist`, etc.) so they can be reused for
future AGT-side AP2 implementations.

## 2. Threat model delta

No new attack surface. The corpus is *pure validation* of an existing
in-tree pure function. Its security value is preventive: a future
refactor that silently relaxes a denial path is caught by a fixture
flip, not by a production incident.

## 3. OWASP mapping

- **OWASP MCP Top 10 — M03 Insecure Configuration:** corpus enforces
  the documented daily/monthly cap behaviour so a configuration drift
  cannot quietly disable cap enforcement.
- **OWASP API Top 10 — API4 Resource Consumption:** `DailyCapExceeded`
  + `MonthlyCapExceeded` + `PerTransferCapExceeded` cases pinned by
  fixture.
- **OWASP API Top 10 — API3 BOLA:** `MandateIdMismatch` fixture
  enforces foreign-key consistency cannot be silently broken.

## 4. AuthN / AuthZ path

N/A — the corpus is in-process test code.

## 5. Secret + key custody

None. Fixtures contain only synthetic public data (`signature` field is
the literal string `"fixture-sig"`, never verified by the validator).

## 6. Egress surface delta

None. Test harness uses local filesystem reads only.

## 7. Audit events emitted

None.

## 8. Failure mode

- Fixture parse failure → test panics with file path + parse error;
  CI fails.
- Unexpected verdict → assertion failure with fixture name + path +
  expected-vs-actual variant; CI fails.
- Coverage-floor failure → second test panics with the missing denial
  kind; CI fails.

No fail-open paths; coverage floor cannot be silently lowered (the
required-kinds slice is in code, not configuration).

## 9. Negative-test coverage

The corpus *is* the negative test coverage:

- `001-happy-path` — Allow.
- `002-mandate-expired` — Deny.MandateExpired.
- `003-currency-mismatch` — Deny.CurrencyMismatch.
- `004-amount-zero` — Deny.AmountZero.
- `005-per-transfer-cap-exceeded` — Deny.PerTransferCapExceeded.
- `006-counterparty-not-allowed` — Deny.CounterpartyNotAllowed.
- `007-wildcard-allowlist-allows` — Allow under `["*"]` allowlist.
- `008-daily-cap-exceeded` — Deny.DailyCapExceeded with 2 prior records.
- `009-daily-cap-window-rolls` — Allow when prior records are older
  than the 24 h window (positive *and* negative information: the
  rolling-window logic is correct only if this fixture passes).
- `010-monthly-cap-exceeded` — Deny.MonthlyCapExceeded with 20 prior
  records.
- `011-replay-detected` — Deny.ReplayDetected.
- `012-attempt-in-future` — Deny.AttemptInFuture.
- `013-attempt-too-old` — Deny.AttemptTooOld.
- `014-mandate-id-mismatch` — Deny.MandateIdMismatch.

The coverage-floor test refuses to merge a corpus that drops any of
the 11 reachable denial kinds.

## 10. Vendored / third-party dependency delta

None. The test uses `serde`, `serde_json`, and the in-tree
`azureclaw_inference_router` crate. No new crate; no `vendor/` change.

## 11. Sign-offs

Two independent reviews per principle 9.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
