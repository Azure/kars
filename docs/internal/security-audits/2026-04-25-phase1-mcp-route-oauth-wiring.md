# Security audit — MCP route OAuth 2.1 wiring (`protected_mcp_route`)

**Date:** 2026-04-25
**PR branch:** `phase1/mcp-route-oauth-wiring`
**Capability owner:** AzureClaw Phase 1 — MCP 2026 transport

## 1. Summary

Wires the `OAuthLayer` from PR 32 in front of the existing `mcp_route()`
JSON-RPC handler, exposing a new constructor:

```rust
pub fn protected_mcp_route(
    state: McpRouteState,
    oauth: Arc<OAuthVerifierConfig>,
) -> Router
```

Production deployments mount `protected_mcp_route(state, cfg)`; `azureclaw
dev` and the test suite continue to mount the bare `mcp_route()`. Selection
is deployment-time, driven by `McpServer.spec.productionMode` once the
controller wires it (Phase 2 — out of scope for this PR).

This is the first PR that puts an OAuth 2.1 token gate on a real wire-level
MCP endpoint. The layer enforces RFC 6750 §3 challenge semantics on every
non-conforming request — no fallthrough to the JSON-RPC pipeline.

## 2. Threat model delta

### Asset gaining new exposure

The `/mcp` POST surface, previously documented as "OAuth gating is the next
PR" (now this PR). No behavioural change for the bare route; the production
mount adds one verification step.

### STRIDE delta vs `docs/threat-model.md`

- **Spoofing (S)** — unauthenticated callers cannot reach the MCP pipeline.
  Verified: `protected_route_rejects_missing_bearer_with_401_and_challenge`,
  `protected_route_rejects_malformed_bearer_with_401`,
  `protected_route_rejects_token_signed_by_untrusted_key_with_401`.
- **Tampering (T)** — JWT signature verification is the layer's job; tampered
  tokens fail. Covered by the untrusted-key test.
- **Information disclosure (I)** — an unauthenticated GET returns 401 (not
  the bare route's 405 + `Allow: POST`); the layer fail-closes *before* the
  method matcher. Verified:
  `protected_route_rejects_get_with_401_before_method_check`.
- **Elevation of privilege (E)** — leaf handler runs only after layer
  attaches `VerifiedToken`; per-tool scope checks (Phase 2) read it via
  `Extension<VerifiedToken>`.

## 3. OWASP mapping

- **OWASP MCP05 — Authentication and Authorization Bypass:** the production
  mount makes the previously-undefended `/mcp` endpoint OAuth 2.1 gated.
- **OWASP LLM10 — Unbounded Consumption:** rate-limit policy still lives in
  AGT (`PolicyDecisionProvider`); this PR is auth, not throttling. Documented
  as a separate concern.
- **OWASP MCP02 — Token Theft / Replay:** layer rejects expired tokens via
  `OAuthVerifierConfig.leeway_seconds`; covered by `mcp::oauth` corpus.

## 4. AuthN / AuthZ path

```
client --(Bearer JWT)--> OAuthLayer --(VerifiedToken in extensions)-->
    mcp_route handler --(MCP JSON-RPC)--> pipeline::process_request
```

- `OAuthLayer::call` runs `verify_access_token` (PR 26).
- On `Err(_)`: returns 401 + `WWW-Authenticate: Bearer error="invalid_token", error_description="..."`.
  Inner service is **not** polled.
- On `Ok(VerifiedToken)`: token attached to `request.extensions_mut()`; inner
  service runs.
- Outage behaviour: the verifier holds JWKS in memory (provided to
  `OAuthVerifierConfig`); no network call per request, no AGT dependency, no
  `Strict`/`CachedRead`/`DegradedDev` ambiguity. The verifier is in-process
  and deterministic.

## 5. Secret + key custody

- The layer holds an `Arc<OAuthVerifierConfig>` containing JWKS public
  keys only. **No private keys, no secrets.**
- Tokens are read from the `Authorization` header and dropped after
  verification; only the parsed `VerifiedToken` (claims) survives in
  request extensions. No token logging.

## 6. Egress surface delta

None. The layer has zero outbound network calls.

## 7. Audit events emitted

This PR emits no audit events. AuditSink integration for OAuth verify
failures is tracked separately (`phase1/audit-sink-migrate-rest`).

The conformance corpus (`tests/conformance/oauth_*` — Phase 1 deliverable
not yet landed) will assert that auth failures are observable to operators
via the standard 401 path; runtime audit emission is a follow-up.

## 8. Failure mode

Default fail-closed:

| Input | Behaviour |
|---|---|
| Missing `Authorization` | 401 + RFC 6750 challenge |
| `Authorization` not `Bearer …` | 401 + RFC 6750 challenge |
| Bearer is malformed JWT | 401 + RFC 6750 challenge |
| Bearer signed by untrusted key | 401 + RFC 6750 challenge |
| Bearer expired / nbf in future / wrong aud | 401 (covered by `mcp::oauth`) |
| Bearer valid | inner handler runs; `Mcp-Session-Id` header preserved on 200 |

No fail-open path. No `outageMode` toggle (the verifier is in-process; there
is no external dependency to be "out").

## 9. Negative-test coverage

Five new in-tree tests in `inference-router/src/routes/mcp.rs`:

- `protected_route_rejects_missing_bearer_with_401_and_challenge`
- `protected_route_rejects_malformed_bearer_with_401`
- `protected_route_rejects_token_signed_by_untrusted_key_with_401`
- `protected_route_accepts_valid_bearer_and_returns_initialize_result`
- `protected_route_rejects_get_with_401_before_method_check`

Each builds an in-process `Router` via `protected_mcp_route(...)`, drives it
with `tower::ServiceExt::oneshot`, and asserts on status + headers. The
positive case additionally verifies that `Mcp-Session-Id` survives the
layer round-trip — a regression test against any future change that
inadvertently strips response headers.

The PR 32 `oauth_layer.rs` corpus (6 tests) exercises the layer in
isolation. This PR's tests exercise it bound to the production MCP
pipeline, locking the integration contract.

## 10. Vendored / third-party dependency delta

None. Reuses workspace dependencies (`axum`, `tower`, `jsonwebtoken`,
`ed25519-dalek`, `base64`, `serde_json`).

Sources consulted:

- RFC 6750 §3 (`WWW-Authenticate` challenge format) —
  <https://datatracker.ietf.org/doc/html/rfc6750#section-3>.
- MCP 2026-03-26 transport spec —
  <https://modelcontextprotocol.io/specification/2025-03-26/basic/transports>.
- `inference-router/src/mcp/oauth_layer.rs` (PR 32) for the layer contract.
- `inference-router/src/mcp/oauth.rs` (PR 26) for `OAuthVerifierConfig`
  field shape.

## 11. Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
