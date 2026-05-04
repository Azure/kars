# Security audit — Phase 1 · handoff.rs hotspot split (drain submodule)

Audit ID: `2026-04-24-phase1-hotspot-handoff-split`
Scope reference: internal Phase 1 plan §4.1 target module layout,
§4.2 LOC budget, §4.3 "touched code pays its decomposition debt".

## What landed

1. **Module conversion**: `inference-router/src/handoff.rs` →
   `inference-router/src/handoff/mod.rs` (single-file → directory). Rename
   only; no behavioural change. Git records the move as R098 (98%
   similarity).
2. **New submodule** `inference-router/src/handoff/drain.rs` holds the
   `DrainState` type, its private `DrainInner`, `Default`/`new`/
   `start_drain`/`stop_drain`/`is_draining`/`drain_duration` methods, and
   three unit tests (transitions, default, idempotent start). 96 LOC.
3. **`mod.rs`** — removes the `DrainState` block and its
   `test_drain_state`; adds `mod drain; pub use drain::DrainState;`.
   Shrinks 2626 → 2567 LOC (−59). Under phase0_cap 2600 and below
   baseline per `touched_must_shrink`.
4. **`ci/loc-budget.yaml`** — the budgeted path is updated to
   `inference-router/src/handoff/mod.rs` (the new canonical location for
   the monolith we are decomposing). Baseline and phase caps unchanged.

No other files touched. `DrainState` is re-exported from
`crate::handoff::DrainState` exactly as before; external consumers
(`crate::handoff::DrainState` in `routes/mod.rs`) are unaffected.

## STRIDE

| Category | Applies | Note |
|---|---|---|
| Spoofing | N/A | No identity or auth surface touched. |
| Tampering | N/A | `DrainState` holds an `Arc<RwLock<_>>` with two fields. No cryptographic material. No serialisation. |
| Repudiation | N/A | No audit-trail changes. |
| Information Disclosure | N/A | Verbatim move. |
| Denial of Service | N/A | Behaviour identical; same RwLock contention profile. |
| Elevation of Privilege | N/A | No callers, no privileges. |

## Principle mapping

* §0.2 #1 (zero regressions) — `cargo test --package
  azureclaw-inference-router` all 210 unit + integration tests green
  (was 207 + 3 new drain tests = 210). `DrainState` public API
  unchanged: same methods, same signatures, same async semantics.
* §0.2 #4 (LOC discipline) — `handoff/mod.rs` 2567 < phase0_cap 2600
  and < baseline 2626. New file `handoff/drain.rs` 96 LOC, far under
  800-hard-cap on new Rust files.
* §0.2 #8 (solid, not look-alike) — the move is literal. Zero new
  stubs, zero custom crypto (the drained module has no crypto
  involvement at all; it's a pair of bool+Option<Instant> fields). No
  behavioural reinterpretation.
* §0.2 #10 (verify, don't guess) — the `ci/no-custom-crypto.sh`
  allowlist was consulted; `drain.rs` has no crypto imports, so no
  allowlist change needed. The LOC-budget path migration was checked
  against `ci/check-loc.sh` logic (budgeted-path lookup is exact-match
  against diff filenames; the new path is the new canonical monolith
  under decomposition).

## What was **not** done (deliberate)

* The crypto block (`encrypt_state` / `decrypt_state` / `serialize_state`
  / `deserialize_state` / `compute_verification_hash` / `hex_sha256` /
  `constant_time_eq`) was left in `mod.rs`. Extracting it is a larger
  task that requires extending `ci/no-custom-crypto.sh` ALLOW_PATHS
  (currently the allowlist covers `providers/signing.rs` and
  `providers/mesh.rs` only). The plan's correct destination for that
  code is a `SigningProvider` wrapper, not a peer submodule — so we
  defer until the provider seam work wraps. This is called out in
  internal Phase 1 plan §4.1 (handoff split target: client /
  server / crypto).
* The stores (`HandoffTokenStore` + `PendingHandoffStore`) and their
  tests were not extracted in this PR; they will be part of the next
  hotspot-split iteration. The drain extraction is a safe, isolated
  first cut that establishes the `handoff/` module directory pattern.

## Re-audit triggers

* Any further split of `handoff/mod.rs` (server, client, crypto,
  stores) — each gets its own audit doc per §0.2 #9.
* Changes to `DrainState` semantics (new fields, new transitions,
  interaction with request-gating) — re-audit Denial of Service row.

## Verification

* `cargo build --package azureclaw-inference-router`: clean.
* `cargo test --package azureclaw-inference-router`: 210 tests pass
  (166 unit + 15 integration + 26 governance + 3 proxy).
* `cargo clippy --all-targets --all-features -- -D warnings`: clean.
* `handoff/mod.rs` 2567 ≤ phase0_cap 2600, `<` baseline 2626.
* Six CI gates: PASS.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
