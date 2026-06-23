# Security Audit — operator TUI: drop `blessed-contrib` (npm-audit clean)

PR: Azure/kars (branch `fix/cli-npm-audit-drop-blessed-contrib`)

## Scope

The capability-path change is in `cli/src/commands/operator.ts` plus a new
helper `cli/src/lib/operator-tui.ts`. It removes the `blessed-contrib`
dependency from `@kars-runtime/cli` and reimplements the only three widgets the
operator dashboard used (`grid`, `table`, `log`) on top of plain `blessed`:

- `makeGrid(screen, 12, 12)` reproduces `contrib.grid`'s percentage-based cell
  geometry (and default line border).
- `makeTable(opts)` reproduces `contrib.table` as a bordered `blessed.box`
  containing a fixed header line + a scrollable `blessed.list` exposed as
  `.rows` — the exact surface `operator.ts` consumes
  (`setData({headers,data})`, `.rows.selected/.select/.focus`, `.show/.hide`,
  `.style.border.fg`).
- The activity log switches to `blessed.log` (`.log()` is identical).

Motivation: `blessed-contrib` pulls vulnerable transitive dependencies with no
consumer-applicable fix — `lodash` (GHSA-r5fr-rjxr-66jc code injection via
`_.template`; GHSA-f23m-r3pf-42rh prototype pollution) and `xml2js` via
`map-canvas` (GHSA-776f-qx25-q3cc prototype pollution). npm `overrides` in a
published package are ignored when it is installed as a dependency, so the only
durable remedy is to stop depending on `blessed-contrib`. `blessed` alone has
zero advisories. Verified: a consumer `npm install` of the packed tarball now
reports `found 0 vulnerabilities` (was 5: 3 high + 2 moderate).

Non-capability changes in the same PR (noted, not the audit subject): CLI
version bump 0.1.1 → 0.1.2; `tools/cleanup-ghcr-tags.sh` keeps clean-release
per-arch tags; `docs/security/supply-chain-posture.md` records the npm-audit
result and the reviewed-benign Socket.dev `execa` "obfuscated code" alert.

## Threat model

### T1: New TUI widget code — does it introduce a new attack surface? (NO)
`operator-tui.ts` is pure local-terminal rendering. It performs no I/O, no
process spawning, no network, and no filesystem access — it constructs
`blessed` widgets and formats fixed-width text. It runs only inside
`kars operator`, an interactive dashboard the user already invokes against
clusters they are authenticated to. No new privilege, capability, or data path
is introduced; the change is strictly a dependency-surface *reduction*.

### T2: Table cell rendering — injection / escaping? (NO REGRESSION)
The agent-table cells are plain operator-derived strings (sandbox name, status,
runtime, model, age, cluster) — the same data `contrib.table` rendered before.
`blessed` tag markup (`{...}`) is only enabled where it already was; cell
values are not attacker-controlled (they come from the user's own cluster /
CRDs) and are pad/truncated, not evaluated. Selection math preserves the prior
`.rows.selected` semantics (data-row indexed, header excluded), verified by a
headless render smoke-test exercising `setData`, `select`, and `style` mutation.

### T3: Removing a dependency — supply-chain effect (STRICT IMPROVEMENT)
Dropping `blessed-contrib` removes `lodash`, `map-canvas`, and `xml2js` from the
installed tree, taking the published CLI from 5 advisories to 0 and shrinking
the dependency count. No functionality the operator used is lost (grid/table/log
are reimplemented; the unused `map`/chart widgets are gone with their deps).

## What this audit does NOT cover

- The broader operator command logic (sandbox listing, egress approve/deny,
  model switch) — unchanged by this PR and covered by prior operator audits.
- The `kars dev --release --target local-k8s` image-load path — tracked
  separately.

## Verdict

Accept. The change reduces the published CLI's supply-chain surface to a clean
`npm audit`, introduces no new runtime capability, network path, or
attacker-reachable input, and preserves the dashboard's behaviour (verified by
the full 801-test suite + a headless TUI smoke-test).

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
