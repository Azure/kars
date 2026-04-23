//! End-to-end proxy test against fake upstream servers.
//!
//! Exercises the full `proxy::forward` → auth (IMDS / API-key) → upstream flow
//! without touching live Azure. Every JSON comes from
//! `tests/fixtures/foundry/` — sanitized captures of real responses.
//!
//! Env vars are process-global, so both test functions take a shared mutex
//! before touching the environment.

// The mutex guard is held across `.await` points intentionally: env mutation
// must remain serialized for the whole test. `tokio::sync::Mutex` would add a
// dev-dep without a real benefit here — these tests are IO-bound and never
// re-enter the lock.
#![allow(clippy::await_holding_lock)]

mod common;

use axum::http::{HeaderMap, Method};
use azureclaw_inference_router::auth::WorkloadIdentityAuth;
use azureclaw_inference_router::proxy::{UpstreamConfig, forward};
use bytes::Bytes;
use common::{FakeAd, FakeAzure, FakeImds, FixtureRoute};
use std::sync::Mutex;

// Env vars are process-wide. Serialize tests that mutate them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn scrub_env() {
    unsafe {
        // SAFETY: Tests are serialized through ENV_LOCK; no other thread is reading
        // these env vars concurrently. This is required on modern Rust because
        // `remove_var` / `set_var` are now unsafe to reflect global-state risk.
        for k in [
            "AZURE_OPENAI_API_KEY",
            "AZURE_TENANT_ID",
            "AZURE_CLIENT_ID",
            "IMDS_CLIENT_ID",
            "AZURE_FEDERATED_TOKEN_FILE",
            "AZURE_AD_ENDPOINT",
            "AZURE_IMDS_ENDPOINT",
        ] {
            std::env::remove_var(k);
        }
    }
}

unsafe fn set_env(k: &str, v: &str) {
    unsafe { std::env::set_var(k, v) }
}

#[tokio::test]
async fn api_key_mode_proxies_chat_completion_with_filter_results() {
    let _g = ENV_LOCK.lock().unwrap();
    scrub_env();
    unsafe { set_env("AZURE_OPENAI_API_KEY", "test-key-zzz") };

    let azure = FakeAzure::start(vec![FixtureRoute::from_file(
        "POST",
        "/openai/v1/chat/completions",
        "chat_completion_ok.json",
    )])
    .await;

    let auth = WorkloadIdentityAuth::new();
    assert!(auth.is_api_key_mode(), "expected API-key mode");

    let upstream = UpstreamConfig {
        endpoint: azure.base_url(),
        deployment: "gpt-4o".to_string(),
        sandbox_name: "test-sandbox".to_string(),
    };
    let client = reqwest::Client::new();

    let req_body = serde_json::json!({
        "messages": [{"role": "user", "content": "hello fixture"}]
    });
    let body_bytes = Bytes::from(serde_json::to_vec(&req_body).unwrap());

    let (status, _headers, resp) = forward(
        &auth,
        &client,
        &upstream,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        body_bytes,
    )
    .await
    .expect("proxy forward");

    assert_eq!(status.as_u16(), 200, "fake Azure should return 200");
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
    // Router should have preserved the prompt_filter_results block the downstream
    // safety.rs parser depends on.
    assert!(v["prompt_filter_results"].is_array());
    assert_eq!(v["usage"]["total_tokens"], 55);

    // Validate the request that actually reached the fake upstream.
    let log = azure.log.entries();
    assert_eq!(log.len(), 1, "one upstream call");
    let rec = &log[0];
    assert_eq!(rec.method, "POST");
    assert_eq!(rec.path, "/openai/v1/chat/completions");
    // Bearer auth header should be present.
    let has_bearer = rec
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("authorization") && v.starts_with("Bearer "));
    assert!(has_bearer, "upstream must receive Bearer auth");
    // Model should have been injected by build_upstream_url.
    let body: serde_json::Value = serde_json::from_slice(&rec.body).unwrap();
    assert_eq!(body["model"], "gpt-4o");
}

#[tokio::test]
async fn wi_mode_falls_back_to_imds_and_proxies_embeddings() {
    let _g = ENV_LOCK.lock().unwrap();
    scrub_env();

    // Configure WI mode but without a federated token file — this forces the
    // WI exchange to fail, which in turn triggers the IMDS fallback path.
    let imds = FakeImds::start().await;
    let ad = FakeAd::start().await;
    unsafe {
        set_env("AZURE_TENANT_ID", "fake-tenant");
        set_env("AZURE_CLIENT_ID", "fake-client");
        // point WI at a path that will not exist
        set_env(
            "AZURE_FEDERATED_TOKEN_FILE",
            "/nonexistent/azureclaw-test-token",
        );
        set_env("AZURE_AD_ENDPOINT", &ad.base_url());
        set_env("AZURE_IMDS_ENDPOINT", &imds.base_url());
    }

    let azure = FakeAzure::start(vec![FixtureRoute::from_file(
        "POST",
        "/openai/v1/embeddings",
        "embeddings_ok.json",
    )])
    .await;

    let auth = WorkloadIdentityAuth::new();
    assert!(!auth.is_api_key_mode(), "expected WI mode");

    let upstream = UpstreamConfig {
        endpoint: azure.base_url(),
        deployment: "text-embedding-3-small".to_string(),
        sandbox_name: "test-sandbox-wi".to_string(),
    };
    let client = reqwest::Client::new();
    let body = Bytes::from(r#"{"input":"hello"}"#.as_bytes().to_vec());

    let (status, _headers, resp) = forward(
        &auth,
        &client,
        &upstream,
        Method::POST,
        "embeddings",
        &HeaderMap::new(),
        body,
    )
    .await
    .expect("proxy forward (IMDS)");

    assert_eq!(status.as_u16(), 200);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(v["model"], "text-embedding-3-small");
    assert_eq!(v["data"][0]["embedding"].as_array().unwrap().len(), 5);

    // IMDS must have been hit (WI path failed; fallback kicked in).
    assert!(
        !imds.log.is_empty(),
        "IMDS fallback should have been invoked"
    );
    let imds_req = &imds.log.entries()[0];
    assert_eq!(imds_req.path, "/metadata/identity/oauth2/token");
    // Metadata header is mandatory for IMDS.
    let has_metadata = imds_req
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("metadata") && v == "true");
    assert!(has_metadata, "IMDS request must carry Metadata: true");

    // AD may have been hit once for the failed WI exchange — that's fine,
    // but the token actually used must be the IMDS one.
    let azure_req = &azure.log.entries()[0];
    let auth_header = azure_req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    assert!(auth_header.starts_with("Bearer "));
    assert!(
        auth_header.contains("fake-imds-token-abc123"),
        "upstream must receive the IMDS-issued token, got {auth_header}"
    );
}

#[tokio::test]
async fn upstream_error_status_is_propagated() {
    let _g = ENV_LOCK.lock().unwrap();
    scrub_env();
    unsafe { set_env("AZURE_OPENAI_API_KEY", "test-key-err") };

    let azure = FakeAzure::start(vec![
        FixtureRoute::from_file("POST", "/openai/v1/chat/completions", "error_429.json")
            .with_status(429),
    ])
    .await;

    let auth = WorkloadIdentityAuth::new();
    let upstream = UpstreamConfig {
        endpoint: azure.base_url(),
        deployment: "gpt-4o".to_string(),
        sandbox_name: "test-sandbox-429".to_string(),
    };
    let client = reqwest::Client::new();

    let (status, _headers, resp) = forward(
        &auth,
        &client,
        &upstream,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from(r#"{"messages":[]}"#.as_bytes().to_vec()),
    )
    .await
    .expect("forward should surface upstream errors as data, not Err");

    assert_eq!(status.as_u16(), 429);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(v["error"]["type"], "rate_limit_exceeded");
}
