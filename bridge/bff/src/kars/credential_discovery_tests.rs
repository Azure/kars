// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn enroll(state: &mut TestApi, namespace: &str) -> String {
    let path = format!(
        "/apis/kars.azure.com/v1alpha1/namespaces/{namespace}/karscredentialgrants/workspace"
    );
    state.objects.insert(path.clone(), json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":namespace,"uid":"grant","generation":1},
        "spec":{"enabled":true,"workspaceUid":"namespace","agentKeys":[],
            "integrationStores":[{"secret":{"name":"kars-inference-providers","uid":"secret"},"purpose":"inference"}]},
        "status":{"phase":"Ready","observedGeneration":1,"sources":[],"legacySources":[]}
    }));
    state.objects.insert(
        format!("/api/v1/namespaces/{namespace}"),
        json!({
            "apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace,"uid":"namespace"}
        }),
    );
    let mut secret = state.secret.clone();
    secret["metadata"]["name"] = "kars-inference-providers".into();
    secret["metadata"]["namespace"] = namespace.into();
    state.objects.insert(
        format!("/api/v1/namespaces/{namespace}/secrets/kars-inference-providers"),
        secret,
    );
    path
}

#[tokio::test]
async fn optional_configuration_handlers_accept_unenrolled_stores_without_secret_reads() {
    for enrolled in [false, true] {
        let (cluster, state, server) = fixture().await;
        let namespace = cluster.core_namespace();
        if enrolled {
            let mut state = state.lock().unwrap();
            let path = enroll(&mut state, &namespace);
            state.objects.get_mut(&path).unwrap()["spec"]["integrationStores"] = json!([]);
        }
        let app = crate::state::AppState::for_test_client(cluster.client.clone(), &namespace);
        let providers = crate::routes::operator::list_additional_providers(State(app.clone()))
            .await
            .unwrap()
            .0;
        assert!(providers.is_empty());
        let github = crate::routes::github::get_app(State(app)).await.unwrap().0;
        assert!(!github.configured);
        assert!(!github.storage_ready);
        assert!(github.slug.is_none() && github.install_url.is_none());
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, path, _)| { method == "GET" && !path.contains("/secrets/") })
        );
        server.abort();
    }
}

fn enroll_github(state: &mut TestApi, namespace: &str) -> (String, String) {
    let grant = enroll(state, namespace);
    state.objects.get_mut(&grant).unwrap()["spec"]["integrationStores"] = json!([
        {"secret":{"name":"kars-github-app","uid":"secret"},"purpose":"github-app"}
    ]);
    let old = format!("/api/v1/namespaces/{namespace}/secrets/kars-inference-providers");
    let path = format!("/api/v1/namespaces/{namespace}/secrets/kars-github-app");
    let mut secret = state.objects.remove(&old).unwrap();
    secret["metadata"]["name"] = "kars-github-app".into();
    secret["data"] = json!({});
    state.objects.insert(path.clone(), secret);
    (grant, path)
}

#[tokio::test]
async fn github_setup_distinguishes_enrolled_empty_storage_without_exposing_credentials() {
    for scenario in ["empty", "partial", "blank", "configured"] {
        let (cluster, state, server) = fixture().await;
        let namespace = cluster.core_namespace();
        {
            let mut state = state.lock().unwrap();
            let (_, path) = enroll_github(&mut state, &namespace);
            state.objects.get_mut(&path).unwrap()["data"] = match scenario {
                "empty" => json!({}),
                "partial" => json!({"GITHUB_APP_ID":"MTIz"}),
                "blank" => json!({"GITHUB_APP_ID":"MTIz", "GITHUB_APP_PRIVATE_KEY":"ICA="}),
                // Not a signing key: discovery must not make a GitHub request.
                _ => json!({"GITHUB_APP_ID":"MTIz", "GITHUB_APP_PRIVATE_KEY":"dGVzdC1rZXk="}),
            };
        }
        let app = crate::state::AppState::for_test_client(cluster.client.clone(), &namespace);
        let github = crate::routes::github::get_app(State(app)).await.unwrap().0;
        assert_eq!(serde_json::to_value(github).unwrap(), json!({
            "configured": scenario == "configured", "storage_ready": true,
            "slug": null, "install_url": null,
        }));
        let calls = &state.lock().unwrap().calls;
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|(method, _, _)| method == "GET"));
        server.abort();
    }
}

#[tokio::test]
async fn github_setup_requires_enrollment_before_signing_or_verifying_a_submitted_key() {
    for scenario in ["no-grant", "no-store", "ready"] {
        let (cluster, state, server) = fixture().await;
        let namespace = cluster.core_namespace();
        {
            let mut state = state.lock().unwrap();
            if scenario == "no-store" {
                let grant = enroll(&mut state, &namespace);
                state.objects.get_mut(&grant).unwrap()["spec"]["integrationStores"] = json!([]);
            } else if scenario == "ready" {
                enroll_github(&mut state, &namespace);
            }
        }
        let app = crate::state::AppState::for_test_client(cluster.client.clone(), &namespace);
        let error = crate::routes::github::put_app(State(app.clone()), axum::Json(
            crate::routes::github::GithubAppRequest {
                app_id: "123".into(),
                private_key: "-----BEGIN PRIVATE KEY----- invalid-test-fixture".into(),
            },
        )).await.unwrap_err();
        if scenario == "ready" {
            assert!(matches!(error, crate::error::AppError::Rejected(_)));
        } else {
            assert!(matches!(error, crate::error::AppError::CredentialAuthorityUnavailable));
            let response = error.into_response();
            assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
            let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error"]["code"], "credential_authority_unavailable");
            assert!(body["error"]["message"].as_str().unwrap().contains("operator must enroll or repair"));
            assert!(!body.to_string().contains("invalid-test-fixture"));
            if scenario == "no-store" {
                assert!(matches!(crate::routes::github::delete_app(State(app)).await,
                    Err(crate::error::AppError::CredentialAuthorityUnavailable)));
            }
        }
        assert!(state.lock().unwrap().calls.iter().all(|(method, path, _)| {
            method == "GET" && (scenario == "ready" || !path.contains("/secrets/"))
        }));
        server.abort();
    }
}

#[tokio::test]
async fn github_manifest_registration_checks_storage_and_existing_app_before_exchange() {
    for scenario in [
        "no-grant",
        "no-store",
        "configured",
        "no-version",
        "invalid-code",
    ] {
        let (cluster, state, server) = fixture().await;
        let namespace = cluster.core_namespace();
        {
            let mut state = state.lock().unwrap();
            if scenario == "no-store" {
                let grant = enroll(&mut state, &namespace);
                state.objects.get_mut(&grant).unwrap()["spec"]["integrationStores"] = json!([]);
            } else if scenario == "configured" || scenario == "no-version" {
                let (_, path) = enroll_github(&mut state, &namespace);
                if scenario == "configured" {
                    state.objects.get_mut(&path).unwrap()["data"] = json!({
                        "GITHUB_APP_ID":"MTIz", "GITHUB_APP_PRIVATE_KEY":"dGVzdC1rZXk="
                    });
                } else {
                    state.objects.get_mut(&path).unwrap()["metadata"]
                        .as_object_mut()
                        .unwrap()
                        .remove("resourceVersion");
                }
            }
        }
        let app = crate::state::AppState::for_test_client(cluster.client.clone(), &namespace);
        let principal = crate::auth::Principal {
            sub: "admin".into(),
            name: "Admin".into(),
            roles: vec!["admin".into()],
        };
        let error = crate::routes::github::register_app(
            State(app),
            axum::Extension(principal),
            axum::Json(crate::routes::github::GithubManifestRequest {
                code: if scenario == "invalid-code" {
                    "../SECRET_CODE"
                } else {
                    "SECRET_CODE"
                }
                .into(),
            }),
        )
        .await
        .unwrap_err();
        match scenario {
            "configured" => assert!(matches!(error, crate::error::AppError::Conflict(_))),
            "invalid-code" => assert!(matches!(error, crate::error::AppError::BadRequest(_))),
            _ => assert!(matches!(
                error,
                crate::error::AppError::CredentialAuthorityUnavailable
            )),
        }
        assert!(!format!("{error:?}").contains("SECRET_CODE"));
        let state = state.lock().unwrap();
        assert!(state.calls.iter().all(|(method, _, _)| method == "GET"));
        if scenario == "invalid-code" {
            assert!(state.calls.is_empty());
        }
        server.abort();
    }
}

#[tokio::test]
async fn github_manifest_store_fences_concurrent_updates_and_keeps_upload_replacement() {
    for scenario in [
        "success",
        "another-registration",
        "before-read",
        "before-patch",
        "patch-race",
        "revoked",
    ] {
        let (cluster, state, server) = fixture().await;
        let namespace = cluster.core_namespace();
        let (grant, path) = enroll_github(&mut state.lock().unwrap(), &namespace);
        let setup = cluster.github_app_setup().await.unwrap();
        let (uid, version) = setup.store_identity.unwrap();
        let expected = Some((uid.as_str(), version.as_str()));
        if scenario == "another-registration" {
            cluster
                .save_github_app("456", "winning-test-key", expected)
                .await
                .unwrap();
        }
        {
            let mut state = state.lock().unwrap();
            match scenario {
                "before-read" | "before-patch" => {
                    state.target_read_mutation = Some((
                        path.clone(),
                        if scenario == "before-read" { 2 } else { 3 },
                        json!({"data":{"GITHUB_APP_ID":"NDU2", "GITHUB_APP_PRIVATE_KEY":"d2lubmluZy10ZXN0LWtleQ=="}}),
                    ));
                }
                "patch-race" => state.gate_patch_conflicts = 1,
                "revoked" => {
                    state.objects.get_mut(&grant).unwrap()["spec"]["enabled"] = false.into()
                }
                _ => {}
            }
        }
        let result = cluster
            .save_github_app("123", "new-test-key", expected)
            .await;
        if scenario == "success" {
            result.unwrap();
            assert_eq!(
                cluster.github_app_creds().await.unwrap(),
                Some(("123".into(), "new-test-key".into()))
            );
            // PUT retains the supported explicit replacement path.
            cluster
                .save_github_app("789", "upload-test-key", None)
                .await
                .unwrap();
            assert_eq!(
                cluster.github_app_creds().await.unwrap(),
                Some(("789".into(), "upload-test-key".into()))
            );
        } else {
            let error = result.unwrap_err();
            assert!(!error.to_string().contains("test-key"));
            let state = state.lock().unwrap();
            assert_ne!(state.objects[&path]["data"]["GITHUB_APP_ID"], "MTIz");
            let patches = state
                .calls
                .iter()
                .filter(|(method, called, _)| method == "PATCH" && called == &path)
                .count();
            assert_eq!(
                patches,
                usize::from(matches!(scenario, "patch-race" | "another-registration")),
                "{scenario}"
            );
        }
        server.abort();
    }
}

#[tokio::test]
async fn optional_github_discovery_keeps_enrolled_store_validation() {
    for scenario in [
        "valid",
        "forbidden",
        "disabled",
        "stale",
        "replaced-namespace",
        "missing-secret",
        "replaced-secret",
        "wrong-type",
        "terminating-secret",
    ] {
        let (cluster, state, server) = fixture().await;
        let namespace = cluster.core_namespace();
        {
            let mut state = state.lock().unwrap();
            let (grant, path) = enroll_github(&mut state, &namespace);
            state.objects.get_mut(&path).unwrap()["data"] =
                json!({"GITHUB_APP_ID":"MTIz", "GITHUB_APP_PRIVATE_KEY":"dGVzdC1rZXk="});
            match scenario {
                "forbidden" => state.forbidden = true,
                "disabled" => {
                    state.objects.get_mut(&grant).unwrap()["spec"]["enabled"] = false.into()
                }
                "missing-secret" => {
                    state.objects.remove(&path);
                }
                "replaced-secret" => {
                    state.objects.get_mut(&path).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                "wrong-type" => {
                    state.objects.get_mut(&path).unwrap()["type"] = "kubernetes.io/tls".into()
                }
                "stale" => {
                    state.objects.get_mut(&grant).unwrap()["status"]["observedGeneration"] = 0.into()
                }
                "replaced-namespace" => {
                    state.objects.get_mut(&format!("/api/v1/namespaces/{namespace}")).unwrap()
                        ["metadata"]["uid"] = "replacement".into()
                }
                "terminating-secret" => {
                    state.objects.get_mut(&path).unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-01-01T00:00:00Z".into()
                }
                _ => {}
            }
        }
        let result = cluster.github_app_creds().await;
        if scenario == "valid" {
            assert_eq!(result.unwrap(), Some(("123".into(), "test-key".into())));
        } else {
            let error = result.expect_err(scenario).to_string();
            assert!(!error.contains("PRIVATE_VALUE_SENTINEL"));
            assert!(!error.contains("test-key"));
            let app = crate::state::AppState::for_test_client(cluster.client.clone(), &namespace);
            let discovery = crate::routes::github::get_app(State(app.clone())).await.unwrap_err();
            let submission = crate::routes::github::put_app(State(app), axum::Json(
                crate::routes::github::GithubAppRequest {
                    app_id: "123".into(),
                    private_key: "-----BEGIN PRIVATE KEY----- invalid-test-fixture".into(),
                },
            )).await.unwrap_err();
            for error in [discovery, submission] {
                if matches!(scenario, "forbidden" | "missing-secret") {
                    assert!(matches!(error, crate::error::AppError::Upstream(_)));
                } else {
                    assert!(matches!(error, crate::error::AppError::CredentialAuthorityUnavailable));
                }
                assert!(!error.to_string().contains("invalid-test-fixture"));
            }
        }
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method == "GET")
        );
        server.abort();
    }
}

#[tokio::test]
async fn optional_provider_discovery_does_not_read_unenrolled_secrets() {
    let (cluster, state, server) = fixture().await;
    let namespace = cluster.core_namespace();
    assert!(cluster.additional_provider_keys().await.unwrap().is_empty());
    assert_eq!(state.lock().unwrap().calls.len(), 1);
    let path = enroll(&mut state.lock().unwrap(), &namespace);
    state.lock().unwrap().objects.get_mut(&path).unwrap()["spec"]["integrationStores"] = json!([]);
    assert!(cluster.additional_provider_keys().await.unwrap().is_empty());
    assert!(
        cluster
            .read_secret_all(&namespace, "kars-inference-providers")
            .await
            .is_err()
    );
    assert!(
        cluster
            .mutate_secret_keys(&namespace, "kars-inference-providers", |keys| {
                keys.insert("new".into(), "value".into());
            })
            .await
            .is_err()
    );
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, path, _)| method == "GET" && !path.contains("/secrets/"))
    );
    server.abort();
}

#[tokio::test]
async fn optional_provider_discovery_reads_only_valid_enrolled_store() {
    let (cluster, state, server) = fixture().await;
    enroll(&mut state.lock().unwrap(), &cluster.core_namespace());
    let keys = cluster.additional_provider_keys().await.unwrap();
    assert_eq!(keys.get("client-id").map(String::as_str), Some("old"));
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    server.abort();
}

#[tokio::test]
async fn optional_provider_discovery_preserves_authority_and_transport_errors() {
    for scenario in [
        "forbidden",
        "disabled",
        "stale",
        "malformed",
        "terminating-grant",
        "missing-namespace",
        "replaced-namespace",
        "terminating-namespace",
        "missing-secret",
        "replaced-secret",
        "wrong-type",
        "terminating-secret",
    ] {
        let (cluster, state, server) = fixture().await;
        let namespace = cluster.core_namespace();
        {
            let mut state = state.lock().unwrap();
            let grant = enroll(&mut state, &namespace);
            let ns = format!("/api/v1/namespaces/{namespace}");
            let secret = format!("{ns}/secrets/kars-inference-providers");
            match scenario {
                "forbidden" => state.forbidden = true,
                "disabled" => {
                    state.objects.get_mut(&grant).unwrap()["spec"]["enabled"] = false.into()
                }
                "stale" => {
                    state.objects.get_mut(&grant).unwrap()["status"]["observedGeneration"] =
                        0.into()
                }
                "malformed" => {
                    state.objects.get_mut(&grant).unwrap()["spec"]["integrationStores"] = json!({})
                }
                "terminating-grant" => {
                    state.objects.get_mut(&grant).unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-01-01T00:00:00Z".into()
                }
                "missing-namespace" => {
                    state.objects.remove(&ns);
                }
                "replaced-namespace" => {
                    state.objects.get_mut(&ns).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                "terminating-namespace" => {
                    state.objects.get_mut(&ns).unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-01-01T00:00:00Z".into()
                }
                "missing-secret" => {
                    state.objects.remove(&secret);
                }
                "replaced-secret" => {
                    state.objects.get_mut(&secret).unwrap()["metadata"]["uid"] =
                        "replacement".into()
                }
                "wrong-type" => {
                    state.objects.get_mut(&secret).unwrap()["type"] = "kubernetes.io/tls".into()
                }
                "terminating-secret" => {
                    state.objects.get_mut(&secret).unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-01-01T00:00:00Z".into()
                }
                _ => unreachable!(),
            }
        }
        let error = cluster
            .additional_provider_keys()
            .await
            .expect_err(scenario);
        assert!(!error.to_string().contains("PRIVATE_VALUE_SENTINEL"));
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method == "GET")
        );
        server.abort();
    }
}
