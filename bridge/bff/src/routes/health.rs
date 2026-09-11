// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — health & readiness endpoints.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::state::AppState;

/// Liveness payload — the process is up and serving.
#[derive(Serialize)]
pub struct Health {
    status: &'static str,
    service: &'static str,
    version: &'static str,
}

/// `GET /healthz` — liveness. Always 200 while the process serves.
pub async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok",
        service: "kars-bridge-bff",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Readiness payload — reports real dependency wiring.
#[derive(Serialize)]
pub struct Readiness {
    status: &'static str,
    /// Whether every required Kars API is readable in the default namespace.
    cluster_configured: bool,
}

/// `GET /readyz` — readiness. Probes every required Kars API and fails closed.
pub async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let cluster_configured = match state.cluster() {
        Some(c) => match c.ping(state.default_namespace()).await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "Bridge readiness requires compatible, accessible Kars APIs");
                false
            }
        },
        None => false,
    };
    let status = if cluster_configured {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(Readiness {
            status: if cluster_configured {
                "ok"
            } else {
                "unavailable"
            },
            cluster_configured,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request, Response};
    use std::sync::{Arc, Mutex};
    use tower::{ServiceExt, service_fn};

    const REQUIRED_APIS: &[(&str, &str)] = &[
        ("KarsSandbox", "karssandboxes"),
        ("KarsTask", "karstasks"),
        ("KarsTeam", "karsteams"),
        ("KarsProfile", "karsprofiles"),
        ("KarsSkill", "karsskills"),
        ("KarsApproval", "karsapprovals"),
        ("EgressApproval", "egressapprovals"),
        ("KarsReceipt", "karsreceipts"),
        ("McpServer", "mcpservers"),
        ("InferencePolicy", "inferencepolicies"),
        ("ToolPolicy", "toolpolicies"),
        ("KarsMemory", "karsmemories"),
        ("KarsEval", "karsevals"),
        ("KarsSREAction", "karssreactions"),
        ("KarsCredentialGrant", "karscredentialgrants"),
    ];

    #[derive(Clone, Copy)]
    enum Reply {
        Healthy,
        ApiError { index: usize, code: u16 },
        TransportError,
        InvalidJson,
        NeverRespond,
        SlowResponses,
    }

    fn state(reply: Reply, namespace: &str) -> (AppState, Arc<Mutex<Vec<String>>>) {
        let paths = Arc::new(Mutex::new(Vec::new()));
        let requests = paths.clone();
        let service = service_fn(move |request: Request<_>| {
            let requests = requests.clone();
            async move {
                assert_eq!(request.method(), Method::GET);
                assert!(
                    request
                        .uri()
                        .query()
                        .unwrap()
                        .split('&')
                        .any(|param| param == "limit=1")
                );
                let index = {
                    let mut paths = requests.lock().unwrap();
                    let index = paths.len();
                    paths.push(request.uri().path().to_string());
                    index
                };
                if matches!(reply, Reply::SlowResponses) {
                    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
                }
                let (code, body) = match reply {
                    Reply::ApiError { index: at, code } if index == at => (
                        code,
                        serde_json::json!({
                            "apiVersion": "v1", "kind": "Status", "status": "Failure",
                            "reason": "ReadinessTestFailure", "code": code,
                            "message": "upstream diagnostics must stay server-side"
                        })
                        .to_string(),
                    ),
                    Reply::TransportError => {
                        return Err(std::io::Error::other("connection refused"));
                    }
                    Reply::InvalidJson => (200, "not JSON".to_string()),
                    Reply::NeverRespond => std::future::pending().await,
                    _ => (
                        200,
                        serde_json::json!({
                            "apiVersion": "kars.azure.com/v1alpha1",
                            "kind": "List", "metadata": {}, "items": []
                        })
                        .to_string(),
                    ),
                };
                Ok(Response::builder()
                    .status(code)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap())
            }
        });
        (
            AppState::for_test_client(kube::Client::new(service, namespace), namespace),
            paths,
        )
    }

    async fn probe(state: AppState, path: &str) -> (StatusCode, serde_json::Value) {
        let response = crate::routes::router(state)
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn assert_unavailable(state: AppState) {
        let (status, body) = probe(state.clone(), "/readyz").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            serde_json::json!({"status": "unavailable", "cluster_configured": false})
        );
        assert_eq!(probe(state, "/healthz").await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn readiness_requires_all_guardrail_apis_but_not_teams_credentials() {
        for namespace in ["kars-system", "bridge-workspace"] {
            let (state, paths) = state(Reply::Healthy, namespace);
            assert!(state.teams_internal_secret().is_none());
            assert!(state.teams_entra_role_map().is_empty());
            let (status, body) = probe(state, "/readyz").await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                body,
                serde_json::json!({"status": "ok", "cluster_configured": true})
            );
            let expected: Vec<_> = REQUIRED_APIS
                .iter()
                .map(|(_, plural)| {
                    format!("/apis/kars.azure.com/v1alpha1/namespaces/{namespace}/{plural}")
                })
                .collect();
            assert_eq!(*paths.lock().unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn readiness_fails_closed_for_each_missing_required_api() {
        for (index, (kind, _)) in REQUIRED_APIS.iter().enumerate() {
            let reply = Reply::ApiError { index, code: 404 };
            let (app_state, paths) = state(reply, "kars-system");
            assert_unavailable(app_state).await;
            assert_eq!(paths.lock().unwrap().len(), index + 1, "{kind}");

            let (app_state, _) = state(reply, "kars-system");
            let error = app_state
                .cluster()
                .unwrap()
                .ping("kars-system")
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("kars.azure.com/v1alpha1/{kind}")),
                "{error}"
            );
        }
    }

    #[tokio::test]
    async fn readiness_fails_closed_on_auth_rbac_throttling_and_server_errors() {
        for code in [401, 403, 429, 500, 503] {
            let (state, paths) = state(Reply::ApiError { index: 0, code }, "kars-system");
            assert_unavailable(state).await;
            assert_eq!(paths.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn readiness_fails_closed_on_transport_and_decode_errors() {
        for reply in [Reply::TransportError, Reply::InvalidJson] {
            let (state, paths) = state(reply, "kars-system");
            assert_unavailable(state).await;
            assert_eq!(paths.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn readiness_timeout_does_not_block_liveness() {
        let (state, _) = state(Reply::NeverRespond, "kars-system");
        tokio::time::timeout(std::time::Duration::from_secs(7), assert_unavailable(state))
            .await
            .expect("readiness must finish before the Helm probe's ten-second timeout");
    }

    #[tokio::test]
    async fn readiness_budget_is_shared_across_all_required_api_requests() {
        let (state, paths) = state(Reply::SlowResponses, "kars-system");
        tokio::time::timeout(std::time::Duration::from_secs(7), assert_unavailable(state))
            .await
            .expect("readiness must not reset its timeout for each API");
        let count = paths.lock().unwrap().len();
        assert!(count > 1 && count < REQUIRED_APIS.len());
    }
}
