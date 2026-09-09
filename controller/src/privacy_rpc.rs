// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The only operation is read-only verification of one current observation
//! credential. This listener never shares the plaintext metrics server.

mod authority;
pub(crate) mod discovery;
mod identity;
mod publication;
#[cfg(test)]
mod tests;

use crate::observation_privacy::{self as wire, Endpoint};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use kube::Client;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{RwLock, Semaphore},
};

struct ServerState {
    client: Client,
    endpoint: RwLock<Option<Endpoint>>,
    capacity: Arc<Semaphore>,
}

fn deny() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"capability":wire::CAPABILITY,"allowed":false})),
    )
        .into_response()
}

fn app(state: Arc<ServerState>) -> Router {
    Router::new()
        .route(wire::PATH, post(verify))
        .fallback(|| async { deny() })
        .with_state(state)
}

async fn verify(State(state): State<Arc<ServerState>>, request: Request) -> Response {
    let Ok(_permit) = state.capacity.clone().try_acquire_owned() else {
        return deny();
    };
    let operation = async {
        if request.method() != Method::POST
            || request.uri().query().is_some()
            || request.headers().get_all("authorization").iter().count() != 1
            || request
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                != Some("application/json")
        {
            return None;
        }
        let token = request
            .headers()
            .get("authorization")?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        let token = token.to_string();
        let endpoint = state.endpoint.read().await.clone()?;
        let bytes = to_bytes(request.into_body(), wire::MAX_BODY).await.ok()?;
        let request: wire::Request = serde_json::from_slice(&bytes).ok()?;
        let proof = authority::verify(&state.client, &request, &token, &endpoint)
            .await
            .ok()?;
        if !proof.matches(&request) || state.endpoint.read().await.as_ref() != Some(&endpoint) {
            return None;
        }
        Some(proof)
    };
    match tokio::time::timeout(Duration::from_secs(wire::DEADLINE_SECONDS), operation).await {
        Ok(Some(proof)) => Json(proof).into_response(),
        _ => deny(),
    }
}

pub(crate) fn start(client: Client) {
    if std::env::var("KARS_OBSERVATION_PRIVACY_RPC_ENABLED").as_deref() != Ok("true") {
        return;
    }
    tokio::spawn(async move {
        if let Err(error) = supervise(client).await {
            tracing::error!(error=%error, "Private observation verifier unavailable");
        }
    });
}

async fn supervise(client: Client) -> Result<(), String> {
    let namespace =
        std::env::var("POD_NAMESPACE").map_err(|_| "Controller namespace unavailable")?;
    let pod_name = std::env::var("POD_NAME").map_err(|_| "Controller Pod name unavailable")?;
    let pod_uid = std::env::var("POD_UID").map_err(|_| "Controller Pod UID unavailable")?;
    let state = Arc::new(ServerState {
        client: client.clone(),
        endpoint: RwLock::new(None),
        capacity: Arc::new(Semaphore::new(4)),
    });
    let mut server: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let result = async {
            let prepared = identity::prepare(&client, &namespace).await?;
            if state.endpoint.read().await.as_ref() != Some(&prepared.endpoint)
                || server.as_ref().is_none_or(|s| s.is_finished())
            {
                publication::withdraw(&client, &namespace, &pod_name, &pod_uid).await?;
                *state.endpoint.write().await = None;
                if let Some(previous) = server.take() {
                    previous.abort();
                    let _ = previous.await;
                }
                let listener = crate::private_tls::Listener {
                    tcp: TcpListener::bind(("0.0.0.0", wire::PORT))
                        .await
                        .map_err(|_| "Privacy TLS port unavailable")?,
                    tls: crate::private_tls::tls_from_pem(
                        prepared.certificate.as_bytes(),
                        prepared.key.as_bytes(),
                    )?,
                };
                let router = app(state.clone());
                server = Some(tokio::spawn(async move {
                    if axum::serve(listener, router).await.is_err() {
                        tracing::error!("Private observation verifier listener stopped");
                    }
                }));
                *state.endpoint.write().await = Some(prepared.endpoint.clone());
            }
            publication::publish(&client, &prepared.endpoint, &pod_name, &pod_uid).await?;
            Ok::<_, String>(())
        }
        .await;
        if result.is_err() {
            *state.endpoint.write().await = None;
            let _ = publication::withdraw(&client, &namespace, &pod_name, &pod_uid).await;
            tracing::warn!(
                "Private observation verifier is pending live identity/privacy qualification"
            );
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}
