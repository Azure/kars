# 2026-04-25 — Phase 1: A2A AgentCard verifier conformance corpus

## Summary

Adds an integration-level conformance corpus for the inbound A2A
AgentCard verifier (`inference-router/src/a2a/card_verifier.rs`) at
`inference-router/tests/a2a_card_verifier_conformance.rs`. Fifteen
scenarios cover every reachable rejection variant of `CardVerifyError`
plus an Allow happy path and a URL-prefix-binding-match Allow case. A
`coverage_floor` test asserts every required variant is exercised so
the corpus cannot silently regress when new variants are added.

This is a tests-only change. No production code paths are modified.

## Threat model delta

The verifier is the trust-binding gatekeeper for inbound A2A traffic
(see ADR `docs/adr/0001-a2a-ingress-isolation.md`). Without per-variant
negative coverage, a refactor could regress one rejection branch (e.g.
`Expired` → silently accept) without the unit tests inside `card_verifier.rs`
catching it, because those tests focus on the happy path of each
helper. This corpus exercises the *public* API end to end: build a
real signed card, mutate the wire form or config, verify, assert the
exact rejection variant.

STRIDE coverage:

- **Tampering** — `tampered_name`, `tampered_url`, `tampered_freshness`
  scenarios mutate signed payload bytes after signing; verifier must
  reject with `Signature`.
- **Spoofing** — `unknown_kid`, `empty_trust_anchors`, `no_signatures_field`
  scenarios attempt to bypass identity binding; verifier must reject.
- **Replay** — `expired_card`, `not_yet_valid_card` scenarios test
  validity-window enforcement.
- **Elevation of privilege via downgrade** — `wrong_protocol_version`
  exercises the protocol-version pin defence.
- **Information disclosure / DoS** — `malformed_envelope`,
  `malformed_freshness` test that hostile parse input is rejected
  without a panic or a partially-trusted state.

## OWASP mapping

This corpus directly exercises controls listed in `docs/security-mcp-top10.md`
once the doc lands in Phase 1; aligned with:

- **OWASP LLM 2025 LLM07: System Prompt / Identity Spoofing** —
  reject misbinding (URL prefix), tampered identity (name).
- **OWASP A2A draft Top 10 A03: Card Replay** — `Expired` /
  `NotYetValid` enforce validity-window.
- **OWASP A2A draft Top 10 A05: Trust Anchor Misconfiguration** —
  `empty_trust_anchors` defends against the silent-accept-everything
  failure mode.

## AuthN / AuthZ path

Not applicable to this PR — the corpus is consumed by the in-tree
`verify_inbound_card` function which is a pure synchronous helper.
The function is already wired into `routes/a2a.rs` (PR 27) where
the eventual `OAuth 2.1 verifier` tower layer will gate the route.
This corpus provides a behavioural floor for that integration.

## Secret + key custody

Tests use deterministic Ed25519 seeds (single-byte fill, e.g.
`[7u8; 32]`) — never used in production. No secrets land in repo.

## Egress surface delta

None — pure unit/integration tests.

## Audit events emitted

None — verifier is invoked synchronously and reports its verdict via
`Result`. Audit emission is the caller's concern (will land in a
follow-up that wires `routes/a2a.rs` into `main.rs` behind the OAuth
layer with `AuditSink::append_with_dedup`).

## Failure mode

Every scenario asserts that an unhappy input produces a *typed*
rejection rather than a panic, an `unwrap()` failure, or a partially-
populated `VerifiedCallerIdentity`. A failing test in this corpus is
a hard CI block.

The `coverage_floor` test fails the build if a future PR adds a new
`CardVerifyError` variant without adding a corresponding scenario.

## Negative-test coverage

The corpus *is* the negative-test coverage. 13 of 15 scenarios are
rejection cases; 2 are Allow cases (happy path + URL-prefix match).

## Vendored / third-party dependency delta

None.

## Verification

- `cargo test --package azureclaw-inference-router --test a2a_card_verifier_conformance`
  → 15 passed.
- `cargo test --package azureclaw-inference-router --lib` → 491 passed
  (unchanged).
- `cargo clippy --package azureclaw-inference-router --all-targets -- -D warnings`
  clean.

## Sources consulted

- `inference-router/src/a2a/card_verifier.rs` (verifier implementation).
- `inference-router/src/a2a/card_signing.rs` (signing helper used to
  build the realistic test cards).
- `inference-router/tests/ap2_conformance.rs` (precedent corpus pattern;
  this PR mirrors its structure for symmetry).
- A2A 1.0.0 spec §4.4
  (<https://a2a-protocol.org/v1.0.0/specification#44-agent-discovery-objects>)
  — confirms `protocolVersion`, `validFrom`, `validUntil`, signature
  envelope shape.

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
