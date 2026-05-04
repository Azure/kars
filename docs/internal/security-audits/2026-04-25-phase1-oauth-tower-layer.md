# Security audit — OAuth 2.1 bearer-token verification as a tower::Layer

**Date:** 2026-04-25
**PR branch:** `phase1/oauth-tower-layer`
**Capability owner:** AzureClaw Phase 1 — MCP 2026 / OAuth 2.1

## 1. Summary

Adds `inference-router/src/mcp/oauth_layer.rs` — a `tower::Layer` that
gates an inner axum router on bearer-token verification. The layer is the
HTTP-shaped wrapper around the in-tree pure-sync verifier
`mcp::oauth::verify_access_token` (which has 19 in-tree tests covering
RFC 7515 / RFC 8725 / RFC 9700 / RFC 6749 negative cases).

This is one of the three blockers for wiring the MCP route binding (PR 26)
and A2A route binding (PR 27) into `main.rs`. The other two —
SigningProvider plumbing and AuditSink emission — are independently
shipped pieces; this PR closes the OAuth piece.

The layer intentionally ships standalone: it does **not** modify any
existing route, does **not** wire into `main.rs`, and does **not** assume
any particular JWKS-fetch strategy. The caller hands an
`Arc<OAuthVerifierConfig>` to the layer constructor; how that arc is
materialised and refreshed is outside this PR's scope.

## 2. Threat model delta

### Asset gaining new exposure
None directly. The layer is an off-path module; it adds a new compilation
unit but no new HTTP surface, no persisted state, no network egress.

### STRIDE diff against `docs/threat-model.md`
- **Spoofing** (S2 — unauthenticated client poses as an MCP/A2A peer):
  the layer is the enforcement point that closes this for any router
  group it wraps. Without a wrapping layer, the routes fall back to
  whatever upstream auth they are deployed behind.
- **Information disclosure** (I2 — error messages leak verifier
  internal state): bounded by the `OAuthError::Display` impl, which is
  ASCII-only and never embeds bearer-token bytes, JWKS material, or
  claim contents. Adversarial token inputs cannot reflect arbitrary
  content into the `WWW-Authenticate` challenge — embedded double-
  quotes are stripped before formatting; `HeaderValue::from_str`
  failure falls back to a fixed `Bearer error="invalid_token"` string.
- **Denial of service** (D2 — slow / oversized token CPU cost): bounded
  by `verify_access_token` which decodes the JWT header before any
  signature operation; the existing in-tree tests cover oversized token
  envelopes via the underlying `decode_header` path.

## 3. OWASP mapping

- **OWASP MCP01 — Tool Definition / Discovery Manipulation:** the layer
  is the entrypoint that authorises every MCP request before any tool
  dispatch. Without it, the MCP route binding (PR 26) was effectively
  unauthenticated.
- **OWASP MCP05 — Authentication and Authorization Bypass:** the layer
  uniformly enforces RFC 6750 §3 challenge semantics. Missing
  `Authorization` header, malformed bearer scheme, untrusted-key-signed
  token, and invalid-signature token all collapse to a 401 with the
  same `WWW-Authenticate` challenge shape — no oracle distinguishes
  "not present" from "present but wrong".
- **OWASP LLM06 — Insecure Output Handling:** the `WWW-Authenticate`
  header value is constructed via `format!` with the
  `OAuthError::Display` output (plain ASCII), then fed through
  `HeaderValue::from_str` which validates the bytes; on failure we fall
  back to a fixed challenge string. No path produces an
  out-of-spec header value.

## 4. AuthN / AuthZ path

- **Caller:** any external HTTP client whose request lands on a router
  wrapped by `OAuthLayer`.
- **Identity proof:** RFC 6750 bearer token in the `Authorization`
  header, verified by `verify_access_token` (alg pinned to the
  configured allow-list, `kid` resolved against the issuer-trusted JWK
  set, audience exact-match, signature verified via `jsonwebtoken`,
  `exp` honoured with configured leeway, optional scopes enforced).
- **Outage behaviour:** `Strict` by construction. The layer has no
  cached-decision path and no fail-open path; if the
  `OAuthVerifierConfig`'s issuer map is empty, every token fails
  `UnknownIssuer`. Hot reload is achieved by replacing the `Arc<...>`
  the layer holds — explicitly out of scope for this PR.
- **Anonymous fall-through:** none. Every wrapped route requires a
  verifying token. Routes that should be unauthenticated (e.g.
  `/.well-known/agent.json`) must not be wrapped.

## 5. Secret + key custody

The layer stores **only public** verifier material (the issuer JWK sets).
No private keys are read or written. Agent (UID 1000) cannot read the
verifier config — the layer holds it inside the router process,
never written to disk in this PR.

## 6. Egress surface delta

None. The layer is purely incoming-request-side. JWKS fetching is
outside this PR's scope (the caller hands a pre-populated config).

## 7. Audit events emitted

None directly emitted by this PR. The downstream handler (after the
layer attaches `VerifiedToken` to extensions) is responsible for
emitting `mcp.request.allowed` / `a2a.request.allowed` events, which
will reference the audit doc for that route once SigningProvider /
AuditSink wiring lands.

The 401 path emits no audit event in this PR — matching the "no oracle"
property; emitting an event per failed auth would let an attacker flood
the audit chain. The router-level rate limit covers that surface.

## 8. Failure mode

Every failure mode lands **fail-closed** with a 401:

| Input | Outcome |
|-------|---------|
| Missing `Authorization` header | 401 `WWW-Authenticate: Bearer error="invalid_token", error_description="missing or malformed Authorization header (expected `Bearer <token>`)"` |
| Malformed bearer (not a JWT) | 401 with `MalformedHeader` description |
| `alg=none` token | 401 with `AlgNone` description |
| Untrusted issuer | 401 with `UnknownIssuer` description |
| Untrusted kid | 401 with `UnknownKid` description |
| Wrong key under known kid | 401 with `ValidationFailed` description |
| Expired token | 401 with `ValidationFailed` description |
| Wrong audience | 401 with `ValidationFailed` description |
| Missing required scope | 401 with `MissingScope` description |
| Adversarial header value (would-be-malformed) | 401 with bare `Bearer error="invalid_token"` fallback |

The success path attaches the verified token to
`request.extensions_mut()` and forwards to the inner service unchanged.

## 9. Negative-test coverage

In `inference-router/src/mcp/oauth_layer.rs::tests` (6 new in-tree
tests):

- `missing_authorization_header_yields_401_with_challenge`
- `malformed_bearer_yields_401`
- `token_signed_by_untrusted_key_yields_401`
- `valid_token_attaches_verified_token_to_request_extensions`
- `challenge_header_is_always_valid_ascii`
- `extension_extractor_helper_returns_token`

These are layer-shape tests. The full negative matrix for
`verify_access_token` is in `oauth.rs::tests` (19 tests, all green at
HEAD). The layer tests assert the wiring contract: 401 + challenge on
all reject paths, extensions installed on accept.

## 10. Vendored / third-party dependency delta

No new crates. Reuses existing workspace dependencies:
- `tower 0.5` (already in workspace)
- `axum 0.8` (already in workspace)
- `jsonwebtoken` (already used by `mcp::oauth`)

Sources consulted:
- RFC 6750 §3 (Bearer token `WWW-Authenticate` challenge format).
- RFC 6750 §3.1 (`error_description` parameter).
- RFC 7515, RFC 8725, RFC 9700 (already cited in `mcp::oauth`).
- tokio.rs blog 2021-05-14, "Pitfall #2: poll_ready", referenced in
  the tower service-clone pattern used in `OAuthService::call`.

## 11. Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
