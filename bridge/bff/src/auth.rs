// kars Bridge BFF — authenticated principal and persona authorization boundary.
//
// The Next.js web tier verifies the user's OIDC-derived `bridge-session` cookie
// and forwards that same HS256 token in X-Kars-Principal-Token. The BFF verifies
// it independently before serving any API route, derives the actor from signed
// claims, and enforces persona routes server-side. Browser-supplied actor headers
// or body fields never become authority.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

pub const PRINCIPAL_HEADER: &str = "x-kars-principal-token";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub sub: String,
    pub name: String,
    pub roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PrincipalClaims {
    sub: String,
    name: String,
    #[serde(default)]
    roles: Vec<String>,
    exp: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequiredPersona {
    SignedIn,
    User,
    Operator,
    Auditor,
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn required_persona(path: &str, method: &Method) -> RequiredPersona {
    if path == "/api/operator/audit" && *method == Method::GET {
        return RequiredPersona::Auditor;
    }
    if path.starts_with("/api/operator/") {
        return RequiredPersona::Operator;
    }
    if path.starts_with("/api/namespaces/")
        && (path.contains("/receipt") || path.ends_with("/compliance"))
    {
        return RequiredPersona::SignedIn;
    }
    if path.starts_with("/api/") {
        return RequiredPersona::User;
    }
    RequiredPersona::SignedIn
}

fn has_role(principal: &Principal, required: RequiredPersona) -> bool {
    let has = |role: &str| principal.roles.iter().any(|r| r == role);
    if has("admin") {
        return true;
    }
    match required {
        RequiredPersona::SignedIn => !principal.roles.is_empty(),
        RequiredPersona::User => has("user") || has("operator"),
        RequiredPersona::Operator => has("operator"),
        RequiredPersona::Auditor => has("auditor"),
    }
}

fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"error":{"code":code,"message":message}}).to_string(),
        ))
        .expect("static error response")
}

fn verify_principal(token: &str, secret: &str) -> Option<Principal> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let claims = decode::<PrincipalClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()?
    .claims;
    let _ = claims.exp;
    if claims.sub.is_empty() || claims.roles.is_empty() {
        return None;
    }

    Some(Principal {
        sub: claims.sub,
        name: claims.name,
        roles: claims.roles,
    })
}

fn local_dev_principal() -> Principal {
    let roles = std::env::var("BRIDGE_ROLES")
        .unwrap_or_else(|_| "admin,operator,auditor,user".into())
        .split(',')
        .map(str::trim)
        .filter(|r| matches!(*r, "admin" | "operator" | "auditor" | "user"))
        .map(str::to_string)
        .collect();
    Principal {
        sub: "local-dev".into(),
        name: std::env::var("BRIDGE_OPERATOR").unwrap_or_else(|_| "bridge-operator@local".into()),
        roles,
    }
}

pub async fn require_token(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if matches!(path, "/healthz" | "/readyz") {
        return next.run(req).await;
    }

    // Internal Teams decision endpoint uses its own secret-header auth —
    // bypass the principal token validation entirely for this path.
    if path.starts_with("/api/internal/teams/") {
        req.extensions_mut().insert(local_dev_principal());
        return next.run(req).await;
    }

    if let Some(secret) = state.principal_secret() {
        let Some(token) = req
            .headers()
            .get(PRINCIPAL_HEADER)
            .and_then(|v| v.to_str().ok())
        else {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "principal_required",
                "a signed Bridge user session is required",
            );
        };
        let Some(principal) = verify_principal(token, secret) else {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "invalid_principal",
                "the Bridge user session is invalid or expired",
            );
        };
        let required = required_persona(path, req.method());
        if !has_role(&principal, required) {
            return json_error(
                StatusCode::FORBIDDEN,
                "forbidden",
                "the signed-in persona is not authorized for this API route",
            );
        }
        // Route handlers consume this typed extension for immutable actor
        // attribution. Remove the raw token before downstream logging.
        req.headers_mut().remove(PRINCIPAL_HEADER);
        req.extensions_mut().insert(principal);
        return next.run(req).await;
    }

    // Local-dev compatibility: when SSO/principal auth is not configured, retain
    // the existing optional shared bearer guard for writes.
    let Some(expected) = state.api_token() else {
        req.extensions_mut().insert(local_dev_principal());
        return next.run(req).await;
    };
    if !is_mutating(req.method()) {
        req.extensions_mut().insert(local_dev_principal());
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t == expected)
        .unwrap_or(false);
    if ok {
        req.extensions_mut().insert(local_dev_principal());
        next.run(req).await
    } else {
        json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "bearer token required for mutating requests",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(roles: &[&str]) -> Principal {
        Principal {
            sub: "subject-1".into(),
            name: "alice".into(),
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
        }
    }

    #[test]
    fn route_personas_are_server_enforced() {
        assert_eq!(
            required_persona("/api/operator/mcpservers", &Method::GET),
            RequiredPersona::Operator
        );
        assert_eq!(
            required_persona("/api/operator/audit", &Method::GET),
            RequiredPersona::Auditor
        );
        assert_eq!(
            required_persona(
                "/api/namespaces/kars-system/approvals/a/decision",
                &Method::POST
            ),
            RequiredPersona::User
        );
        assert_eq!(
            required_persona("/api/namespaces/kars-system/tasks", &Method::POST),
            RequiredPersona::User
        );
        assert_eq!(
            required_persona(
                "/api/namespaces/kars-system/tasks/task/receipt",
                &Method::GET
            ),
            RequiredPersona::SignedIn
        );
        assert_eq!(
            required_persona(
                "/api/namespaces/kars-system/tasks/task/receipt/verify",
                &Method::POST
            ),
            RequiredPersona::SignedIn
        );
    }

    #[test]
    fn persona_role_matrix_matches_shared_tenant_contract() {
        assert!(has_role(
            &principal(&["operator"]),
            RequiredPersona::Operator
        ));
        assert!(!has_role(
            &principal(&["operator"]),
            RequiredPersona::Auditor
        ));
        assert!(!has_role(
            &principal(&["auditor"]),
            RequiredPersona::Operator
        ));
        assert!(has_role(&principal(&["auditor"]), RequiredPersona::Auditor));
        assert!(has_role(&principal(&["user"]), RequiredPersona::User));
        assert!(!has_role(&principal(&["auditor"]), RequiredPersona::User));
    }
}
