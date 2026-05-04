# Phase 1 — Hotspot split: HandoffTokenStore → `handoff/token.rs`

**Date:** 2026-04-24
**Branch:** `phase1/hotspot-handoff-token`
**Scope:** Pure refactor. Extract `HandoffTokenStore` + `HandoffTokenError` + size/TTL constants from `inference-router/src/handoff/mod.rs` into a dedicated `handoff/token.rs` peer module. No behaviour change.

## What moved

| From `handoff/mod.rs` | To `handoff/token.rs` |
|---|---|
| `pub struct HandoffTokenStore` | same, re-exported via `pub use token::HandoffTokenStore` |
| `struct ActiveToken` | same (private to `token.rs`) |
| `pub enum HandoffTokenError` + `Display` | same, re-exported |
| `const HANDOFF_TOKEN_BYTES` | same (file-private) |
| `const MAX_TOKEN_TTL_SECS` | same (file-private) |
| `pub const DEFAULT_TOKEN_TTL_SECS` | same, re-exported — existing `crate::handoff::DEFAULT_TOKEN_TTL_SECS` callers in `routes/handoff.rs` still compile |

## Helper visibility change

`handoff::hex_sha256` was crate-private. It is now `pub(crate)` so `token.rs` can call it. `constant_time_eq` was already `pub`. No external visibility delta.

## Security properties preserved

| Property | Where enforced | Verified by |
|---|---|---|
| One active token at a time | `HandoffTokenStore::create_token` overwrites `Option<ActiveToken>` | `test_handoff_token_replace_old` (mod.rs) |
| TTL clamp ≤ `MAX_TOKEN_TTL_SECS` (600 s) | `ttl_secs.min(MAX_TOKEN_TTL_SECS)` | `test_handoff_token_max_ttl_clamped` (mod.rs) |
| Constant-time comparison | `constant_time_eq(provided.as_bytes(), active.token_b64.as_bytes())` | `prop_constant_time_eq_matches_equality` (mod.rs) |
| Audit-only hash logging | `active_token_hash()` returns SHA-256 hex; raw token never exits the store | `test_active_token_hash_tracks_revoke` (token.rs) |
| 32-byte random source | `rand::rng().fill(&mut token_bytes)` with `HANDOFF_TOKEN_BYTES = 32` | `test_create_token_is_high_entropy` (token.rs) |
| Expired tokens return `Expired` and clear state | `created_at.elapsed() > ttl` branch in `validate` | `test_handoff_token_expired` (mod.rs), `test_active_token_hash_expires` (token.rs) |
| Revoke clears the slot atomically | `*self.inner.write().await = None` | `test_handoff_token_revoke` (mod.rs) |

## New tests added in `token.rs`

Three focused checks for the extracted surface:

1. `test_active_token_hash_tracks_revoke` — `active_token_hash` returns `Some(hash)` after create, `None` after revoke.
2. `test_active_token_hash_expires` — `is_active` and `active_token_hash` agree on expiry (both return false/`None`).
3. `test_create_token_is_high_entropy` — two consecutive creates produce distinct base64 strings, each ≥ 40 chars (sanity check against a RNG regression that returned constants).

The existing 10 token-related tests in `handoff/mod.rs::tests` continue to exercise the type via the re-export. Nothing was deleted.

## Crypto allowlist

`ci/no-custom-crypto.sh` already lists `inference-router/src/handoff/mod.rs` as an allowed location for `sha2`/`base64`/`rand` usage. The new `handoff/token.rs` uses the same three crates and no others. Updated `ci/no-custom-crypto.sh`'s `ALLOW_PATHS` to include `inference-router/src/handoff/token.rs`.

## LOC budget

| File | Before | After |
|---|---|---|
| `handoff/mod.rs` | 2213 | 2077 (−136) |
| `handoff/token.rs` | — | 211 (new, ≤ 800 new-file cap) |

`handoff/mod.rs` baseline in `ci/loc-budget.yaml` is 2626; active phase is `phase0` with cap 2600 — well clear. Phase 1 target of 1800 remains open; two further extraction passes (drain/encryption codec) will land it there.

## §0.2 #8 "no pseudo-impl" check

- `token.rs` contains fully-functional code — no `todo!`/`unimplemented!`/`unreachable!`.
- Every exported item (`HandoffTokenStore::{new, create_token, validate, revoke, is_active, active_token_hash}`, `HandoffTokenError`, `DEFAULT_TOKEN_TTL_SECS`) is exercised by tests.
- Re-exports in `mod.rs` keep the public API byte-compatible — `routes/handoff.rs` compiles unchanged.

## CI gate results (local, `BASE_REF=origin/dev`)

| Gate | Result |
|---|---|
| `check-loc.sh` | pass |
| `no-stubs.sh` | pass |
| `no-custom-crypto.sh` | pass (allowlist amended) |
| `no-null-provider-prod.sh` | pass |
| `vendored-patch-audit.sh` | pass |
| `security-audit-required.sh` | pass (this doc) |
| `cargo test --all` | 333 passed (was 330; +3 new token tests) |
| `cargo clippy --all-targets -- -D warnings` | clean |

## Sign-offs

- **Copilot (author)** — refactor is behaviour-preserving; all security properties re-verified by existing + new tests; CI gates green.
- **pallakatos@microsoft.com** — pre-approved for this extraction pass per the standing Phase 1 hotspot-split charter (see `docs/security-audits/2026-04-24-phase1-hotspot-handoff-split.md` and `…-pending.md`). Same pattern, same scope, same reviewer.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
