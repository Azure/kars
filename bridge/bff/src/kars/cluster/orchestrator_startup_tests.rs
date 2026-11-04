// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Cluster, verify_orchestrator_source};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn source() -> Value {
    json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
        "metadata":{"name":"bridge-orchestrator", "namespace":"kars-system", "uid":"source-uid", "resourceVersion":"9",
            "labels":{"operator":"retained"}, "annotations":{"operator":"retained"}},
        "spec":{"suspended":true, "runtime":{"kind":"OpenClaw", "openclaw":{"image":"operator-selected"}},
            "agent":{"instructions":"operator-selected instructions"},
            "inferenceRef":{"name":"bridge-orchestrator-inference"},
            "inferenceBudgetRef":{"scope":"GovernedInference", "account":{"namespace":"kars-system", "name":"budget-account", "uid":"account-uid"},
                "root":{"kind":"KarsSandbox", "resource":{"namespace":"kars-system", "name":"bridge-orchestrator", "uid":"source-uid"}, "workspaceUid":"workspace-uid", "clusterUid":"cluster-uid"},
                "taskUid":"source-uid", "rootTaskUid":"source-uid", "parentTaskUid":null, "authorizationDigest":"sha256:retained"}}
    })
}

#[test]
fn orchestrator_startup_rejects_wrong_source_authority_without_repairing_it() {
    let original = source();
    for (pointer, replacement) in [
        ("/metadata/name", json!("other")),
        ("/metadata/namespace", json!("other")),
        ("/metadata/uid", json!("")),
        ("/metadata/resourceVersion", json!("")),
        ("/metadata/deletionTimestamp", json!("2026-01-01T00:00:00Z")),
        (
            "/metadata/ownerReferences",
            json!([{"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask", "name":"task", "uid":"task-uid"}]),
        ),
        ("/spec/inferenceRef/name", json!("other")),
        ("/spec/inferenceBudgetRef/root/kind", json!("KarsTask")),
        (
            "/spec/inferenceBudgetRef/root/resource/uid",
            json!("other-uid"),
        ),
        (
            "/spec/inferenceBudgetRef/root/resource/name",
            json!("other"),
        ),
        (
            "/spec/inferenceBudgetRef/root/resource/namespace",
            json!("other"),
        ),
        ("/spec/inferenceBudgetRef/taskUid", json!("other-uid")),
        ("/spec/inferenceBudgetRef/rootTaskUid", json!("other-uid")),
        (
            "/spec/inferenceBudgetRef/parentTaskUid",
            json!("parent-uid"),
        ),
        ("/spec/inferenceBudgetRef/account", json!({})),
    ] {
        let mut changed = original.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        changed.pointer_mut(parent).unwrap()[key] = replacement;
        let source = serde_json::from_value(changed).unwrap();
        assert!(verify_orchestrator_source(&source).is_err(), "{pointer}");
    }
}

struct Fixture {
    source: Value,
    initial_missing: bool,
    policy_missing: bool,
    reads: Mutex<usize>,
    requests: Mutex<Vec<(Method, String)>>,
}

async fn handle(
    State(state): State<Arc<Fixture>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    state
        .requests
        .lock()
        .unwrap()
        .push((method.clone(), uri.path().into()));
    let missing = || {
        (StatusCode::NOT_FOUND, Json(json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"NotFound", "message":"fixture", "code":404}))).into_response()
    };
    if uri.path().contains("/karssandboxes") {
        if method == Method::GET {
            let mut reads = state.reads.lock().unwrap();
            *reads += 1;
            if state.initial_missing && *reads == 1 {
                return missing();
            }
            return Json(state.source.clone()).into_response();
        }
        assert_eq!(
            method,
            Method::POST,
            "startup must never patch an existing source"
        );
        let proposed: Value = serde_json::from_slice(&body).unwrap();
        assert!(proposed["spec"].get("inferenceBudgetRef").is_none());
        assert!(proposed["spec"].get("suspended").is_none());
        return (StatusCode::CONFLICT, Json(json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"AlreadyExists", "message":"fixture", "code":409}))).into_response();
    }
    assert!(
        uri.path()
            .ends_with("/inferencepolicies/bridge-orchestrator-inference")
    );
    assert_eq!(method, Method::GET);
    if state.policy_missing {
        return missing();
    }
    Json(json!({"apiVersion":"kars.azure.com/v1alpha1", "kind":"InferencePolicy", "metadata":{"name":"bridge-orchestrator-inference", "namespace":"kars-system"}, "spec":{"modelPreference":{"primary":{"provider":"retained", "deployment":"selected"}}}})).into_response()
}

#[tokio::test]
async fn orchestrator_startup_reuses_bound_unbound_and_concurrent_sources_without_writes() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for (bound, raced, bad_identity, policy_missing) in [
        (true, false, false, false),
        (false, false, false, false),
        (true, true, false, false),
        (false, true, false, false),
        (true, true, true, false),
        (true, false, true, false),
        (true, false, false, true),
        (false, false, false, true),
    ] {
        let mut source = source();
        if !bound {
            source["spec"]
                .as_object_mut()
                .unwrap()
                .remove("inferenceBudgetRef");
        }
        if bad_identity {
            source["metadata"]["ownerReferences"] = json!([{"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask", "name":"task", "uid":"task-uid"}]);
        }
        let state = Arc::new(Fixture {
            source,
            initial_missing: raced,
            policy_missing,
            reads: Mutex::new(0),
            requests: Mutex::new(Vec::new()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = kube::Config::new(
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        );
        let app = Router::new().fallback(handle).with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cluster = Cluster {
            client: kube::Client::try_from(config).unwrap(),
        };
        for _ in 0..2 {
            assert_eq!(
                cluster.ensure_orchestrator_sandbox().await.is_ok(),
                !bad_identity && !policy_missing
            );
        }
        let requests = state.requests.lock().unwrap();
        assert!(
            !requests
                .iter()
                .any(|(method, _)| *method == Method::PATCH || *method == Method::PUT)
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(method, _)| *method == Method::POST)
                .count(),
            usize::from(raced)
        );
        server.abort();
    }
}
