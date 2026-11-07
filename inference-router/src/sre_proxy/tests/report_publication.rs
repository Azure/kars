// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::sre_report::{Identity, KEY, MAX_TEXT, NAME, NAMESPACE, PATH};
use serde_json::Value;

fn payload() -> Value {
    let now = Utc::now().timestamp_millis() as u64;
    json!({"sweep_id":"a".repeat(32),"started_at_ms":now-1000,"finished_at_ms":now,
        "turn_status":"blocked","text":"Inference unavailable; diagnostic coverage unknown",
        "truncated":false,"interval_seconds":900,"timeout_seconds":180})
}

fn install_target(f: &Fixture) {
    let mut target = Identity {
        registration_uid: "registration".into(),
        source_namespace: "kars-system".into(),
        source_uid: "source".into(),
        namespace_uid: "namespace".into(),
    }
    .target();
    target["metadata"]["uid"] = json!("target-uid");
    target["metadata"]["resourceVersion"] = json!("1");
    f.privacy.lock().unwrap().report_target = Some(target);
}

async fn publish(f: &Fixture, payload: &Value) -> reqwest::Response {
    f.client
        .post(format!("{}{PATH}", f.url))
        .bearer_auth(&f.token)
        .json(payload)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn reports_require_bearer_live_source_and_nonrevoked_authority() {
    for failure in ["missing-bearer", "wrong-bearer", "revoked", "replaced"] {
        let f = fixture().await;
        install_target(&f);
        let mut request = f.client.post(format!("{}{PATH}", f.url)).json(&payload());
        match failure {
            "wrong-bearer" => request = request.bearer_auth("not-the-agent-credential"),
            "revoked" => {
                f.privacy.lock().unwrap().revoked = true;
                request = request.bearer_auth(&f.token);
            }
            "replaced" => {
                f.privacy.lock().unwrap().replaced = true;
                request = request.bearer_auth(&f.token);
            }
            _ => {}
        }
        let response = request.send().await.unwrap();
        assert_eq!(
            response.status(),
            if failure.ends_with("bearer") {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            },
            "{failure}"
        );
        assert!(
            !f.privacy
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|(method, _, _)| method == "PUT")
        );
    }
}

#[tokio::test]
async fn reports_are_latest_only_idempotent_and_never_proposals_or_general_writes() {
    let f = fixture().await;
    install_target(&f);
    let input = payload();
    let first = publish(&f, &input).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: Value = first.json().await.unwrap();
    let second = publish(&f, &input).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.json::<Value>().await.unwrap(), first);
    let mut conflicting = input.clone();
    conflicting["text"] = json!("different content");
    assert_eq!(
        publish(&f, &conflicting).await.status(),
        StatusCode::CONFLICT
    );
    let mut older = input.clone();
    older["sweep_id"] = json!("b".repeat(32));
    assert_eq!(publish(&f, &older).await.status(), StatusCode::CONFLICT);
    let path = format!("/api/v1/namespaces/{NAMESPACE}/configmaps/{NAME}");
    for method in [
        reqwest::Method::POST,
        reqwest::Method::PUT,
        reqwest::Method::PATCH,
        reqwest::Method::DELETE,
    ] {
        assert_eq!(
            f.client
                .request(method, format!("{}{path}", f.url))
                .bearer_auth(&f.token)
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let state = f.privacy.lock().unwrap();
    let writes: Vec<_> = state
        .calls
        .iter()
        .filter(|(method, _, _)| method == "PUT")
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].1, path);
    assert!(
        state
            .calls
            .iter()
            .all(|(_, path, _)| !path.ends_with("/karssreactions"))
    );
    let stored = &state.report_target.as_ref().unwrap()["data"][KEY];
    let report: Value = serde_json::from_str(stored.as_str().unwrap()).unwrap();
    assert_eq!(report["report"]["turn_status"], "blocked");
    assert_eq!(report["identity"]["source_uid"], "source");
}

#[tokio::test]
async fn report_input_bounds_unknown_fields_and_wrong_targets_fail_closed() {
    let f = fixture().await;
    install_target(&f);
    for case in [
        "oversized-text",
        "unknown-field",
        "invalid-id",
        "invalid-status",
        "future",
        "reversed-time",
    ] {
        let mut input = payload();
        match case {
            "oversized-text" => input["text"] = json!("x".repeat(MAX_TEXT + 1)),
            "unknown-field" => input["target"] = json!("arbitrary-configmap"),
            "invalid-id" => input["sweep_id"] = json!("../other"),
            "invalid-status" => input["turn_status"] = json!("healthy"),
            "future" => input["finished_at_ms"] = json!(Utc::now().timestamp_millis() + 60_000),
            _ => input["finished_at_ms"] = json!(1),
        }
        assert_eq!(
            publish(&f, &input).await.status(),
            StatusCode::BAD_REQUEST,
            "{case}"
        );
    }
    assert_eq!(
        f.client
            .post(format!("{}{PATH}", f.url))
            .bearer_auth(&f.token)
            .header("content-type", "application/json")
            .body("x".repeat(65537))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    for field in ["registration", "source", "missing", "cas"] {
        install_target(&f);
        {
            let mut state = f.privacy.lock().unwrap();
            match field {
                "registration" => {
                    state.report_target.as_mut().unwrap()["metadata"]["annotations"]
                        [crate::sre_report::OWNER] = json!("other")
                }
                "source" => {
                    state.report_target.as_mut().unwrap()["metadata"]["annotations"]
                        [crate::sre_report::SOURCE] = json!("other")
                }
                "missing" => state.report_target = None,
                _ => state.report_conflict = true,
            }
        }
        assert_eq!(
            publish(&f, &payload()).await.status(),
            if field == "missing" {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::CONFLICT
            },
            "{field}"
        );
    }
}

#[tokio::test]
async fn reports_accept_empty_kubernetes_data_and_replace_only_the_latest_sweep() {
    for empty in ["missing", "null", "empty"] {
        let f = fixture().await;
        install_target(&f);
        {
            let mut state = f.privacy.lock().unwrap();
            let target = state.report_target.as_mut().unwrap();
            match empty {
                "missing" => {
                    target.as_object_mut().unwrap().remove("data");
                }
                "null" => target["data"] = Value::Null,
                _ => target["data"] = json!({}),
            }
        }
        let mut first = payload();
        first["finished_at_ms"] = json!(first["finished_at_ms"].as_u64().unwrap() - 100);
        assert_eq!(
            publish(&f, &first).await.status(),
            StatusCode::OK,
            "{empty}"
        );
        let mut next = first.clone();
        next["sweep_id"] = json!("b".repeat(32));
        next["finished_at_ms"] = json!(first["finished_at_ms"].as_u64().unwrap() + 1);
        next["turn_status"] = json!("failed");
        assert_eq!(publish(&f, &next).await.status(), StatusCode::OK);
        assert_eq!(publish(&f, &first).await.status(), StatusCode::CONFLICT);
        let state = f.privacy.lock().unwrap();
        let target = state.report_target.as_ref().unwrap();
        assert_eq!(target["data"].as_object().unwrap().len(), 1);
        assert_eq!(target["metadata"]["uid"], "target-uid");
        let stored =
            crate::sre_report::Report::decode(target["data"][KEY].as_str().unwrap()).unwrap();
        assert_eq!(stored.report.sweep_id, "b".repeat(32));
        assert_eq!(stored.report.turn_status, "failed");
    }
}

#[tokio::test]
async fn retirement_between_storage_read_and_write_blocks_publication() {
    for replaced in [false, true] {
        let f = fixture().await;
        install_target(&f);
        {
            let mut state = f.privacy.lock().unwrap();
            state.revoke_after_report_read = !replaced;
            state.replace_after_report_read = replaced;
        }
        assert_eq!(
            publish(&f, &payload()).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(
            !f.privacy
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|(method, _, _)| method == "PUT")
        );
    }
}

#[tokio::test]
async fn revoked_or_replaced_sources_cannot_replay_previously_accepted_reports() {
    let f = fixture().await;
    install_target(&f);
    let input = payload();
    assert_eq!(publish(&f, &input).await.status(), StatusCode::OK);
    f.privacy.lock().unwrap().revoked = true;
    assert_eq!(
        publish(&f, &input).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    {
        let mut state = f.privacy.lock().unwrap();
        state.revoked = false;
        state.replaced = true;
    }
    assert_eq!(
        publish(&f, &input).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        f.privacy
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(method, _, _)| method == "PUT")
            .count(),
        1
    );
}
