// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn observation_duplicate_authorization_cannot_hide_purpose_from_legacy_loopback_routes() {
    let (_server, state, metadata) = fixture().await;
    for (method, path) in [
        ("GET", "/egress/learned"),
        ("POST", "/egress/learned/clear"),
    ] {
        let request = Request::builder()
            .uri(path)
            .method(method)
            .extension(ConnectInfo(
                "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
            ))
            .header("authorization", format!("Bearer {}", observer_token()))
            .header("authorization", "Bearer unrelated")
            .body(Body::empty())
            .unwrap();
        let response = router(state.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert!(metadata.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn observation_active_sre_uses_fresh_rpc_proof_without_ambient_secret_reads() {
    let (server, mut state, metadata) = fixture().await;
    let mut binding = state.services.observer.as_ref().unwrap().binding().clone();
    binding.privacy_epoch = Some("current".into());
    let client = kube::Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    Arc::get_mut(&mut state.services).unwrap().observer = Some(Observer::for_test(
        binding,
        observer_token(),
        "secret-uid:1".into(),
        client,
    ));
    {
        let mut metadata = metadata.lock().unwrap();
        metadata.objects.get_mut(SANDBOX).unwrap()["status"]["serviceObservation"]["privacyEpoch"] =
            "current".into();
        metadata.objects.insert(REGISTRATION.into(), json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
            "metadata":{"name":"canonical","uid":"registration","generation":1},
            "spec":{"enabled":true},"status":{"phase":"Ready","observedGeneration":1,
                "privacyRevision":crate::sre_privacy::REVISION,"privacyEpoch":"current","legacySecretAccessDenied":true}
        }));
    }
    assert_eq!(
        call(&state, SCOPE, "GET", Some(&observer_token()), None)
            .await
            .0,
        StatusCode::OK
    );
    metadata
        .lock()
        .unwrap()
        .verifier
        .as_ref()
        .unwrap()
        .control
        .lock()
        .unwrap()
        .fault = "deny".into();
    assert_eq!(
        call(&state, SCOPE, "GET", Some(&observer_token()), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let data = metadata.lock().unwrap();
    assert!(
        !data
            .calls
            .iter()
            .any(|(_, path, _)| path.contains("/secrets"))
    );
    assert_eq!(
        data.verifier
            .as_ref()
            .unwrap()
            .control
            .lock()
            .unwrap()
            .calls
            .len(),
        2
    );
}

#[tokio::test]
async fn observation_missing_old_controller_capability_and_expired_binding_are_unavailable() {
    for mode in ["absent", "expired"] {
        let (server, mut state, metadata) = fixture().await;
        let mut binding = state.services.observer.as_ref().unwrap().binding().clone();
        if mode == "absent" {
            binding.verifier = None
        } else {
            binding.expires_at = chrono::Utc::now().timestamp() - 1
        }
        let client =
            kube::Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        Arc::get_mut(&mut state.services).unwrap().observer = Some(Observer::for_test(
            binding,
            observer_token(),
            "secret-uid:1".into(),
            client,
        ));
        assert_eq!(
            call(&state, SCOPE, "GET", Some(&observer_token()), None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert!(metadata.lock().unwrap().calls.is_empty());
    }
}

#[tokio::test]
async fn observation_prepared_only_allows_verifier_backed_scope_discovery_not_learned_data() {
    let (_server, state, metadata) = fixture().await;
    metadata.lock().unwrap().objects.get_mut(SANDBOX).unwrap()["status"]["serviceObservation"]["phase"] =
        "Prepared".into();
    let (status, scope) = call(&state, SCOPE, "GET", Some(&observer_token()), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        scope["privacy_verifier"],
        crate::observation_privacy::CAPABILITY
    );
    assert_eq!(
        call(
            &state,
            LEARNED,
            "GET",
            Some(&observer_token()),
            scope["scope_id"].as_str()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn observation_scope_reset_during_rpc_cannot_consume_the_old_scope_proof() {
    let (_server, state, metadata) = fixture().await;
    let verifier = metadata.lock().unwrap().verifier.as_ref().unwrap().clone();
    verifier.control.lock().unwrap().fault = "delay".into();
    let old = state.services.requests.scope().unwrap();
    let reader = state.clone();
    let pending =
        tokio::spawn(
            async move { call(&reader, SCOPE, "GET", Some(&observer_token()), None).await },
        );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !verifier.control.lock().unwrap().calls.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    state.services.reset(&old.id, None).unwrap();
    assert_eq!(pending.await.unwrap().0, StatusCode::CONFLICT);
}
