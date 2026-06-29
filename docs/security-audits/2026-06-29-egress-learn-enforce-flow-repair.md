# Security Audit — Egress learn/enforce flow repair (operator toggle + CLI approve/deny/enforce)

Date: 2026-06-29
Scope: `cli/src/commands/egress.ts`, `cli/src/commands/operator/actions.ts`, `cli/src/commands/egress.test.ts`, `tests/e2e-manual/scenarios/egress_lifecycle.sh`.
Gated paths: `cli/src/commands/egress.ts`, `cli/src/commands/operator/actions.ts`.

## Summary

Two operator-reported bugs in the egress learning/enforcement flow, both caused
by CLI/operator code still calling router endpoints that **Slice 5c.1 removed**
(`/egress/approve`, `/egress/deny`, `/egress/enforce`, `/egress/pending`). The
authoritative model is now: baseline allowlist = `KarsSandbox.spec.networkPolicy.
allowedEndpoints` compiled into a controller-published, cosign-verified bundle
(`allowlistRef`); temporary grants = `EgressApproval` CRs; `egressMode`
(Learn|Strict) drives the router via the `EGRESS_MODE` env var with a live
`POST /egress/learn {enabled}` toggle.

1. **Operator (TUI) could not move Strict → Learn.** `learnEgress` called the
   runtime `/egress/learn` probe FIRST, uncaught, so if it threw the authoritative
   CRD patch was skipped. Fix: patch the CRD `egressMode` first (mirrors the
   working `enforceEgress`), then a best-effort `{enabled:true}` probe in its own
   `.catch`. (The old call also sent no body, defaulting the router to
   `enabled:false`, i.e. it DISABLED learn even when it ran.) Added a symmetric
   best-effort `{enabled:false}` toggle to `enforceEgress`.

2. **`kars egress --approve/--deny/--enforce/--pending` + the default status view
   hit removed endpoints** (the reported `exit code 1`). Re-pointed to the real
   mechanisms:
   - `--approve <domain[:port]>` adds `host:port` (default **:443**) to the
     baseline `allowedEndpoints` and re-signs (sign-by-default).
   - `--deny <domain>` removes the host and re-signs. **`--deny` is now in the
     signing context** so a revocation actually updates the authoritative bundle
     (otherwise the old signed allowlist would keep serving the host — a fail-open
     revocation).
   - `--enforce` patches `egressMode=Strict` and signs the baseline.
   - `--pending` and the status view show learned-but-not-allowlisted domains.

## Security analysis

### T1: New capability / attack surface? (NO)
- No new endpoint/route/privilege. All mutations go through `kubectl patch`/`apply`
  against existing CRDs (`KarsSandbox`, `EgressApproval`) and the existing
  cosign-signing pipeline (`runSignFlow`). Reads are `kubectl get` + the existing
  in-pod router probe. Agents hold no new capability; this is operator tooling.

### T2: Security-control change? (STRENGTHENED / NEUTRAL)
- The signed-allowlist control is unchanged and now correctly driven from the CLI:
  - `--deny` re-signs by default → revocation is authoritative (was fail-open).
  - `runSignFlow` remains fail-CLOSED: `allowlistRef` is patched ONLY after a
    successful cosign sign; on any failure the previous signed bundle stays
    authoritative and the CLI warns + exits non-zero.
  - Port-less baseline entries are normalized to `:443` before signing so the
    signer (which requires a concrete port) cannot silently drop an existing
    allowed host from the re-signed bundle (data-loss / accidental-deny guard).
- `egressMode` is patched on the CRD (durable, authoritative); the live
  `/egress/learn` probe is now strictly best-effort and can never block or revert
  the CRD source of truth.

### T3: Availability / fail-open risk? (REDUCED)
- Operator learn toggle no longer wedges on a probe error. CLI approve/deny/enforce
  are self-healing: in a signing context they always re-sign the current baseline,
  so a prior run that patched the inline list but failed to sign is reconciled on
  re-run (the failure is surfaced with an explicit "not yet authoritative" warning).
- Local Docker dev is preserved: `--approve/--deny/--enforce` (which need the CRD +
  signer) refuse clearly in Docker mode; `--learn/--learned` keep the runtime-only
  path. The merge-patch replaces only `allowedEndpoints`, preserving sibling
  `networkPolicy` fields (`egressMode`, `allowlistRef`).

### Anti-affordances preserved
- The router's removal of the in-process approval side door is respected — the CLI
  never re-introduces a runtime "approve this host" mutation; approval is a signed
  CRD change. No call to a removed endpoint remains in the CLI/operator paths.

## Verification
- CLI typecheck (`tsc --noEmit`) + `oxlint` (0 errors; 29 pre-existing warnings,
  none in the changed files) + `npm run build` clean.
- `vitest`: 903 pass / 2 skipped (49 files). `egress.test.ts` gains coverage for
  `parseDomainPort` (default :443, host:port, URL/port-range rejection),
  `unionEndpoint`/`removeHost` (idempotency, distinct-port, port-less preservation
  + :443-equivalence), and the updated signing-context error text.
- `tests/e2e-manual/scenarios/egress_lifecycle.sh` rewritten to the CRD model
  (egressMode patch + EgressApproval create/delete); `bash -n` + `shellcheck -S
  error` clean.
- Two independent rubber-duck reviews (k8s + kars-architecture lens); all
  blocking findings (deny-not-signed fail-open, enforce not live-disabling learn,
  port-less drop, Docker-mode regression, manual-E2E dead endpoints) addressed.

## Verdict
Accept. Repairs a broken security workflow by routing CLI/operator actions through
the authoritative CRD + signed-allowlist pipeline; strengthens revocation
(deny re-signs), keeps signing fail-closed, and removes the last callers of the
deleted in-router approval endpoints. No security control weakened.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
