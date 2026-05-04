# Security Audit — Phase 1: MCP route binding (`POST /mcp`)

**Date:** 2026-04-25
**Scope:** `inference-router/src/routes/mcp.rs`, `inference-router/src/routes/mod.rs`
**Branch:** `phase1/mcp-route-binding`

## 1. Summary

Adds the axum binding for the MCP 2025-03-26 Streamable HTTP transport. The
pure-function pipeline (`mcp::pipeline::process_request`) was landed in prior
PRs; this PR makes it reachable over HTTP via `POST /mcp` and `GET /mcp` (the
GET method returns `405 Method Not Allowed` with `Allow: POST` because SSE
long-polling is not yet implemented).

The route is mounted as a sub-router with its own typed state
(`McpRouteState`), independent of the global `AppState`. This keeps coupling
explicit and lets handlers be exercised end-to-end via
`tower::ServiceExt::oneshot` without constructing an `AppState` (which does
network I/O at build time).

## 2. Threat model delta

| Asset | New exposure | STRIDE |
|---|---|---|
| Router process memory | Inbound MCP frames now reach the JSON-RPC dispatch path. | Denial-of-Service (oversized body) |
| Initialize-only state minter | Session id minted from `OsRng` and surfaced over HTTP. | Tampering / Information disclosure |
| Tool execution surface | `tools/list` + `tools/call` reachable over HTTP via `EchoDispatcher`. | Spoofing / Tampering |

All three exposures were already addressed in the underlying pipeline:

- Body size cap (`MAX_FRAME_BYTES` = 4 MiB) → `413 Payload Too Large` *before*
  any JSON parsing. Verified by `post_mcp_oversized_returns_413`.
- `Accept` header negotiation → `406` if both `application/json` and
  `text/event-stream` are not listed. Verified by
  `post_mcp_missing_accept_returns_406` and `post_mcp_only_json_accept_returns_406`.
- `OsRngSessionMinter` is the production minter; `FixedMinter` exists only
  inside `#[cfg(test)]`.

This PR does not add new authentication or trust assumptions; OAuth 2.1
verification is the next PR (`mcp::oauth` is the pure verifier already merged).

## 3. OWASP mapping

OWASP MCP Top 10 (preview):

- **M01 — Insecure transport / framing.** Body-size cap, `Accept`
  negotiation, JSON-RPC parse-error envelope returned over HTTP 200, and
  notification-only batches mapped to `202 Accepted` per spec.
- **M02 — Auth bypass.** Not yet enforced at this layer; tracked as the next
  PR. Audit doc explicitly notes this is *not* claimed as covered.
- **M07 — Tool-call confused deputy.** `EchoDispatcher` only — no upstream
  tool servers reachable yet. Real `ToolDispatcher` impls land alongside the
  `McpServer` reconciler.

OWASP LLM Top 10 v2.0:

- **LLM02 (Sensitive Information Disclosure)** — none yet; pipeline does not
  emit logs containing request payloads.

## 4. AuthN / AuthZ path

- **Today:** none at the route layer. Anonymous callers can hit `POST /mcp`.
  This is acceptable because the route is not yet mounted on the production
  router (`main.rs` unchanged in this PR — wiring lands once the OAuth 2.1
  verifier is bound as a tower layer in the next PR).
- **Outage behaviour:** `Strict` (default) fail-closed will be the OAuth
  layer's responsibility; this binding has no AGT call path of its own.

## 5. Secret + key custody

None. The MCP route reads no secrets and does not sign any payload. Session
ids are non-secret CSPRNG output (32 bytes hex) per spec.

## 6. Egress surface delta

Zero new outbound destinations. `EchoDispatcher` is in-process. `McpServer`
upstream calls are a future PR.

## 7. Audit events emitted

None at this layer. The pipeline does not call `AuditSink`. When tools that
reach upstream services are added, they will append via
`AuditSink::append_with_dedup` per existing pattern in `routes/inference.rs`.

## 8. Failure mode

All paths fail-closed and total:

| Input | Output | Test |
|---|---|---|
| body > 4 MiB | `413` | `post_mcp_oversized_returns_413` |
| missing `Accept` | `406` | `post_mcp_missing_accept_returns_406` |
| only `application/json` | `406` | `post_mcp_only_json_accept_returns_406` |
| malformed JSON | `200` + `-32700` parse error envelope | `post_mcp_malformed_json_returns_200_with_parse_error` |
| unknown method | `200` + `-32601` method-not-found | `post_mcp_unknown_method_returns_method_not_found` |
| notification only | `202` empty body | `post_mcp_notification_only_returns_202` |
| `GET /mcp` | `405` + `Allow: POST` | `get_mcp_returns_405_with_allow_header` |
| valid `initialize` | `200` + `Mcp-Session-Id` header | `post_mcp_initialize_returns_session_header_and_result` |

## 9. Negative-test coverage

13 `#[cfg(test)]` tests in the same file. Each negative path documented in §8
has at least one explicit assertion. Covered scenarios mirror the
`tests/conformance/` MCP corpus that lands in a follow-up PR (the corpus is
fixture-driven; these tests are wire-level axum oneshots).

## 10. Vendored / third-party dependency delta

- `tower::ServiceExt` (already a workspace dep) — added to dev-deps usage in
  this file via the existing dev-dependency declaration; no new crates.
- No new production dependencies.

## 11. Sign-offs

Capability author and independent reviewer have reviewed the threat-model
delta, negative-test surface, and OWASP MCP Top 10 mapping. Both confirm
that `POST /mcp` is **not** wired into the production router pipeline by
this PR — wiring is gated on the OAuth 2.1 layer landing next.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>

## References

- MCP 2025-03-26 Streamable HTTP — <https://modelcontextprotocol.io/specification/2025-03-26/basic/transports>
- JSON-RPC 2.0 — <https://www.jsonrpc.org/specification>
- `inference-router/src/mcp/pipeline.rs` (pure pipeline; tests already cover the dispatch logic)
