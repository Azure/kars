use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::post,
};
use tower::ServiceExt;

#[path = "credential_review_tests.rs"]
mod reviewed_flow;

const GRANT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
const TARGET: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/target";
const SOURCE: &str = "/api/v1/namespaces/work/secrets/kars-credential-input-sandbox-target";
const PRIVATE: &str = "PRIVATE_CREDENTIAL_VALUE_NEVER_LOGGED_OR_RETURNED";

#[derive(Clone, Copy)]
pub(super) enum Fault {
    Status,
    TargetUid,
    Spec,
    Policy,
    SourceUid,
    GrantUid,
    Api(u16),
    CreateAck,
    BindAck,
}

pub(super) fn api_failure(code: u16) -> Response {
    let reason = match code {
        403 => "Forbidden",
        409 => "Conflict",
        422 => "Invalid",
        _ => "ServiceUnavailable",
    };
    (
        StatusCode::from_u16(code).unwrap(),
        axum::Json(json!({
            "apiVersion":"v1","kind":"Status","status":"Failure","code":code,"reason":reason,
            "message":PRIVATE,"details":{"causes":[{"message":PRIVATE}]}
        })),
    )
        .into_response()
}

pub(super) fn before_request(state: &mut TestApi, method: &Method, path: &str) -> Option<Response> {
    if method != Method::PATCH || path != TARGET {
        return None;
    }
    let fault = state.fault?;
    if matches!(fault, Fault::CreateAck | Fault::BindAck) {
        return None;
    }
    state.fault = None;
    if let Fault::Api(code) = fault {
        return Some(api_failure(code));
    }
    // Interleave after the handler's third target GET and before the real
    // UID/RV comparison in the controlled API's PATCH implementation.
    let target = state.objects.get_mut(TARGET).unwrap();
    target["metadata"]["resourceVersion"] = "2".into();
    match fault {
        Fault::Status => target["status"] = json!({"phase":"Prepared","observedGeneration":1}),
        Fault::TargetUid => target["metadata"]["uid"] = "replacement-target".into(),
        Fault::Spec => {
            target["metadata"]["generation"] = 2.into();
            target["spec"]["suspended"] = false.into();
        }
        Fault::Policy => {
            target["metadata"]["generation"] = 2.into();
            target["spec"]["inferenceRef"]["name"] = "changed-policy".into();
        }
        Fault::SourceUid => {
            state.objects.get_mut(SOURCE).unwrap()["metadata"]["uid"] = "replacement-source".into()
        }
        Fault::GrantUid => {
            state.objects.get_mut(GRANT).unwrap()["metadata"]["uid"] = "replacement-grant".into()
        }
        _ => unreachable!(),
    }
    None
}

pub(super) fn after_write(state: &mut TestApi, method: &Method, path: &str) -> Option<Response> {
    let lost = matches!(state.fault, Some(Fault::CreateAck))
        && method == Method::POST
        && path == "/api/v1/namespaces/work/secrets"
        || matches!(state.fault, Some(Fault::BindAck)) && method == Method::PATCH && path == TARGET;
    if !lost {
        return None;
    }
    state.fault = None;
    let code = if method == Method::POST {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let stream = tokio_stream::iter([Err::<Bytes, _>(std::io::Error::from(
        std::io::ErrorKind::ConnectionReset,
    ))]);
    Some((code, Body::from_stream(stream)).into_response())
}

fn prepare(state: &mut TestApi) {
    state.objects.insert(GRANT.into(), json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1","generation":1},
        "spec":{"enabled":true,"workspaceUid":"namespace","agentKeys":[],
            "integrationStores":[],"legacyImports":[]},
        "status":{"phase":"Ready","observedGeneration":1,"sources":[],"legacySources":[]}
    }));
    state.objects.insert(TARGET.into(), json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
        "metadata":{"name":"target","namespace":"work","uid":"uid-target","resourceVersion":"1","generation":1},
        "spec":{"suspended":true,"inferenceRef":{"name":"reviewed-policy"}},
        "status":{"phase":"Pending"}
    }));
}

async fn write(cluster: &Cluster, target_uid: &str) -> (StatusCode, Value) {
    let app = Router::new()
        .route(
            "/api/operator/credentials",
            post(crate::routes::operator::put_credential),
        )
        .with_state(crate::state::AppState::for_test_client(
            cluster.client.clone(),
            "work",
        ));
    let request = Request::builder()
        .method("POST")
        .uri("/api/operator/credentials")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "namespace":"work","kind":"KarsSandbox","target":"target","targetUid":target_uid,
                "key":"SLACK_BOT_TOKEN","value":PRIVATE,
            })
            .to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let code = response.status();
    let bytes = to_bytes(response.into_body(), 16384).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(PRIVATE));
    (code, serde_json::from_slice(&bytes).unwrap())
}

fn mutations(state: &TestApi) -> Vec<(&str, &str)> {
    state
        .calls
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .map(|(method, path, _)| (method.as_str(), path.as_str()))
        .collect()
}

#[tokio::test]
async fn credential_handler_status_rv_race_returns_typed_409_without_retry_or_partial_write_adoption()
 {
    let (cluster, state, server) = fixture().await;
    {
        let mut state = state.lock().unwrap();
        prepare(&mut state);
        state.fault = Some(Fault::Status);
    }
    let (code, body) = write(&cluster, "uid-target").await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "conflict");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("review before resubmitting")
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(
            mutations(&state),
            [
                ("POST", "/api/v1/namespaces/work/secrets"),
                ("PATCH", TARGET)
            ]
        );
        assert_eq!(
            state
                .calls
                .iter()
                .filter(|(_, path, _)| path == GRANT)
                .count(),
            1
        );
        assert_eq!(
            state
                .calls
                .iter()
                .filter(|(method, path, _)| method == "GET" && path == TARGET)
                .count(),
            3
        );
        assert!(
            !state
                .calls
                .iter()
                .any(|(method, path, _)| method == "GET" && path == SOURCE)
        );
        assert_eq!(state.objects[TARGET]["metadata"]["resourceVersion"], "2");
        assert_eq!(state.objects[TARGET]["metadata"]["generation"], 1);
        assert!(
            state.objects[TARGET]["spec"]
                .get("credentialBindings")
                .is_none()
        );
        assert_eq!(state.objects[SOURCE]["metadata"]["uid"], "created-source");
        assert!(
            state.objects[SOURCE]["data"]["SLACK_BOT_TOKEN"]
                == json!(k8s_openapi::ByteString(PRIVATE.as_bytes().to_vec()))
        );
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn credential_handler_authority_changes_cannot_trigger_a_rebase_retry_or_owned_rollback() {
    for fault in [
        Fault::TargetUid,
        Fault::Spec,
        Fault::Policy,
        Fault::SourceUid,
        Fault::GrantUid,
    ] {
        let (cluster, state, server) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            prepare(&mut state);
            state.fault = Some(fault);
        }
        let (code, body) = write(&cluster, "uid-target").await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "conflict");
        {
            let state = state.lock().unwrap();
            assert_eq!(
                mutations(&state),
                [
                    ("POST", "/api/v1/namespaces/work/secrets"),
                    ("PATCH", TARGET)
                ]
            );
            assert!(
                state.objects[TARGET]["spec"]
                    .get("credentialBindings")
                    .is_none()
            );
            assert!(
                state.objects.contains_key(SOURCE),
                "No multi-object rollback or replacement deletion is authorized"
            );
            if matches!(fault, Fault::SourceUid) {
                assert_eq!(
                    state.objects[SOURCE]["metadata"]["uid"],
                    "replacement-source"
                );
            }
            if matches!(fault, Fault::GrantUid) {
                assert_eq!(state.objects[GRANT]["metadata"]["uid"], "replacement-grant");
            }
        }
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn credential_handler_only_typed_api_409_is_a_conflict_and_denials_remain_fatal() {
    for upstream in [403, 409, 422, 503] {
        let (cluster, state, server) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            prepare(&mut state);
            state.fault = Some(Fault::Api(upstream));
        }
        let (code, body) = write(&cluster, "uid-target").await;
        assert_eq!(
            code,
            if upstream == 409 {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_GATEWAY
            }
        );
        assert_eq!(
            body["error"]["code"],
            if upstream == 409 {
                "conflict"
            } else {
                "upstream_error"
            }
        );
        assert_eq!(
            mutations(&state.lock().unwrap()),
            [
                ("POST", "/api/v1/namespaces/work/secrets"),
                ("PATCH", TARGET),
            ]
        );
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn credential_handler_lost_create_or_bind_ack_never_replays_or_deletes_uncertain_side_effects()
 {
    for fault in [Fault::CreateAck, Fault::BindAck] {
        let (cluster, state, server) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            prepare(&mut state);
            state.fault = Some(fault);
        }
        let (code, body) = write(&cluster, "uid-target").await;
        assert_eq!(code, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["code"], "upstream_error");
        {
            let state = state.lock().unwrap();
            let writes = mutations(&state);
            assert_eq!(
                writes
                    .iter()
                    .filter(|(method, _)| *method == "POST")
                    .count(),
                1
            );
            assert_eq!(
                writes
                    .iter()
                    .filter(|(method, _)| *method == "PATCH")
                    .count(),
                usize::from(matches!(fault, Fault::BindAck))
            );
            assert!(!writes.iter().any(|(method, _)| *method == "DELETE"));
            assert_eq!(state.objects[SOURCE]["metadata"]["uid"], "created-source");
            assert_eq!(
                state.objects[TARGET]["spec"]
                    .get("credentialBindings")
                    .is_some(),
                matches!(fault, Fault::BindAck)
            );
        }
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn credential_handler_rejects_stale_review_and_source_collision_without_adoption() {
    for collision in [false, true] {
        let (cluster, state, server) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            prepare(&mut state);
            if collision {
                state.objects.insert(SOURCE.into(), json!({
                    "apiVersion":"v1","kind":"Secret","type":"Opaque",
                    "metadata":{"name":"kars-credential-input-sandbox-target","namespace":"work",
                        "uid":"foreign-source","resourceVersion":"7"},
                    "data":{"SLACK_BOT_TOKEN":k8s_openapi::ByteString(PRIVATE.as_bytes().to_vec())}
                }));
            }
        }
        let (code, body) = write(
            &cluster,
            if collision {
                "uid-target"
            } else {
                "old-target"
            },
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "conflict");
        {
            let state = state.lock().unwrap();
            assert!(state.calls.iter().all(|(method, path, _)| method == "GET"
                || (collision && method == "POST" && path == "/api/v1/namespaces/work/secrets")));
            assert!(
                !state
                    .calls
                    .iter()
                    .any(|(method, path, _)| method == "GET" && path == SOURCE)
            );
            assert!(
                state.objects[TARGET]["spec"]
                    .get("credentialBindings")
                    .is_none()
            );
            if collision {
                assert_eq!(state.objects[SOURCE]["metadata"]["uid"], "foreign-source");
            } else {
                assert!(!state.objects.contains_key(SOURCE));
            }
        }
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn credential_handler_success_still_binds_the_created_source_once_after_metadata_ack() {
    let (cluster, state, server) = fixture().await;
    prepare(&mut state.lock().unwrap());
    let (code, body) = write(&cluster, "uid-target").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["stored"], true);
    assert_eq!(body["source"]["uid"], "created-source");
    assert_eq!(
        mutations(&state.lock().unwrap()),
        [
            ("POST", "/api/v1/namespaces/work/secrets"),
            ("PATCH", TARGET),
        ]
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn credential_handler_unready_or_changed_workspace_authority_conflicts_before_any_write() {
    for change in ["disabled", "workspace", "generation"] {
        let (cluster, state, server) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            prepare(&mut state);
            let grant = state.objects.get_mut(GRANT).unwrap();
            match change {
                "disabled" => grant["spec"]["enabled"] = false.into(),
                "workspace" => grant["spec"]["workspaceUid"] = "replacement".into(),
                "generation" => grant["metadata"]["generation"] = 2.into(),
                _ => unreachable!(),
            }
        }
        let (code, body) = write(&cluster, "uid-target").await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "conflict");
        {
            let state = state.lock().unwrap();
            assert!(mutations(&state).is_empty());
            assert!(!state.objects.contains_key(SOURCE));
            assert!(
                state.objects[TARGET]["spec"]
                    .get("credentialBindings")
                    .is_none()
            );
        }
        server.abort();
        let _ = server.await;
    }
}
