// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Same-pod keyless GitHub services. Credentials never cross the agent boundary.

use super::{
    AppState,
    github_policy::{self, Target},
};
use crate::{
    github_app::{self, Error, GitHubApp},
    github_services::GitHubServices,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

const API_LIMIT: usize = 2 * 1024 * 1024;
const GIT_LIMIT: usize = 16 * 1024 * 1024;
const LOG_LIMIT: usize = 32 * 1024 * 1024;
const LOG_TAIL: usize = 2 * 1024 * 1024;

#[derive(Clone)]
struct Service {
    config: Arc<GitHubServices>,
    client: reqwest::Client,
    blocklist: Arc<crate::blocklist::Blocklist>,
    sandbox: Arc<String>,
    slots: Arc<Semaphore>,
}

impl Service {
    fn for_state(state: &AppState) -> Result<Self, ()> {
        let config = state
            .services
            .github
            .get_or_init(|| {
                let client = github_app::client().map_err(|_| ())?;
                let identity = state
                    .services
                    .requests
                    .scope()
                    .ok()
                    .filter(|_| state.services.identity_valid)
                    .map(|scope| scope.identity);
                Ok(Arc::new(GitHubServices::new(identity, client)))
            })
            .clone()?;
        Ok(Self {
            client: config.client.clone(),
            slots: config.slots.clone(),
            config,
            blocklist: Arc::new(state.blocklist.clone()),
            sandbox: state.sandbox_name.clone(),
        })
    }
}

pub fn routes(state: AppState) -> Router<AppState> {
    let Ok(service) = Service::for_state(&state) else {
        return Router::new();
    };
    service_routes().with_state(service)
}

pub(super) struct Fetch {
    service: Service,
    peer: SocketAddr,
    request: Request,
}

impl Fetch {
    pub(super) async fn send(self) -> Response {
        // Preserve the socket-derived same-pod proof, never synthesize a local hop.
        tokio::time::timeout(
            Duration::from_secs(30),
            handler(State(self.service), ConnectInfo(self.peer), self.request),
        )
        .await
        .unwrap_or_else(|_| {
            deny(
                StatusCode::GATEWAY_TIMEOUT,
                "GitHub fetch deadline exceeded",
            )
        })
    }
}

pub(super) async fn prepare_fetch(
    state: &AppState,
    peer: Option<SocketAddr>,
    req: &serde_json::Value,
) -> Result<Option<Fetch>, Box<Response>> {
    let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let candidate = reqwest::Url::parse(url).ok().is_some_and(|url| {
        url.host_str().is_some_and(|host| {
            host.trim_end_matches('.')
                .eq_ignore_ascii_case("api.github.com")
        })
    });
    if !candidate {
        return Ok(None);
    }
    let service = Service::for_state(state).map_err(|_| {
        deny(
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub service configuration is unavailable",
        )
    })?;
    prepare_service_fetch(service, peer, req).await
}

async fn prepare_service_fetch(
    service: Service,
    peer: Option<SocketAddr>,
    req: &serde_json::Value,
) -> Result<Option<Fetch>, Box<Response>> {
    match service.config.current().await {
        Ok(Some(_)) => {}
        Ok(None) if !service.config.was_configured() => return Ok(None),
        _ => {
            return Err(Box::new(deny(
                StatusCode::SERVICE_UNAVAILABLE,
                "GitHub service configuration is unavailable",
            )));
        }
    }
    let peer = peer.filter(|peer| peer.ip().is_loopback()).ok_or_else(|| {
        deny(
            StatusCode::FORBIDDEN,
            "GitHub fetch requires a same-pod peer",
        )
    })?;
    let request = fetch_request(req).ok_or_else(|| {
        deny(
            StatusCode::FORBIDDEN,
            "GitHub fetch requires a canonical repository GET without credentials",
        )
    })?;
    Ok(Some(Fetch {
        service,
        peer,
        request,
    }))
}

fn fetch_request(req: &serde_json::Value) -> Option<Request> {
    let url = req.get("url")?.as_str()?;
    let path = url.strip_prefix(github_app::API)?;
    if !path.starts_with("/repos/")
        || path.contains('#')
        || req.get("method").is_some_and(|v| v.as_str() != Some("GET"))
        || req
            .get("body")
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        || req.get("headers").is_some_and(|v| {
            !v.as_object().is_some_and(|headers| {
                headers.iter().all(|(key, value)| {
                    (key.eq_ignore_ascii_case("accept") || key.eq_ignore_ascii_case("user-agent"))
                        && value.as_str().is_some()
                })
            })
        })
    {
        return None;
    }
    // Parse the original path, not URL-normalized dot segments or encodings.
    let uri: axum::http::Uri = format!("/gh-api{path}").parse().ok()?;
    github_policy::target(&uri, &Method::GET, false)?;
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .ok()
}

fn service_routes() -> Router<Service> {
    Router::new()
        .route("/git/{*path}", any(handler))
        .route("/gh-api/{*path}", any(handler))
        .route(
            "/v1/github-token",
            any(|| async { deny(StatusCode::GONE, "Raw GitHub credentials are not available") }),
        )
        .route("/v1/github/status", get(status))
}

fn deny(status: StatusCode, message: &'static str) -> Response {
    (status, [("cache-control", "no-store")], message).into_response()
}

async fn status(
    State(state): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    if !peer.ip().is_loopback() {
        return deny(StatusCode::NOT_FOUND, "Not found");
    }
    match state.config.current().await {
        Ok(Some(app)) => (
            [("cache-control", "no-store")],
            axum::Json(
                serde_json::json!({"enabled":true,"write":app.write_enabled(),"keyless":true}),
            ),
        )
            .into_response(),
        Ok(None) => deny(StatusCode::NOT_FOUND, "GitHub services are not configured"),
        Err(_) => deny(
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub service configuration is invalid",
        ),
    }
}

async fn handler(
    State(state): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    if !peer.ip().is_loopback() {
        return deny(StatusCode::NOT_FOUND, "Not found");
    }
    let Ok(_permit) = state.slots.clone().try_acquire_owned() else {
        return deny(
            StatusCode::TOO_MANY_REQUESTS,
            "GitHub service capacity exceeded",
        );
    };
    match tokio::time::timeout(Duration::from_secs(90), execute(&state, request)).await {
        Ok(response) => response,
        Err(_) => deny(
            StatusCode::GATEWAY_TIMEOUT,
            "GitHub service deadline exceeded; do not replay mutations automatically",
        ),
    }
}

async fn execute(state: &Service, request: Request) -> Response {
    let app = match state.config.current().await {
        Ok(Some(app)) => app,
        Ok(None) => return deny(StatusCode::NOT_FOUND, "GitHub services are not configured"),
        Err(_) => {
            return deny(
                StatusCode::SERVICE_UNAVAILABLE,
                "GitHub service configuration is invalid",
            );
        }
    };
    let (parts, body) = request.into_parts();
    let Some(target) = github_policy::target(&parts.uri, &parts.method, app.write_enabled()) else {
        return deny(
            StatusCode::FORBIDDEN,
            "GitHub method, path or query is outside the bounded service surface",
        );
    };
    if !app.allows(&target.repo) {
        return deny(
            StatusCode::FORBIDDEN,
            "Repository is outside the operator-granted scope",
        );
    }
    if git_request_gzip(target.git, &parts.method, &parts.headers).is_err() {
        return deny(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Only a single gzip Content-Encoding on Git POST is supported",
        );
    }
    // The validated repository binding admits only this bounded service, not
    // opaque GitHub egress. Threat/model fences still apply; log download hosts
    // separately require ordinary signed egress permission.
    if state
        .blocklist
        .check_github_service_egress(github_app::API, &state.sandbox)
        .await
        .is_err()
        || state
            .blocklist
            .check_github_service_egress(&target.url, &state.sandbox)
            .await
            .is_err()
    {
        return deny(
            StatusCode::FORBIDDEN,
            "GitHub destination is not allowed by egress policy",
        );
    }
    let limit = if target.git { GIT_LIMIT } else { API_LIMIT };
    let body = match to_bytes(body, limit).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return deny(
                StatusCode::PAYLOAD_TOO_LARGE,
                "GitHub request exceeds the service limit",
            );
        }
    };
    if parts.method == Method::GET && !body.is_empty() {
        return deny(StatusCode::BAD_REQUEST, "GET bodies are not supported");
    }
    if !target.git && parts.method == Method::POST && !valid_json_body(&body, &target.url) {
        return deny(
            StatusCode::BAD_REQUEST,
            "GitHub mutation requires a bounded JSON object and same-repository PR head",
        );
    }
    let token = match app.token(&target.repo).await {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(error = %error, "GitHub installation authentication failed");
            return deny(
                StatusCode::BAD_GATEWAY,
                "GitHub installation authentication failed",
            );
        }
    };
    // Revocation/rotation observed while minting must prevent a new dispatch.
    if !state.config.unchanged(&app).await {
        return deny(
            StatusCode::CONFLICT,
            "GitHub authority changed before dispatch",
        );
    }
    dispatch(state, &app, &target, parts, body, &token).await
}

fn git_request_gzip(git: bool, method: &Method, headers: &HeaderMap) -> Result<bool, ()> {
    let mut encodings = headers.get_all("content-encoding").iter();
    let Some(encoding) = encodings.next() else {
        return Ok(false);
    };
    if !git
        || *method != Method::POST
        || encodings.next().is_some()
        || !encoding
            .to_str()
            .is_ok_and(|value| value.eq_ignore_ascii_case("gzip"))
    {
        return Err(());
    }
    Ok(true)
}

async fn dispatch(
    state: &Service,
    app: &Arc<GitHubApp>,
    target: &Target,
    parts: axum::http::request::Parts,
    body: bytes::Bytes,
    token: &str,
) -> Response {
    let gzip = match git_request_gzip(target.git, &parts.method, &parts.headers) {
        Ok(gzip) => gzip,
        Err(()) => {
            return deny(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Only a single gzip Content-Encoding on Git POST is supported",
            );
        }
    };
    let url = target.url.clone();
    #[cfg(test)]
    let url = url.replacen(github_app::API, app.api_for_test(), 1);
    let mut builder = state
        .client
        .request(parts.method.clone(), &url)
        .header("User-Agent", "kars-inference-router");
    if target.git {
        builder = builder.basic_auth("x-access-token", Some(&token));
        if let Some(value) = parts.headers.get("git-protocol") {
            // Only a known protocol option, not arbitrary forwarded headers.
            if value == "version=2" {
                builder = builder.header("Git-Protocol", "version=2");
            }
        }
        if parts.method == Method::POST {
            let service = if target.url.ends_with("/git-receive-pack") {
                "receive"
            } else {
                "upload"
            };
            builder = builder.header(
                "Content-Type",
                format!("application/x-git-{service}-pack-request"),
            );
            if gzip {
                // Preserve the bounded wire body and emit only the validated coding.
                builder = builder.header("Content-Encoding", "gzip");
            }
        }
    } else {
        builder = builder
            .bearer_auth(&token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if parts.method == Method::POST {
            builder = builder.header("Content-Type", "application/json");
        }
    }
    // Never forward cookies, inbound auth, proxy headers, request IDs, or
    // Connection-nominated headers. Never retry an accepted request (even 401).
    let upstream = match builder.body(body).send().await {
        Ok(response) => response,
        Err(_) => {
            return deny(
                StatusCode::BAD_GATEWAY,
                "GitHub upstream request failed; mutation outcome may be unknown",
            );
        }
    };
    if upstream.status() == StatusCode::UNAUTHORIZED {
        app.invalidate(&target.repo, token).await;
    }
    response(state, app, target, upstream).await
}

fn valid_json_body(body: &[u8], url: &str) -> bool {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_slice(body) else {
        return false;
    };
    if url.ends_with("/pulls") {
        object
            .get("head")
            .and_then(|head| head.as_str())
            .is_some_and(|head| {
                !head.is_empty()
                    && head.len() <= 255
                    && !head.contains(':')
                    && !head.chars().any(char::is_control)
            })
    } else {
        true
    }
}

async fn egress(state: &Service, url: &str) -> bool {
    state
        .blocklist
        .check_egress(url, &state.sandbox)
        .await
        .is_ok()
}

async fn response(
    state: &Service,
    app: &Arc<GitHubApp>,
    target: &Target,
    upstream: reqwest::Response,
) -> Response {
    if target.logs && upstream.status() == StatusCode::FOUND {
        let location = upstream
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .and_then(github_policy::log_redirect);
        let Some(location) = location else {
            return deny(
                StatusCode::BAD_GATEWAY,
                "GitHub log redirect is not permitted",
            );
        };
        if !egress(state, location.as_str()).await {
            return deny(
                StatusCode::FORBIDDEN,
                "GitHub log destination is not allowed by egress policy",
            );
        }
        if !state.config.unchanged(app).await {
            return deny(
                StatusCode::CONFLICT,
                "GitHub authority changed before log download",
            );
        }
        return download(state, location).await;
    }
    finish(
        upstream,
        if target.git {
            GIT_LIMIT
        } else if target.logs {
            LOG_LIMIT
        } else {
            API_LIMIT
        },
        target.logs,
        !target.git && !target.logs,
    )
    .await
}

async fn download(state: &Service, location: reqwest::Url) -> Response {
    // Fresh request with no bearer/cookies/Referer; redirects remain disabled.
    let download = match state
        .client
        .get(location)
        .header("User-Agent", "kars-inference-router")
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return deny(StatusCode::BAD_GATEWAY, "GitHub signed log download failed"),
    };
    finish(download, LOG_LIMIT, true, false).await
}

fn remove_repository_credentials(value: &mut serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            let mut removed = object.remove("temp_clone_token").is_some();
            for child in object.values_mut() {
                removed |= remove_repository_credentials(child);
            }
            removed
        }
        serde_json::Value::Array(array) => {
            let mut removed = false;
            for child in array {
                removed |= remove_repository_credentials(child);
            }
            removed
        }
        _ => false,
    }
}

async fn finish(upstream: reqwest::Response, limit: usize, logs: bool, json: bool) -> Response {
    let status = upstream.status();
    if !status.is_success() {
        // Suppress signed URLs, upstream diagnostic bodies, cookies and Link
        // headers. Keep actionable HTTP status without upstream body leakage.
        let status = if status.is_redirection() {
            StatusCode::BAD_GATEWAY
        } else {
            status
        };
        return deny(status, "GitHub upstream did not complete the request");
    }
    let mut headers = HeaderMap::new();
    if !logs {
        if let Some(content_type) = upstream.headers().get("content-type") {
            headers.insert("content-type", content_type.clone());
        }
    } else {
        headers.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
    }
    let mut bytes = match github_app::bounded(upstream, limit).await {
        Ok(bytes) => bytes,
        Err(Error::Limit) => {
            return deny(
                StatusCode::BAD_GATEWAY,
                "GitHub response exceeds the service limit",
            );
        }
        Err(_) => return deny(StatusCode::BAD_GATEWAY, "GitHub response was interrupted"),
    };
    if json && !bytes.is_empty() {
        let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return deny(StatusCode::BAD_GATEWAY, "GitHub returned invalid JSON");
        };
        // Repository objects also appear nested in pull requests and other REST results.
        if remove_repository_credentials(&mut value) {
            bytes = match serde_json::to_vec(&value) {
                Ok(bytes) if bytes.len() <= limit => bytes,
                _ => {
                    return deny(
                        StatusCode::BAD_GATEWAY,
                        "GitHub response exceeds the service limit",
                    );
                }
            };
        }
        headers.insert(
            "content-type",
            "application/json; charset=utf-8".parse().unwrap(),
        );
    }
    if logs && bytes.len() > LOG_TAIL {
        bytes.drain(..bytes.len() - LOG_TAIL);
        headers.insert("x-kars-log-truncated", "true".parse().unwrap());
    }
    headers.insert("cache-control", "no-store".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    (status, headers, Body::from(bytes)).into_response()
}

#[cfg(test)]
#[path = "github_proxy_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "egress_github_tests.rs"]
mod fetch_tests;
