//! Reverse proxy logic — forwards inference requests to Azure OpenAI / AI Foundry.

use anyhow::Result;
use axum::body::Body;
use axum::http::{Request, Response};
use crate::auth::WorkloadIdentityAuth;

/// Forward an inference request to the upstream Azure OpenAI endpoint.
pub async fn forward_request(
    auth: &WorkloadIdentityAuth,
    upstream_url: &str,
    mut req: Request<Body>,
) -> Result<Response<Body>> {
    // Acquire token via Workload Identity (no API keys)
    let token = auth
        .get_token("https://cognitiveservices.azure.com")
        .await?;

    // Strip any credentials the sandbox may have tried to inject
    req.headers_mut().remove("authorization");
    req.headers_mut().remove("api-key");

    // Inject Managed Identity bearer token
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().unwrap(),
    );

    // Forward via reqwest (TODO: use hyper client for zero-copy)
    let client = reqwest::Client::new();
    let method = req.method().clone();
    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await?;

    let upstream_resp = client
        .request(method, upstream_url)
        .headers(req.headers().clone())
        .body(body_bytes.clone())
        .send()
        .await?;

    let status = upstream_resp.status();
    let headers = upstream_resp.headers().clone();
    let resp_body = upstream_resp.bytes().await?;

    let mut response = Response::builder().status(status);
    for (k, v) in headers.iter() {
        response = response.header(k, v);
    }

    Ok(response.body(Body::from(resp_body))?)
}
