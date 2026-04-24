# Security audit — Phase 1 · handoff/pending submodule extraction

Audit ID: `2026-04-24-phase1-hotspot-handoff-pending`
Scope reference: `docs/implementation-plan.md` §4.1 target module
layout, §9.9.9 (confirmation gate security model).

## What landed

1. **`inference-router/src/handoff/pending.rs`** (NEW) — extracts
   `PendingHandoffStore`, `PendingHandoffError`, `PendingHandoffStatus`,
   and their private support types (`PendingHandoffInner`,
   `PendingHandoff`) from the monolith. 400 LOC including 6 tests
   (`create_and_confirm`, `wrong_token`, `confirm_after_delay`,
   `rate_limit`, `cancel`, `no_pending`).
2. **`inference-router/src/handoff/mod.rs`**:
   * `mod pending; pub use pending::{PendingHandoffError,
     PendingHandoffStatus, PendingHandoffStore};` — external crates
     still import via `crate::handoff::PendingHandoffStore`.
   * Private constant `CONFIRMATION_TOKEN_HEX_LEN` removed from mod.rs
     (now lives in pending.rs, scope-private where it belongs).
   * 2567 → 2206 LOC (−361; under phase0_cap 2600 and, critically,
     within ~400 of the phase1_cap 1800).

No other files touched. All external reference sites — `routes/mod.rs`
(`use crate::handoff::{... PendingHandoffStore};`), `routes/handoff.rs`
test fixtures, etc. — resolve identically via the public re-export.

## STRIDE

| Category | Applies | Note |
|---|---|---|
| Spoofing | N/A | No auth surface changed. Token generation, comparison, and TTL semantics are unchanged. |
| **Tampering** | Positive | Code that defends against LLM self-confirm lives in a focused, ~330-line file now, easier to audit in isolation. The confirmation-token generation still uses `rand::rng().fill(...)` (same call path as before); the comparison still routes through `super::constant_time_eq`. |
| Repudiation | N/A | Metrics calls (`HANDOFF_PENDING_EVENTS` counters for `created`, `rate_limited`, `expired`, `too_fast`, `invalid_token`, `confirmed`, `no_pending`, `cancelled`) are preserved byte-for-byte. |
| **Information Disclosure** | N/A | Public re-exports match the prior surface exactly. No new field or method became visible. |
| **Denial of Service** | N/A | Rate-limiting semantics unchanged: `HANDOFF_REQUEST_COOLDOWN_SECS` is still the sole gate, still read from the parent module's constant. |
| **Elevation of Privilege** | N/A | No caller gains or loses privilege. |

## Principle mapping

* §0.2 #1 (zero regressions) — `cargo test --package
  azureclaw-inference-router` 210 tests pass. The six PendingHandoff
  tests migrated verbatim (bodies identical; only the `mod`
  surrounding them changed) and still exercise the same code paths
  via the same public API.
* §0.2 #4 (LOC) — `handoff/mod.rs` shrinks by 361 LOC to 2206. The new
  `pending.rs` is 400 LOC, well below the new-file 800 cap. The
  aggregate is slightly less than before (small savings from folding
  the section header / doc comments / imports only once).
* §0.2 #8 (solid, not look-alike) — literal extraction. `rand::Rng`
  and `serde::Serialize` moved into `pending.rs`. `Instant`, `Duration`,
  `Arc`, `RwLock` also re-imported locally. No signatures changed;
  `Default`/`new` semantics identical. The `CONFIRMATION_TOKEN_HEX_LEN`
  constant was duplicated rather than exposed via `pub(super)` — the
  value is scope-private (it is a pending-store detail, not a
  handoff-wide constant), and publishing it across module boundaries
  for the sake of a refactor would have created a weaker invariant
  than leaving it file-scoped.
* §0.2 #9 — this document.
* §0.2 #10 — `rand::rng().fill(&mut bytes)` is the current-API call
  shape of `rand` 0.9 (crate was upgraded in an earlier PR; verified
  against the `rand` 0.9.1 docs before porting).

## What was **not** done (deliberate)

* **HandoffTokenStore** (the companion store at mod.rs lines ~208-331)
  stayed in mod.rs. It's a smaller, less-critical store and will move
  in a later pass when the handoff mod.rs is close to its phase1
  target (1800). Splitting "something that works" for its own sake
  risks the exact "pseudo-improvements" forbidden by §0.2 #8 — this
  PR pulled out the store whose standalone file size (330 LOC) plus
  its tests (110 LOC) carry meaningful security content.
* **The crypto codec block** (encrypt_state / decrypt_state / …) stays
  in mod.rs under the existing `ci/no-custom-crypto.sh` allowlist for
  `inference-router/src/handoff/mod.rs`. Extraction into a provider
  wrapper is tracked in plan §1.2.
* **HandoffSession / HandoffPhase / SnapshotItemCounts** — the
  state-machine tracker near lines 580-800 of mod.rs — deferred; its
  usage crosses routes + metrics + middleware and warrants its own
  audit doc when extracted.

## Re-audit triggers

* Change to `rand`-crate major version — re-verify the `.fill()`
  signature and entropy source.
* Change to `CONFIRMATION_MIN_DELAY_SECS`, `PENDING_HANDOFF_TTL_SECS`,
  or `HANDOFF_REQUEST_COOLDOWN_SECS` — these remain in the parent
  `mod.rs`; touching any invalidates the "LLM self-confirm" threat
  model assumption (§9.9.9).
* Any new caller of `PendingHandoffStore::confirm` outside
  `routes/handoff.rs` — re-audit to make sure constant-time
  comparison stays on the privileged path.

## Verification

* `cargo build --package azureclaw-inference-router`: clean.
* `cargo test --package azureclaw-inference-router`: 210 tests pass
  (166 unit + 15 integration + 26 governance + 3 proxy). The six
  pending-handoff tests run in `handoff::pending::tests` rather than
  `handoff::tests`; all pass.
* `cargo clippy --all-targets --all-features -- -D warnings`: clean.
* `handoff/mod.rs` 2206 ≤ phase0_cap 2600, well under baseline 2626.
* Six CI gates: PASS.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
