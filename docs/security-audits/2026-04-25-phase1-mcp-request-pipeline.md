# Security Audit: `phase1/mcp-request-pipeline`

**Capability:** complete MCP Streamable HTTP request pipeline as a
pure synchronous function. The entire body→response transform short
of axum binding. The future POST `/mcp` route is now a 6-line
transport wrapper around `mcp::process_request`.

## 1. Summary

- New `inference-router/src/mcp/pipeline.rs` (≈ 580 lines incl. tests).
  Public API: `process_request(body, accept_header, config, minter) → ProcessOutcome`.
- `ProcessOutcome` covers all four HTTP-level outcomes:
  - `JsonRpcResponse { body, session_id }` → HTTP 200 + body + optional `Mcp-Session-Id` header
  - `Accepted` → HTTP 202 (notifications-only)
  - `PayloadTooLarge` → HTTP 413
  - `NotAcceptable(_)` → HTTP 406 with diagnostic
- Implements the entire MCP 2025-03-26 §transports + §lifecycle on the inbound POST path:
  - Body size cap before any JSON parsing (DoS defence)
  - `Accept` header negotiation (must list both `application/json` AND `text/event-stream`)
  - JSON-RPC 2.0 frame parsing (single + batch + nested-batch rejection)
  - Method dispatch: `initialize`, `ping`, future methods plug in via `handle_request`
  - Notifications: fire-and-forget; all-notifications batches yield 202
  - Inbound `Response` frames rejected (server-side hardening per JSON-RPC §6)
  - Empty batches → -32600 InvalidRequest
- Total function: every input yields a valid `ProcessOutcome`. No panics, no `unwrap`/`expect` on user data, no I/O.

## 2. Threat model

`process_request` is what the MCP TCP/TLS handler will call on every
inbound POST `/mcp`. Pre-auth attack surface; total-function discipline + budget bounding are mandatory.

| Threat | Mitigation | Test |
|---|---|---|
| Memory exhaustion via huge body | `body.len() > MAX_FRAME_BYTES` short-circuits **before** JSON parse | `payload_too_large_short_circuits` |
| Cap edge-case off-by-one | At-cap (== MAX_FRAME_BYTES) accepted; > cap refused | `body_at_exactly_max_frame_bytes_is_accepted` |
| `Accept`-header content sniff abuse | Reject any header that doesn't list both required types | `missing_accept_header_is_406`, `accept_only_json_is_406`, `accept_only_sse_is_406` |
| Malformed JSON DoS / parser amplification | Surfaces as JSON-RPC `-32700 ParseError` at HTTP 200 | `malformed_json_returns_parse_error_at_http_200` |
| Empty batch | `-32600 InvalidRequest` (per JSON-RPC §6) | `empty_batch_returns_invalid_request` |
| Nested batch | Rejected at parse, surfaces as `-32600` | `nested_batch_is_rejected_at_parse` |
| Server-side abuse via inbound `Response` frame | `-32600 InvalidRequest` with diagnostic | `server_rejects_inbound_response_frame` |
| Unknown method | `-32601 MethodNotFound` with method name in data | `unknown_method_returns_method_not_found` |
| Notification flood (no responses) | Returns `Accepted` (HTTP 202), no body produced — caller handles | `notification_returns_accepted`, `batch_of_only_notifications_returns_accepted` |
| Mixed batch with unidentifiable session | `initialize`-in-batch surfaces session id; first wins on multi-init | `batch_with_initialize_surfaces_session_id` |
| Correlation id forgery | `id` field preserved through every error path | `id_preserved_for_unknown_method_error` |

### Failure-mode containment

Serialisation fallback: if `serde_json::to_vec` ever fails (it
shouldn't on owned `Response` values), the pipeline falls back to a
hand-built `InternalError` envelope rather than panicking. Worst case
the body is empty and the route handler maps it to a 500. We never
crash the listener.

### What this layer DOES NOT do

- Does not bind to a port, parse HTTP, or call `axum`. That's the
  route handler's job.
- Does not authenticate the caller. Future PRs add this once OAuth 2.1
  / RFC 9700 BCP lands as `phase1/mcp-2026-oauth21`.
- Does not write OTel spans. Future PRs add `phase1/otel-genai-semconv`.

The route handler will be a thin wrapper:

```rust
async fn post_mcp(headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    let outcome = process_request(
        &body, headers.get("accept").and_then(|v| v.to_str().ok()),
        &state.mcp_config, &state.session_minter,
    );
    match outcome {
        ProcessOutcome::JsonRpcResponse { body, session_id } => {
            let mut resp = Response::new(body.into());
            *resp.status_mut() = StatusCode::OK;
            if let Some(sid) = session_id {
                resp.headers_mut().insert("Mcp-Session-Id", sid.as_str().parse().unwrap());
            }
            resp
        }
        ProcessOutcome::Accepted => Response::builder().status(202).body(Body::empty()).unwrap(),
        ProcessOutcome::PayloadTooLarge => Response::builder().status(413).body(Body::empty()).unwrap(),
        ProcessOutcome::NotAcceptable(msg) => {
            Response::builder().status(406).body(Body::from(msg)).unwrap()
        }
    }
}
```

## 3. Tests

- 18 new unit tests in `mcp::pipeline::tests` covering every threat
  in the table above plus happy paths for `initialize`, `ping`, and
  notifications.
- 328 router lib tests pass (was 310 — +18).
- `cargo clippy --all-targets -- -D warnings` clean.
- All 7 CI gates green.

## 4. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
