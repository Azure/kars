# Security audit — MCP 2026 Streamable HTTP negative-only edge cases

**Date:** 2026-04-25
**PR branch:** `phase1/mcp-negative-edge-cases`
**Capability owner:** AzureClaw Phase 1 — MCP 2026 transport

## 1. Summary

Adds `inference-router/tests/mcp_negative_edge_cases.rs` — five
integration tests pinning the three off-happy-path scenarios that the
Phase 1 plan calls out explicitly:

1. **Oversized frame at exactly `MAX_FRAME_BYTES + 1`** — pins the
   reject side of the boundary the in-tree corpus only pins from the
   accept side.
2. **`Mcp-Session-Id` minter collision** — a buggy minter that emits
   the same id three times must not panic the pipeline.
3. **Batch with mixed JSON-RPC id types (Number, String, Null)** —
   each id must round-trip verbatim into the corresponding response
   slot.

Tests-only PR. No production code changes.

## 2. Threat model delta

### Asset gaining new exposure
None. Tests-only.

### STRIDE

- **Denial of service (D)** — boundary test prevents a future
  off-by-one that would let a 4 MiB + 1 byte frame through; the
  oversized-payload short-circuit is the cheapest backpressure the
  router has.
- **Repudiation / spoofing (R/S)** — id-round-trip test prevents a
  future change from coalescing or reordering responses, which would
  let one client's response leak to another (id is the only correlation
  the JSON-RPC client has).
- **Tampering (T)** — minter-collision test pins that the pipeline
  doesn't crash on a buggy minter; without this, a Phase 2 session
  ledger bug could trip a panic and take down the router.

## 3. OWASP mapping

- **OWASP MCP04 — Protocol-Level Misbehaviour:** the boundary tests
  guarantee that any future regression of `MAX_FRAME_BYTES` enforcement
  is caught at PR time.
- **OWASP LLM10 — Unbounded Consumption:** the +1 boundary test is the
  unit-level check that complements the route-level 413 mapping in
  `routes::mcp::outcome_to_response`.

## 4. AuthN / AuthZ path

Not applicable — pipeline-level tests run before any route, before any
OAuth layer. They exercise the spec-conformance core; auth is layered
on top via `protected_mcp_route` (PR 34).

## 5. Secret + key custody

None.

## 6. Egress surface delta

None.

## 7. Audit events emitted

None.

## 8. Failure mode

Each test asserts the pipeline produces the **exact** outcome the
spec mandates:

| Scenario | Asserted outcome |
|---|---|
| Body of `MAX_FRAME_BYTES + 1` bytes | `ProcessOutcome::PayloadTooLarge` |
| Sub-MAX body, valid frame | `ProcessOutcome::JsonRpcResponse` |
| Three sequential `initialize` calls with a colliding minter | three `JsonRpcResponse` outcomes, each carrying the (deterministically duplicate) session id; no panic |
| Batch `[Number, String, Null]` ids | response array same length, ids preserved positionally |
| Batch `[Null, Null]` ids | response array, both ids round-trip as `null` |

## 9. Negative-test coverage

Five tests; one positive sanity check (`body_well_under_max_is_processed_normally`)
to ensure the boundary test means what we think it means.

The full pipeline corpus (24 in-tree tests in
`inference-router/src/mcp/pipeline.rs::tests`) covers happy paths,
parse errors, accept-header negotiation, tool dispatch, and inbound
`Response`-frame rejection. This PR is purely the boundary scenarios
the in-tree corpus elides.

## 10. Vendored / third-party dependency delta

None. Reuses `serde_json`. No new symbols added to the crate's public
API.

Sources consulted:

- JSON-RPC 2.0 §4 (id types) —
  <https://www.jsonrpc.org/specification#request_object>.
- MCP 2026-03-26 transport spec, "Frame size" section.
- `inference-router/src/mcp/pipeline.rs` for the corpus this PR
  extends.

## 11. Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
