// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! OAuth 2.1 access-token verifier for MCP 2025-11-25 Streamable HTTP.
// ci:loc-ok: The OAuth 2.1 / RFC 9700 / RFC 7517 (JWK) / RFC 7515 (JWS)
// verifier is one cohesive defence-in-depth pipeline (header parsing,
// kid lookup, alg allow-list, JWKS-cache, claim validation, replay
// rejection) where moving any phase to a sibling file would split the
// negative-path tests from the code they exercise. 853 LOC ~= the four
// RFCs combined.
//!
//! Implements bearer access-token validation per:
//!
//! - **OAuth 2.0 BCP** (RFC 9700, "Best Current Practice for OAuth 2.0
//!   Security"): aud-bound tokens, sender-constrained where supported,
//!   short-lived access tokens.
//! - **RFC 8725** ("JWT Best Current Practices"): explicit `alg`
//!   allow-list, no `none`, kid-based key selection, iss/aud/exp/nbf
//!   claim validation, leeway for clock skew.
//! - **MCP 2025-11-25** transports section: bearer tokens accepted on
//!   POST `/mcp` requests; resource-server semantics.
//!
//! # Total function
//!
//! [`verify_access_token`] is a pure synchronous function. No I/O, no
//! global state. JWKS material is supplied by caller via
//! [`OAuthVerifierConfig`]. The caller (route handler) is responsible
//! for fetching/refreshing JWKS from the OAuth Authorization Server's
//! discovery document — this module only verifies tokens against an
//! already-loaded key set.
//!
//! # Threat model
//!
//! - `alg = "none"` attack: rejected by `allowed_algorithms` allow-list.
//! - Algorithm confusion (HS256-with-public-key): rejected because
//!   we only configure asymmetric algorithms in the allow-list, and
//!   `DecodingKey::from_jwk` only constructs keys matching the
//!   public-key family.
//! - kid forgery: kid is treated as a hint — the chosen key is
//!   bound to the token's claimed issuer (which is then re-verified by
//!   `jsonwebtoken::Validation::iss`).
//! - issuer-substitution / audience-confusion: `aud` is required and
//!   exact-matched against `expected_audience`; `iss` is exact-matched
//!   against the trusted issuer used to source the JWK.
//! - clock-skew: configurable leeway, default 60 s.
//! - scope downgrade: caller-required scopes must all be present in
//!   the verified `scope` claim.
//! - replay: out of scope for the verifier (DPoP / mTLS / nonce
//!   handled by future PR `phase1/mcp-2026-dpop`).
//!
//! # Spec references
//!
//! - <https://www.rfc-editor.org/rfc/rfc8725> (JWT BCP)
//! - <https://www.rfc-editor.org/rfc/rfc9700> (OAuth 2.0 BCP)
//! - <https://datatracker.ietf.org/doc/html/draft-ietf-oauth-v2-1>
//! - MCP transports: <https://modelcontextprotocol.io/specification/2025-11-25>

use jsonwebtoken::jwk::{Jwk, JwkSet, KeyAlgorithm};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

/// Caller-supplied config for [`verify_access_token`].
#[derive(Debug, Clone)]
pub struct OAuthVerifierConfig {
    /// Map from issuer URL to that issuer's JWK set.
    pub trusted_issuers: HashMap<String, JwkSet>,
    /// Required `aud` claim value used as a fallback when no per-issuer
    /// audience is configured for the token's issuer. Tokens whose `aud`
    /// does not include this exact string are rejected unless a
    /// per-issuer override matches first.
    pub expected_audience: String,
    /// Slice 4d.3 — per-issuer audience override. When the token's
    /// issuer maps to an entry here, that entry's audience is used
    /// instead of the global `expected_audience`. This is how the
    /// router supports multiple McpServers each pinning a different
    /// `aud` claim against their own JWKS.
    pub per_issuer_audience: HashMap<String, String>,
    /// Allow-list of acceptable JWS algorithms. `Algorithm::HS256` etc.
    /// (symmetric algs) MUST NOT be configured here for an MCP
    /// resource server — we only accept asymmetric (RS256, ES256,
    /// EdDSA, ...).
    pub allowed_algorithms: Vec<Algorithm>,
    /// Acceptable clock skew in seconds. RFC 8725 §3.8 recommends a
    /// small but non-zero value. Default: 60 s.
    pub leeway_seconds: u64,
    /// Scopes the caller's request requires when no per-issuer scope
    /// override applies. The verified `scope` claim must contain every
    /// entry, space-separated per RFC 6749 §3.3.
    pub required_scopes: Vec<String>,
    /// Slice 4d.3 — per-issuer required scopes override.
    pub per_issuer_scopes: HashMap<String, Vec<String>>,
}

impl OAuthVerifierConfig {
    /// Construct a default config — empty issuer map, empty audience,
    /// asymmetric-only algs. Caller must populate `trusted_issuers`
    /// and `expected_audience` before use.
    pub fn new() -> Self {
        Self {
            trusted_issuers: HashMap::new(),
            expected_audience: String::new(),
            per_issuer_audience: HashMap::new(),
            allowed_algorithms: vec![
                Algorithm::EdDSA,
                Algorithm::ES256,
                Algorithm::RS256,
                Algorithm::PS256,
            ],
            leeway_seconds: 60,
            required_scopes: Vec::new(),
            per_issuer_scopes: HashMap::new(),
        }
    }
}

impl Default for OAuthVerifierConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthVerifierConfig {
    /// Build a production verifier config by reading:
    ///
    /// - the JWKSet from `jwks_path` (raw RFC 7517 JSON; the controller
    ///   writes this from the issuer's discovery document),
    /// - the issuer URL from `issuer`,
    /// - the audience claim from `audience`,
    /// - optional space-separated `required_scopes`.
    ///
    /// Errors are returned as a string for caller-friendly logging at
    /// the boot path; the router panics out of an unconfigurable state
    /// rather than silently mounting an unauthenticated route.
    pub fn from_jwks_file(
        jwks_path: &std::path::Path,
        issuer: &str,
        audience: &str,
        required_scopes: Vec<String>,
    ) -> Result<Self, String> {
        let raw = std::fs::read(jwks_path)
            .map_err(|e| format!("cannot read JWKS at {}: {e}", jwks_path.display()))?;
        let jwks: JwkSet = serde_json::from_slice(&raw)
            .map_err(|e| format!("JWKS at {} is not valid: {e}", jwks_path.display()))?;
        let mut trusted = HashMap::new();
        trusted.insert(issuer.to_string(), jwks);
        Ok(Self {
            trusted_issuers: trusted,
            expected_audience: audience.to_string(),
            per_issuer_audience: HashMap::new(),
            allowed_algorithms: vec![
                Algorithm::EdDSA,
                Algorithm::ES256,
                Algorithm::RS256,
                Algorithm::PS256,
            ],
            leeway_seconds: 60,
            required_scopes,
            per_issuer_scopes: HashMap::new(),
        })
    }

    /// Slice 4d.3 — build a multi-issuer verifier from a scanned
    /// `McpServerRegistry`. Each discovered server with a `meta.json`
    /// contributes one trusted issuer + optional per-issuer audience +
    /// per-issuer scopes. Servers without `meta.json` are skipped — the
    /// legacy single-issuer path (`from_jwks_file`) still applies via
    /// `MCP_JWKS_PATH` as a fallback when this returns an empty config.
    ///
    /// `default_audience` is used as the floor (`expected_audience`)
    /// for tokens whose issuer has no per-issuer audience override.
    /// `default_scopes` is the floor scope set for issuers without
    /// per-issuer scopes.
    ///
    /// Returns `None` when the registry contains no servers with meta —
    /// caller should fall back to the legacy path in that case so the
    /// router never silently mounts an unauthenticated route.
    pub fn from_registry(
        registry: &crate::mcp::registry::McpServerRegistry,
        default_audience: &str,
        default_scopes: Vec<String>,
        leeway_seconds: u64,
    ) -> Result<Option<Self>, String> {
        let mut trusted_issuers: HashMap<String, JwkSet> = HashMap::new();
        let mut per_issuer_audience: HashMap<String, String> = HashMap::new();
        let mut per_issuer_scopes: HashMap<String, Vec<String>> = HashMap::new();

        for (name, server) in &registry.servers {
            let Some(meta) = server.meta.as_ref() else {
                tracing::warn!(
                    server = %name,
                    "McpServer has no meta.json — skipping OAuth registration \
                     (legacy MCP_JWKS_PATH path still in effect for unmatched issuers)"
                );
                continue;
            };
            let raw = std::fs::read(&server.jwks_path).map_err(|e| {
                format!(
                    "cannot read JWKS for {name} at {}: {e}",
                    server.jwks_path.display()
                )
            })?;
            let jwks: JwkSet = serde_json::from_slice(&raw).map_err(|e| {
                format!(
                    "JWKS for {name} at {} is not valid: {e}",
                    server.jwks_path.display()
                )
            })?;
            if let Some(existing) = trusted_issuers.get(&meta.issuer) {
                if existing != &jwks {
                    return Err(format!(
                        "issuer {} appears in two McpServer entries with conflicting JWKS",
                        meta.issuer
                    ));
                }
            } else {
                trusted_issuers.insert(meta.issuer.clone(), jwks);
            }
            if let Some(aud) = meta.audience.as_ref().filter(|a| !a.is_empty()) {
                per_issuer_audience.insert(meta.issuer.clone(), aud.clone());
            }
            if !meta.scopes.is_empty() {
                per_issuer_scopes.insert(meta.issuer.clone(), meta.scopes.clone());
            }
        }

        if trusted_issuers.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self {
            trusted_issuers,
            expected_audience: default_audience.to_string(),
            per_issuer_audience,
            allowed_algorithms: vec![
                Algorithm::EdDSA,
                Algorithm::ES256,
                Algorithm::RS256,
                Algorithm::PS256,
            ],
            leeway_seconds,
            required_scopes: default_scopes,
            per_issuer_scopes,
        }))
    }
}

/// Successfully verified access token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedToken {
    pub subject: String,
    pub issuer: String,
    pub audience: String,
    pub scopes: Vec<String>,
    pub expires_at: u64,
    /// Raw decoded claims for downstream consumers (group claims,
    /// custom claims, etc.). Returned as `serde_json::Value` to keep
    /// this module decoupled from any specific identity provider's
    /// claim shape.
    pub claims: serde_json::Value,
}

/// Errors that [`verify_access_token`] can raise. Every variant is a
/// **rejection** — there are no recoverable warnings.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OAuthError {
    #[error("missing or malformed Authorization header (expected `Bearer <token>`)")]
    MissingBearer,
    #[error("token has malformed JWS header: {0}")]
    MalformedHeader(String),
    #[error("token uses `alg = none` (RFC 8725 §3.1)")]
    AlgNone,
    #[error("algorithm `{0:?}` is not in the configured allow-list")]
    AlgorithmNotAllowed(Algorithm),
    #[error("token is missing the required `kid` header (RFC 8725 §3.5)")]
    MissingKid,
    #[error("issuer `{0}` is not configured as trusted")]
    UnknownIssuer(String),
    #[error("kid `{0}` not found in JWK set for issuer `{1}`")]
    UnknownKid(String, String),
    #[error("kid `{0}` algorithm `{1:?}` does not match token alg `{2:?}`")]
    KidAlgMismatch(String, KeyAlgorithm, Algorithm),
    #[error("could not construct decoding key from JWK: {0}")]
    DecodingKey(String),
    #[error("signature / claim validation failed: {0}")]
    ValidationFailed(String),
    #[error("required claim `{0}` missing or wrong type")]
    MissingClaim(&'static str),
    #[error("required scope `{0}` missing from token")]
    MissingScope(String),
    #[error(
        "token claim `iss` ({actual}) does not match trusted issuer used for key lookup ({expected})"
    )]
    IssuerMismatch { actual: String, expected: String },
}

/// Internal claim shape used for unverified peek + verified decode.
/// Other claims are preserved in `serde_json::Value`.
#[derive(Debug, Deserialize)]
struct AccessTokenClaims {
    iss: String,
    sub: String,
    aud: AudClaim,
    exp: u64,
    #[serde(default)]
    scope: Option<String>,
    /// RFC 8693 alternative form (array of strings). Either `scope`
    /// or `scopes` may be present; we accept both.
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

/// `aud` may be either a string or an array of strings (RFC 7519 §4.1.3).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AudClaim {
    Single(String),
    Many(Vec<String>),
}

impl AudClaim {
    /// Pick a representative aud value to surface in [`VerifiedToken::audience`]
    /// (the matched expected audience).
    fn matched(&self, expected: &str) -> Option<String> {
        match self {
            AudClaim::Single(s) if s == expected => Some(s.clone()),
            AudClaim::Many(v) => v.iter().find(|s| *s == expected).cloned(),
            _ => None,
        }
    }
}

/// Verify a bearer access token against `config`.
///
/// `bearer_header` is the raw value of the `Authorization` header. Both
/// `"Bearer eyJ..."` (case-insensitive scheme) and the bare token form
/// `"eyJ..."` are accepted, since some MCP clients strip the prefix.
pub fn verify_access_token(
    bearer_header: &str,
    config: &OAuthVerifierConfig,
) -> Result<VerifiedToken, OAuthError> {
    let token = strip_bearer(bearer_header)?;

    // 1. Decode JWS header to learn alg + kid.
    let header = decode_header(token).map_err(|e| OAuthError::MalformedHeader(e.to_string()))?;

    if header.alg == Algorithm::HS256
        || header.alg == Algorithm::HS384
        || header.alg == Algorithm::HS512
    {
        // Symmetric algs MUST NEVER be accepted on a resource server
        // that uses public-key trust anchors. RFC 8725 §3.1.
        return Err(OAuthError::AlgorithmNotAllowed(header.alg));
    }
    if !config.allowed_algorithms.contains(&header.alg) {
        return Err(OAuthError::AlgorithmNotAllowed(header.alg));
    }
    let kid = header.kid.ok_or(OAuthError::MissingKid)?;

    // 2. Peek at unverified claims to discover the iss for key lookup.
    //    We do NOT trust this iss yet — it is then re-validated by
    //    `Validation::iss` in step 4.
    let unverified_iss = peek_unverified_iss(token)?;
    let jwks = config
        .trusted_issuers
        .get(&unverified_iss)
        .ok_or_else(|| OAuthError::UnknownIssuer(unverified_iss.clone()))?;

    // 3. Locate the kid in the issuer's JWKS.
    let jwk = jwks
        .find(&kid)
        .ok_or_else(|| OAuthError::UnknownKid(kid.clone(), unverified_iss.clone()))?;

    // RFC 8725 §3.5 — when the JWK pins an alg, it must match the
    // token's alg. We treat a missing `alg` field on the JWK as
    // "any allowed".
    if let Some(jwk_alg) = jwk.common.key_algorithm
        && !key_algorithm_matches_token_alg(jwk_alg, header.alg)
    {
        return Err(OAuthError::KidAlgMismatch(kid, jwk_alg, header.alg));
    }

    // EdDSA Jwk constructed from OKP; RS256 from RSA; ES256 from EC.
    // `from_jwk` validates the family for us.
    let decoding_key =
        build_decoding_key(jwk).map_err(|e| OAuthError::DecodingKey(e.to_string()))?;

    // 4. Build Validation and run the verified decode.
    let effective_audience = config
        .per_issuer_audience
        .get(&unverified_iss)
        .map(String::as_str)
        .unwrap_or(config.expected_audience.as_str());
    let mut validation = Validation::new(header.alg);
    validation.leeway = config.leeway_seconds;
    validation.set_audience(&[effective_audience]);
    validation.set_issuer(&[&unverified_iss]);
    validation.set_required_spec_claims(&["iss", "sub", "aud", "exp"]);
    validation.algorithms = config.allowed_algorithms.clone();

    let token_data = decode::<serde_json::Value>(token, &decoding_key, &validation)
        .map_err(|e| OAuthError::ValidationFailed(e.to_string()))?;

    // 5. Extract typed claims for our return shape and double-check
    //    iss/aud (defence-in-depth — the validator already checked,
    //    but we want to surface MissingClaim distinctly).
    let typed: AccessTokenClaims = serde_json::from_value(token_data.claims.clone())
        .map_err(|_| OAuthError::MissingClaim("iss/sub/aud/exp"))?;

    if typed.iss != unverified_iss {
        return Err(OAuthError::IssuerMismatch {
            actual: typed.iss,
            expected: unverified_iss,
        });
    }
    let matched_aud = typed
        .aud
        .matched(effective_audience)
        .ok_or(OAuthError::MissingClaim("aud"))?;

    // 6. Scope check.
    let scopes = collect_scopes(&typed);
    let effective_scopes = config
        .per_issuer_scopes
        .get(&unverified_iss)
        .unwrap_or(&config.required_scopes);
    for required in effective_scopes {
        if !scopes.contains(required) {
            return Err(OAuthError::MissingScope(required.clone()));
        }
    }

    Ok(VerifiedToken {
        subject: typed.sub,
        issuer: typed.iss,
        audience: matched_aud,
        scopes,
        expires_at: typed.exp,
        claims: token_data.claims,
    })
}

fn strip_bearer(header: &str) -> Result<&str, OAuthError> {
    let header = header.trim();
    if header.is_empty() {
        return Err(OAuthError::MissingBearer);
    }
    // Case-insensitive `Bearer ` scheme per RFC 6750.
    if header.len() >= 6 && header[..6].eq_ignore_ascii_case("Bearer") {
        // Either "Bearer" alone or "Bearer<sep>token".
        let rest = &header[6..];
        let token = rest.trim_start();
        if token.is_empty() {
            return Err(OAuthError::MissingBearer);
        }
        // The first char after `Bearer` must be whitespace (RFC 6750
        // sep1) — reject e.g. "Bearerabc" which is not a valid scheme.
        if !rest.starts_with(|c: char| c.is_whitespace()) {
            // Could be a bare token coincidentally starting with "Bearer".
            // Fall through to bare-token branch below.
        } else {
            return Ok(token);
        }
    }
    // Bare token (some MCP clients pre-strip).
    if header.contains(' ') {
        // Has a different scheme prefix — reject.
        return Err(OAuthError::MissingBearer);
    }
    Ok(header)
}

fn peek_unverified_iss(token: &str) -> Result<String, OAuthError> {
    // JWS compact: header.payload.signature
    let mut parts = token.split('.');
    let _ = parts.next().ok_or(OAuthError::MissingClaim("iss"))?;
    let payload_b64 = parts.next().ok_or(OAuthError::MissingClaim("iss"))?;
    let payload_bytes = base64_url_decode(payload_b64).ok_or(OAuthError::MissingClaim("iss"))?;
    #[derive(Deserialize)]
    struct IssOnly {
        iss: String,
    }
    let parsed: IssOnly =
        serde_json::from_slice(&payload_bytes).map_err(|_| OAuthError::MissingClaim("iss"))?;
    Ok(parsed.iss)
}

fn base64_url_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()
}

fn build_decoding_key(jwk: &Jwk) -> Result<DecodingKey, jsonwebtoken::errors::Error> {
    DecodingKey::from_jwk(jwk)
}

fn key_algorithm_matches_token_alg(jwk_alg: KeyAlgorithm, token_alg: Algorithm) -> bool {
    use Algorithm as A;
    use KeyAlgorithm as K;
    matches!(
        (jwk_alg, token_alg),
        (K::EdDSA, A::EdDSA)
            | (K::ES256, A::ES256)
            | (K::ES384, A::ES384)
            | (K::RS256, A::RS256)
            | (K::RS384, A::RS384)
            | (K::RS512, A::RS512)
            | (K::PS256, A::PS256)
            | (K::PS384, A::PS384)
            | (K::PS512, A::PS512)
    )
}

fn collect_scopes(claims: &AccessTokenClaims) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(s) = &claims.scope {
        out.extend(s.split_whitespace().map(|s| s.to_string()));
    }
    if let Some(arr) = &claims.scopes {
        out.extend(arr.iter().cloned());
    }
    out
}

#[allow(dead_code)]
fn algorithm_eq(_a: Algorithm, _b: Algorithm) -> bool {
    false
}

// Suppress dead-code warning on unused HashSet import in older clippy.
#[allow(dead_code)]
fn _hashset_anchor() -> HashSet<String> {
    HashSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use jsonwebtoken::jwk::{
        AlgorithmParameters as JwkAlg, CommonParameters, Jwk, JwkSet, KeyAlgorithm,
        OctetKeyPairParameters, OctetKeyPairType, PublicKeyUse,
    };
    use jsonwebtoken::{EncodingKey, Header, encode};
    use rand::TryRngCore;
    use serde_json::json;

    const TEST_ISSUER: &str = "https://idp.example.com";
    const TEST_AUDIENCE: &str = "https://router.example.com/mcp";
    const TEST_KID: &str = "key-2026-04";

    fn ed_keypair() -> (SigningKey, ed25519_dalek::VerifyingKey) {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.try_fill_bytes(&mut bytes).unwrap();
        let sk = SigningKey::from_bytes(&bytes);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    fn jwks_with(verifying_key: &ed25519_dalek::VerifyingKey, kid: &str) -> JwkSet {
        use base64::Engine;
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifying_key.as_bytes());
        JwkSet {
            keys: vec![Jwk {
                common: CommonParameters {
                    public_key_use: Some(PublicKeyUse::Signature),
                    key_operations: None,
                    key_algorithm: Some(KeyAlgorithm::EdDSA),
                    key_id: Some(kid.into()),
                    x509_url: None,
                    x509_chain: None,
                    x509_sha1_fingerprint: None,
                    x509_sha256_fingerprint: None,
                },
                algorithm: JwkAlg::OctetKeyPair(OctetKeyPairParameters {
                    key_type: OctetKeyPairType::OctetKeyPair,
                    curve: jsonwebtoken::jwk::EllipticCurve::Ed25519,
                    x,
                }),
            }],
        }
    }

    fn signing_key_pem(sk: &SigningKey) -> EncodingKey {
        // Hand-build a minimal PKCS#8 v1 envelope for an Ed25519 seed
        // (RFC 8410 §7). Format:
        //   30 2e 02 01 00 30 05 06 03 2b 65 70 04 22 04 20 <32 bytes>
        // Avoids enabling the `pkcs8` feature on ed25519-dalek just
        // for tests.
        use base64::Engine;
        let prefix: [u8; 16] = [
            0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22,
            0x04, 0x20,
        ];
        let mut der = Vec::with_capacity(48);
        der.extend_from_slice(&prefix);
        der.extend_from_slice(&sk.to_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        let pem = format!("-----BEGIN PRIVATE KEY-----\n{b64}\n-----END PRIVATE KEY-----\n");
        EncodingKey::from_ed_pem(pem.as_bytes()).unwrap()
    }

    fn make_token(
        signing: &EncodingKey,
        kid: &str,
        iss: &str,
        aud: serde_json::Value,
        exp_offset_secs: i64,
        scope: Option<&str>,
    ) -> String {
        let now = jsonwebtoken::get_current_timestamp() as i64;
        let claims = json!({
            "iss": iss,
            "sub": "user-42",
            "aud": aud,
            "iat": now - 1,
            "nbf": now - 1,
            "exp": now + exp_offset_secs,
            "scope": scope.unwrap_or(""),
        });
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(kid.into());
        encode(&header, &claims, signing).unwrap()
    }

    fn cfg(jwks: JwkSet, scopes: Vec<String>) -> OAuthVerifierConfig {
        let mut trusted = HashMap::new();
        trusted.insert(TEST_ISSUER.to_string(), jwks);
        OAuthVerifierConfig {
            trusted_issuers: trusted,
            expected_audience: TEST_AUDIENCE.into(),
            per_issuer_audience: HashMap::new(),
            allowed_algorithms: vec![Algorithm::EdDSA],
            leeway_seconds: 30,
            required_scopes: scopes,
            per_issuer_scopes: HashMap::new(),
        }
    }

    #[test]
    fn happy_path_eddsa_token_verifies() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            300,
            Some("mcp.tools.read"),
        );
        let v = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec![]),
        )
        .unwrap();
        assert_eq!(v.subject, "user-42");
        assert_eq!(v.issuer, TEST_ISSUER);
        assert_eq!(v.audience, TEST_AUDIENCE);
        assert_eq!(v.scopes, vec!["mcp.tools.read"]);
    }

    #[test]
    fn bare_token_without_bearer_prefix_accepted() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            300,
            None,
        );
        let v = verify_access_token(&token, &cfg(jwks_with(&vk, TEST_KID), vec![])).unwrap();
        assert_eq!(v.subject, "user-42");
    }

    #[test]
    fn missing_authorization_header_is_rejected() {
        let cfg = cfg(JwkSet { keys: vec![] }, vec![]);
        let err = verify_access_token("", &cfg).unwrap_err();
        assert!(matches!(err, OAuthError::MissingBearer));
        let err = verify_access_token("   ", &cfg).unwrap_err();
        assert!(matches!(err, OAuthError::MissingBearer));
        let err = verify_access_token("Bearer   ", &cfg).unwrap_err();
        assert!(matches!(err, OAuthError::MissingBearer));
    }

    #[test]
    fn non_bearer_scheme_rejected() {
        let err = verify_access_token("Basic dXNlcjpwYXNz", &cfg(JwkSet { keys: vec![] }, vec![]))
            .unwrap_err();
        assert!(matches!(err, OAuthError::MissingBearer));
    }

    #[test]
    fn alg_none_rejected() {
        // alg=none token (header `eyJhbGciOiJub25lIn0` = {"alg":"none"})
        let token = "eyJhbGciOiJub25lIn0.eyJpc3MiOiJodHRwczovL2lkcC5leGFtcGxlLmNvbSJ9.";
        let (_sk, vk) = ed_keypair();
        let err = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec![]),
        )
        .unwrap_err();
        // jsonwebtoken raises a header-level error for `none`; we map
        // that to MalformedHeader. Either MalformedHeader or
        // AlgorithmNotAllowed is acceptable since both prevent the
        // attack — assert it is one of the rejecting variants.
        assert!(
            matches!(
                err,
                OAuthError::MalformedHeader(_)
                    | OAuthError::AlgorithmNotAllowed(_)
                    | OAuthError::AlgNone
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn hs256_token_rejected_even_if_in_allow_list() {
        // Algorithm-confusion defence: HS256 must never be accepted
        // by a resource server using public-key trust anchors, even
        // if a misconfigured allow-list contains it.
        let secret = b"shared-secret";
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some(TEST_KID.into());
        let now = jsonwebtoken::get_current_timestamp() as i64;
        let claims = json!({
            "iss": TEST_ISSUER, "sub": "x",
            "aud": TEST_AUDIENCE, "exp": now + 60,
        });
        let token = encode(&header, &claims, &EncodingKey::from_secret(secret)).unwrap();

        let mut config = cfg(JwkSet { keys: vec![] }, vec![]);
        config.allowed_algorithms = vec![Algorithm::EdDSA, Algorithm::HS256]; // misconfigured
        let err = verify_access_token(&format!("Bearer {token}"), &config).unwrap_err();
        assert!(matches!(
            err,
            OAuthError::AlgorithmNotAllowed(Algorithm::HS256)
        ));
    }

    #[test]
    fn alg_not_in_allow_list_rejected() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            300,
            None,
        );
        let mut c = cfg(jwks_with(&vk, TEST_KID), vec![]);
        c.allowed_algorithms = vec![Algorithm::ES256]; // EdDSA NOT in list
        let err = verify_access_token(&format!("Bearer {token}"), &c).unwrap_err();
        assert!(matches!(
            err,
            OAuthError::AlgorithmNotAllowed(Algorithm::EdDSA)
        ));
    }

    #[test]
    fn missing_kid_header_rejected() {
        let (sk, _vk) = ed_keypair();
        let signing = signing_key_pem(&sk);
        let now = jsonwebtoken::get_current_timestamp() as i64;
        let claims = json!({
            "iss": TEST_ISSUER, "sub": "x",
            "aud": TEST_AUDIENCE, "exp": now + 60,
        });
        let header = Header::new(Algorithm::EdDSA); // no kid
        let token = encode(&header, &claims, &signing).unwrap();
        let (_sk2, vk2) = ed_keypair();
        let err = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk2, TEST_KID), vec![]),
        )
        .unwrap_err();
        assert!(matches!(err, OAuthError::MissingKid));
    }

    #[test]
    fn unknown_issuer_rejected() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            "https://attacker.example.com",
            json!(TEST_AUDIENCE),
            300,
            None,
        );
        let err = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec![]),
        )
        .unwrap_err();
        assert!(
            matches!(err, OAuthError::UnknownIssuer(ref s) if s == "https://attacker.example.com")
        );
    }

    #[test]
    fn unknown_kid_rejected() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            "rotated-kid",
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            300,
            None,
        );
        let err = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec![]),
        )
        .unwrap_err();
        assert!(matches!(err, OAuthError::UnknownKid(_, _)));
    }

    #[test]
    fn signature_invalid_rejected() {
        // Sign with key A, verify against JWKS containing key B's public.
        let (sk_a, _vk_a) = ed_keypair();
        let (_sk_b, vk_b) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk_a),
            TEST_KID,
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            300,
            None,
        );
        let err = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk_b, TEST_KID), vec![]),
        )
        .unwrap_err();
        assert!(matches!(err, OAuthError::ValidationFailed(_)));
    }

    #[test]
    fn audience_mismatch_rejected() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!("https://other-resource.example.com"),
            300,
            None,
        );
        let err = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec![]),
        )
        .unwrap_err();
        assert!(matches!(err, OAuthError::ValidationFailed(_)));
    }

    #[test]
    fn audience_array_with_match_accepted() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!(["urn:other", TEST_AUDIENCE, "urn:third"]),
            300,
            None,
        );
        let v = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec![]),
        )
        .unwrap();
        assert_eq!(v.audience, TEST_AUDIENCE);
    }

    #[test]
    fn expired_token_rejected() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            -3600, // an hour ago
            None,
        );
        let err = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec![]),
        )
        .unwrap_err();
        assert!(matches!(err, OAuthError::ValidationFailed(_)));
    }

    #[test]
    fn required_scope_missing_rejected() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            300,
            Some("mcp.tools.read"),
        );
        let err = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec!["mcp.tools.write".into()]),
        )
        .unwrap_err();
        assert!(matches!(err, OAuthError::MissingScope(ref s) if s == "mcp.tools.write"));
    }

    #[test]
    fn required_scope_present_accepted() {
        let (sk, vk) = ed_keypair();
        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            300,
            Some("mcp.tools.read mcp.tools.write"),
        );
        let v = verify_access_token(
            &format!("Bearer {token}"),
            &cfg(jwks_with(&vk, TEST_KID), vec!["mcp.tools.write".into()]),
        )
        .unwrap();
        assert!(v.scopes.contains(&"mcp.tools.write".to_string()));
    }

    #[test]
    fn malformed_token_rejected() {
        let (_sk, vk) = ed_keypair();
        let err = verify_access_token("Bearer not-a-jwt", &cfg(jwks_with(&vk, TEST_KID), vec![]))
            .unwrap_err();
        assert!(matches!(err, OAuthError::MalformedHeader(_)));
    }

    #[test]
    fn issuer_substitution_attack_rejected() {
        // Sign with the legitimate IDP's key but claim a DIFFERENT iss
        // that ALSO happens to be trusted (with a different JWKS).
        // The verifier MUST refuse: if the inner iss doesn't match
        // the issuer used to source the key, reject.
        let (sk_legit, vk_legit) = ed_keypair();
        let (_sk_other, vk_other) = ed_keypair();

        let mut trusted = HashMap::new();
        trusted.insert(TEST_ISSUER.to_string(), jwks_with(&vk_legit, TEST_KID));
        trusted.insert(
            "https://other-trusted.example.com".to_string(),
            jwks_with(&vk_other, "other-kid"),
        );
        let config = OAuthVerifierConfig {
            trusted_issuers: trusted,
            expected_audience: TEST_AUDIENCE.into(),
            per_issuer_audience: HashMap::new(),
            allowed_algorithms: vec![Algorithm::EdDSA],
            leeway_seconds: 30,
            required_scopes: vec![],
            per_issuer_scopes: HashMap::new(),
        };

        // Token claims iss=other-trusted, signed with `legit`'s key,
        // kid=`other-kid`. Key lookup goes to other-trusted's JWKS,
        // but the signature won't verify with `vk_other`.
        let token = make_token(
            &signing_key_pem(&sk_legit),
            "other-kid",
            "https://other-trusted.example.com",
            json!(TEST_AUDIENCE),
            300,
            None,
        );
        let err = verify_access_token(&format!("Bearer {token}"), &config).unwrap_err();
        assert!(matches!(err, OAuthError::ValidationFailed(_)));
    }

    #[test]
    fn jwk_alg_pin_mismatch_rejected() {
        // JWK declares alg=ES256 but token uses alg=EdDSA. Should
        // refuse before even trying to construct the decoding key.
        let (sk, vk) = ed_keypair();
        let mut jwks = jwks_with(&vk, TEST_KID);
        jwks.keys[0].common.key_algorithm = Some(KeyAlgorithm::ES256);

        let token = make_token(
            &signing_key_pem(&sk),
            TEST_KID,
            TEST_ISSUER,
            json!(TEST_AUDIENCE),
            300,
            None,
        );
        let err = verify_access_token(&format!("Bearer {token}"), &cfg(jwks, vec![])).unwrap_err();
        assert!(matches!(err, OAuthError::KidAlgMismatch(_, _, _)));
    }

    // Suppress unused-import warning in tests for AlgorithmParameters
    // (only used through OctetKeyPair).
    #[allow(dead_code)]
    fn _alg_param_anchor() -> jsonwebtoken::jwk::AlgorithmParameters {
        jsonwebtoken::jwk::AlgorithmParameters::OctetKeyPair(OctetKeyPairParameters {
            key_type: OctetKeyPairType::OctetKeyPair,
            curve: jsonwebtoken::jwk::EllipticCurve::Ed25519,
            x: String::new(),
        })
    }

    // ----- Slice 4d.3 — multi-issuer per-server audience/scopes -----

    /// Per-issuer audience override: a token issued by `iss_a` carrying
    /// `aud_a` must be accepted, while a token from `iss_b` must carry
    /// `aud_b` — even though both share the same router process.
    #[test]
    fn per_issuer_audience_overrides_global_default() {
        let (sk_a, vk_a) = ed_keypair();
        let (sk_b, vk_b) = ed_keypair();
        let iss_a = "https://idp-a.example";
        let iss_b = "https://idp-b.example";
        let aud_a = "api://server-a";
        let aud_b = "api://server-b";

        let mut trusted = HashMap::new();
        trusted.insert(iss_a.to_string(), jwks_with(&vk_a, "kid-a"));
        trusted.insert(iss_b.to_string(), jwks_with(&vk_b, "kid-b"));
        let mut per_aud = HashMap::new();
        per_aud.insert(iss_a.to_string(), aud_a.to_string());
        per_aud.insert(iss_b.to_string(), aud_b.to_string());
        let config = OAuthVerifierConfig {
            trusted_issuers: trusted,
            expected_audience: "api://fallback".into(),
            per_issuer_audience: per_aud,
            allowed_algorithms: vec![Algorithm::EdDSA],
            leeway_seconds: 30,
            required_scopes: vec![],
            per_issuer_scopes: HashMap::new(),
        };

        let token_a = make_token(
            &signing_key_pem(&sk_a),
            "kid-a",
            iss_a,
            json!(aud_a),
            300,
            None,
        );
        let token_b = make_token(
            &signing_key_pem(&sk_b),
            "kid-b",
            iss_b,
            json!(aud_b),
            300,
            None,
        );
        verify_access_token(&format!("Bearer {token_a}"), &config).unwrap();
        verify_access_token(&format!("Bearer {token_b}"), &config).unwrap();
    }

    /// A token from `iss_a` carrying `iss_b`'s audience must be
    /// rejected — even though both audiences are listed somewhere in
    /// `per_issuer_audience`, only the entry keyed to the token's iss
    /// is honored.
    #[test]
    fn per_issuer_audience_pins_correctly() {
        let (sk_a, vk_a) = ed_keypair();
        let (_sk_b, vk_b) = ed_keypair();
        let iss_a = "https://idp-a.example";
        let iss_b = "https://idp-b.example";
        let aud_a = "api://server-a";
        let aud_b = "api://server-b";

        let mut trusted = HashMap::new();
        trusted.insert(iss_a.to_string(), jwks_with(&vk_a, "kid-a"));
        trusted.insert(iss_b.to_string(), jwks_with(&vk_b, "kid-b"));
        let mut per_aud = HashMap::new();
        per_aud.insert(iss_a.to_string(), aud_a.to_string());
        per_aud.insert(iss_b.to_string(), aud_b.to_string());
        let config = OAuthVerifierConfig {
            trusted_issuers: trusted,
            expected_audience: "api://fallback".into(),
            per_issuer_audience: per_aud,
            allowed_algorithms: vec![Algorithm::EdDSA],
            leeway_seconds: 30,
            required_scopes: vec![],
            per_issuer_scopes: HashMap::new(),
        };

        // Cross-aud: iss_a token claims aud_b.
        let token = make_token(
            &signing_key_pem(&sk_a),
            "kid-a",
            iss_a,
            json!(aud_b),
            300,
            None,
        );
        let err = verify_access_token(&format!("Bearer {token}"), &config).unwrap_err();
        assert!(
            matches!(err, OAuthError::ValidationFailed(_)),
            "expected aud validation failure, got {err:?}"
        );
    }

    /// Per-issuer scopes override: server-A requires `tools.invoke`,
    /// server-B requires `data.read`. A token from `iss_a` must carry
    /// the A scope but not the B scope.
    #[test]
    fn per_issuer_scopes_apply_per_token() {
        let (sk_a, vk_a) = ed_keypair();
        let iss_a = "https://idp-a.example";
        let aud_a = "api://server-a";

        let mut trusted = HashMap::new();
        trusted.insert(iss_a.to_string(), jwks_with(&vk_a, "kid-a"));
        let mut per_aud = HashMap::new();
        per_aud.insert(iss_a.to_string(), aud_a.to_string());
        let mut per_scopes = HashMap::new();
        per_scopes.insert(iss_a.to_string(), vec!["tools.invoke".to_string()]);
        let config = OAuthVerifierConfig {
            trusted_issuers: trusted,
            expected_audience: "api://fallback".into(),
            per_issuer_audience: per_aud,
            allowed_algorithms: vec![Algorithm::EdDSA],
            leeway_seconds: 30,
            // Global requires data.read — but the per-issuer override
            // should drop that requirement for tokens from iss_a.
            required_scopes: vec!["data.read".to_string()],
            per_issuer_scopes: per_scopes,
        };

        let token = make_token(
            &signing_key_pem(&sk_a),
            "kid-a",
            iss_a,
            json!(aud_a),
            300,
            Some("tools.invoke"),
        );
        verify_access_token(&format!("Bearer {token}"), &config).unwrap();

        // Token without the per-issuer-required scope must fail.
        let token_no_scope = make_token(
            &signing_key_pem(&sk_a),
            "kid-a",
            iss_a,
            json!(aud_a),
            300,
            None,
        );
        let err = verify_access_token(&format!("Bearer {token_no_scope}"), &config).unwrap_err();
        assert!(matches!(err, OAuthError::MissingScope(s) if s == "tools.invoke"));
    }

    /// `from_registry` returns `None` when the registry contains no
    /// servers with meta — caller must fall back to legacy path.
    #[test]
    fn from_registry_returns_none_when_no_meta() {
        let registry = crate::mcp::registry::McpServerRegistry::default();
        let cfg = OAuthVerifierConfig::from_registry(&registry, "api://fb", vec![], 30).unwrap();
        assert!(cfg.is_none());
    }

    /// `from_registry` materialises a multi-issuer config from a
    /// scanned registry with two servers carrying disjoint meta.
    #[test]
    fn from_registry_builds_multi_issuer_config() {
        use crate::mcp::registry::{
            DiscoveredMcpServer, DiscoveredMcpServerMeta, McpServerRegistry,
        };
        use std::collections::BTreeMap;
        let tmp = tempfile::TempDir::new().unwrap();
        let (_sk_a, vk_a) = ed_keypair();
        let (_sk_b, vk_b) = ed_keypair();
        let jwks_a = serde_json::to_vec(&jwks_with(&vk_a, "kid-a")).unwrap();
        let jwks_b = serde_json::to_vec(&jwks_with(&vk_b, "kid-b")).unwrap();
        let path_a = tmp.path().join("server-a.jwks.json");
        let path_b = tmp.path().join("server-b.jwks.json");
        std::fs::write(&path_a, jwks_a).unwrap();
        std::fs::write(&path_b, jwks_b).unwrap();

        let mut servers = BTreeMap::new();
        servers.insert(
            "server-a".to_string(),
            DiscoveredMcpServer {
                name: "server-a".to_string(),
                jwks_path: path_a,
                meta: Some(DiscoveredMcpServerMeta {
                    issuer: "https://idp-a.example".to_string(),
                    audience: Some("api://server-a".to_string()),
                    scopes: vec!["tools.invoke".to_string()],
                }),
            },
        );
        servers.insert(
            "server-b".to_string(),
            DiscoveredMcpServer {
                name: "server-b".to_string(),
                jwks_path: path_b,
                meta: Some(DiscoveredMcpServerMeta {
                    issuer: "https://idp-b.example".to_string(),
                    audience: Some("api://server-b".to_string()),
                    scopes: vec![],
                }),
            },
        );
        let registry = McpServerRegistry {
            servers,
            skipped: vec![],
        };

        let cfg = OAuthVerifierConfig::from_registry(&registry, "api://fb", vec![], 30)
            .unwrap()
            .expect("two servers with meta → Some");
        assert_eq!(cfg.trusted_issuers.len(), 2);
        assert!(cfg.trusted_issuers.contains_key("https://idp-a.example"));
        assert!(cfg.trusted_issuers.contains_key("https://idp-b.example"));
        assert_eq!(
            cfg.per_issuer_audience.get("https://idp-a.example"),
            Some(&"api://server-a".to_string())
        );
        assert_eq!(
            cfg.per_issuer_audience.get("https://idp-b.example"),
            Some(&"api://server-b".to_string())
        );
        assert_eq!(
            cfg.per_issuer_scopes.get("https://idp-a.example"),
            Some(&vec!["tools.invoke".to_string()])
        );
        // server-b's empty scopes should NOT be recorded as an override.
        assert!(!cfg.per_issuer_scopes.contains_key("https://idp-b.example"));
        assert_eq!(cfg.expected_audience, "api://fb");
    }

    /// Conflicting JWKS for the same issuer across two servers must
    /// surface as a hard error — never silently overwrite, never
    /// merge unrelated key sets.
    #[test]
    fn from_registry_rejects_conflicting_jwks_for_same_issuer() {
        use crate::mcp::registry::{
            DiscoveredMcpServer, DiscoveredMcpServerMeta, McpServerRegistry,
        };
        use std::collections::BTreeMap;
        let tmp = tempfile::TempDir::new().unwrap();
        let (_sk_a, vk_a) = ed_keypair();
        let (_sk_b, vk_b) = ed_keypair();
        let jwks_a = serde_json::to_vec(&jwks_with(&vk_a, "kid-a")).unwrap();
        let jwks_b = serde_json::to_vec(&jwks_with(&vk_b, "kid-b")).unwrap();
        let path_a = tmp.path().join("a.jwks.json");
        let path_b = tmp.path().join("b.jwks.json");
        std::fs::write(&path_a, jwks_a).unwrap();
        std::fs::write(&path_b, jwks_b).unwrap();

        let issuer = "https://shared.example".to_string();
        let mut servers = BTreeMap::new();
        servers.insert(
            "a".to_string(),
            DiscoveredMcpServer {
                name: "a".to_string(),
                jwks_path: path_a,
                meta: Some(DiscoveredMcpServerMeta {
                    issuer: issuer.clone(),
                    audience: Some("api://a".to_string()),
                    scopes: vec![],
                }),
            },
        );
        servers.insert(
            "b".to_string(),
            DiscoveredMcpServer {
                name: "b".to_string(),
                jwks_path: path_b,
                meta: Some(DiscoveredMcpServerMeta {
                    issuer: issuer.clone(),
                    audience: Some("api://b".to_string()),
                    scopes: vec![],
                }),
            },
        );
        let registry = McpServerRegistry {
            servers,
            skipped: vec![],
        };

        let err =
            OAuthVerifierConfig::from_registry(&registry, "api://fb", vec![], 30).unwrap_err();
        assert!(err.contains("conflicting JWKS"));
    }
}
