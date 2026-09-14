// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{Extension, Router, body::to_bytes, middleware};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::{ServiceExt, service_fn};

const SECRET: &str = "test-only-bridge-principal-signing-key";
const BUDGETS: &str = "/api/v1/namespaces/kars-system/configmaps/kars-inference-budgets";
const RETENTION: &str = "/api/v1/namespaces/kars-system/configmaps/kars-retention-policy";
const MUTATIONS: &[&str] = &[
    "/api/operator/inference-budgets/cluster",
    "/api/operator/inference-budgets/workspaces/work",
    "/api/operator/inference-budgets/users/alice",
    "/api/operator/retention-policy",
];

#[derive(Default)]
struct TestApi {
    calls: Vec<(Method, String)>,
    objects: std::collections::BTreeMap<String, Value>,
}

fn fixture() -> (AppState, Arc<Mutex<TestApi>>) {
    let api = Arc::new(Mutex::new(TestApi::default()));
    let store = api.clone();
    let service = service_fn(move |request: Request<_>| {
        let store = store.clone();
        async move {
            let (parts, body) = request.into_parts();
            let bytes = to_bytes(Body::new(body), 65536).await.unwrap();
            let path = parts.uri.path();
            let mut api = store.lock().unwrap();
            api.calls.push((parts.method.clone(), path.into()));
            let (status, value) = if [BUDGETS, RETENTION].contains(&path) {
                if parts.method == Method::PATCH {
                    let value: Value = serde_json::from_slice(&bytes).unwrap();
                    api.objects.insert(path.into(), value.clone());
                    (200, value)
                } else {
                    assert_eq!(parts.method, Method::GET);
                    match api.objects.get(path) {
                        Some(value) => (200, value.clone()),
                        None => (
                            404,
                            json!({
                                "apiVersion":"v1","kind":"Status","status":"Failure",
                                "reason":"NotFound","code":404,"message":"not found"
                            }),
                        ),
                    }
                }
            } else {
                assert_eq!(parts.method, Method::GET);
                assert!(
                    path == "/apis/kars.azure.com/v1alpha1/karstasks"
                        || path == "/api/v1/namespaces/kars-system/configmaps",
                    "unexpected Kubernetes request: {path}"
                );
                (
                    200,
                    json!({"apiVersion":"v1","kind":"List","metadata":{},"items":[]}),
                )
            };
            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(value.to_string()))
                    .unwrap(),
            )
        }
    });
    (
        AppState::for_test_client(kube::Client::new(service, "work"), "work")
            .with_principal_secret(Some(SECRET.into())),
        api,
    )
}

fn signed(roles: &[&str], secret: &str, expires: i64) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        &json!({"sub":"alice","name":"Alice","roles":roles,"exp":expires}),
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

fn token(roles: &[&str]) -> String {
    signed(roles, SECRET, chrono::Utc::now().timestamp() + 3600)
}

fn app(state: AppState) -> Router {
    crate::routes::router(state.clone()).layer(middleware::from_fn_with_state(state, require_token))
}

fn request(path: &str, method: Method, token: Option<&str>, body: Value) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", "bridge-role=admin")
        .header("x-bridge-role", "admin");
    if let Some(token) = token {
        request = request.header(PRINCIPAL_HEADER, token);
    }
    request.body(Body::from(body.to_string())).unwrap()
}

fn input(path: &str, clear: bool) -> Value {
    if path.ends_with("retention-policy") {
        json!({"default_ttl_seconds":if clear { 0 } else { 3600 }})
    } else {
        json!({"daily_tokens":100,"mode":"strict","clear":clear})
    }
}

#[test]
fn admin_route_and_role_matrix_does_not_promote_operators_or_gate_reads() {
    for path in MUTATIONS {
        for method in [Method::PUT, Method::POST, Method::PATCH, Method::DELETE] {
            assert_eq!(required_persona(path, &method), RequiredPersona::Admin);
        }
        assert_eq!(
            required_persona(path, &Method::GET),
            RequiredPersona::Operator
        );
    }
    for role in ["user", "auditor", "operator", "viewer"] {
        let principal = Principal {
            sub: "alice".into(),
            name: "Alice".into(),
            roles: vec![role.into()],
        };
        assert!(!has_role(&principal, RequiredPersona::Admin));
    }
    assert_eq!(
        required_persona("/api/operator/inferencepolicies/example", &Method::PATCH),
        RequiredPersona::Operator
    );
    assert_eq!(
        required_persona("/api/operator/retention-policy-extra", &Method::PUT),
        RequiredPersona::Operator
    );
}

#[tokio::test]
async fn direct_bff_admin_mutations_deny_missing_forged_expired_and_non_admin_principals() {
    let invalid = [
        None,
        Some("forged".into()),
        Some(signed(
            &["admin"],
            "wrong-key",
            chrono::Utc::now().timestamp() + 3600,
        )),
        Some(signed(
            &["admin"],
            SECRET,
            chrono::Utc::now().timestamp() - 3600,
        )),
    ];
    for token in invalid {
        let (state, api) = fixture();
        for path in MUTATIONS {
            let response = app(state.clone())
                .oneshot(request(
                    path,
                    Method::PUT,
                    token.as_deref(),
                    input(path, false),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        assert!(api.lock().unwrap().calls.is_empty());
    }
    for role in ["operator", "user", "auditor", "viewer"] {
        let (state, api) = fixture();
        let token = token(&[role]);
        for path in MUTATIONS {
            for clear in [false, true] {
                let response = app(state.clone())
                    .oneshot(request(path, Method::PUT, Some(&token), input(path, clear)))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{role}: {path}");
            }
        }
        assert!(api.lock().unwrap().calls.is_empty());
    }
}

#[tokio::test]
async fn budget_and_retention_handlers_require_admin_even_without_route_middleware() {
    let (state, api) = fixture();
    let principal = Principal {
        sub: "alice".into(),
        name: "Alice".into(),
        roles: vec!["operator".into()],
    };
    for path in MUTATIONS {
        let response = crate::routes::router(state.clone())
            .layer(Extension(principal.clone()))
            .oneshot(request(path, Method::PUT, None, input(path, false)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    assert!(api.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn signed_admins_can_set_and_clear_each_budget_scope_and_retention() {
    for (index, path) in MUTATIONS.iter().enumerate() {
        let (state, api) = fixture();
        let token = token(&["admin"]);
        for clear in [false, true] {
            let response = app(state.clone())
                .oneshot(request(path, Method::PUT, Some(&token), input(path, clear)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap())
                    .unwrap();
            let api = api.lock().unwrap();
            if index == 3 {
                assert_eq!(body["default_ttl_seconds"], if clear { 0 } else { 3600 });
                assert_eq!(
                    api.objects[RETENTION]["data"]["defaultTtlSeconds"],
                    if clear { "0" } else { "3600" }
                );
            } else {
                let hierarchy: Value = serde_json::from_str(
                    api.objects[BUDGETS]["data"]["budgets.json"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                let rule = match index {
                    0 => &hierarchy["cluster"],
                    1 => &hierarchy["workspaces"]["work"],
                    _ => &hierarchy["users"]["alice"],
                };
                if clear {
                    assert!(rule.is_null());
                } else {
                    assert_eq!(rule["daily_tokens"], 100);
                    assert_eq!(rule["mode"], "strict");
                }
            }
        }
        assert_eq!(
            api.lock()
                .unwrap()
                .calls
                .iter()
                .filter(|(method, _)| *method == Method::PATCH)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn operator_reads_stay_available_while_user_and_auditor_console_reads_stay_denied() {
    for role in ["operator", "admin", "user", "auditor", "viewer"] {
        let (state, api) = fixture();
        let token = token(&[role]);
        let allowed = matches!(role, "operator" | "admin");
        for path in [
            "/api/operator/inference-budgets",
            "/api/operator/retention-policy",
        ] {
            let response = app(state.clone())
                .oneshot(request(path, Method::GET, Some(&token), Value::Null))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if allowed {
                    StatusCode::OK
                } else {
                    StatusCode::FORBIDDEN
                }
            );
        }
        assert_eq!(api.lock().unwrap().calls.is_empty(), !allowed);
    }
}
