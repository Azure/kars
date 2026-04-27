//! OAuth 2.1 bearer-token verification as a `tower::Layer`.
//!
//! ## Why a tower layer
//!
//! [`super::oauth::verify_access_token`] is a pure synchronous function
//! that does not know about HTTP types. To gate the MCP and A2A
//! routers on bearer-token verification (per
//! internal Phase 1 plan §7 and the McpServer CRD's
//! `spec.productionMode: true` semantics), we need to:
//!
//! 1. Inspect the `Authorization` header on every incoming request.
//! 2. Reject (with **401** + an RFC 6750-shaped `WWW-Authenticate`
//!    challenge) when verification fails.
//! 3. Make the [`VerifiedToken`] available to downstream handlers via
//!    [`http::Extensions`] so handlers can authorise based on
//!    `subject` / `audience` / `scopes` without re-decoding.
//!
//! Steps 1–3 are pure plumbing around the in-tree verifier; doing this
//! work in a [`tower::Layer`] means it composes uniformly with axum
//! routers, with `Router::route_layer`, and with handler-level
//! `From<&Extensions>` extractors — the same shape the MCP and A2A
//! sub-routers (`routes::mcp::mcp_route`, `routes::a2a::a2a_routes`)
//! already build with their own state.
//!
//! ## What this layer does NOT do
//!
//! - It does not fetch the JWK set. JWKS materialisation is an I/O
//!   concern that lives one layer up: the caller hands an already-
//!   populated [`OAuthVerifierConfig`] (typically wrapped in
//!   `Arc<...>`) into the layer constructor.
//! - It does not refresh keys. Hot reload is achieved by replacing the
//!   `Arc<OAuthVerifierConfig>` the layer holds; that's the caller's
//!   problem (e.g. an `arc-swap`-backed singleton driven by a watcher).
//! - It does not surface `Bearer realm=`. The `WWW-Authenticate`
//!   challenge it emits is the minimum mandated by RFC 6750 §3:
//!   `Bearer error="invalid_token", error_description="..."`. The
//!   `realm` parameter is optional per §3.1 and we choose not to set
//!   one — the MCP / A2A specs don't require it.
//! - It does not allow anonymous fall-through. Every route this layer
//!   wraps requires a verifying token. Routes that don't want auth
//!   should not be wrapped.
//!
//! ## Wiring intent
//!
//! `routes::mcp::mcp_route(...)` and `routes::a2a::a2a_routes(...)` are
//! both authored as sub-routers with their own `RouterState`. Once the
//! caller has materialised an `Arc<OAuthVerifierConfig>` (from the
//! parent McpServer / A2AAgent CRD spec), the wiring is:
//!
//! ```ignore
//! let layer = OAuthLayer::new(verifier_config);
//! Router::new()
//!     .merge(routes::mcp::mcp_route(state.clone()).route_layer(layer.clone()))
//!     .merge(routes::a2a::a2a_routes(state).route_layer(layer))
//! ```
//!
//! The actual `main.rs` wiring is gated on the SigningProvider /
//! AuditSink trio that ships in a follow-up; this module's tests use
//! `oneshot` against a pinned MakeService to validate the layer in
//! isolation.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use tower::{Layer, Service};

use super::oauth::{OAuthError, OAuthVerifierConfig, VerifiedToken, verify_access_token};

/// Token-verification middleware factory.
///
/// Construct once per route group; clone freely (the underlying config
/// lives behind an [`Arc`]).
#[derive(Clone)]
pub struct OAuthLayer {
    config: Arc<OAuthVerifierConfig>,
}

impl OAuthLayer {
    /// Build a new layer from an already-materialised config.
    pub fn new(config: Arc<OAuthVerifierConfig>) -> Self {
        Self { config }
    }
}

impl<S> Layer<S> for OAuthLayer {
    type Service = OAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OAuthService {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// Service produced by [`OAuthLayer::layer`].
#[derive(Clone)]
pub struct OAuthService<S> {
    inner: S,
    config: Arc<OAuthVerifierConfig>,
}

impl<S> Service<Request<Body>> for OAuthService<S>
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        // tower contract: clone the inner service after `poll_ready`
        // returned `Ready` for *this* one to preserve readiness on the
        // long-lived original. See tokio.rs/blog 2021-05-14 §"Pitfall #2".
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let config = Arc::clone(&self.config);

        Box::pin(async move {
            let bearer = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);

            let bearer = match bearer {
                Some(b) => b,
                None => return Ok(unauthorised_response(&OAuthError::MissingBearer)),
            };

            match verify_access_token(&bearer, &config) {
                Ok(token) => {
                    req.extensions_mut().insert(token);
                    inner.call(req).await
                }
                Err(e) => Ok(unauthorised_response(&e)),
            }
        })
    }
}

/// Build the **401** response per RFC 6750 §3 with a `Bearer` challenge.
///
/// `error_description` carries a single quoted string per §3.1; we use
/// the `OAuthError`'s `Display` impl. Embedded double-quotes are
/// stripped (we don't need them — every `OAuthError::Display` we ship
/// is plain ASCII). If the resulting header value contains anything
/// invalid for an HTTP header (control chars, CR, LF), we fall back to
/// the bare `Bearer` challenge to avoid emitting a malformed response.
fn unauthorised_response(err: &OAuthError) -> Response<Body> {
    let description = err.to_string().replace('"', "'");
    let challenge = format!(
        "Bearer error=\"invalid_token\", error_description=\"{}\"",
        description
    );
    let www_authenticate = HeaderValue::from_str(&challenge)
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer error=\"invalid_token\""));

    let mut resp = Response::new(Body::from("Unauthorized"));
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp.headers_mut()
        .insert(header::WWW_AUTHENTICATE, www_authenticate);
    resp
}

/// Convenience: extract the [`VerifiedToken`] previously installed by
/// [`OAuthLayer`] on a request's [`http::Extensions`]. Handlers reach
/// for this rather than re-decoding the bearer header.
pub fn verified_token(extensions: &axum::http::Extensions) -> Option<&VerifiedToken> {
    extensions.get::<VerifiedToken>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::oauth::OAuthVerifierConfig;
    use axum::routing::get;
    use axum::{Router, extract::Extension};
    use base64::Engine;
    use ed25519_dalek::SigningKey;
    use jsonwebtoken::jwk::{
        AlgorithmParameters as JwkAlg, CommonParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm,
        OctetKeyPairParameters, OctetKeyPairType, PublicKeyUse,
    };
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::json;
    use std::collections::HashMap;
    use tower::ServiceExt;

    const TEST_KID: &str = "layer-kid-1";
    const TEST_ISS: &str = "https://layer.example/iss";
    const TEST_AUD: &str = "https://layer.example/aud";

    fn ed_keypair_seeded(seed: u8) -> (SigningKey, ed25519_dalek::VerifyingKey) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    fn jwks_with(vk: &ed25519_dalek::VerifyingKey, kid: &str) -> JwkSet {
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vk.as_bytes());
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
                    curve: EllipticCurve::Ed25519,
                    x,
                }),
            }],
        }
    }

    /// Build a PKCS#8 v1 PEM Ed25519 private key (RFC 8410 §7) without
    /// enabling the `pkcs8` feature on ed25519-dalek. Mirrors the helper
    /// in `oauth.rs::tests`.
    fn signing_key_pem(sk: &SigningKey) -> EncodingKey {
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

    fn cfg(jwks: JwkSet) -> Arc<OAuthVerifierConfig> {
        let mut trusted = HashMap::new();
        trusted.insert(TEST_ISS.to_string(), jwks);
        Arc::new(OAuthVerifierConfig {
            trusted_issuers: trusted,
            expected_audience: TEST_AUD.into(),
            allowed_algorithms: vec![Algorithm::EdDSA],
            leeway_seconds: 30,
            required_scopes: vec![],
        })
    }

    fn issue_token(sk: &SigningKey, kid: &str) -> String {
        let now = jsonwebtoken::get_current_timestamp() as i64;
        let claims = json!({
            "iss": TEST_ISS,
            "sub": "layer-sub",
            "aud": TEST_AUD,
            "iat": now - 1,
            "nbf": now - 1,
            "exp": now + 600,
            "scope": "mcp.read"
        });
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(kid.into());
        encode(&header, &claims, &signing_key_pem(sk)).unwrap()
    }

    /// Test harness: a leaf handler that asserts a `VerifiedToken` was
    /// installed by the layer and echoes its subject in the response.
    fn echo_router(layer: OAuthLayer) -> Router {
        async fn echo(Extension(t): Extension<VerifiedToken>) -> String {
            t.subject
        }
        Router::new().route("/echo", get(echo)).layer(layer)
    }

    #[tokio::test]
    async fn missing_authorization_header_yields_401_with_challenge() {
        let (_sk, vk) = ed_keypair_seeded(7);
        let layer = OAuthLayer::new(cfg(jwks_with(&vk, TEST_KID)));
        let app = echo_router(layer);

        let req = Request::builder().uri("/echo").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let challenge = resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(challenge.starts_with("Bearer error=\"invalid_token\""));
    }

    #[tokio::test]
    async fn malformed_bearer_yields_401() {
        let (_sk, vk) = ed_keypair_seeded(7);
        let layer = OAuthLayer::new(cfg(jwks_with(&vk, TEST_KID)));
        let app = echo_router(layer);

        let req = Request::builder()
            .uri("/echo")
            .header(header::AUTHORIZATION, "Bearer not-a-jwt")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(
            resp.headers()
                .get(header::WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("error=\"invalid_token\"")
        );
    }

    #[tokio::test]
    async fn token_signed_by_untrusted_key_yields_401() {
        let (sk, _vk) = ed_keypair_seeded(7);
        // Trust a different key under the same kid; signature won't verify.
        let (_other_sk, other_vk) = ed_keypair_seeded(9);
        let layer = OAuthLayer::new(cfg(jwks_with(&other_vk, TEST_KID)));
        let app = echo_router(layer);

        let token = issue_token(&sk, TEST_KID);
        let req = Request::builder()
            .uri("/echo")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_token_attaches_verified_token_to_request_extensions() {
        let (sk, vk) = ed_keypair_seeded(7);
        let layer = OAuthLayer::new(cfg(jwks_with(&vk, TEST_KID)));
        let app = echo_router(layer);

        let token = issue_token(&sk, TEST_KID);
        let req = Request::builder()
            .uri("/echo")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 64).await.unwrap();
        assert_eq!(&body_bytes[..], b"layer-sub");
    }

    #[tokio::test]
    async fn challenge_header_is_always_valid_ascii() {
        // Adversarial bearer: alg=none token. Display impl is plain
        // ASCII; we assert the resulting WWW-Authenticate parses as a
        // header value and round-trips to a string.
        let (_sk, vk) = ed_keypair_seeded(7);
        let layer = OAuthLayer::new(cfg(jwks_with(&vk, TEST_KID)));
        let app = echo_router(layer);

        let req = Request::builder()
            .uri("/echo")
            .header(header::AUTHORIZATION, "Bearer eyJhbGciOiJub25lIn0.e30.")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let h = resp.headers().get(header::WWW_AUTHENTICATE).unwrap();
        let _ = h.to_str().expect("WWW-Authenticate is valid ASCII");
    }

    #[test]
    fn extension_extractor_helper_returns_token() {
        let mut ext = axum::http::Extensions::new();
        let t = VerifiedToken {
            subject: "abc".into(),
            issuer: "iss".into(),
            audience: "aud".into(),
            scopes: vec![],
            expires_at: 0,
            claims: serde_json::Value::Null,
        };
        ext.insert(t.clone());
        assert_eq!(verified_token(&ext), Some(&t));
    }
}
