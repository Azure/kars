// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fixture() -> (Configuration, Value) {
    let identity = Identity {
        namespace: "workspace".into(),
        namespace_uid: "workspace-uid".into(),
        owner_kind: "KarsTeam".into(),
        owner_name: "team".into(),
        owner_uid: "team-uid".into(),
        principal: "role:researcher".into(),
    };
    let map = json!({"apiVersion":"v1","kind":"ConfigMap","metadata": {
        "name":identity.map_name(),"namespace":identity.namespace,"uid":"map-uid","resourceVersion":"1",
        "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":identity.owner_kind,
            "name":identity.owner_name,"uid":identity.owner_uid,"controller":true,"blockOwnerDeletion":false}]},
        "data":{"memory.json":serde_json::to_string(&Store::empty(identity.clone())).unwrap()}});
    (
        Configuration {
            identity,
            map_uid: "map-uid".into(),
            api_url: String::new(),
            ca: String::new(),
            token: "test-private-token".into(),
            expires_at: i64::MAX,
            sandbox_uid: "sandbox-uid".into(),
        },
        map,
    )
}

#[test]
fn builtin_memory_rejects_replaced_foreign_or_corrupted_objects() {
    let (config, map) = fixture();
    assert!(check_map(&map, &config).is_ok());
    for pointer in [
        "/metadata/uid",
        "/metadata/namespace",
        "/metadata/name",
        "/metadata/ownerReferences/0/uid",
        "/metadata/deletionTimestamp",
        "/data/memory.json",
    ] {
        let mut changed = map.clone();
        if pointer == "/metadata/deletionTimestamp" {
            changed["metadata"]["deletionTimestamp"] = json!("now");
        } else {
            *changed.pointer_mut(pointer).unwrap() = json!("different");
        }
        assert!(check_map(&changed, &config).is_err(), "{pointer}");
    }
}

#[tokio::test]
async fn builtin_memory_scope_denial_makes_no_api_call() {
    let server = MockServer::start().await;
    let (config, _) = fixture();
    for scope in [json!("shared"), json!("private_other"), json!(3)] {
        let result = execute(
            &json!({"operation":"search","scope":scope}),
            &config,
            &reqwest::Client::new(),
            server.uri().parse().unwrap(),
        )
        .await;
        assert!(result.is_err());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn builtin_memory_conflict_retry_preserves_other_writes_and_survives_client_restart() {
    let server = MockServer::start().await;
    let (config, map) = fixture();
    let state = Arc::new(Mutex::new(map));
    let conflict = Arc::new(AtomicBool::new(true));
    let captured = state.clone();
    let inject = conflict.clone();
    Mock::given(wiremock::matchers::any())
        .respond_with(move |request: &Request| {
            assert_eq!(
                request.headers["authorization"],
                "Bearer test-private-token"
            );
            let mut persisted = captured.lock().unwrap();
            if request.method.as_str() == "GET" {
                return ResponseTemplate::new(200).set_body_json(persisted.clone());
            }
            assert_eq!(request.method.as_str(), "PUT");
            if inject.swap(false, Ordering::SeqCst) {
                let mut store: Store =
                    serde_json::from_str(persisted["data"]["memory.json"].as_str().unwrap())
                        .unwrap();
                store
                    .update(
                        "Helsinki is the existing preference",
                        "another-sandbox",
                        "before",
                    )
                    .unwrap();
                persisted["data"]["memory.json"] = serde_json::to_string(&store).unwrap().into();
                persisted["metadata"]["resourceVersion"] = json!("2");
                return ResponseTemplate::new(409);
            }
            let mut updated: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(
                updated["metadata"]["resourceVersion"],
                persisted["metadata"]["resourceVersion"]
            );
            updated["metadata"]["resourceVersion"] = json!("3");
            *persisted = updated;
            ResponseTemplate::new(200).set_body_json(persisted.clone())
        })
        .mount(&server)
        .await;
    let result = execute(
        &json!({"operation":"update","text":"Helsinki has a new preference"}),
        &config,
        &reqwest::Client::new(),
        server.uri().parse().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(result["backend"], "builtin");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["approved_knowledge"], false);
    let reloaded = execute(
        &json!({"operation":"search","text":"Helsinki"}),
        &config,
        &reqwest::Client::new(),
        server.uri().parse().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(reloaded["retrieval"], "lexical");
    assert_eq!(reloaded["memories"].as_array().unwrap().len(), 2);
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn builtin_memory_denied_unavailable_or_unacknowledged_operations_never_succeed() {
    for status in [403, 503] {
        let server = MockServer::start().await;
        let (config, _) = fixture();
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        assert!(
            execute(
                &json!({"operation":"update","text":"private fact"}),
                &config,
                &reqwest::Client::new(),
                server.uri().parse().unwrap()
            )
            .await
            .is_err()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
    let server = MockServer::start().await;
    let (config, map) = fixture();
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(map))
        .mount(&server)
        .await;
    assert!(
        execute(
            &json!({"operation":"update","text":"private fact"}),
            &config,
            &reqwest::Client::new(),
            server.uri().parse().unwrap()
        )
        .await
        .is_err()
    );
}
