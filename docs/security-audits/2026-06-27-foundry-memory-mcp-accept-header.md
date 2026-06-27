# Security Audit — Foundry memory MCP Accept header (fix runtime memory end-to-end)

Date: 2026-06-27
Scope: `runtimes/openclaw/src/core/router-client.ts`, `runtimes/hermes/src/kars_runtime_hermes/plugin/foundry.py`, `runtimes/hermes/src/kars_runtime_hermes/plugin/router_client.py`, `inference-router/src/routes/mcp.rs` (tests only), plus runtime tests.
Gated paths: `runtimes/openclaw/src/core/router-client.ts`, `inference-router/src/routes/mcp.rs`.

## Summary

Persistent agent memory was broken end-to-end from OpenClaw and Hermes: the
agent's `foundry_memory` tool returned **"Accept must include both
application/json and text/event-stream"** whenever it tried to read/write.

Root cause (traced through every layer): the memory tool is a thin client that
posts a single JSON-RPC `tools/call` to the router's `/platform/mcp` endpoint.
That endpoint is an **MCP Streamable-HTTP** server and, per spec, requires the
POST `Accept` header to advertise **both** `application/json` and
`text/event-stream` (`inference-router/src/mcp/pipeline.rs` →
`validate_accept_header`); otherwise it returns HTTP 406 before any dispatch.
The two hand-rolled thin clients I introduced in the v0.1.19 memory
consolidation (`callPlatformTool` in OpenClaw, `_call_platform_tool` in Hermes)
sent **no** `Accept` header, so every memory call 406'd. The unit tests mocked
the transport, so they never exercised the real header negotiation.

It is specifically these two clients: OpenClaw's other Foundry tools call the
router proxy directly (no MCP negotiation), and the maf-python / openai-agents /
langgraph / pydantic-ai runtimes use MCP clients that already send the header.

**Fix:** both thin clients now send `Accept: application/json,
text/event-stream` (mirroring the maf-python client). Hermes' `router_client.call`
gained an optional `headers` parameter to carry it.

## T1: New capability / attack surface? (NO)
- No new endpoint, tool, route, privilege, or network path. One request header is
  added to an existing in-pod loopback call (`127.0.0.1:8443/platform/mcp`). The
  `inference-router` change is **test-only** (added `#[tokio::test]` cases inside
  `#[cfg(test)]`). No production router behaviour changes.

## T2: Security-control change? (NEUTRAL)
- The router's Accept enforcement is unchanged and still in force (a new test
  asserts the 406 still fires when the header is absent, so a future client
  regression is caught). The client simply now satisfies the spec-required
  negotiation. No auth/crypto/governance/isolation change — store/scope
  resolution, IMDS token minting (audience `https://ai.azure.com`), and the
  Foundry contract are all untouched and continue to live in the router.

## T3: Availability / fail-open risk? (REDUCED)
- Restores a completely-broken capability (memory) with no fail-open: malformed
  requests still 406, upstream auth failures still surface as the router's
  existing `AuthMisconfigured` CRD signal. Adding the header cannot make a
  previously-rejected request succeed in any way other than the intended one.

## Verification
- **Client side (the bug):**
  - OpenClaw `platform-mcp-accept.test.ts` — a REAL loopback HTTP server that
    enforces the same Accept negotiation as the router: asserts the thin client
    sends both media types and parses the JSON-RPC result; plus a sanity check
    that the server 406s when the header is omitted.
  - Hermes `test_router_client.py` (headers forwarded to httpx) +
    `test_foundry_http_fetch.py` (the memory tool sends the Accept header).
- **Server side (the contract):**
  - `inference-router` `platform_foundry_memory_dispatches_to_upstream_with_accept`
    — `/platform/mcp` + Accept → real `PlatformDispatcher` → wiremock upstream
    receives `:update_memories` with the correct `items[]`/`scope`/`update_delay`
    contract; and `platform_foundry_memory_without_accept_is_406`.
- Suites: OpenClaw 125, Hermes 149 (1 pre-existing unrelated `kars_agt_mesh`
  wheel skip), inference-router lib 951 — all green. `cargo clippy -D warnings`
  + `cargo fmt --check` + `ruff` + `tsc` + `oxlint` clean.

## Verdict
Accept. Single-header correctness fix that restores agent memory end-to-end,
covered on both sides of the client/server contract; no security control
weakened (and the server-side enforcement is now regression-tested).

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
