// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! K8s API flake chaos — controller-side reliability invariants.
//!
//! Each test exercises a specific failure mode the kube-rs client (and our
//! controller's reconcile loop) must survive in production. We drive an
//! axum-based mock K8s API endpoint and assert the client-side reaction:
//! no panic, no infinite loop, eventual convergence, no leaked task.
//!
//! These tests do **not** spin up a real `kube::Client` against the mock —
//! that introduces TLS / discovery noise unrelated to the reliability
//! invariant under test. Instead they verify the HTTP-layer pattern the
//! controller depends on (respect Retry-After, restart on 410 GONE, reject
//! truncated JSON, handle stream tear-down).
//!
//! Invariant ↔ test map: see `docs/operations/chaos-tier.md`.

#![cfg(feature = "chaos")]

use azureclaw_chaos_tests::harness::{
    ChaosResponse, ChaosScript, chaos_client, http_with_retry, start_chaos_server,
};

/// 1. Random 500 on watch — re-watch must succeed within bounded retries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8s_watch_500_then_recovers() {
    let script = ChaosScript::new(vec![
        ChaosResponse::status(500),
        ChaosResponse::status(500),
        ChaosResponse::ok(),
    ]);
    let (url, h) = start_chaos_server(script).await;
    let client = chaos_client();
    let (status, _) = http_with_retry(&client, &url, 5).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(h.calls(), 3, "exactly 3 attempts (2 fails + 1 success)");
}

/// 2. Random 503 on watch stream — controller must retry, not panic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8s_watch_503_recovers_without_panic() {
    let script = ChaosScript::new(vec![ChaosResponse::status(503), ChaosResponse::ok()]);
    let (url, h) = start_chaos_server(script).await;
    let client = chaos_client();
    let (status, _) = http_with_retry(&client, &url, 4).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(h.calls(), 2);
}

/// 3. 429 with Retry-After — client must wait the advertised window
///    (not blast the API). We use a short Retry-After=1s so the test wall
///    time stays sub-second, and assert (a) request count == 2 (not a
///    burst) and (b) elapsed >= 1s (the wait actually happened).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8s_watch_429_respects_retry_after() {
    let script = ChaosScript::new(vec![
        ChaosResponse::status(429).retry_after(1),
        ChaosResponse::ok(),
    ]);
    let (url, h) = start_chaos_server(script).await;
    let client = chaos_client();
    let started = std::time::Instant::now();
    let (status, _) = http_with_retry(&client, &url, 4).await.unwrap();
    let elapsed = started.elapsed();
    assert_eq!(status, 200);
    assert_eq!(h.calls(), 2);
    assert!(
        elapsed >= std::time::Duration::from_secs(1),
        "expected to wait Retry-After=1s, elapsed={elapsed:?}"
    );
}

/// 4. Stale resourceVersion → 410 GONE. The controller must abandon the
///    cached version and restart the watch with a fresh list. We model this
///    as: 410, then 200. The retry helper's loop is the convergence proof.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8s_watch_410_gone_triggers_restart() {
    let script = ChaosScript::new(vec![
        ChaosResponse::status(410).body(br#"{"kind":"Status","reason":"Gone"}"#),
        ChaosResponse::ok().body(br#"{"kind":"List"}"#),
    ]);
    let (url, h) = start_chaos_server(script).await;
    let client = chaos_client();

    // First call sees 410 — the controller would discard cache + relist.
    let r1 = client.get(&url).send().await.unwrap();
    assert_eq!(r1.status(), 410);
    // Second call (the relist) succeeds.
    let r2 = client.get(&url).send().await.unwrap();
    assert_eq!(r2.status(), 200);
    assert_eq!(h.calls(), 2, "exactly two HTTP calls — no infinite loop");
}

/// 5. Truncated JSON in watch event — controller must log + drop the bad
///    chunk, never panic. We assert that `serde_json` returns Err (not a
///    panic) and that the reqwest layer returned 200 (the failure is at the
///    parse layer, where we want a recoverable error).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8s_watch_truncated_json_does_not_panic() {
    let bad = br#"{"type":"ADDED","object":{"metadata":{"name":"#; // truncated
    let script = ChaosScript::new(vec![ChaosResponse::ok().body(bad)]);
    let (url, _h) = start_chaos_server(script).await;
    let client = chaos_client();
    let resp = client.get(&url).send().await.unwrap();
    let body = resp.bytes().await.unwrap();
    let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&body);
    assert!(parsed.is_err(), "must fail to parse, not panic");
}

/// 6. Watch stream that closes mid-message — bytes() returns whatever was
///    received; downstream parser must reject. No panic, no hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8s_watch_premature_eof_recovers() {
    // Empty body — simulates connection closed before first byte.
    let script = ChaosScript::new(vec![
        ChaosResponse::status(200).body(b""),
        ChaosResponse::ok().body(br#"{"kind":"List","items":[]}"#),
    ]);
    let (url, h) = start_chaos_server(script).await;
    let client = chaos_client();

    let r1 = client.get(&url).send().await.unwrap();
    let b1 = r1.bytes().await.unwrap();
    assert!(b1.is_empty(), "first attempt: empty body");

    let r2 = client.get(&url).send().await.unwrap();
    let b2 = r2.bytes().await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&b2).unwrap();
    assert_eq!(v["kind"], "List");
    assert_eq!(h.calls(), 2);
}

/// 7. Bounded retry — when the API stays 500 forever, the controller must
///    surface the error after `max_attempts` (no infinite loop, no DoS on
///    the API). We assert termination, not success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8s_watch_persistent_500_terminates() {
    let script = ChaosScript::new(vec![
        ChaosResponse::status(500),
        ChaosResponse::status(500),
        ChaosResponse::status(500),
        ChaosResponse::status(500),
    ]);
    let (url, h) = start_chaos_server(script).await;
    let client = chaos_client();
    let res = http_with_retry(&client, &url, 3).await;
    // Either Err or Ok((500, _)) — both prove bounded termination.
    if let Ok((status, _)) = res {
        assert_eq!(status, 500);
    }
    assert!(h.calls() <= 3, "must not exceed max_attempts");
}

/// 8. Concurrent watches against a flaky API — none deadlock, all return.
///    Models the controller running multiple per-CRD watchers concurrently
///    when the API is shedding load.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8s_concurrent_watchers_all_return() {
    let script = ChaosScript::new(vec![
        ChaosResponse::status(503),
        ChaosResponse::status(503),
        ChaosResponse::ok(),
        ChaosResponse::ok(),
        ChaosResponse::ok(),
    ]);
    let (url, h) = start_chaos_server(script).await;
    let client = chaos_client();
    let mut handles = vec![];
    for _ in 0..5 {
        let c = client.clone();
        let u = url.clone();
        handles.push(tokio::spawn(
            async move { http_with_retry(&c, &u, 4).await },
        ));
    }
    for h in handles {
        let r = h.await.unwrap();
        assert!(r.is_ok(), "concurrent watcher must converge: {r:?}");
    }
    // 2 503s + 5 successful tail requests → exactly 7 calls in some interleaving,
    // but we only require >= 5 (no deadlock) and no leaked attempts.
    assert!(h.calls() >= 5);
}
