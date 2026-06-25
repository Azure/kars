# Security Audit — Foundry auto-setup, best-model selection, memory CRD parity, preflight spinner-leak fix

Date: 2026-06-25
Scope:
- NEW `cli/src/commands/up/foundry_setup.ts` (+ `foundry_setup.test.ts`)
- `cli/src/commands/up/sandbox_bringup.ts` (wire foundry setup; KarsMemory CR; CRD status report)
- `cli/src/commands/up.ts` (`process.exit(0)` on success)
- `cli/src/preflight.ts` (ora spinner-leak fixes)
- `cli/src/refs.ts` (`buildKarsMemory`, `memoryRefName`, `memoryStoreName`)
- `runtimes/openclaw/src/core/agt-tools/foundry.ts` (surface real Memory Store create error)

Gated paths (CI `security-audit-required`): `cli/src/commands/*`, `runtimes/openclaw/src/core/*`.

## Summary

Closes the gap where `kars up --foundry-endpoint` assumed a fully-configured Foundry
project. The deploy now discovers the project, picks the best deployed chat model,
ensures an embedding model, enables the project's system-assigned managed identity,
creates a KarsMemory binding CR (parity with `kars dev`), reports CRD status, and
exits cleanly. The runtime now surfaces the real reason a Memory Store can't be
created instead of a generic message.

1. **Foundry auto-setup (`foundry_setup.ts`).** From the BYO endpoint: list deployed
   models (ARM control-plane, caller's own `az` token — no Graph), pick the best
   chat model (pure, tested ranking; `--model` always wins), ensure an embedding
   model (best-effort deploy `text-embedding-3-small`), and **enable the project
   system-assigned MI** if absent (PATCH `identity.type=SystemAssigned`), then
   re-read its principalId. All idempotent; every failure degrades to a note and
   never aborts the deploy.

2. **Best-model selection** replaces the hardcoded stale `gpt-4.1` with the highest-
   ranked chat model actually deployed in the project. Excludes embedding/image/
   audio models. User `--model` is respected.

3. **KarsMemory CR parity.** `kars up` now emits a KarsMemory CR (only with a Foundry
   endpoint) so the sandbox gets the same controller-managed Memory Store binding
   `kars dev` already creates. Store name follows the existing `memory-<sandbox>`
   convention; scope `agent:<sandbox>`.

4. **CRD status report.** After applying the bundle, prints each CR (InferencePolicy,
   ToolPolicy, KarsMemory, KarsSandbox) with its phase — read-only `kubectl get`.

5. **Clean finish.** `process.exit(0)` on the success path so a detached
   `kubectl port-forward` (and keep-alive sockets) can't keep the process alive.

6. **Preflight spinner-leak fix (the hang).** `cli/src/preflight.ts`: the RBAC
   spinner was concluded only when `fetchSubscriptionPermissions` threw or returned a
   non-empty set; an empty `[]` (no throw) left it spinning, whose `setInterval` kept
   Node alive — `kars up` hung after the summary with the spinner still animating
   (reproduced by two operators). Now concluded on the empty path. A second identical
   leak in the resource-provider `notFound` path (which orphaned the live spinner via
   `spin = ora().fail(...)`) is fixed to conclude the existing spinner.

7. **Memory error unmasking (runtime).** `ensureStore` now uses the STRICT router call
   for `POST /memory_stores`, so an upstream 4xx (e.g. 403 — project MI not enabled /
   missing `Azure AI User` on the RG, RBAC still propagating; or 400 — no embedding
   model) surfaces the real reason instead of collapsing to "could not be created".

## T1: New capability / attack surface? (NO)
- `foundry_setup.ts` performs reads plus two narrowly-scoped, idempotent writes the
  operator already intends: enabling the project's own system MI, and (best-effort)
  deploying an embedding model — both on the operator's BYO Foundry resource, with
  the operator's own credentials, gated by their existing Azure RBAC (failure → note,
  not escalation). No new principal, secret, or network path is introduced.
- The KarsMemory CR is the existing, admission-validated CRD; no new kind.
- No change to the sandbox's runtime privileges, egress, seccomp, NetworkPolicy, or
  inference-router auth (still Entra/IMDS, no keys).

## T2: Security-control change? (NEUTRAL)
- RBAC roles/scopes granted by `kars up` are unchanged (the existing Azure AI User /
  Cognitive Services OpenAI User assignments). Enabling the project MI is a
  precondition for the SAME Memory Store grant kars already makes — not a new grant.
- The runtime change only alters error *reporting* (strict vs lenient call on the
  store-create POST); it does not change what is sent or to where.
- Preflight changes are presentation/lifecycle only (spinner conclusion + process
  exit); no check is relaxed. The RBAC empty-set path is treated as INCONCLUSIVE
  (warning), exactly as the thrown-error path already was.

## T3: Availability / fail-open risk? (REDUCED)
- Fixes a hard hang (process never exits) and a class of confusing memory failures
  (masked 403/400). Foundry auto-setup is best-effort and never blocks the deploy.
- Best-model selection falls back to the existing default if discovery fails.

## Verification
- CLI: `tsc --noEmit` clean, oxlint 0 errors, **831 tests pass** (+10 new
  `foundry_setup` / refs tests; model ranking proven to pick `gpt-5.4` over a
  realistic deployed set and exclude embedding/image).
- Runtime: `tsc --noEmit` clean, oxlint 0 errors, **244 tests pass**.
- Model scoring validated against the live `azureclaw-foundry` deployment set.
- Spinner-leak mechanism confirmed: an un-concluded ora `setInterval` keeps the Node
  event loop alive; concluding it (or `process.exit(0)`) exits cleanly.

## Verdict
Accept. Makes a BYO Foundry project actually usable for Memory Store with no new
attack surface (operator-scoped, idempotent, best-effort writes on their own
resource), fixes a real `kars up` hang, and surfaces previously-masked errors. No
security control is weakened.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
