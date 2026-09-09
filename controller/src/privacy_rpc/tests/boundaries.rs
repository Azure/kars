// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn privacy_rpc_has_no_proxy_mutation_mint_or_arbitrary_secret_surface() {
    let rig = Rig::new(true).await;
    for path in [
        "/internal/access-requests/reset",
        "/api/v1/secrets",
        "/token",
        "/internal/observations/verify-privacy?url=http://evil",
    ] {
        let response = rig
            .client
            .post(format!("{}{path}", rig.origin))
            .bearer_auth(TOKEN)
            .json(&rig.request)
            .send()
            .await
            .unwrap();
        assert!(!response.status().is_success(), "{path}");
    }
    let mut body = serde_json::to_value(&rig.request).unwrap();
    body["secretName"] = "sre-api-router-identity".into();
    let response = rig
        .client
        .post(format!("{}{}", rig.origin, wire::PATH))
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let mut oversized = serde_json::to_value(&rig.request).unwrap();
    oversized["identity"]["padding"] = "x".repeat(wire::MAX_BODY).into();
    let response = rig
        .client
        .post(format!("{}{}", rig.origin, wire::PATH))
        .bearer_auth(TOKEN)
        .json(&oversized)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert!(rig.data.lock().unwrap().calls.is_empty());
    let _held = rig
        .state
        .capacity
        .clone()
        .acquire_many_owned(4)
        .await
        .unwrap();
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn privacy_rpc_deadline_is_bounded_and_returns_no_backend_diagnostics() {
    let rig = Rig::new(true).await;
    rig.data.lock().unwrap().delay = true;
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::FORBIDDEN
    );
}
