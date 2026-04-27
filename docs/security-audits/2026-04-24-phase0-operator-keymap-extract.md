# Security audit — Operator TUI keymap extraction (Phase 0)

**Date:** 2026-04-24
**Capability:** pure-data extraction of operator-TUI key bindings + status-bar copy into `cli/src/commands/operator/keymap.ts`.
**Branch:** `phase0/operator-tui-keymap-extract`
**Plan section:** internal Phase 1 plan §4.2 (monotonic-decrease LOC budget) + §6 item 12 (Phase 0 decomposition slice #2 — first operator.ts shrink).

## 1. Summary

Extract the status-bar copy for the three view modes (agents / topology /
cluster) and the canonical key-bindings table from `operator.ts` into a
new `cli/src/commands/operator/keymap.ts` module. No runtime behaviour
change: the 3 `statusBar.setContent(...)` calls now call
`statusBarForAgents|Topology|Cluster(...)` which return byte-identical
strings to the originals (verified by new unit tests asserting the
exact tag-markup substrings). `operator.ts` shrinks 2932 → 2894,
crossing the Phase 0 LOC cap of 2900.

## 2. Threat model delta

None. Pure refactor. STRIDE surface unchanged. No new file I/O, no new
network call, no new spawn / exec, no new CR mutation, no new secret
access.

## 3. AuthN / AuthZ, secret custody, egress

No change.

## 4. Audit events

No change — operator TUI still emits the same `kubectl` verbs it did
before.

## 5. Failure mode

- `keymap.ts` has zero external deps (no `blessed`, no `commander`,
  no runtime side-effects on import). A typo in a status-bar string
  manifests as a compile error (TypeScript literal types) or a unit-test
  diff.
- If `operator/keymap.ts` fails to resolve at runtime, the whole
  `operator` subcommand fails on import — a loud, deterministic error,
  not a silent behaviour change.

## 6. Negative-test coverage

- `keymap.test.ts::BINDINGS has all expected keys` — guards against
  accidental removal of `Tab`, `↑/↓ j/k`, `a`, `d`, `q / Esc`, `Enter`.
- `keymap.test.ts::every binding has a non-empty action and known
  scope` — catches empty strings and scope typos.
- `keymap.test.ts::agents view with agents focus highlights [Agents]`
  + three siblings — assert exact blessed-markup contents that the
  original inline code produced.
- Compat suite `operator-tui.spec.ts`: 11 passed / 8 todo (unchanged
  from pre-extraction baseline).

## 7. Dependency delta

None. No new npm package; no new Rust crate.

## 8. Internal-boundary posture

N/A — internal refactor.

## 9. LOC budget impact

| File | Before | After | Phase 0 cap | Slack |
|---|---|---|---|---|
| `cli/src/commands/operator.ts` | 2932 | 2894 | 2900 | 6 |
| `cli/src/commands/operator/keymap.ts` | — | 88 | 800 (new-file hard) | 712 |
| `cli/src/commands/operator/keymap.test.ts` | — | 57 | (tests exempt) | — |

## 10. Sign-offs

- Author: `Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>`
- Reviewer sign-off: pending user review per local-only workflow.

### Re-audit triggers

- A bindings change that alters behaviour (not just labels).
- New status-bar view mode (e.g., the MCP / ToolPolicy panels planned
  for Phase 1) — adds new functions in keymap.ts and new tests here.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
