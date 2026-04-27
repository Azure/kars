# Security audit — Phase 1 A2A JSON-RPC method dispatch

**Date:** 2026-04-25
**Branch:** `phase1/a2a-jsonrpc-dispatch`
**Capability:** A2A 1.0.0 JSON-RPC method dispatch — `message/send`,
`tasks/get`, `tasks/cancel` — with pluggable `TaskStore` and `TaskIdMinter`
seams. Pure-function module; not yet wired into a live HTTP route (route
binding lands in a separate cluster-validated PR).

## 1. Summary

This change adds `inference-router/src/a2a/jsonrpc_dispatch.rs`, a
self-contained pure-function dispatch module implementing the three core
A2A 1.0.0 task-management methods over JSON-RPC 2.0. The module exposes:

- `TaskStore` trait + `InMemoryTaskStore` reference implementation.
- `TaskIdMinter` trait + `CounterTaskIdMinter` (deterministic, tests) and
  `OsRngTaskIdMinter` (production, 16 bytes from `rand::rng()`).
- `Task`, `Message`, `TaskState` types (camelCase / kebab-case wire format
  per spec).
- `handle_message_send`, `handle_tasks_get`, `handle_tasks_cancel`.

JSON-RPC envelope types are reused from `crate::mcp::jsonrpc` because both
protocols are JSON-RPC 2.0 over HTTP; a2a-application-specific error codes
(`A2aErrorCode::TaskNotFound`, `TaskNotCancelable`) come from the existing
`crate::a2a::error` module.

## 2. Threat model delta

No new attack surface in this PR — the module is not yet bound to an HTTP
route. When subsequently bound, the relevant STRIDE categories are:

- **Spoofing:** addressed by upstream `verify_inbound_card` (PR 21,
  `phase1/a2a-card-verifier`) which validates the caller's signed AgentCard
  *before* dispatch is invoked.
- **Tampering:** JSON-RPC body integrity bound to TLS termination + signed
  AgentCard chain.
- **Information disclosure:** `tasks/get` returns task data only by id; ids
  are 32 hex characters (128 bits of entropy) from `OsRngTaskIdMinter`,
  unguessable.
- **Denial of service:** task store is bounded by AGT
  `PolicyDecisionProvider` rate-limit + `TaskStore::insert` returns
  `StoreError::Conflict` on collision (no silent overwrite).

## 3. OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM03 Training Data Poisoning / LLM06 Sensitive
  Info Disclosure:** N/A at this layer (no LLM call here).
- **OWASP MCP Top 10 — M02 Broken Authentication:** dispatch is invoked
  *after* the caller's AgentCard is verified by PR 21. This module never
  authenticates on its own.
- **OWASP API Top 10 — API3 Broken Object Property Level Authz:**
  `tasks/get` and `tasks/cancel` look up tasks by id only; future PR adding
  multi-tenant scoping must extend `TaskStore` with a tenant key. Tracked
  in plan §7.

## 4. AuthN / AuthZ path

This module performs **no authentication or authorization** of its own.
It is the pure-function tail end of an authenticated pipeline:

1. Inbound HTTPS request hits the `/a2a` route (binding lands in a future
   cluster-validated PR).
2. `verify_inbound_card` (PR 21) validates the caller's signed AgentCard
   against the trust store.
3. `PolicyDecisionProvider::decide` is called with the verified caller
   identity + tool/method.
4. Only on `Verdict::Allow` is `handle_message_send`/`handle_tasks_*`
   invoked.

Outage behaviour is governed by the upstream policy provider's
`spec.agt.outageMode` (Strict / CachedRead / DegradedDev), not this module.

## 5. Secret + key custody

None. No secrets, keys, or tokens flow through this module. `TaskStore`
holds only task ids (16 random bytes hex-encoded), `TaskState` enum, and
caller-supplied `Message` blobs.

## 6. Egress surface delta

None. This is a pure-function in-process module; no outbound network calls.

## 7. Audit events emitted

This PR does not emit audit events directly. The HTTP route that will
later wrap this dispatch is responsible for calling
`AuditSink::append(...)` on each method invocation; audit-emission tests
will land with the route-binding PR. The module returns sufficient detail
in its `Response` (task id, transition state) for the wrapping route to
log without re-entering the store.

## 8. Failure mode

All failures fail-closed (return JSON-RPC error response, no partial
state):

| Condition | Response |
|---|---|
| Malformed `params` (missing `message` etc.) | `InvalidParams` (-32602) |
| `tasks/get` for unknown id | `A2aErrorCode::TaskNotFound` (-32001) |
| `tasks/cancel` on terminal state (`completed`/`canceled`/`failed`/`rejected`) | `A2aErrorCode::TaskNotCancelable` (-32002) |
| `TaskStore::insert` id-collision (1-in-2¹²⁸) | `InternalError` (-32603); caller retries with fresh id |
| Concurrent `tasks/cancel` race | last-writer-wins on store; both callers receive success since cancel from non-terminal is idempotent |

No fail-open paths. No `unwrap()` on caller-controlled data.

## 9. Negative-test coverage

19 unit tests in `a2a::jsonrpc_dispatch::tests` covering:

- `message/send` happy path → task created in `submitted` state.
- `message/send` preserves caller-supplied JSON-RPC id (string / number).
- `message/send` emits `state` field as kebab-case (`"submitted"`).
- `tasks/get` for known id returns task; for unknown id returns
  `TaskNotFound`.
- `tasks/cancel` for non-terminal id transitions to `canceled`.
- `tasks/cancel` for each terminal state returns `TaskNotCancelable`
  (`completed`, `canceled`, `failed`, `rejected`).
- Malformed `params` returns `InvalidParams`, never panics.
- `OsRngTaskIdMinter` produces 32-hex-char ids; two consecutive mints
  differ.
- `CounterTaskIdMinter` is deterministic.
- `InMemoryTaskStore::insert` rejects id collision with `StoreError::Conflict`.

These are the positive + negative cases for the dispatch layer in
isolation. End-to-end protocol-conformance tests (tampered message,
replayed transfer, schema-mismatch wire frames) land with the route-
binding PR per the conformance-corpus rule in plan §5.4.

## 10. Vendored / third-party dependency delta

None. This module uses only:

- `serde` / `serde_json` — already in workspace.
- `crate::mcp::jsonrpc` — internal envelope types (JSON-RPC 2.0 is the
  shared substrate of MCP 2026 and A2A 1.0.0).
- `crate::a2a::error` — internal A2A application error codes.
- `rand::rng()` — already used by `OsRngSessionMinter` in
  `crate::mcp::initialize`. No new crate or feature.

No vendored-patch updates. `vendor/agentmesh-*/` untouched.

## 11. Sign-offs

Two independent reviews per principle 9 of the implementation plan.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
