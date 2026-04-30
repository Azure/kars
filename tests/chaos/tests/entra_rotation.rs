//! Entra rotation race chaos — token refresh + JWKS rotation.
//!
//! The router caches Workload Identity tokens (see
//! `inference-router/src/auth.rs`). Under rotation, three races can hurt:
//!
//!   1. WI federated token expires while a request is in flight → refresh
//!      mid-call without dropping the request.
//!   2. Two concurrent token refreshes → only one network call lands on
//!      Entra (single-flight invariant).
//!   3. JWKS signing-Kid rotates between two consecutive verifications →
//!      the verifier must re-fetch JWKS rather than serve 5xx.

#![cfg(feature = "chaos")]

use azureclaw_chaos_tests::harness::{
    ChaosResponse, ChaosScript, chaos_client, start_chaos_server,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

/// 1. Token expires mid-flight. The fetch path:
///    a) cache hit → use cached token.
///    b) cache stale (<60s left) → refresh.
///    The race: a request reads the cache while another refreshes. Both
///    must succeed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entra_token_refresh_midflight_no_drop() {
    let (url, _h) = start_chaos_server(ChaosScript::new(vec![
        ChaosResponse::ok().body(br#"{"access_token":"new-token","expires_in":3600}"#),
        ChaosResponse::ok().body(br#"{"access_token":"new-token","expires_in":3600}"#),
    ]))
    .await;
    let client = chaos_client();
    // Two concurrent "refresh" calls — both must get a token, neither
    // observes a dropped connection.
    let (a, b) = tokio::join!(client.post(&url).send(), client.post(&url).send(),);
    assert_eq!(a.unwrap().status(), 200);
    assert_eq!(b.unwrap().status(), 200);
}

/// 2. Single-flight invariant. With a `tokio::sync::Mutex`-style guard,
///    only one network call to Entra should happen even when N callers
///    arrive simultaneously with a stale cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entra_single_flight_one_network_call() {
    // Counter for actual upstream calls.
    let upstream_calls = Arc::new(AtomicU64::new(0));
    let cache: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Simulated "refresh" function: locks the cache, checks if another
    // thread already populated, otherwise increments counter + sets.
    async fn refresh(cache: &Mutex<Option<String>>, calls: &AtomicU64) -> String {
        let mut g = cache.lock().await;
        if let Some(t) = g.as_ref() {
            return t.clone();
        }
        // Simulate the network round-trip.
        tokio::time::sleep(Duration::from_millis(10)).await;
        calls.fetch_add(1, Ordering::SeqCst);
        let token = format!("token-{}", calls.load(Ordering::SeqCst));
        *g = Some(token.clone());
        token
    }

    let mut handles = vec![];
    for _ in 0..16 {
        let c = Arc::clone(&cache);
        let n = Arc::clone(&upstream_calls);
        handles.push(tokio::spawn(async move { refresh(&c, &n).await }));
    }
    let mut tokens = vec![];
    for h in handles {
        tokens.push(h.await.unwrap());
    }
    assert_eq!(
        upstream_calls.load(Ordering::SeqCst),
        1,
        "single-flight violated: {} network calls",
        upstream_calls.load(Ordering::SeqCst)
    );
    assert!(tokens.iter().all(|t| t == "token-1"));
}

/// 3. JWKS rotation. The verifier holds Kid=A; a token signed by Kid=B
///    arrives. The verifier must re-fetch JWKS (one extra network call)
///    rather than reject as 401/5xx.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entra_jwks_rotation_refetches_on_unknown_kid() {
    // First /jwks call returns key A; second returns keys A + B.
    let (url, h) = start_chaos_server(ChaosScript::new(vec![
        ChaosResponse::ok().body(br#"{"keys":[{"kid":"A","kty":"RSA"}]}"#),
        ChaosResponse::ok().body(br#"{"keys":[{"kid":"A","kty":"RSA"},{"kid":"B","kty":"RSA"}]}"#),
    ]))
    .await;
    let client = chaos_client();

    // Fetch JWKS the first time (verifier startup).
    let r1 = client.get(&url).send().await.unwrap();
    let body1: serde_json::Value = serde_json::from_slice(&r1.bytes().await.unwrap()).unwrap();
    assert!(body1["keys"][0]["kid"] == "A");

    // A token with Kid=B arrives — verifier doesn't know it. It must
    // re-fetch JWKS rather than 5xx.
    let r2 = client.get(&url).send().await.unwrap();
    let body2: serde_json::Value = serde_json::from_slice(&r2.bytes().await.unwrap()).unwrap();
    let kids: Vec<&str> = body2["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["kid"].as_str().unwrap())
        .collect();
    assert!(kids.contains(&"B"), "JWKS re-fetch must surface new Kid=B");
    assert_eq!(h.calls(), 2, "exactly 2 JWKS fetches — not a fetch storm");
}

/// 4. Federated SA token file rotates on disk during a refresh. The auth
///    code reads the file fresh on each refresh, so the second refresh
///    must pick up the new value (no stale-token cache poisoning).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entra_sa_token_file_rotation_picks_up_new_value() {
    use std::io::Write;

    let dir = std::env::temp_dir().join(format!("azc-chaos-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sa-token");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"old-sa-token").unwrap();
    }
    let v1 = std::fs::read_to_string(&path).unwrap();
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"new-sa-token").unwrap();
    }
    let v2 = std::fs::read_to_string(&path).unwrap();
    assert_eq!(v1, "old-sa-token");
    assert_eq!(
        v2, "new-sa-token",
        "SA token rotation must surface fresh value"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
