// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    response::Response,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::{ServiceExt, service_fn};

fn fixture(
    name: &str,
    bytes: &[u8],
    binary: bool,
    retained: bool,
) -> (AppState, Arc<Mutex<Vec<String>>>) {
    let key = artifact_key(name);
    let artifact = if binary {
        json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"kars-mission-artifacts-task"},
            "binaryData":{key:k8s_openapi::ByteString(bytes.to_vec())}})
    } else {
        json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"kars-mission-artifacts-task"},
            "data":{key:std::str::from_utf8(bytes).unwrap()}})
    };
    let calls = Arc::new(Mutex::new(Vec::new()));
    let requests = calls.clone();
    let service = service_fn(move |request: Request<_>| {
        let artifact = artifact.clone();
        let requests = requests.clone();
        async move {
            assert_eq!(request.method(), Method::GET);
            let path = request.uri().path();
            requests.lock().unwrap().push(path.to_string());
            let (status, body) = match path {
                "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/task" if retained => (
                    404,
                    json!({
                        "apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404,"message":"not found"
                    }),
                ),
                "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/task" => (
                    200,
                    json!({
                        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
                        "metadata":{"name":"task","namespace":"work","annotations":{"kars.azure.com/owner-sub":"owner"}},
                        "spec":{"objective":"test","envelope":{"tier":1,"authorityCeiling":1}}
                    }),
                ),
                "/api/v1/namespaces/kars-system/configmaps/kars-mission-output-task" => (
                    200,
                    json!({
                        "apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"kars-mission-output-task"},
                        "data":{"ownerSub":"owner"}
                    }),
                ),
                "/api/v1/namespaces/kars-system/configmaps/kars-mission-artifacts-task" => {
                    (200, artifact)
                }
                _ => panic!("unexpected artifact API call: {path}"),
            };
            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
        }
    });
    (
        AppState::for_test_client(kube::Client::new(service, "work"), "work"),
        calls,
    )
}

async fn fetch(state: AppState, file: &str, owner: &str, method: Method) -> Response {
    crate::routes::router(state)
        .layer(Extension(Principal {
            sub: owner.into(),
            name: owner.into(),
            roles: vec!["user".into()],
        }))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/namespaces/work/tasks/task/artifact/{file}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn active_and_unknown_artifacts_download_unchanged_from_live_or_retained_tasks() {
    let payload = b"<script>fetch('/api/operator/retention-policy')</script>";
    for (name, mime) in [
        ("report.html", "text/html; charset=utf-8"),
        ("report.HTM", "text/html; charset=utf-8"),
        ("diagram.svg", "image/svg+xml"),
        ("diagram.SVG", "image/svg+xml"),
        ("document.pdf", "application/pdf"),
        ("document.xhtml", "application/octet-stream"),
        ("data.xml", "application/octet-stream"),
        ("script.js", "application/octet-stream"),
        ("opaque", "application/octet-stream"),
    ] {
        for binary in [false, true] {
            for retained in [false, true] {
                let (state, _) = fixture(name, payload, binary, retained);
                let response = fetch(state, name, "owner", Method::GET).await;
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.headers()["content-type"], mime);
                assert_eq!(
                    response.headers()["content-disposition"],
                    format!("attachment; filename=\"{name}\"")
                );
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                assert_eq!(
                    response.headers()["content-security-policy"],
                    "sandbox; default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
                );
                assert_eq!(response.headers()["cache-control"], "private, no-store");
                assert_eq!(
                    to_bytes(response.into_body(), 65536)
                        .await
                        .unwrap()
                        .as_ref(),
                    payload
                );
            }
        }
    }
}

#[tokio::test]
async fn passive_artifact_previews_keep_inline_viewing_without_mime_sniffing_or_byte_changes() {
    for name in [
        "notes.md",
        "notes.txt",
        "events.log",
        "data.json",
        "data.csv",
        "config.yaml",
        "image.png",
        "image.jpg",
    ] {
        for binary in [false, true] {
            let bytes: &[u8] = if binary {
                b"\x89PNG\r\n\x1a\n\x00\xff"
            } else {
                b"<html><script>never execute</script></html>"
            };
            let (state, _) = fixture(name, bytes, binary, false);
            let response = fetch(state, name, "owner", Method::GET).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["content-disposition"],
                format!("inline; filename=\"{name}\"")
            );
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .starts_with("sandbox;")
            );
            assert_eq!(
                to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap()
                    .as_ref(),
                bytes
            );
        }
    }
}

#[tokio::test]
async fn artifact_head_has_the_same_protection_and_filename_is_header_safe() {
    let name = "report\"\r\n.svg";
    let (state, _) = fixture(name, b"<svg/>", false, false);
    let response = fetch(state, "report%22%0D%0A.svg", "owner", Method::HEAD).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"report___.svg\""
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(response.headers().contains_key("content-security-policy"));
    assert!(
        to_bytes(response.into_body(), 65536)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn artifact_response_hardening_preserves_ownership_and_missing_file_denials() {
    for retained in [false, true] {
        let (state, calls) = fixture("report.html", b"<html/>", false, retained);
        let response = fetch(state.clone(), "report.html", "other-user", Method::GET).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|path| path.ends_with("kars-mission-artifacts-task"))
        );
        let response = fetch(state, "missing.html", "owner", Method::GET).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
