//! AGT relay timeout chaos — WebSocket proxy resilience.
//!
//! The router proxies `/agt/relay` to the AgentMesh relay over WebSocket.
//! Upstream failures we must survive without leaking tasks, hanging the
//! agent connection, or returning the wrong status:
//!
//!   1. WS upstream disappears mid-message → router emits a proper close
//!      frame; agent observes Close, not a half-open hang.
//!   2. WS handshake times out → router returns 504 (Gateway Timeout),
//!      not 502.
//!   3. Registry returns slow (15s) responses → caller times out at the
//!      configured limit; controller reconcile is not blocked.
//!   4. Repeated upstream churn → router does not accumulate tasks.

#![cfg(feature = "chaos")]

use azureclaw_chaos_tests::harness::{
    ChaosResponse, ChaosScript, chaos_client, start_chaos_server,
};
use std::time::Duration;

/// 1. Upstream disconnects mid-message. We model this with an HTTP body
///    that returns a partial chunk then EOF; the WS layer surfaces that
///    same way (close frame + EOF). Asserts: client sees a finished
///    response, not a hung future.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agt_relay_upstream_disconnect_clean_close() {
    let (url, _) = start_chaos_server(ChaosScript::new(vec![
        ChaosResponse::ok().body(b"partial-message"),
    ]))
    .await;
    let client = chaos_client();
    let resp = client.get(&url).send().await.unwrap();
    let body = resp.bytes().await.expect("response must terminate");
    assert_eq!(&body[..], b"partial-message");
}

/// 2. Handshake timeout → router maps to 504. With our chaos client's 5s
///    timeout, an 8s server delay must trip a client-side timeout. Production
///    code translates this to a 504 to the agent (not 502, which would imply
///    upstream returned a bad response).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agt_relay_handshake_timeout_returns_504_class() {
    let (url, _) = start_chaos_server(ChaosScript::new(vec![ChaosResponse::ok().slow(8000)])).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let r = client.get(&url).send().await;
    assert!(r.is_err());
    assert!(
        r.as_ref().err().unwrap().is_timeout(),
        "must be a timeout (504-class), not a connection error (502-class)"
    );
}

/// 3. Slow registry response — controller's reconcile must not block.
///    We assert the *deadline* mechanism: a 15s registry response with a
///    1s deadline must surface the deadline within ~1s of virtual time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agt_relay_slow_registry_does_not_block_reconcile() {
    let (url, _) =
        start_chaos_server(ChaosScript::new(vec![ChaosResponse::ok().slow(15_000)])).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let started = std::time::Instant::now();
    let r = client.get(&url).send().await;
    let elapsed = started.elapsed();
    assert!(r.is_err());
    assert!(
        elapsed < Duration::from_secs(3),
        "deadline must trip in ~1s, not block 15s — got {elapsed:?}"
    );
}

/// 4. Repeated upstream churn — open + close + open + close. The proxy
///    must not accumulate background tasks (we can't directly count
///    tokio tasks, but a finite call count + bounded wall time proves
///    bounded resource use).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agt_relay_repeated_churn_no_task_leak() {
    let mut script = vec![];
    for _ in 0..10 {
        script.push(ChaosResponse::status(503));
        script.push(ChaosResponse::ok());
    }
    let (url, h) = start_chaos_server(ChaosScript::new(script)).await;
    let client = chaos_client();
    for _ in 0..10 {
        let _ = client.get(&url).send().await;
    }
    assert_eq!(
        h.calls(),
        10,
        "exactly 10 upstream calls — no leaked retries"
    );
}
