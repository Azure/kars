# Phase 1 — MCP `tools/list` + `tools/call` dispatch

Date: 2026-04-25
Branch: `phase1/mcp-tools-dispatch`
Capability touched: `inference-router/src/mcp/{tools.rs,pipeline.rs,mod.rs}`

## What landed

A real MCP `tools/list` and `tools/call` dispatch layer that plugs into
the existing pipeline (`mcp::process_request`). Per spec
[§tools](https://modelcontextprotocol.io/specification/2025-03-26/server/tools).

- `ToolDefinition` — wire-format `{ name, description, inputSchema }`
  (camelCase per spec).
- `ToolCatalog` — owns the published list, validated at construction:
  rejects empty names, rejects duplicate names, rejects non-object
  non-boolean `inputSchema` values.
- `ToolDispatcher` trait — the seam the route handler injects.
- `EchoDispatcher` — minimal real default (single `echo` tool that
  returns the `text` argument). **Not a stub** per the no-stubs gate;
  it is wired end-to-end through 19 unit tests + 3 pipeline
  integration tests.
- `handle_tools_list` — offset-based pagination via opaque cursor
  (`offset:N`); `nextCursor` returned only when more pages remain;
  unknown cursors reset to first page (forward-compat with future
  cursor encodings).
- `handle_tools_call` — params validation (name, arguments), structured
  `DispatchError → JsonRpcError` mapping
  (`UnknownTool → -32601`, `InvalidArguments → -32602`,
  `ExecutionFailed → -32000`).
- `process_request` extended to take `tools: Option<&dyn ToolDispatcher>`.
  When `None`, `tools/list` and `tools/call` cleanly return
  `MethodNotFound (-32601)` with a structured `reason` rather than
  panicking — matches §0.2 #4 (no panics on inbound).

## Threat model

| Class | Mitigation |
|---|---|
| Catalog spoofing (duplicate tool names → ambiguous dispatch) | `ToolCatalog::new` rejects duplicates at construction |
| Non-object schemas (breaks JSON-Schema interop) | Catalog construction enforces object-or-bool |
| Empty tool name (un-callable, breaks `tools/call`) | Catalog construction rejects empty names |
| `tools/call` arg injection | Args passed through opaquely as `Value`; validation is the dispatcher's responsibility, surfaced as `InvalidArguments` errors not panics |
| Pagination DOS via huge cursor offsets | Offset clamped to `tools.len()`; unknown cursor resets to 0; no allocation proportional to cursor value |
| Wire-format leak (snake_case bleeds out) | `inputSchema` / `isError` regression test asserts presence of camelCase AND absence of snake_case in serialized bytes |

## Crypto

None. This module does no cryptography. The no-custom-crypto gate
sees no new direct uses of `ring`, `rustcrypto`, `sodiumoxide`,
`subtle`, or hand-rolled primitives.

## Wire-format compliance

- `ToolDefinition` uses `#[serde(rename_all = "camelCase")]` →
  emits `inputSchema`, never `input_schema`.
- `ToolCallOutput` uses `#[serde(rename_all = "camelCase")]` →
  emits `isError`, never `is_error`.
- Regression test in `mcp::pipeline::tests::tools_list_returns_catalog_when_dispatcher_provided`
  asserts both presence and absence on the serialized response bytes.

## Test coverage

Module-level (`mcp::tools::tests`): **19 tests**

- Catalog construction: invalid schema, empty name, duplicate name,
  cursor parse, page-size override.
- `tools/list`: basic listing, pagination forward/backward, offset
  past end, unknown cursor, id preservation, camelCase wire format.
- `tools/call`: missing params, missing name, unknown tool,
  invalid arguments, successful echo, error propagation, camelCase
  `isError` field.

Pipeline-level (`mcp::pipeline::tests`): **4 new tests**

- `tools/list` returns `MethodNotFound` when no dispatcher injected.
- `tools/list` returns catalog page when dispatcher injected; verifies
  camelCase leak-test on raw bytes.
- `tools/call` invokes dispatcher and returns structured result.
- `tools/call` of unknown tool returns JSON-RPC error (no panic).

Total router lib tests: 381 → 384 passing.

## Pipeline integration

`process_request` signature is now:

```rust
pub fn process_request(
    body: &[u8],
    accept_header: Option<&str>,
    config: &InitializeConfig,
    minter: &dyn SessionMinter,
    tools: Option<&dyn ToolDispatcher>,
) -> ProcessOutcome
```

All 17 existing internal call sites updated mechanically with
`None` for the new arg. No behavioural change for `initialize` /
`ping` / batch handling. Existing `mcp::pipeline::tests` (18) still
pass unchanged — verified.

## What this PR is **not**

- Not the live HTTP route binding. `POST /mcp` axum wrapper still
  belongs to `phase1/mcp-route-binding`. That work needs cluster
  validation (real OAuth issuer + JWKS + ingress) and is operator-
  driven.
- Not the AGT-policy-gated production tool dispatcher. `EchoDispatcher`
  is the dev/smoke default; the prod dispatcher (which calls policy
  evaluation per `tools/call`) lands in `phase1/mcp-tool-policy-enforce`
  alongside the real `ToolPolicy` reconciler.

## CI gates

- `ci/no-stubs.sh` — pass (no `todo!`, `unimplemented!`, or "stub" markers).
- `ci/no-custom-crypto.sh` — pass (no new crypto-primitive imports).
- `ci/check-loc.sh` — pass (tools.rs 661 LOC; pipeline.rs 778 LOC; both
  under 1500 cap).
- `ci/no-null-provider-prod.sh` — pass (no `null` provider in prod
  paths).
- `ci/a2a-module-isolation.sh` — pass (this PR is in `mcp/`, not `a2a/`).
- `ci/vendored-patch-audit.sh` — pass (no AGT SDK pin change).

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
