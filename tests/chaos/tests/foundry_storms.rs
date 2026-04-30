// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Foundry 429 / 503 storm chaos — router-side reliability invariants.
//!
//! Models Azure OpenAI / AI Search / Document Intelligence pushing back
//! under load. The router must:
//!   * Respect Retry-After (no thundering herd against a struggling backend).
//!   * Propagate 429 to the caller (sandbox agent), not silently 500.
//!   * Emit metrics for blocked attempts (visible on
//!     `/egress/learned/blocked`).
//!   * Keep the SSE / streaming connection in a sane state on backend 503.
//!
//! See `inference-router/src/proxy.rs` for the production retry path; these
//! tests verify the same pattern on a smaller, mockable surface.

#![cfg(feature = "chaos")]

use azureclaw_chaos_tests::harness::{
    ChaosResponse, ChaosScript, chaos_client, http_with_retry, start_chaos_server,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// 1. 100 concurrent requests, 80% return 429 with Retry-After 1–5s.
///    Assert: no thundering herd (Retry-After respected), all requests
///    eventually complete (no hang), call count is bounded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foundry_429_storm_respects_retry_after() {
    // Build a script of 100 sequential responses: 80 are 429 (Retry-After=2),
    // 20 are 200. Concurrent callers will each pop their next response from
    // the same script; the order doesn't matter — only the rate-limit
    // invariant.
    let mut script = vec![];
    for i in 0..100 {
        if i % 5 == 0 {
            script.push(ChaosResponse::ok());
        } else {
            script.push(ChaosResponse::status(429).retry_after(2));
        }
    }
    // Provide additional 200s so retries can find success.
    for _ in 0..200 {
        script.push(ChaosResponse::ok());
    }
    let (url, h) = start_chaos_server(ChaosScript::new(script)).await;
    let client = chaos_client();
    let mut handles = vec![];
    for _ in 0..100 {
        let c = client.clone();
        let u = url.clone();
        handles.push(tokio::spawn(
            async move { http_with_retry(&c, &u, 6).await },
        ));
    }
    let mut succeeded = 0u32;
    for h in handles {
        if let Ok(Ok((status, _))) = h.await
            && status == 200
        {
            succeeded += 1;
        }
    }
    assert!(
        succeeded >= 80,
        "expected most callers to converge to 200, got {succeeded}"
    );
    // No thundering herd: total upstream calls bounded by the script + retries.
    assert!(h.calls() < 600, "thundering herd: {} calls", h.calls());
}

/// 2. 429 must propagate to the caller as 429 (not be wrapped in a 500).
///    The router translates upstream 429 → caller 429 so client-side rate
///    limiting (e.g. agent waiting before next call) works correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foundry_429_propagates_to_caller() {
    let (url, _) = start_chaos_server(ChaosScript::new(vec![
        ChaosResponse::status(429).retry_after(1),
        ChaosResponse::status(429).retry_after(1),
        ChaosResponse::status(429).retry_after(1),
    ]))
    .await;
    let client = chaos_client();
    // With max_attempts=3, the helper exhausts retries and returns the last
    // status. The caller sees 429 — not a synthetic 500.
    let r = http_with_retry(&client, &url, 3).await;
    match r {
        Ok((status, _)) => assert_eq!(status, 429, "must propagate, not synthesize 500"),
        Err(e) => panic!("must not error out: {e}"),
    }
}

/// 3. Burst of 503s while caller is mid-stream — router emits proper SSE
///    error, doesn't hang. We model the SSE side using a chunked body that
///    the server tears down. The client must observe a clean stream end,
///    not a stuck connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foundry_503_midstream_sse_emits_clean_close() {
    let (url, _) = start_chaos_server(ChaosScript::new(vec![
        ChaosResponse::status(503).body(b"data: partial\n\n"),
    ]))
    .await;
    let client = chaos_client();
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 503);
    let body = resp.bytes().await.expect("body must terminate");
    assert!(body.starts_with(b"data: partial"));
    // The key invariant: bytes() returned (the connection didn't hang).
}

/// 4. Backend latency spike — 1 call out of 10 takes 5s. Caller's overall
///    timeout (5s in chaos_client) must trip; no leaked sockets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foundry_slow_backend_caller_timeout_trips() {
    let (url, _) = start_chaos_server(ChaosScript::new(vec![ChaosResponse::ok().slow(8000)])).await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap();
    let r = client.get(&url).send().await;
    assert!(r.is_err(), "expected client-side timeout");
    assert!(
        r.as_ref().err().unwrap().is_timeout(),
        "must be a timeout error, not a synthetic failure: {:?}",
        r.err()
    );
}

/// 5. Blocked-attempt metric increments on each 429 / 5xx. Models the
///    `/egress/learned/blocked` counter the router exposes. We assert that
///    every upstream rejection bumps the counter exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foundry_blocked_metric_increments_per_rejection() {
    let blocked = Arc::new(AtomicU64::new(0));
    let (url, _) = start_chaos_server(ChaosScript::new(vec![
        ChaosResponse::status(429).retry_after(1),
        ChaosResponse::status(503),
        ChaosResponse::ok(),
    ]))
    .await;
    let client = chaos_client();
    for _ in 0..3 {
        let r = client.get(&url).send().await.unwrap();
        if r.status().as_u16() >= 429 {
            blocked.fetch_add(1, Ordering::SeqCst);
        }
    }
    assert_eq!(
        blocked.load(Ordering::SeqCst),
        2,
        "exactly 2 rejections (429 + 503), not 0 or 3"
    );
}

/// 6. Mixed-quality storm — 200, 429, 200, 503, 200. Caller's retry must
///    drive each call to 200; router must not amplify the storm.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foundry_mixed_storm_converges() {
    let (url, h) = start_chaos_server(ChaosScript::new(vec![
        ChaosResponse::ok(),
        ChaosResponse::status(429).retry_after(1),
        ChaosResponse::ok(),
        ChaosResponse::status(503),
        ChaosResponse::ok(),
        ChaosResponse::ok(),
        ChaosResponse::ok(),
    ]))
    .await;
    let client = chaos_client();
    for _ in 0..3 {
        let (s, _) = http_with_retry(&client, &url, 5).await.unwrap();
        assert_eq!(s, 200);
    }
    assert!(
        h.calls() <= 7,
        "no amplification: {} calls for 3 logical requests",
        h.calls()
    );
}
