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
async fn observation_active_sre_cannot_reuse_status_only_privacy_or_gain_ambient_secret_reads() {
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
        StatusCode::FORBIDDEN
    );
    assert!(metadata.lock().unwrap().calls.is_empty());
}
