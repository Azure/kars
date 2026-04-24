# Security audit — `azureclaw convert` CLI skeleton

**Date:** 2026-04-24
**Capability:** new CLI subcommand `azureclaw convert` (surface only; no conversion logic).
**Branch:** `phase0/kubectl-convert-skeleton`
**Plan section:** `docs/implementation-plan.md` §2.2 + §6 item 13

## 1. Summary

Add a new Commander subcommand with fixed `--to` / `--file` /
`--sandbox-ref` / `--dry-run` / `--allow-lossy` surface. All invocations
exit with code 3 ("not yet implemented") and an explanatory stderr
message pointing to the Phase 2 mapping doc. Argument parsing rejects
unknown `--to` targets (exit 2) and missing `--sandbox-ref` on overlay
mode (exit 2).

Files:

- `cli/src/commands/convert.ts` — the subcommand (64 LOC of handler,
  within all budgets).
- `cli/src/commands/convert.test.ts` — 3 unit tests on the exported
  `parseTarget` helper.
- `cli/src/cli.ts` — one import + one `addCommand` call.

## 2. Threat model delta

| STRIDE | Applies? | Notes |
|---|---|---|
| Spoofing | No | No network, no auth, no identity surface. |
| Tampering | No | Reads argv only; does not touch files or the cluster. |
| Repudiation | No | |
| Information disclosure | Low | Echoes user-supplied `--file` path to stderr; no file read. |
| DoS | No | Argument parse is O(1). |
| Elevation of privilege | No | Pure local exit-3. |

**OWASP LLM Top 10:** N/A.
**OWASP MCP Top 10:** N/A.

## 3. AuthN / AuthZ

None. No K8s API call, no cluster connectivity, no credential read.

## 4. Secret / key custody

None.

## 5. Egress delta

None.

## 6. Audit events

None.

## 7. Failure mode

- Invalid `--to` → exit 2 with clear stderr.
- Missing `--sandbox-ref` on overlay → exit 2.
- Every successful parse → exit 3 ("Phase 2 deliverable").
- No path produces exit 0 — prevents scripts from silently depending on
  a not-yet-implemented conversion (principle §0.2 #8). When the real
  translator lands in Phase 2 the exit contract will be documented
  explicitly in its audit doc.

## 8. Negative-test coverage

- `convert.test.ts::rejects unknown targets` — `"yaml"`, `"native"`, `""` all return `undefined`.
- `convert.test.ts::rejects undefined target` — no default past the commander layer.
- Smoke test: `node dist/index.js convert --to bogus -f x.yaml` → exit 2, stderr shows error.
- Smoke test: `node dist/index.js convert --to overlay -f x.yaml` → exit 2, stderr demands `--sandbox-ref`.

## 9. Dependency delta

None. Uses existing `commander` and `chalk` deps.

## 10. Internal-boundary posture

Consume-only: the command does not ship conversion logic, so it cannot
conflict with any MSFT product surface. Phase 2 real-conversion audit
will re-evaluate against `docs/internal-boundaries.md`.

## 11. Sign-offs

- Author: `Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>`
- Reviewer sign-off: pending user review per local-only workflow.

### Re-audit triggers

- Phase 2 wires real YAML parsing, file I/O, and CR emission.
- The command gains `--apply` or any cluster-writing flag.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
