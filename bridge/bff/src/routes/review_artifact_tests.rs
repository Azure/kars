// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request},
    response::Response,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::service_fn;

async fn request_revision(
    manifest: Option<Value>,
    contents: Option<BTreeMap<String, String>>,
    artifact_nonce: &str,
    summary: &str,
) -> (AppResult<Json<ReviewStateDto>>, Vec<Value>) {
    request_revision_with_count(manifest, contents, artifact_nonce, summary, None).await
}

async fn request_revision_with_count(
    manifest: Option<Value>,
    contents: Option<BTreeMap<String, String>>,
    artifact_nonce: &str,
    summary: &str,
    count: Option<&str>,
) -> (AppResult<Json<ReviewStateDto>>, Vec<Value>) {
    let task = json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
        "metadata":{"name":"task","namespace":"kars-system","uid":"task-uid","resourceVersion":"17",
            "annotations":{"kars.azure.com/owner-sub":"owner",
                "kars.azure.com/run-requested":"run-1","kars.azure.com/run-completed":"run-1"}},
        "spec":{"objective":"Prepare the useful briefing","envelope":{"tier":1,"authorityCeiling":1},
            "execution":{"launch":false}}
    });
    let metadata = |kind: &str, nonce: &str| {
        json!({
            "name":format!("kars-mission-{kind}-task"),"namespace":"kars-system",
            "annotations":{"kars.azure.com/mission-task-uid":"task-uid",
                "kars.azure.com/mission-run-nonce":nonce,"kars.azure.com/mission-principal-name":"task"},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
                "name":"task","uid":"task-uid","controller":true}]
        })
    };
    let mut output = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":metadata("output","run-1"),
        "data":{"taskName":"task","taskUid":"task-uid","assignmentNonce":"run-1","output":summary}});
    if let Some(manifest) = manifest {
        output["data"]["artifacts"] = manifest.to_string().into();
    }
    if let Some(count) = count {
        output["data"]["artifactCount"] = count.into();
    }
    let artifacts = json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":metadata("artifacts", artifact_nonce),"data":contents});
    let writes = Arc::new(Mutex::new(Vec::new()));
    let observed = writes.clone();
    let service = service_fn(move |request: Request<_>| {
        let task = task.clone();
        let output = output.clone();
        let artifacts = artifacts.clone();
        let observed = observed.clone();
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_string();
            let (status, body) = if method == Method::GET {
                if path.ends_with("/karstasks/task") {
                    (200, task)
                } else if path.ends_with("/configmaps/kars-mission-output-task") {
                    (200, output)
                } else if path.ends_with("/configmaps/kars-mission-artifacts-task") {
                    (200, artifacts)
                } else if path.ends_with("/configmaps/kars-mission-review-task") {
                    (
                        404,
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure",
                        "reason":"NotFound","code":404,"message":"not found"}),
                    )
                } else {
                    panic!("unexpected GET {path}");
                }
            } else {
                let body: Value = serde_json::from_slice(
                    &to_bytes(Body::new(request.into_body()), 1024 * 1024)
                        .await
                        .unwrap(),
                )
                .unwrap();
                observed.lock().unwrap().push(body.clone());
                if method == Method::PATCH && path.ends_with("/karstasks/task") {
                    assert_eq!(body["metadata"]["uid"], "task-uid");
                    assert_eq!(body["metadata"]["resourceVersion"], "17");
                    assert!(body.get("spec").is_none());
                    (200, task)
                } else {
                    assert_eq!(method, Method::POST);
                    assert!(path.ends_with("/configmaps"));
                    (201, body)
                }
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
    let state = AppState::for_test_client(kube::Client::new(service, "kars-system"), "kars-system");
    let result = post_review(
        State(state),
        Extension(Principal {
            sub: "owner".into(),
            name: "Reviewer".into(),
            roles: vec!["user".into()],
        }),
        Path(("kars-system".into(), "task".into())),
        Json(ReviewRequest {
            decision: "request_changes".into(),
            comment: Some("Correct the closing time only".into()),
            assignment_nonce: "run-1".into(),
        }),
    )
    .await;
    let writes = writes.lock().unwrap().clone();
    (result, writes)
}

#[tokio::test]
async fn stopped_file_review_carries_complete_documents_not_just_completion_note() {
    let text = format!(
        "{}Keep the second break at 12:30. 日本語",
        "Useful detail\n".repeat(500)
    );
    let manifest = json!([
        {"name":"brief.md","size_bytes":text.len()},
        {"name":"checklist.md","size_bytes":16},
        {"name":"response.md","size_bytes":4},
        {"name":super::super::tasks::ROUTER_OBSERVATIONS_ARTIFACT,"size_bytes":7}
    ]);
    let (result, writes) = request_revision(
        Some(manifest),
        Some(BTreeMap::from([
            ("brief.md".into(), text.clone()),
            ("checklist.md".into(), "Keep both breaks".into()),
            ("response.md".into(), "Done".into()),
            (
                super::super::tasks::ROUTER_OBSERVATIONS_ARTIFACT.into(),
                "PRIVATE".into(),
            ),
            ("not-in-manifest.txt".into(), "UNREFERENCED".into()),
        ])),
        "run-1",
        "Done; see attached files.",
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(writes.len(), 2);
    let encoded = writes[0]["metadata"]["annotations"]["kars.azure.com/run-objective-b64"]
        .as_str()
        .unwrap();
    let objective = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
    assert!(objective.contains(&serde_json::to_string(&text).unwrap()));
    assert!(objective.contains("Keep both breaks"));
    assert!(objective.contains("Done; see attached files."));
    assert!(objective.contains("Correct the closing time only"));
    assert!(!objective.contains("PRIVATE") && !objective.contains("UNREFERENCED"));
    assert_eq!(writes[1]["data"]["assignmentNonce"], "run-1");
}

#[tokio::test]
async fn unreadable_stale_binary_or_size_mismatched_files_never_request_a_revision() {
    let manifest = json!([{"name":"brief.md","size_bytes":4}]);
    for (contents, nonce) in [
        (None, "run-1"),
        (Some(BTreeMap::new()), "run-1"),
        (
            Some(BTreeMap::from([("brief.md".into(), "bad size".into())])),
            "run-1",
        ),
        (
            Some(BTreeMap::from([("brief.md".into(), "text".into())])),
            "other-run",
        ),
    ] {
        let (result, writes) =
            request_revision(Some(manifest.clone()), contents, nonce, "Done").await;
        assert!(matches!(result, Err(AppError::Conflict(_))));
        assert!(writes.is_empty());
    }
}

#[tokio::test]
async fn invalid_or_oversized_revision_context_never_mutates_the_task_or_journal() {
    for (manifest, contents, summary) in [
        (
            Some(json!({"invalid":"manifest"})),
            None,
            "Done".to_string(),
        ),
        (
            Some(json!([{"name":"brief.md","size_bytes":-1}])),
            None,
            "Done".to_string(),
        ),
        (None, None, "x".repeat(MAX_REVISION_OBJECTIVE_BYTES)),
        (
            Some(json!([{"name":"brief.md","size_bytes":MAX_REVISION_OBJECTIVE_BYTES}])),
            Some(BTreeMap::from([(
                "brief.md".into(),
                "\n".repeat(MAX_REVISION_OBJECTIVE_BYTES),
            )])),
            "Done".to_string(),
        ),
    ] {
        let (result, writes) = request_revision(manifest, contents, "run-1", &summary).await;
        assert!(matches!(result, Err(AppError::Conflict(_))));
        assert!(writes.is_empty());
    }
}

#[tokio::test]
async fn text_only_review_keeps_the_complete_prior_output() {
    let text = format!("{}End of original deliverable", "x".repeat(4100));
    let (result, writes) = request_revision(None, None, "run-1", &text).await;
    assert!(result.is_ok());
    let encoded = writes[0]["metadata"]["annotations"]["kars.azure.com/run-objective-b64"]
        .as_str()
        .unwrap();
    let objective = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
    assert!(objective.contains(&text));
}

#[tokio::test]
async fn revision_context_accepts_the_exact_serialized_byte_limit_only() {
    let (result, writes) = request_revision(None, None, "run-1", "").await;
    assert!(result.is_ok());
    let encoded = writes[0]["metadata"]["annotations"]["kars.azure.com/run-objective-b64"]
        .as_str()
        .unwrap();
    let objective = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
    let available = MAX_REVISION_OBJECTIVE_BYTES - serde_json::to_vec(&objective).unwrap().len();
    let (result, _) = request_revision(None, None, "run-1", &"x".repeat(available)).await;
    assert!(result.is_ok());
    let (result, writes) = request_revision(None, None, "run-1", &"x".repeat(available + 1)).await;
    assert!(matches!(result, Err(AppError::Conflict(_))));
    assert!(writes.is_empty());
}

#[tokio::test]
async fn modern_counted_records_without_a_manifest_preserve_both_complete_files() {
    let text = format!(
        "{}Keep all of this. 日本語",
        "Detailed prior output\n".repeat(500)
    );
    let (result, writes) = request_revision_with_count(
        None,
        Some(BTreeMap::from([
            ("brief.md".into(), text.clone()),
            ("checklist.md".into(), "Keep both breaks".into()),
            ("response.md".into(), "Done".into()),
            (
                super::super::tasks::ROUTER_OBSERVATIONS_ARTIFACT.into(),
                "PRIVATE".into(),
            ),
        ])),
        "run-1",
        "Done",
        Some("4"),
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    let encoded = writes[0]["metadata"]["annotations"]["kars.azure.com/run-objective-b64"]
        .as_str()
        .unwrap();
    let objective = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
    assert!(objective.contains(&serde_json::to_string(&text).unwrap()));
    assert!(objective.contains("Keep both breaks"));
    assert!(!objective.contains("PRIVATE"));
}

#[tokio::test]
async fn modern_missing_stale_binary_or_inconsistent_counted_files_fail_closed() {
    for (contents, nonce, count) in [
        (None, "run-1", "4"),
        (
            Some(BTreeMap::from([("brief.md".into(), "text".into())])),
            "run-1",
            "2",
        ),
        (
            Some(BTreeMap::from([("brief.md".into(), "text".into())])),
            "other-run",
            "1",
        ),
        (None, "run-1", "unknown"),
        (None, "run-1", "-1"),
    ] {
        let (result, writes) =
            request_revision_with_count(None, contents, nonce, "Done", Some(count)).await;
        assert!(matches!(result, Err(AppError::Conflict(_))));
        assert!(writes.is_empty());
    }
}
