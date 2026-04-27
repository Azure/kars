# Security audit — Phase 1 handoff auth middleware extraction

**Capability:** `inference_router::handoff::auth` — extraction of the
three handoff-endpoint auth middleware functions
(`handoff_auth_middleware`, `handoff_init_auth_middleware`,
`handoff_status_auth_middleware`) from `handoff::mod` into their own
submodule. Phase 1 hotspot decomposition (plan §4.2 / §7 item 8).

**Branch:** `phase1/handoff-auth-extract`
**Date:** 2026-04-25

## 1. Summary

Pure refactor — no behaviour change. The three middleware functions
move out of `inference-router/src/handoff/mod.rs` (1954 → 1770 LOC,
crossing under the Phase 1 1800-LOC cap) into a new
`inference-router/src/handoff/auth.rs` (207 LOC). The functions are
re-exported from `crate::handoff` so every call-site in
`crate::routes::*` and the binary keeps compiling unchanged. The
constant-time-equal helper `super::constant_time_eq` is reused (still
in `mod.rs` because it is also called by `routes.rs` and `main.rs`).

## 2. Threat model delta

None. No new entry point, no relaxed check, no new bypass. The
critical security invariants of the three endpoints — admin token
required, no localhost bypass on read-write paths, two-token check on
the full handoff path, prompt-injection mitigation by keeping the
handoff token out of agent-process memory — are byte-identical to the
in-`mod.rs` version.

## 3. OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM01 (Prompt Injection):** the comment in
  `handoff_auth_middleware` documenting why **no localhost bypass** is
  permitted on the full path is preserved verbatim. The agent
  (UID 1000) cannot bypass auth by issuing localhost calls because the
  handoff token never enters its process memory.
- **OWASP MCP Top 10 — A02 (Broken Authentication / Token Theft):**
  the validation goes through
  `state.handoff_tokens.validate(token).await` (the in-memory
  `HandoffTokenStore`). The store still owns lifetimes and
  TTL-expiration. Status middleware still allows localhost bypass
  because read-only status is safe; this asymmetry is documented in
  the function-level rustdoc.

## 4. AuthN / AuthZ path

Unchanged.
- Full path: `Authorization: Bearer <admin>` + `X-Handoff-Token: <t>`.
- Init path: `Authorization: Bearer <admin>` only (the handoff token
  is created by this endpoint).
- Status path: localhost ⇒ allow. Otherwise admin token required.
- All three use [`super::constant_time_eq`] for the admin-token
  comparison.

## 5. Secret + key custody

Unchanged. No keys live in the new module — it dereferences
`AppState::admin_token` and `AppState::handoff_tokens` and never
clones either onto a long-lived heap object.

## 6. Egress surface delta

Zero.

## 7. Audit events emitted

Identical `tracing::warn!` / `tracing::info!` / `tracing::error!`
events with the same field names (`path`, `token_hash`, `error`).
Operators consuming the structured logs see no change.

## 8. Failure mode

**Fail-closed.** Every error path returns the same status code and
body as before. No new fall-through.

## 9. Negative-test coverage

Existing handoff suite covers wrong-admin-token, missing-admin-token,
missing-handoff-token, wrong-handoff-token, expired-handoff-token,
localhost-bypass-blocked-on-full-path, localhost-bypass-allowed-on-
status. All 585 lib tests pass pre + post extraction.

## 10. Vendored / third-party dependency delta

None. Same `axum` middleware shape; just moved across files.

## 11. Sign-offs

Plan §4.2 / §7 item 8 — handoff/mod.rs Phase 1 cap is 1800 LOC; this
PR brings it to 1770 (combined with PR 41's crypto extraction:
2075 → 1770, **−305 LOC**). All 6 CI gates green; clippy clean; lib
tests at 585.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
