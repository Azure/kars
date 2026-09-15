// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::{ServiceExt, service_fn};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-15T15:00:30Z")
        .unwrap()
        .into()
}

fn cm(name: &str, key: &str, value: Value) -> ConfigMap {
    serde_json::from_value(json!({
        "apiVersion":"v1","kind":"ConfigMap",
        "metadata": {
            "name":name,"namespace":"kars-system","uid":"report-uid","resourceVersion":"1",
            "labels":{"app.kubernetes.io/managed-by":"Helm","kars.azure.com/witness-addon":"true"},
            "annotations":{"meta.helm.sh/release-name":"kars-datapath-witness","meta.helm.sh/release-namespace":"kars-system"}
        },
        "data": {key: value.to_string()}
    })).unwrap()
}

fn settings(enabled: bool) -> ConfigMap {
    cm(
        "kars-datapath-witness-settings",
        "settings.json",
        json!({
            "schema_version":1,"enabled":enabled,"release_revision":2,
            "config_digest":"a".repeat(64),"sandboxes":["demo"]
        }),
    )
}

fn report() -> Value {
    json!({
        "schema_version":1,"status":"observed","generated_at":"2026-09-15T15:00:30Z",
        "started_at":"2026-09-15T15:00:15Z","window_seconds":15,
        "release_revision":2,"config_digest":"a".repeat(64),"publisher_uid":"report-uid",
        "coverage":"partial","event_count":1,"nodes_targeted":["node-1"],"nodes_with_events":["node-1"],
        "sandboxes":[{
            "namespace":"kars-demo","sandbox":"demo","egress_mode":"Strict",
            "declared_hosts":[],"observed_dns":["example.com"],"observed_connects":0,
            "beyond_declared":["example.com"],"unused_declared":[],"verdict":"BEYOND-DECLARED"
        }]
    })
}

fn classify_report(value: Value) -> DatapathWitnessDto {
    classify(
        Ok(Some(settings(true))),
        Ok(Some(cm("kars-datapath-witness", "witness.json", value))),
        now(),
    )
}

#[test]
fn witness_missing_is_unknown_not_not_installed_and_intent_is_separate() {
    let dto = classify(Ok(None), Ok(None), now());
    assert_eq!(dto.state, "missing");
    assert_eq!(dto.requested_enabled, None);
    assert_eq!(dto.installation_state, "unknown");
    assert!(!dto.enabled);
    assert!(!dto.install_hint.contains("install.sh"));
    let dto = classify(Ok(Some(settings(true))), Ok(None), now());
    assert_eq!(dto.state, "pending");
    assert_eq!(dto.requested_enabled, Some(true));
    let dto = classify(Ok(Some(settings(false))), Ok(None), now());
    assert_eq!(dto.state, "disabled");
    assert_eq!(dto.installation_state, "unknown");
}

#[test]
fn witness_fresh_and_empty_samples_never_claim_complete_coverage() {
    let dto = classify_report(report());
    assert_eq!(dto.state, "observed");
    assert!(dto.enabled);
    assert_eq!(dto.coverage, "partial");
    assert_eq!(dto.age_seconds, Some(0));
    assert_eq!(dto.sandboxes[0].verdict, "BEYOND-DECLARED");
    let mut empty = report();
    empty["status"] = json!("empty");
    empty["event_count"] = json!(0);
    empty["nodes_with_events"] = json!([]);
    empty["sandboxes"][0]["observed_dns"] = json!([]);
    empty["sandboxes"][0]["beyond_declared"] = json!([]);
    empty["sandboxes"][0]["verdict"] = json!("NO-TRAFFIC");
    let dto = classify_report(empty);
    assert_eq!(dto.state, "empty");
    assert!(!dto.enabled);
    assert_eq!(dto.coverage, "partial");
}

#[test]
fn witness_malformed_stale_future_and_identity_fail_closed() {
    for (field, value) in [
        ("generated_at", json!("invalid")),
        ("generated_at", json!("2026-09-15T15:01:01Z")),
        ("schema_version", json!(2)),
        ("window_seconds", json!(0)),
        ("publisher_uid", json!("replaced-object")),
        ("release_revision", json!(1)),
        ("config_digest", json!("b".repeat(64))),
        ("coverage", json!("complete")),
        ("nodes_with_events", json!(["different-node"])),
        ("event_count", json!(0)),
        ("started_at", json!("2026-09-15T15:00:29Z")),
        ("sandboxes", json!([])),
        (
            "sandboxes",
            json!([{"sandbox":"demo","verdict":"COMPLIANT"}]),
        ),
    ] {
        let mut doc = report();
        doc[field] = value;
        let dto = classify_report(doc);
        assert_eq!(dto.state, "invalid", "{field}");
        assert!(!dto.enabled);
        assert!(dto.sandboxes.is_empty());
    }
    let mut inconsistent = report();
    inconsistent["sandboxes"][0]["verdict"] = json!("LEARN");
    assert_eq!(classify_report(inconsistent).state, "invalid");
    let mut inconsistent = report();
    inconsistent["sandboxes"][0]["beyond_declared"] = json!(["never-observed.example"]);
    assert_eq!(classify_report(inconsistent).state, "invalid");
    let mut stale = report();
    stale["generated_at"] = json!("2026-09-15T14:57:29Z");
    let dto = classify_report(stale);
    assert_eq!(dto.state, "stale");
    assert_eq!(dto.age_seconds, Some(181));
    assert!(!dto.enabled);
    assert!(dto.sandboxes.is_empty());
    let boundary = classify(
        Ok(Some(settings(true))),
        Ok(Some(cm("kars-datapath-witness", "witness.json", report()))),
        now() + chrono::Duration::seconds(180),
    );
    assert_eq!(boundary.state, "observed");
}

#[test]
fn witness_empty_configmaps_legacy_and_failed_capture_are_distinct() {
    let mut object = cm("kars-datapath-witness", "witness.json", report());
    object
        .data
        .as_mut()
        .unwrap()
        .insert("witness.json".into(), "{".into());
    assert_eq!(
        classify(Ok(Some(settings(true))), Ok(Some(object.clone())), now()).state,
        "invalid"
    );
    object.data = None;
    assert_eq!(
        classify(Ok(Some(settings(true))), Ok(Some(object.clone())), now()).state,
        "pending"
    );
    object.metadata.labels = None;
    assert_eq!(classify(Ok(None), Ok(Some(object)), now()).state, "invalid");
    let mut legacy = report();
    legacy.as_object_mut().unwrap().remove("schema_version");
    let dto = classify_report(legacy);
    assert_eq!(dto.state, "legacy");
    assert!(!dto.enabled);
    let mut failed = report();
    failed["status"] = json!("unavailable");
    failed["diagnostic"] = json!("secret should never be returned");
    failed["sandboxes"] = json!([]);
    let dto = classify_report(failed);
    assert_eq!(dto.state, "unavailable");
    assert!(
        !serde_json::to_string(&dto)
            .unwrap()
            .contains("secret should")
    );
    let mut invalid = settings(true);
    invalid
        .data
        .as_mut()
        .unwrap()
        .insert("settings.json".into(), "{}".into());
    let dto = classify(Ok(Some(invalid)), Ok(None), now());
    assert_eq!(dto.state, "invalid");
    assert_eq!(dto.requested_enabled, None);
}

#[tokio::test]
async fn witness_route_only_gets_two_fixed_configmaps_and_preserves_api_errors() {
    for status in [200, 403, 500, 0] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let service = service_fn(move |request: Request<_>| {
            let seen = seen.clone();
            async move {
                assert_eq!(request.method(), Method::GET);
                let path = request.uri().path().to_owned();
                seen.lock().unwrap().push(path.clone());
                let name = path
                    .strip_prefix("/api/v1/namespaces/kars-system/configmaps/")
                    .unwrap();
                assert!(
                    ["kars-datapath-witness-settings", "kars-datapath-witness"].contains(&name)
                );
                if status == 0 {
                    return Err(std::io::Error::other(
                        "arbitrary credentials or transport details",
                    ));
                }
                let value = if status == 200 {
                    if name.ends_with("settings") {
                        serde_json::to_value(settings(true)).unwrap()
                    } else {
                        let mut doc = report();
                        doc["generated_at"] = json!(Utc::now().to_rfc3339());
                        doc["started_at"] =
                            json!((Utc::now() - chrono::Duration::seconds(16)).to_rfc3339());
                        serde_json::to_value(cm(name, "witness.json", doc)).unwrap()
                    }
                } else {
                    json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","code":status,"message":"arbitrary credentials or upstream details"})
                };
                Ok::<_, std::io::Error>(
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(value.to_string()))
                        .unwrap(),
                )
            }
        });
        let state = AppState::for_test_client(kube::Client::new(service, "work"), "work");
        let app = crate::routes::router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/operator/datapath-witness")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["state"],
            if status == 200 {
                "observed"
            } else {
                "unavailable"
            }
        );
        assert!(!String::from_utf8_lossy(&bytes).contains("arbitrary credentials"));
        assert_eq!(calls.lock().unwrap().len(), 2);
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/api/operator/datapath-witness")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 405);
        }
        assert_eq!(calls.lock().unwrap().len(), 2);
    }
}
