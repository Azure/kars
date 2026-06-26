# Security Audit — KarsMemory scope underscore convention (fix v0.1.19 CEL regression)

Date: 2026-06-26
Scope: `cli/src/refs.ts`, `cli/src/commands/dev/local-k8s.ts`, `cli/src/commands/memory.ts` (+ test), `tests/e2e/run.sh`, `tests/e2e-manual/scenarios/crd_admission.sh`, `examples/full-stack-demo/*`, `docs/api/crd-reference.md`, `docs/cli-reference.md`.
Gated path: `cli/src/commands/memory.ts`, `cli/src/commands/dev/local-k8s.ts`.

## Summary

v0.1.19 added a CEL charset rule to `KarsMemory.spec.scope` (rejects colons,
which the Foundry Memory Store 400s) but left colon-form scopes
(`agent:<name>`) in CLI defaults, the example demo, docs, and the E2E fixture.
Result: the E2E "KarsMemory apply rejected" failure, and — more importantly —
`kars dev --target local-k8s` and `cli/src/refs.ts` would generate a KarsMemory
CR that admission now rejects. This fix moves every generated/default/example
scope to the underscore convention (`agent_<name>`), matching the router
(`effective_scope` / `sanitize_scope`) and both runtime plugins, and adds
client-side charset validation in `kars memory` for a clear pre-apply error.

## T1: New capability / attack surface? (NO)
- No new role, endpoint, or privilege. Pure convention/string change: the
  generated `scope` value flips `:` → `_`. The Memory Store partition key
  semantics are unchanged (scope still isolates each sandbox's memories).

## T2: Security-control change? (HARDENING / NEUTRAL)
- Adds a client-side `^[A-Za-z0-9_./%+@-]+$` check in `validateMemorySpec`
  mirroring the CRD CEL rule — a clearer failure mode, no relaxation. The
  authoritative guard remains admission CEL + the router's `sanitize_scope`.
- Generated scopes were never security-relevant; the change keeps each sandbox
  scoped to its own `agent_<name>` partition exactly as before.

## T3: Availability / fail-open risk? (REDUCED)
- Fixes a real regression: `kars dev` / `kars memory` / the demo previously
  produced CRs that admission rejected. No fail-open — invalid scopes are still
  rejected (now with a clear client-side message before the API call).

## Verification
- CLI: `tsc --noEmit` + oxlint clean; 851 vitest pass incl. new tests asserting
  a colon scope is rejected and the valid charset (`session_1-a.b@c/d`) is
  accepted, and the generated/default scopes use `_`.
- E2E + manual CRD-admission fixtures updated to `agent_<name>` so the Kind E2E
  `KarsMemory apply` admits again.
- Examples + docs (`demo.yaml`, `crd-reference.md`, `cli-reference.md`) updated
  so copy-pasted commands aren't rejected by admission.

## Verdict
Accept. Completes the v0.1.19 scope-charset change consistently across CLI
codegen, examples, docs, and tests; no security control weakened.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
