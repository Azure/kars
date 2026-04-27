# Security Audit: `phase1/hotspot-pass2-inference-split`

**Capability:** decomposes `inference-router/src/routes/inference.rs`
(1833 LOC, 333 over the §4.2 Phase 1 cap of 1500) by lifting the three
pure body-translation helpers — `uuid_v4`, `chat_to_responses_body`,
`responses_to_chat_body` — and their five unit tests into a new
sibling module `routes/inference_translate.rs`.

**Type:** mechanical refactor, zero behaviour change.

## 1. Summary

Before this PR, `inference.rs` carried ~340 lines of pure JSON-shape
translation logic at the bottom of the file (lines 1360–1697) plus
~135 lines of associated tests (lines 1699–1833). These helpers have
no router or AppState dependency — they take `&Bytes` in, return
`Bytes` out, and are tolerant of malformed input (return original
bytes on parse failure).

This PR:

- Creates `inference-router/src/routes/inference_translate.rs` (490
  LOC incl. doc-comment + 5 tests).
- Marks the 3 helpers as `pub(super)` (visible only to siblings under
  `routes::`).
- Removes the original block from `inference.rs` (lines 1359–1833 →
  gone) bringing it from 1833 → 1359 LOC, **clear of the §4.2 Phase 1
  hot-file cap of 1500 LOC** and well under the §4.2 800-LOC hard cap
  for new files.
- Adds `use super::inference_translate::{chat_to_responses_body,
  responses_to_chat_body};` to `inference.rs` (uuid_v4 has no
  in-tree caller, kept `#[allow(dead_code)]`).
- Registers `pub(crate) mod inference_translate;` in
  `routes/mod.rs` (alphabetical order preserved between
  `inference_policy` and `signing_ops`).

## 2. Threat model delta

**None.** Pure file-split. No control-flow change, no I/O change, no
visibility widening: `pub(super)` keeps the helpers strictly internal
to `routes::`, identical to the pre-split private `fn` visibility (the
helpers were never accessible from outside `routes::inference` and
remain so).

**STRIDE:**
- Spoofing / Tampering / Repudiation / Information Disclosure / DoS /
  Elevation: all unchanged. Same bytes in → same bytes out.

## 3. OWASP mapping

Not applicable — this PR introduces no new external surface, no new
auth/authz path, no new data flow. The translation helpers themselves
implement the existing Chat ↔ Responses API shape mapping documented
under OWASP LLM01 (prompt injection — *not* affected here, mapping is
stateless and forwards user content verbatim).

## 4. AuthN / AuthZ path

Unchanged. Helpers operate on already-authenticated request/response
bytes downstream of the existing router auth gate.

## 5. Secret + key custody

None. Helpers handle JSON shape only; no secret material flows through
them.

## 6. Egress surface delta

Zero. No new HTTP clients, no new endpoints, no new dependencies.

## 7. Audit events emitted

None. Helpers do not call `AuditSink::append`. Audit events are
emitted by the call-site handlers in `inference.rs`, which are not
moved by this PR.

## 8. Failure mode

Unchanged. Both translators return the original `&Bytes` clone on JSON
parse failure (tolerant pass-through) — identical to pre-split
behaviour. The 5 unit tests covering parse-success cases moved with
the helpers and remain green.

## 9. Negative-test coverage

All 5 existing translation tests moved to the new module's
`tests` submodule and remain green:

- `test_chat_to_responses_simple_message`
- `test_chat_to_responses_tool_calls`
- `test_responses_to_chat_with_tool_calls`
- `test_chat_to_responses_system_to_developer`
- `test_responses_to_chat_with_null_error`

Total test count unchanged: 378 (209 router + 125 controller + 15
mesh + 26 governance + 3 proxy).

## 10. Vendored / third-party dependency delta

None. No `Cargo.toml` change, no `vendor/` change, no npm change.

## 11. LOC budget impact

- `inference-router/src/routes/inference.rs`: 1833 → 1359 (−474). Now
  under the §4.2 Phase 1 cap of 1500. Phase 2 cap is 800 (further
  decomposition is part of `phase2-s9-p0` — not this PR).
- New file `inference-router/src/routes/inference_translate.rs`: 490
  LOC (under the 800-LOC hard cap for new files; well-defined,
  single-responsibility module).
- `inference-router/src/routes/mod.rs`: +1 LOC (module registration).

This satisfies §4.3 "touched code pays its decomposition debt" — the
recent four-seam migrations (audit-sink-migrate-handoff,
policy-provider-migrate-inference) added trait-routing call-sites to
`inference.rs` without paying down its size. This PR pays that debt
in one mechanical lift.

## 12. Sign-offs

Signed-off-by: GitHub Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
