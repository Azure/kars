# 2026-04-29 — Phase 2 / S15.h — `inference-router/src/routes/handoff/mod.rs` hotspot pass

## Scope

Closes the last §4.2 Phase 2 cap. Extracts the largest handler in
`inference-router/src/routes/handoff/mod.rs` (the
`handoff_succession` body, ~209 LOC) to a sibling module so the
parent file lands under the 800-LOC budget.

## Existing implementation surveyed

- `inference-router/src/routes/handoff/mod.rs` (870 LOC pre-slice)
  hosts the full set of `/agt/handoff/...` route handlers plus the
  `spawn_routes()` / `handoff_*_routes()` builders. The
  `handoff_succession` handler (formerly L374–582) was the dominant
  contributor at ~209 LOC: it parses the request body, looks up the
  parent AMID via `lookup_parent_amid`, signs the canonical
  succession message via `state.signing_provider`, and forwards the
  signed envelope to the registry. The body is self-contained — no
  closure-captured state from any other handler.
- `lookup_parent_amid` already lives in
  `inference-router/src/routes/mesh.rs`. `handoff_event` already
  lives in `inference-router/src/routes/audit_events.rs`. Both are
  imported via `crate::routes::...` paths in the new sibling.
- The S15.c / S15.d / S15.e / S15.f hotspot pattern of "lift the
  largest unit, keep bodies byte-identical, only adjust visibility"
  is reused exactly.

## LOC delta

| File | Before | After | Δ |
|---|---|---|---|
| `inference-router/src/routes/handoff/mod.rs` | 870 | **658** | −212 |
| `inference-router/src/routes/handoff/succession.rs` (new) | 0 | 232 | +232 |

§4.2 Phase 2 cap = 800 LOC. **mod.rs is 142 LOC under cap.** This
closes the final §4.2 hotspot.

## Verification

- `cargo build -p azureclaw-inference-router` — clean.
- `cargo clippy -p azureclaw-inference-router --all-targets -- -D warnings` — clean.
- `cargo test -p azureclaw-inference-router --lib` — 608 passed / 0
  failed (unchanged baseline).

## Risk + rollback

- Risk: very low. Function body is byte-identical to the previous
  inline version. Only the visibility marker `pub(super)` and the
  module-relative import paths (`crate::routes::audit_events::...` /
  `crate::routes::mesh::...` rather than `super::audit_events::...`)
  changed. The route registration in `handoff_protected_routes()`
  still resolves through the `use succession::handoff_succession;`
  re-export.
- Rollback: revert this PR. mod.rs returns to 870 LOC and the new
  module disappears.

## Sign-offs

- Implementer: Copilot CLI agent (S15.h ship).
- Reviewer: pending PR review.
