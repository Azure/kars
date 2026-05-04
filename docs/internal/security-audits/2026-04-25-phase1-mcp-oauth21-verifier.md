# Security Audit: `phase1/mcp-oauth21-verifier`

**Capability:** OAuth 2.1 access-token verifier for MCP 2025-03-26 / 2026
Streamable HTTP. Pure synchronous function transforming a bearer header
+ verifier config into a `VerifiedToken` or a structured rejection.

## 1. Summary

- New `inference-router/src/mcp/oauth.rs` (≈ 600 lines incl. tests).
- Public API: `verify_access_token(bearer_header, config) → Result<VerifiedToken, OAuthError>`.
- Built on the workspace's existing `jsonwebtoken = "10"` dep — no new
  crypto crates introduced. Enabled the `rust_crypto` feature flag on
  the workspace dep so v10's pluggable `CryptoProvider` is wired up
  (necessary even for ed25519/ECDSA verification — without a feature
  the lib panics at first use).
- Consumes JWKS material as input (`HashMap<issuer, JwkSet>`); the
  caller (route handler) is responsible for fetching/refreshing JWKS
  from the OAuth Authorization Server's discovery document. This
  module is purely a verifier — no I/O, no global state.

## 2. Threat model

Implements RFC 8725 ("JWT Best Current Practices") + RFC 9700 ("OAuth
2.0 Security BCP") for resource-server token validation.

| Threat | Mitigation | Test |
|---|---|---|
| `alg = "none"` injection (RFC 8725 §3.1) | `decode_header` rejects; `allowed_algorithms` doesn't include `none`; explicit `AlgNone` variant in error catalogue | `alg_none_rejected` |
| Algorithm confusion HS256-with-public-key (RFC 8725 §3.1) | HS256/HS384/HS512 hard-rejected at the head of the verifier even if a misconfigured allow-list permits them | `hs256_token_rejected_even_if_in_allow_list` |
| Algorithm not in caller-defined allow-list | Explicit allow-list comparison after the symmetric-rejection guard | `alg_not_in_allow_list_rejected` |
| `kid` forgery / key-substitution | `kid` treated as a hint; token is validated under the JWKS of the **claimed issuer** which is then re-checked against `Validation::iss` | `unknown_kid_rejected`, `signature_invalid_rejected` |
| Issuer-substitution / cross-trust-domain confusion | iss-from-payload is used to look up which JWKS to use; `Validation::set_issuer` re-verifies after signature; defence-in-depth `IssuerMismatch` check | `unknown_issuer_rejected`, `issuer_substitution_attack_rejected` |
| Audience-confusion | Required claim; exact-string match in `Validation::set_audience` and a defence-in-depth post-decode check; supports both string and array forms (RFC 7519 §4.1.3) | `audience_mismatch_rejected`, `audience_array_with_match_accepted` |
| Expired token / not-yet-valid | `exp` required; `nbf` optional but validated if present; configurable leeway (default 60 s, RFC 8725 §3.8) | `expired_token_rejected` |
| JWK alg/kid mismatch (RFC 8725 §3.5) | If the JWK pins `alg`, it must match the token's `alg`; mismatch is rejected before key construction | `jwk_alg_pin_mismatch_rejected` |
| Missing kid (key-rotation cleanliness) | `MissingKid` error — refuses tokens that can't be unambiguously routed to a key | `missing_kid_header_rejected` |
| Scope downgrade | All `required_scopes` must be present in the verified token | `required_scope_missing_rejected`, `required_scope_present_accepted` |
| Bearer-prefix abuse / wrong scheme | RFC 6750 case-insensitive `Bearer ` parser; `Basic`/other schemes rejected; bare-token form (some MCP clients pre-strip) supported | `missing_authorization_header_is_rejected`, `non_bearer_scheme_rejected`, `bare_token_without_bearer_prefix_accepted` |
| Malformed JWS structure | `decode_header` errors surface as `MalformedHeader(...)` with diagnostic | `malformed_token_rejected` |

### Out of scope (future PRs)

- DPoP / mTLS sender-constraint binding (`phase1/mcp-2026-dpop`).
- JWKS refresh / discovery doc fetch (`phase1/mcp-2026-jwks-cache`).
- Replay detection (`jti` cache) — the verifier surfaces `jti` claims
  through `VerifiedToken.claims` for any caller that wants to enforce.

### Total-function discipline

Every input — empty header, "Bearer ", garbage bytes, valid token,
tampered token, expired token — yields a structured `Result`. No
panic paths, no `unwrap`/`expect` on user data, no I/O. The single
test-helper `unwrap` in test code is gated behind `#[cfg(test)]`.

## 3. Why `jsonwebtoken` over rolling our own

Per §0.2 #8 of the implementation plan ("never roll our own crypto,
framing, or wire format"). `jsonwebtoken` is the de-facto standard
Rust JWT lib (used by Azure SDK, AWS SDK, every major Rust auth
service), audited, maintained, and already a dep in this workspace.
The verifier module uses only its public API:

- `decode_header` — parses + validates JWS header
- `DecodingKey::from_jwk` — constructs verifier key from JWK
- `Validation` — declarative claim checker
- `decode::<T>` — combined signature + claim verification

No private-key handling in this module — verification only.

## 4. Tests

- 19 new unit tests in `mcp::oauth::tests` (one was deduplicated from
  the original draft after a clippy refactor).
- 361 router lib tests pass (was 342 — +19).
- `cargo clippy --all-targets -- -D warnings` clean.
- All 7 CI gates green.

## 5. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
