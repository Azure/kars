# Security Audit — Foundry Memory Store consolidated on the router; RBAC heal on `kars upgrade` (v0.1.19)

Date: 2026-06-26
Scope: `inference-router/src/mcp/platform.rs`, `runtimes/openclaw/src/core/agt-tools/foundry.ts`, `runtimes/openclaw/src/core/router-client.ts`, `runtimes/hermes/src/kars_runtime_hermes/plugin/foundry.py`, `controller/src/crd_validations.rs` (+ synced `controller/src/kars_memory.rs`, `deploy/helm/kars/templates/crd-karsmemory.yaml`), `controller/src/reconciler/mod.rs` (comment only), `cli/src/commands/up/foundry_memory_rbac.ts` (new), `cli/src/commands/upgrade.ts`.
Gated paths: `inference-router/src/mcp`, `controller/src/crd_validations.rs`, `cli/src/commands/up/foundry_memory_rbac.ts`, `cli/src/commands/upgrade.ts`, `runtimes/openclaw/src/core`.

## Summary

Persistent agent memory failed from both OpenClaw and Hermes ("the store
creates but nothing persists / recall 403s"). Root causes spanned every layer.
The fix makes the **inference-router the single owner of the Foundry Memory
Store contract** and turns the two runtime plugins into thin clients.

1. **One contract, in the router.** `foundry.memory`
   (`mcp/platform.rs`) now emits the real Memory Store REST shape (`items[]`
   of message items + `scope` + `options.max_memories` / `update_delay`),
   adds `delete_scope` + `top_k`, pins the data-plane `api-version` on every
   memory path (the proxy does not inject one — a latent bug), defaults the
   store to the per-sandbox convention `memory-<sandbox>`, and resolves the
   scope via `effective_scope()` (agent arg → KarsMemory binding → convention),
   always run through `sanitize_scope()`.

2. **Thin runtime clients.** OpenClaw (`foundry.ts` + new
   `callPlatformTool` in `router-client.ts`) and Hermes (`foundry.py`) now send
   a single JSON-RPC `tools/call` to the loopback-only `/platform/mcp`. They
   carry **no** Foundry contract logic and **no** store-name override — the
   bound KarsMemory CRD is the single source of truth. This makes TS/Python
   wire drift structurally impossible.

3. **Admission guard + RBAC heal.** `KarsMemory.spec.scope` now has a CEL
   charset rule (rejects colons, which the data plane 400s). `kars upgrade`
   gains an idempotent `ensureFoundryMemoryRbac()` step that enables the
   Foundry **project** managed identity, ensures an embedding deployment, and
   grants the project MI `Azure AI User` on the resource group — the identity
   that Memory Store's internal model calls authenticate as. This heals an
   existing cluster's memory on upgrade instead of requiring a fresh `kars up`.

## T1: New capability / attack surface? (NEUTRAL — net reduction)
- The agent-facing surface **shrinks**: the per-call `store_name` override was
  removed from both plugins, so an agent can no longer redirect memory ops to
  an arbitrary store id. Store selection is now infra-controlled (binding /
  `FOUNDRY_MEMORY_STORE_ID` / `memory-<sandbox>`), never agent-controlled.
- `delete_scope` is now reachable by agents, but each sandbox resolves to its
  own store and agent control of `scope` (per-user/session namespacing) already
  existed by design. No new cross-store / cross-tenant boundary is crossed.
- `/platform/mcp` is a pre-existing, loopback-only, no-OAuth endpoint (only the
  in-pod runtime can reach it; the egress-guard init container blocks everything
  else). The thin clients only forward `operation/text/query/scope/top_k`.

## T2: Security-control change? (NEUTRAL / hardening)
- **`sanitize_scope` (`platform.rs`)** coerces the agent-supplied `scope` to the
  Foundry charset `[A-Za-z0-9_-.%+@/]`. `/` is allowed but `scope` is only ever
  placed in the JSON request **body** (`:search_memories` / `:update_memories` /
  `:delete_scope`), never interpolated into a URL path or shell — confirmed by
  grep. No path traversal / SSRF / injection.
- **`store_id` into the URL path** (`/memory_stores/{store_id}…`) resolves only
  from router-only infra inputs (binding / env / `memory-<sandbox>`); not
  attacker-controllable.
- **CEL scope rule (`crd_validations.rs`)** `^[A-Za-z0-9_./%+@-]+$` is fully
  anchored; `+` does not match newlines → no admission bypass. Synced to the
  helm CRD (drift test passes).
- **RBAC helper (`foundry_memory_rbac.ts`)** issues `az` via `execa` with an
  **argv array** (no shell), a hardcoded role-definition GUID, and
  principal/scope values sourced from `az` output. No command injection. The
  grant runs under the **operator's** existing credentials during `kars upgrade`,
  not via any agent-triggerable path — not a privilege-escalation primitive. The
  role granted (`Azure AI User`, `53ca6127-…`) and its RG scope are exactly what
  Memory Store's internal model calls require and match the existing
  `kars up` BYO-Foundry grant; no widening.
- JSON-RPC bodies are built with structured `json!` / object literals, not
  string concatenation. No secrets, weak crypto, or HTTP-downgrade in the diff.

## T3: Availability / fail-open risk? (REDUCED)
- The `kars upgrade` RBAC step is **best-effort and non-fatal**: any failure
  degrades to an actionable note and never fails the upgrade. The role create is
  idempotent (`RoleAssignmentExists` treated as success).
- The router keeps ensure-on-404 + retry, and surfaces upstream 401/403 as a
  CRD `Degraded=AuthMisconfigured` with the exact RBAC remediation — so a
  misconfigured project MI is now legible instead of silent.
- **Reviewer-found bug, fixed before release:** `delete_scope` against a
  non-existent store skips ensure-on-404 (by design — we don't auto-create a
  store just to wipe it), but the later `MemoryStoreMissing` recorder was not
  similarly guarded, so a benign agent-initiated delete could flip the
  KarsMemory CRD to `Degraded`. Added `&& op != "delete_scope"` to the recorder
  and a regression test (`memory_delete_scope_404_does_not_record_memory_store_missing`).

## Verification
- Independent security review (no vulnerabilities found) and code review (the
  `delete_scope`/Degraded bug above, since fixed) on the full diff.
- Router: 51 `mcp::platform` + 948 lib + integration tests pass; `cargo clippy
  --all-targets -- -D warnings` + `cargo fmt --check` clean.
- Controller: 849 tests pass incl. `helm_drift` (Rust ↔ helm CRD in sync) and
  `crd_validations`.
- CLI: 850 vitest (incl. 6 new `foundry_memory_rbac` tests); `tsc --noEmit` +
  oxlint clean.
- OpenClaw: 102 vitest + `tsc` + oxlint clean. Hermes: 27 Foundry tests +
  `ruff check` clean (the JSON-RPC envelope parsed by both thin clients matches
  the router's `result.content[].text` / `isError` / `error.data.reason`).

## Verdict
Accept. Consolidating the Memory Store contract in the router removes the
cross-runtime drift that caused the failures, **reduces** the agent-facing
attack surface (no store override; infra-owned store/scope), adds an admission
charset guard, and heals the project-MI RBAC idempotently on upgrade under
operator credentials. No security control is weakened; the one reviewer-found
correctness bug was fixed and covered by a test.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
