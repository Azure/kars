// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tower::{ServiceExt, service_fn};

fn fixture(
    name: &str,
    bytes: &[u8],
    binary: bool,
    retained: bool,
) -> (AppState, Arc<Mutex<Vec<String>>>) {
    fixture_with_tasks(
        name,
        bytes,
        binary,
        vec![if retained {
            None
        } else {
            Some(artifact_task())
        }],
    )
}

fn artifact_task() -> Value {
    json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
        "metadata":{"name":"task","namespace":"kars-system","uid":"task-uid",
            "annotations":{"kars.azure.com/owner-sub":"owner",
                "kars.azure.com/run-requested":"run-1","kars.azure.com/run-completed":"run-1"}},
        "spec":{"objective":"test","envelope":{"tier":1,"authorityCeiling":1}}
    })
}

fn fixture_with_tasks(
    name: &str,
    bytes: &[u8],
    binary: bool,
    tasks: Vec<Option<Value>>,
) -> (AppState, Arc<Mutex<Vec<String>>>) {
    assert!(!tasks.is_empty());
    let tasks = Arc::new(tasks);
    let task_reads = Arc::new(AtomicUsize::new(0));
    let key = artifact_key(name);
    let metadata = json!({"name":"kars-mission-artifacts-task","namespace":"kars-system",
        "annotations":{"kars.azure.com/mission-task-uid":"task-uid",
            "kars.azure.com/mission-run-nonce":"run-1",
            "kars.azure.com/mission-principal-name":"task"},
        "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
            "name":"task","uid":"task-uid","controller":true}]});
    let artifact = if binary {
        json!({"apiVersion":"v1","kind":"ConfigMap","metadata":metadata,
            "binaryData":{key:k8s_openapi::ByteString(bytes.to_vec())}})
    } else {
        json!({"apiVersion":"v1","kind":"ConfigMap","metadata":metadata,
            "data":{key:std::str::from_utf8(bytes).unwrap()}})
    };
    let calls = Arc::new(Mutex::new(Vec::new()));
    let requests = calls.clone();
    let service = service_fn(move |request: Request<_>| {
        let artifact = artifact.clone();
        let requests = requests.clone();
        let tasks = tasks.clone();
        let task_reads = task_reads.clone();
        async move {
            assert_eq!(request.method(), Method::GET);
            let path = request.uri().path();
            requests.lock().unwrap().push(path.to_string());
            let (status, body) = match path {
                "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karstasks/task" => {
                    let index = task_reads
                        .fetch_add(1, Ordering::SeqCst)
                        .min(tasks.len() - 1);
                    match &tasks[index] {
                        Some(task) => (200, task.clone()),
                        None => (
                            404,
                            json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404,"message":"not found"}),
                        ),
                    }
                }
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
        AppState::for_test_client(kube::Client::new(service, "kars-system"), "kars-system"),
        calls,
    )
}

async fn fetch(state: AppState, file: &str, owner: &str, method: Method) -> Response {
    fetch_uri(
        state,
        &format!("/api/namespaces/kars-system/tasks/task/artifact/{file}?run_nonce=run-1"),
        owner,
        method,
    )
    .await
}

async fn fetch_uri(state: AppState, uri: &str, owner: &str, method: Method) -> Response {
    crate::routes::router(state)
        .layer(Extension(Principal {
            sub: owner.into(),
            name: owner.into(),
            roles: vec!["user".into()],
        }))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn active_and_unknown_artifacts_require_a_current_task_and_download_unchanged() {
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
                if retained {
                    assert_eq!(response.status(), StatusCode::NOT_FOUND);
                    continue;
                }
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

#[tokio::test]
async fn artifact_download_requires_explicit_completed_revision_before_reading_bytes() {
    for (ns, query, expected) in [
        ("kars-system", "", StatusCode::BAD_REQUEST),
        ("kars-system", "?run_nonce=", StatusCode::CONFLICT),
        ("kars-system", "?run_nonce=old-run", StatusCode::CONFLICT),
        ("another", "?run_nonce=run-1", StatusCode::NOT_FOUND),
    ] {
        let (state, calls) = fixture("notes.md", b"private artifact bytes", false, false);
        let response = fetch_uri(
            state,
            &format!("/api/namespaces/{ns}/tasks/task/artifact/notes.md{query}"),
            "owner",
            Method::GET,
        )
        .await;
        assert_eq!(response.status(), expected, "{ns}/{query}");
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|path| path.contains("configmaps"))
        );
    }
    let mut pending = artifact_task();
    pending["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!("prior-run");
    let (state, calls) = fixture_with_tasks(
        "notes.md",
        b"private artifact bytes",
        false,
        vec![Some(pending)],
    );
    assert_eq!(
        fetch(state, "notes.md", "owner", Method::GET)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("configmaps"))
    );
}

#[tokio::test]
async fn artifact_final_fence_withholds_bytes_after_identity_revision_or_owner_changes() {
    for (pointer, value, expected) in [
        ("/metadata/uid", json!("recreated"), StatusCode::CONFLICT),
        (
            "/metadata/annotations/kars.azure.com~1run-requested",
            json!("run-2"),
            StatusCode::CONFLICT,
        ),
        (
            "/metadata/annotations/kars.azure.com~1run-completed",
            json!("run-2"),
            StatusCode::CONFLICT,
        ),
        (
            "/metadata/annotations/kars.azure.com~1owner-sub",
            json!("new-owner"),
            StatusCode::NOT_FOUND,
        ),
        (
            "/metadata/namespace",
            json!("another"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let original = artifact_task();
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let (state, calls) = fixture_with_tasks(
            "notes.md",
            b"private artifact bytes",
            false,
            vec![Some(original.clone()), Some(original), Some(changed)],
        );
        let response = fetch(state, "notes.md", "owner", Method::GET).await;
        assert_eq!(response.status(), expected, "{pointer}");
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("private artifact bytes"));
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|path| path.ends_with("kars-mission-artifacts-task"))
        );
    }
    let mut deleting = artifact_task();
    deleting["metadata"]["deletionTimestamp"] = json!("2026-10-03T08:00:00Z");
    for changed in [Some(deleting), None] {
        let (state, _) = fixture_with_tasks(
            "notes.md",
            b"private artifact bytes",
            false,
            vec![Some(artifact_task()), Some(artifact_task()), changed],
        );
        assert_eq!(
            fetch(state, "notes.md", "owner", Method::GET)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn artifact_final_fence_allows_harmless_status_and_resource_version_updates() {
    let mut fresh = artifact_task();
    fresh["metadata"]["resourceVersion"] = json!("999");
    fresh["status"] = json!({"phase":"Active"});
    let (state, _) = fixture_with_tasks(
        "notes.md",
        b"exact bytes",
        false,
        vec![Some(artifact_task()), Some(artifact_task()), Some(fresh)],
    );
    let response = fetch(state, "notes.md", "owner", Method::GET).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 65536)
            .await
            .unwrap()
            .as_ref(),
        b"exact bytes"
    );
}
