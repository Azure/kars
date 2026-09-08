// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Agent-safe HTTPS facade for the existing Hermes Kubernetes client.

mod backend;
mod policy;
#[cfg(test)]
mod tests;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use backend::Backend;
use futures::StreamExt;
use policy::Route;
use std::{
    io::{self, BufReader},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

const DIRECTORY: &str = "/etc/kars/sre-api";
pub const PORT: u16 = 9446;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

#[derive(Clone)]
struct Proxy {
    backend: Arc<Backend>,
    token: Arc<str>,
    capacity: Arc<Semaphore>,
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({
            "apiVersion":"v1","kind":"Status","status":"Failure",
            "code":status.as_u16(),"reason":"SREProxyDenied","message":message,
        })),
    )
        .into_response()
}

async fn ready(State(proxy): State<Proxy>) -> Response {
    let Ok(_permit) = proxy.capacity.try_acquire() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "SRE proxy capacity is exhausted",
        );
    };
    match proxy.backend.authorize().await {
        Ok(()) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({"ready":true})),
        )
            .into_response(),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SRE authority is not ready",
        ),
    }
}

async fn forward(
    State(proxy): State<Proxy>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if headers.get_all("authorization").iter().count() != 1
        || headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            != Some(proxy.token.as_ref())
    {
        return error(
            StatusCode::UNAUTHORIZED,
            "An SRE proxy credential is required",
        );
    }
    let route = match policy::route(&method, &uri) {
        Ok(route) => route,
        Err(message) => return error(StatusCode::FORBIDDEN, message),
    };
    if headers.contains_key("upgrade")
        || headers
            .get("accept")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| {
                !(matches!(accept, "application/json" | "*/*")
                    || route == Route::Logs && accept == "text/plain")
            })
    {
        return error(
            StatusCode::NOT_ACCEPTABLE,
            "Only the bounded JSON/log SRE API is available",
        );
    }
    let request_body = if route == Route::Proposal {
        if headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("application/json") {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "SRE proposals require application/json",
            );
        }
        let value = match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(value) => value,
            Err(_) => return error(StatusCode::BAD_REQUEST, "Proposal JSON is invalid"),
        };
        match policy::proposal(&value) {
            Ok(value) => Some(value),
            Err(message) => return error(StatusCode::FORBIDDEN, message),
        }
    } else {
        if !body.is_empty() {
            return error(
                StatusCode::BAD_REQUEST,
                "Diagnostic GET bodies are not allowed",
            );
        }
        None
    };
    let Ok(_permit) = proxy.capacity.try_acquire() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "SRE proxy capacity is exhausted",
        );
    };
    let path = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let response = match proxy
        .backend
        .forward(method, path, request_body, route == Route::Logs)
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "SRE Kubernetes authority or transport is unavailable",
            );
        }
    };
    let status = response.status();
    if !status.is_success() {
        return error(
            status,
            "Kubernetes rejected the diagnostic/proposal request",
        );
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let limit = if route == Route::Logs {
        262144
    } else {
        MAX_RESPONSE
    };
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return error(
                StatusCode::BAD_GATEWAY,
                "Kubernetes response was incomplete",
            );
        };
        if bytes.len() + chunk.len() > limit {
            return error(
                StatusCode::BAD_GATEWAY,
                "Kubernetes response exceeds the SRE diagnostic limit",
            );
        }
        bytes.extend_from_slice(&chunk);
    }
    if route == Route::Logs {
        return (
            status,
            [("content-type", "text/plain; charset=utf-8")],
            Body::from(bytes),
        )
            .into_response();
    }
    let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::BAD_GATEWAY, "Kubernetes response was not JSON"),
    };
    let value = if route == Route::Secrets {
        match policy::secret_projection(&value) {
            Ok(value) => value,
            Err(message) => return error(StatusCode::BAD_GATEWAY, message),
        }
    } else {
        value
    };
    (status, axum::Json(value)).into_response()
}

fn app(proxy: Proxy) -> Router {
    Router::new()
        .route("/readyz", get(ready))
        .fallback(forward)
        .layer(DefaultBodyLimit::max(65_536))
        .with_state(proxy)
}

struct Listener {
    tcp: TcpListener,
    tls: TlsAcceptor,
}

impl axum::serve::Listener for Listener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.tcp.accept().await {
                Ok((stream, address)) => {
                    if let Ok(Ok(stream)) = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        self.tls.accept(stream),
                    )
                    .await
                    {
                        return (stream, address);
                    }
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

fn tls(directory: &Path) -> Result<TlsAcceptor, String> {
    let certificates = std::fs::File::open(directory.join("server-cert.pem"))
        .map_err(|_| "SRE TLS certificate unavailable")?;
    let certificates = rustls_pemfile::certs(&mut BufReader::new(certificates))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "SRE TLS certificate invalid")?;
    let key = std::fs::File::open(directory.join("server-key.pem"))
        .map_err(|_| "SRE TLS key unavailable")?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key))
        .map_err(|_| "SRE TLS key invalid")?
        .ok_or("SRE TLS private key missing")?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| "SRE TLS certificate/key mismatch")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

pub async fn start() -> Result<Option<tokio::task::JoinHandle<()>>, String> {
    if std::env::var("KARS_SRE_API_ENABLED").as_deref() != Ok("true") {
        return Ok(None);
    }
    let directory = PathBuf::from(DIRECTORY);
    let backend = Backend::load(&directory)?;
    let token = std::fs::read_to_string(directory.join("agent-token"))
        .map_err(|_| "SRE proxy credential unavailable")?;
    if token.trim().len() != 64 || !token.trim().bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("SRE proxy credential invalid".into());
    }
    let listener = Listener {
        tcp: TcpListener::bind(("127.0.0.1", PORT))
            .await
            .map_err(|_| "SRE loopback TLS listener unavailable")?,
        tls: tls(&directory)?,
    };
    backend.renew_in_background();
    let proxy = Proxy {
        backend,
        token: Arc::from(token.trim()),
        capacity: Arc::new(Semaphore::new(16)),
    };
    let router = app(proxy);
    Ok(Some(tokio::spawn(async move {
        if axum::serve(listener, router).await.is_err() {
            tracing::error!("SRE TLS proxy stopped; router must restart");
            std::process::exit(1);
        }
    })))
}

pub async fn readiness_probe() -> bool {
    let Ok(ca) = std::fs::read(Path::new(DIRECTORY).join("agent-ca.crt")) else {
        return false;
    };
    let Ok(ca) = reqwest::Certificate::from_pem(&ca) else {
        return false;
    };
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(ca)
        .timeout(std::time::Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    client
        .get(format!("https://127.0.0.1:{PORT}/readyz"))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}
