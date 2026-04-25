# 2026-04-25 — phase1/routes-handoff-payload-extract

## Summary

Pure refactor. Splits `inference-router/src/routes/handoff.rs` into a
module:

```
inference-router/src/routes/handoff/
    mod.rs        # router builders + lifecycle handlers
    payload.rs    # snapshot / restore / verify (the payload-heavy
                  #   handlers extracted in this PR)
```

The three extracted handlers (`handoff_snapshot`, `handoff_restore`,
`handoff_verify`) are visibility-promoted to `pub(super)`; routing in
`spawn_routes()` now dispatches via `payload::handoff_snapshot`,
`payload::handoff_restore`, `payload::handoff_verify`. Function bodies
are byte-identical to the originals.

These three handlers carry the bulk of the HTTP-side handoff logic
that touches the encrypted state blob: AES-GCM decryption, snapshot
build, verification-hash compute, and the trust-/audit-/sub-agent
restoration loop. Isolating them from the lifecycle handlers
(drain/decommission/abort/succession/resume/pending/confirm/...) is a
Phase 1 §4.2 hotspot decomposition step.

## Threat model delta

None. No behaviour change. Same routes, same wire shapes, same
encryption path, same audit events. The change is purely module
boundaries: the payload-handling logic now lives in a sibling file
that imports from `crate::routes::audit_events` and
`crate::routes::mesh` instead of relying on parent-module `use`
re-exports.

## OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM02 (Insecure Output Handling) /
  LLM06 (Sensitive Information Disclosure):** unchanged. The handoff
  payload still carries serialized agent state (sanitised chat
  snapshot, trust scores, audit-receipt ids — never raw PII), still
  encrypted with the existing AES-256-GCM cipher implemented in
  `inference-router/src/handoff/crypto.rs` (PR 41).
- **OWASP MCP Top 10 — MCP-08 (Excessive Agency):** restore handler
  unchanged; sub-agent re-spawning still gated by trust + AGT
  policy decisions; the auth middleware (PR 42) is unchanged and
  still applied via `handoff_protected_routes()`.

## AuthN / AuthZ path

Unchanged. The protected-routes builder still wraps these three
handlers in `crate::handoff::auth::handoff_auth_middleware` (the
HMAC bearer-token check extracted to its own file in PR 42). No new
endpoints, no scope changes.

## Secret + key custody

Unchanged. Handoff secret material continues to live in the in-process
`HandoffTokenStore` and `HandoffSession`; the AES-GCM data key is
still derived per-handoff via HKDF-SHA256 from the shared secret in
`crate::handoff::crypto`.

## Egress surface delta

None. Router-internal refactor.

## Audit events

Unchanged. `handoff_event(...)` and `audit_handoff_init(...)` are
still emitted from the moved handlers; `handoff_init` is still
emitted from the lifecycle code in `mod.rs`.

## Failure mode

Unchanged. All three handlers retain their existing fail-closed
posture — `errors::flat(...)` returns 4xx/5xx on transition
violations, oversize blobs, decryption failures, sub-agent restore
failures, etc.

## Negative-test coverage

No new tests in this PR — pure refactor. The existing **595 lib
tests** continue to pass, including the handoff suite that covers
the encrypted blob round-trip, oversize-blob rejection, replay
rejection (via `pending::tests`), wrong-token rejection, and the
state-machine transition gates. Clippy clean.

The trust/restore happy-path is also exercised by the e2e integration
tests; those will run in the next merge train against `dev`.

## Vendored / third-party dependency delta

None. No new crates, no version bumps.

## Sign-offs

- Capability author: Pal Lakatos-Toth — `Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>`
- Independent reviewer: Pal Lakatos-Toth (single-reviewer carry-over per
  Phase 1 hotspot-pass2 governance) — `Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>`

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
