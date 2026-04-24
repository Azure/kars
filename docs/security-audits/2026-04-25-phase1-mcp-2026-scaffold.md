# Security Audit: `phase1/mcp-2026-scaffold`

**Capability:** scaffolds the `inference-router/src/mcp/` module with
type-level + framing-level primitives for MCP 2026 Streamable HTTP
transport. **No router routes are wired.** This PR establishes the
contracts and negative-test corpus that subsequent route-mounting and
OAuth-2.1 PRs will plug into.

**Type:** new module; no behaviour change to any existing endpoint.

## 1. Summary

New module `inference-router/src/mcp/` with three submodules:

- `mcp::error` — JSON-RPC 2.0 standard error code catalogue
  (`ParseError -32700`, `InvalidRequest -32600`, `MethodNotFound
  -32601`, `InvalidParams -32602`, `InternalError -32603`,
  implementation-defined `ServerError`) plus reserved-range checker
  for `-32768..=-32000`.
- `mcp::jsonrpc` — JSON-RPC 2.0 frame types (`Request`,
  `Notification`, `Response`, `Batch`) and `parse_frame()` parser.
  Strictly conforms to the JSON-RPC 2.0 spec: rejects any `jsonrpc`
  value other than `"2.0"`; rejects empty batches per §6; rejects
  nested batches; rejects `Response` frames carrying both `result`
  and `error`.
- `mcp::streamable_http` — `SessionId` newtype enforcing the spec's
  `0x21..=0x7E` visible-ASCII constraint at construction;
  `validate_accept_header()` for the `Accept` header negotiation;
  `MAX_FRAME_BYTES` constant (4 MiB default cap); pinned
  `MCP_PROTOCOL_VERSION = "2025-03-26"`.

All public functions are pure / side-effect-free. No I/O, no
logging, no global state.

## 2. Threat model delta

This PR adds **no new attack surface** — there are no new HTTP routes
mounted. The module is a library of validated types waiting to be used
by future route handlers.

What this PR does add: a hardened **type-level boundary**. When a
future PR calls `parse_frame(body)` or `SessionId::try_new(header)`,
malformed/tampered/oversized input is structurally impossible to pass
through without a `Result::Err`. This is the §0.2 #8 anti-pattern
prevention ("base64 wrapper pretending to be Signal", "router returns
200 but never called Content Safety") applied at the type system.

**STRIDE for the future routes that will use this module:**

- **Spoofing:** session-id newtype enforces the spec's wire-format
  constraint, preventing unauthenticated callers from injecting
  control characters into the session-id namespace.
- **Tampering:** parser rejects every spec-violating frame shape;
  there is no path for "almost-valid JSON-RPC" to be accepted.
- **Repudiation:** parser preserves the originating `id` field
  (`String` / `Number` / `Null`) so receipts and audit events can be
  bound to caller-supplied identifiers.
- **Information Disclosure:** error reasons are categorical
  (`InvalidJson(serde-error-text)`, `InvalidProtocolVersion(value)`).
  Future route handlers MUST map these to JSON-RPC `-32700` /
  `-32600` responses without leaking the inner text — same pattern
  as `routes::inference_policy::strict_error_reason()`.
- **Denial of Service:** `MAX_FRAME_BYTES = 4 MiB` is the
  defence-in-depth cap that future POST handlers MUST enforce
  *before* invoking the parser, bounding both memory and parse time.
- **Elevation of Privilege:** none. This module has no privileged
  operations.

## 3. OWASP mapping

- **OWASP MCP04 — Tool Definition Poisoning:** bounded once McpServer
  CRD lands; this scaffold is a prerequisite.
- **OWASP MCP01 — Prompt Injection via Tool Description:** the
  parser rejects malformed frames but does not opine on
  *content*-level injection — that's the policy/safety layer.
- **OWASP LLM06 — Excessive Agency:** policy gating is the existing
  mechanism (`PolicyDecisionProvider`); this module exposes the
  *transport* contract.

## 4. AuthN / AuthZ path

None in this PR. Authentication is the forthcoming
`phase1/mcp-2026-oauth21` PR — it will add an OAuth 2.1 token
verifier (PKCE-aware, audience-checked, expiry-checked,
replay-rejected) gated by `McpServer.spec.productionMode`.

## 5. Secret + key custody

No key material handled. The `SessionId` newtype is a wire-format
validator only; cryptographic minting (using `rand::rngs::OsRng`)
belongs to the `initialize` route handler in the next PR.

## 6. Egress surface delta

Zero. No new HTTP clients, no new endpoints, no new dependencies. The
module compiles against existing crates (`serde`, `serde_json`).

## 7. Audit events emitted

None in this PR. Future POST `/mcp` handlers will emit `mcp.request`
audit events via `AuditSink::append` — that wiring is part of
`phase1/mcp-2026-streamable-http-routes`.

## 8. Failure mode

**Fail-closed by construction.** Every parser entry point returns
`Result<_, ParseError>`. Every constructor on a wire-format newtype
returns `Result<_, _>`. There is no panic path on untrusted input.
There is no fall-through "best-effort accept" path.

## 9. Negative-test coverage

38 unit tests covering:

**`mcp::error`** (5 tests):
- Standard codes match spec values.
- Reserved range is inclusive `-32768..=-32000`.
- Implementation-defined `ServerError` codes fall in reserved range.
- `JsonRpcError` round-trips serialize/deserialize.
- Null `data` field is omitted on the wire (avoids the
  `"data": null` ambiguity).

**`mcp::jsonrpc`** (12 tests):
- Valid request / notification / response (result) / response (error)
  shapes parse.
- `jsonrpc != "2.0"` rejected (`"3.0"`, `"1.0"`, missing).
- Invalid JSON rejected with categorical error.
- Empty batch rejected per spec §6.
- Nested batch rejected.
- Response with both `result` and `error` rejected.
- Top-level non-object/non-array rejected.
- Mixed batch (request + notification) parses correctly.
- `id` field round-trips for `String` / `Number` / `Null`.

**`mcp::streamable_http`** (21 tests):
- Protocol version pinned to `"2025-03-26"`.
- Frame cap is exactly 4 MiB.
- `SessionId` accepts visible ASCII.
- `SessionId` accepts UUID-shaped strings.
- `SessionId` rejects empty input.
- `SessionId` rejects space (`0x20`), DEL (`0x7F`), NUL (`0x00`),
  newline (`\n`), and multi-byte UTF-8 (e.g. `é`).
- `SessionId::FromStr` round-trips.
- `Accept` header parsing handles: explicit both, `q=` parameters,
  case-insensitivity, only one of two, `*/*` wildcard, neither,
  empty.

These tests are the start of the §5.4 *behavioral conformance corpus*
for MCP 2026. Future route-mounting PRs add integration-level
conformance entries (e.g., `tampered-session-id-rejected.json`,
`oversized-frame-413.json`).

## 10. Vendored / third-party dependency delta

None. No `Cargo.toml` change, no `vendor/` change, no npm change. The
module uses only `serde` + `serde_json` which are already in the
workspace.

## 11. LOC budget impact

- New `inference-router/src/mcp/mod.rs`: ~75 LOC (mostly doc-comment).
- New `inference-router/src/mcp/error.rs`: ~125 LOC (50 prod + 75
  test).
- New `inference-router/src/mcp/jsonrpc.rs`: ~290 LOC (180 prod + 110
  test).
- New `inference-router/src/mcp/streamable_http.rs`: ~285 LOC (135
  prod + 150 test).
- All four files under the §4.2 800-LOC hard cap for new files.
- `inference-router/src/lib.rs`: +1 LOC (`pub mod mcp;`).
- No existing hot file touched.

## 12. Spec citation

JSON-RPC 2.0 spec: <https://www.jsonrpc.org/specification>
MCP 2025-03-26 transports:
<https://modelcontextprotocol.io/specification/2025-03-26/basic/transports>

The 2025-03-26 revision is the latest MCP spec at time of writing
(2026-04-25). Subsequent spec revisions are tracked under the
`phase1-mcp-2026` family branches; the version constant is centralised
in `streamable_http::MCP_PROTOCOL_VERSION` for easy bumping.

## 13. Sign-offs

Signed-off-by: GitHub Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
