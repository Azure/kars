# Operator TUI — Surface Shift-P Panels Hint in Status Bar

- **Date:** 2026-04-30
- **Slice:** post-Phase 2 polish (PR #150)
- **Author:** Phase 2 train

## Scope

S14 (`phase2-tui-redesign`, PR #119) added a Shift-P toggle that opens a
modular per-CRD panels overlay (`cli/src/commands/operator/panels_overlay.ts`).
The handler is wired in `operator.ts:653` and the panels render data from a
pluggable `ClusterDataSource`. The status bar emitted by
`cli/src/commands/operator/keymap.ts` advertised every other key binding
(Tab / Enter / c / t / a / d / e / g / n / r / q / …) but **not** Shift-P,
so users could not discover the overlay without reading source.

This change adds the missing hint.

## Diff summary

`cli/src/commands/operator/keymap.ts` — pure data + status-bar string update:

1. Append `{ key: "Shift-p", action: "toggle CRD panels overlay (S14)", scope: "global" }`
   to the `BINDINGS` table.
2. Append `[P] Panels` to the `AGENTS_ACTIONS` constant (consumed by the
   agents-view status bar).
3. Append `[P] Panels` to the `statusBarForTopology` and `statusBarForCluster`
   strings.

No new key handler is added. The Shift-P binding (`screen.key(["S-p"], …)`)
already existed in `cli/src/commands/operator.ts:653` since S14. No I/O,
no new privilege, no network, no new secret, no provider trait change.

## Threat model delta

**None.** The keymap module is pure data — no `blessed`, no `commander`,
no I/O. The status bar prints a help hint; it does not enable a capability.
The capability (Shift-P toggling the panels overlay) was introduced and
audited in S14 (`docs/security-audits/2026-04-30-phase2-tui-redesign.md`).

## CI gate considerations

`ci/security-audit-required.sh` flags `cli/src/commands/operator/keymap.ts`
as a "capability-introducing" path because the keymap is the canonical
source of truth for TUI input handling. This audit document discharges
the gate for this PR. The change is informational only.

## Verification

| Gate | Result |
|---|---|
| `npx vitest run cli/src/commands/operator/keymap.test.ts` | ✓ 6/6 |
| `bash ci/check-copyright-headers.sh` | ✓ |
| `BASE_REF=origin/main bash ci/check-loc.sh` | ✓ |
| `bash ci/no-stubs.sh` | ✓ |

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
