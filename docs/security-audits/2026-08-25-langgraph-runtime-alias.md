# Security Audit — canonical LangGraph runtime flag

Date: 2026-08-25
Scope: `cli/src/runtime.ts`, `cli/src/runtime.test.ts`,
`cli/src/commands/operator/dialogs/spawn.ts`, `CHANGELOG.md`.
Gated paths: `cli/src/commands/operator/dialogs/spawn.ts`.

## Summary

The CLI previously documented and displayed `langgraph` while its runtime
parser accepted only `lang-graph`. This change makes `langgraph` the canonical
flag and keeps `lang-graph` as an accepted compatibility alias for existing
scripts. Runtime selection still resolves to the same `LangGraph` CRD enum and
the same runtime image/controller plan.

## T1: New capability / attack surface? (NO)

- No runtime, image, endpoint, credential, Kubernetes permission, or network
  path is added.
- Both spellings resolve to the existing `LangGraph` runtime kind.
- The legacy spelling remains accepted but is not advertised in new pickers or
  error messages.

## T2: Security-control change? (NEUTRAL)

- Runtime wiring, sandbox isolation, policy selection, and credential handling
  are unchanged.
- Input normalization remains a closed map from known CLI strings to the
  existing typed `RuntimeKind`; unknown values still fail.

## T3: Availability / fail-open risk? (REDUCED)

- Fixes a documented command that previously failed before deployment.
- Existing automation using `--runtime lang-graph` continues to work.
- No fallback or silent runtime substitution is introduced.

## Verification

- CLI typecheck, lint, build, and full unit tests.
- Focused runtime tests cover canonical, case-insensitive, and legacy alias
  parsing plus picker/error-message behavior.
- `security-audit-required`, LOC, and copyright gates.

## Verdict

Accept. The change fixes an inconsistent public CLI spelling while preserving
the existing alias and runtime behavior.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
