use super::*;
use axum::Extension;
use base64::Engine;

const REVIEW: &str = "/api/operator/credentials/review";
const WRITE: &str = "/api/operator/credentials";
const SIGNING: &str = "private-review-fixture-signing-key";

#[tokio::test]
async fn credential_review_accepts_only_attested_ownership_metadata_transition_after_its_write() {
    for (receipt_present, wrong_from) in [(false, false), (true, true), (true, false)] {
        let accepted = receipt_present && !wrong_from;
        let (cluster, state, server) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            prepare(&mut state);
            state.bind_source_owner = true;
            state.publish_ownership_receipt = receipt_present;
            state.ownership_from_override = wrong_from.then(|| "another-write".into());
        }
        let (status, reviewed) = preview(&cluster, None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, result) = submit(&cluster, &reviewed, PRIVATE).await;
        assert_eq!(
            status,
            if accepted {
                StatusCode::OK
            } else {
                StatusCode::CONFLICT
            }
        );
        {
            let state = state.lock().unwrap();
            assert_eq!(state.source_value_reads, 0);
            assert_eq!(state.objects[SOURCE]["metadata"]["resourceVersion"], "2");
            assert_eq!(
                state.objects[SOURCE]["data"]["SLACK_BOT_TOKEN"],
                json!(k8s_openapi::ByteString(PRIVATE.as_bytes().to_vec()))
            );
            let writes = mutations(&state);
            assert_eq!(
                writes
                    .iter()
                    .filter(|(method, _)| *method == "POST")
                    .count(),
                1
            );
            assert_eq!(
                writes.iter().filter(|(_, path)| *path == TARGET).count(),
                usize::from(accepted)
            );
            if accepted {
                assert_eq!(result["stored"], true);
                assert_eq!(result["resourceVersion"], "2");
            } else {
                assert_eq!(
                    result["error"]["credentialContinuation"]["source"]["version"],
                    "1"
                );
            }
        }
        server.abort();
        let _ = server.await;
    }
}

async fn call(
    cluster: &Cluster,
    path: &str,
    body: Value,
    actor: &str,
    role: &str,
) -> (StatusCode, Value) {
    let principal = crate::auth::Principal {
        sub: actor.into(),
        name: actor.into(),
        roles: vec![role.into()],
    };
    let app = Router::new()
        .route(REVIEW, post(crate::routes::credential_review::review))
        .route(WRITE, post(crate::routes::operator::put_credential))
        .layer(Extension(principal))
        .with_state(
            crate::state::AppState::for_test_client(cluster.client.clone(), "work")
                .with_principal_secret(Some(SIGNING.into())),
        );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 32768).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(PRIVATE));
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn input() -> Value {
    json!({"namespace":"work","kind":"KarsSandbox","target":"target","targetUid":"uid-target","key":"SLACK_BOT_TOKEN"})
}

async fn preview(cluster: &Cluster, continuation: Option<&str>) -> (StatusCode, Value) {
    let mut body = input();
    if let Some(token) = continuation {
        body["continuation"] = token.into();
    }
    call(cluster, REVIEW, body, "operator", "operator").await
}

async fn submit(cluster: &Cluster, review: &Value, value: &str) -> (StatusCode, Value) {
    let mut body = input();
    body["review"] = review["token"].clone();
    body["value"] = value.into();
    call(cluster, WRITE, body, "operator", "operator").await
}

async fn stored_conflict() -> (
    Cluster,
    Arc<Mutex<TestApi>>,
    tokio::task::JoinHandle<()>,
    Value,
    Value,
) {
    let (cluster, state, server) = fixture().await;
    prepare(&mut state.lock().unwrap());
    let (status, first) = preview(&cluster, None).await;
    assert_eq!(status, StatusCode::OK);
    state.lock().unwrap().fault = Some(Fault::Status);
    let (status, failure) = submit(&cluster, &first, PRIVATE).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let continuation = failure["error"]["credentialContinuation"].clone();
    assert_eq!(continuation["outcome"], "source-stored");
    assert_eq!(continuation["source"]["uid"], "created-source");
    (cluster, state, server, first, continuation)
}

#[tokio::test]
async fn credential_review_explicit_acknowledged_resume_binds_once_without_rewriting_or_reading_values()
 {
    let (cluster, state, server, first, continuation) = stored_conflict().await;
    let (status, refreshed) = preview(&cluster, continuation["token"].as_str()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["bindingOnly"], true);
    assert_eq!(refreshed["submission"], 2);
    assert_eq!(refreshed["expiresAt"], first["expiresAt"]);
    assert_eq!(
        refreshed["metadata"]["target"]["intent"],
        first["metadata"]["target"]["intent"]
    );
    assert_eq!(
        refreshed["metadata"]["target"]["generation"],
        first["metadata"]["target"]["generation"]
    );
    assert_eq!(refreshed["metadata"]["source"]["uid"], "created-source");
    let (status, result) = submit(&cluster, &refreshed, PRIVATE).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["stored"], true);
    {
        let state = state.lock().unwrap();
        assert_eq!(
            mutations(&state),
            [
                ("POST", "/api/v1/namespaces/work/secrets"),
                ("PATCH", TARGET),
                ("PATCH", TARGET),
            ]
        );
        assert_eq!(state.source_value_reads, 0);
        assert!(state.source_metadata_reads > 0);
        assert_eq!(
            state.objects[TARGET]["spec"]["credentialBindings"]["sources"][0]["source"]["uid"],
            "created-source"
        );
        assert!(
            state.objects[SOURCE]["data"]["SLACK_BOT_TOKEN"]
                == json!(k8s_openapi::ByteString(PRIVATE.as_bytes().to_vec()))
        );
    }
    let token = refreshed["token"].as_str().unwrap();
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1).unwrap())
        .unwrap();
    assert!(!String::from_utf8_lossy(&payload).contains(PRIVATE));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn credential_review_zero_write_conflict_requires_explicit_refresh_of_unchanged_authority() {
    let (cluster, state, server) = fixture().await;
    prepare(&mut state.lock().unwrap());
    let (_, first) = preview(&cluster, None).await;
    state.lock().unwrap().objects.get_mut(TARGET).unwrap()["metadata"]["resourceVersion"] =
        "2".into();
    let (status, failure) = submit(&cluster, &first, PRIVATE).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(mutations(&state.lock().unwrap()).is_empty());
    let receipt = &failure["error"]["credentialContinuation"];
    assert_eq!(receipt["outcome"], "no-write-attempted");
    assert!(receipt["source"].is_null());
    let (status, fresh) = preview(&cluster, receipt["token"].as_str()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fresh["bindingOnly"], false);
    let (status, _) = submit(&cluster, &fresh, PRIVATE).await;
    assert_eq!(status, StatusCode::OK);
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
async fn credential_review_actor_role_value_and_signature_changes_never_mutate_after_partial_write()
{
    let (cluster, state, server, _, receipt) = stored_conflict().await;
    let (_, fresh) = preview(&cluster, receipt["token"].as_str()).await;
    let writes = mutations(&state.lock().unwrap()).len();
    for (actor, role, value, tamper) in [
        ("other", "operator", PRIVATE, false),
        ("operator", "user", PRIVATE, false),
        ("operator", "operator", "different-credential", false),
        ("operator", "operator", PRIVATE, true),
    ] {
        let mut body = input();
        body["value"] = value.into();
        body["review"] = if tamper {
            format!("{}x", fresh["token"].as_str().unwrap()).into()
        } else {
            fresh["token"].clone()
        };
        let (status, _) = call(&cluster, WRITE, body, actor, role).await;
        assert!(status == StatusCode::CONFLICT || status == StatusCode::FORBIDDEN);
        assert_eq!(mutations(&state.lock().unwrap()).len(), writes);
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn credential_review_changed_intent_grant_source_and_namespace_cannot_be_accepted_by_refresh()
{
    for change in [
        "uid",
        "generation",
        "spec",
        "metadata",
        "grant",
        "grant-policy",
        "source",
        "source-version",
        "workspace",
    ] {
        let (cluster, state, server, _, receipt) = stored_conflict().await;
        let writes = mutations(&state.lock().unwrap()).len();
        {
            let mut state = state.lock().unwrap();
            match change {
                "uid" => {
                    state.objects.get_mut(TARGET).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                "generation" => {
                    state.objects.get_mut(TARGET).unwrap()["metadata"]["generation"] = 2.into()
                }
                "spec" => {
                    state.objects.get_mut(TARGET).unwrap()["spec"]["suspended"] = false.into()
                }
                "metadata" => {
                    state.objects.get_mut(TARGET).unwrap()["metadata"]["annotations"] =
                        json!({"changed":"intent"})
                }
                "grant" => {
                    state.objects.get_mut(GRANT).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                "grant-policy" => {
                    state.objects.get_mut(GRANT).unwrap()["spec"]["agentKeys"] =
                        json!(["OTHER_KEY"])
                }
                "source" => {
                    state.objects.get_mut(SOURCE).unwrap()["metadata"]["uid"] =
                        "replacement".into();
                }
                "source-version" => {
                    state.objects.get_mut(SOURCE).unwrap()["metadata"]["resourceVersion"] =
                        "2".into();
                    state.objects.get_mut(GRANT).unwrap()["status"]["sources"][0]["resourceVersion"] =
                        "2".into();
                }
                "workspace" => {
                    state.objects.get_mut(GRANT).unwrap()["spec"]["workspaceUid"] =
                        "replacement".into()
                }
                _ => unreachable!(),
            }
        }
        let (status, _) = preview(&cluster, receipt["token"].as_str()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(mutations(&state.lock().unwrap()).len(), writes);
        assert_eq!(state.lock().unwrap().source_value_reads, 0);
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn credential_review_valid_receipt_still_fences_changes_between_review_and_submission() {
    for change in ["target", "grant", "source"] {
        let (cluster, state, server, _, receipt) = stored_conflict().await;
        let (_, fresh) = preview(&cluster, receipt["token"].as_str()).await;
        let writes = mutations(&state.lock().unwrap()).len();
        {
            let mut state = state.lock().unwrap();
            let path = match change {
                "target" => TARGET,
                "grant" => GRANT,
                _ => SOURCE,
            };
            state.objects.get_mut(path).unwrap()["metadata"]["resourceVersion"] = "99".into();
        }
        let (status, _) = submit(&cluster, &fresh, PRIVATE).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(mutations(&state.lock().unwrap()).len(), writes);
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn credential_review_collision_denial_and_lost_ack_never_issue_continuation_authority() {
    for fault in [
        Fault::CreateAck,
        Fault::BindAck,
        Fault::Api(403),
        Fault::Api(422),
    ] {
        let (cluster, state, server) = fixture().await;
        prepare(&mut state.lock().unwrap());
        let (_, first) = preview(&cluster, None).await;
        state.lock().unwrap().fault = Some(fault);
        let (status, body) = submit(&cluster, &first, PRIVATE).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body["error"].get("credentialContinuation").is_none());
        server.abort();
        let _ = server.await;
    }
    let (cluster, state, server) = fixture().await;
    prepare(&mut state.lock().unwrap());
    let (_, first) = preview(&cluster, None).await;
    state.lock().unwrap().objects.insert(SOURCE.into(), json!({
        "apiVersion":"v1","kind":"Secret","metadata":{"name":"kars-credential-input-sandbox-target",
            "namespace":"work","uid":"foreign","resourceVersion":"1"},"type":"Opaque",
    }));
    let (status, body) = submit(&cluster, &first, PRIVATE).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].get("credentialContinuation").is_none());
    assert_eq!(
        mutations(&state.lock().unwrap()),
        [("POST", "/api/v1/namespaces/work/secrets")]
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn credential_review_limits_explicit_submissions_without_source_rewrite_or_automatic_bind_retry()
 {
    let (cluster, state, server, _, mut receipt) = stored_conflict().await;
    for submission in [2, 3] {
        let (_, fresh) = preview(&cluster, receipt["token"].as_str()).await;
        assert_eq!(fresh["submission"], submission);
        // Status advances after each explicit review; the server must not rebase.
        {
            let mut state = state.lock().unwrap();
            let target = state.objects.get_mut(TARGET).unwrap();
            let version = target["metadata"]["resourceVersion"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                + 1;
            target["metadata"]["resourceVersion"] = version.to_string().into();
        }
        let (status, failure) = submit(&cluster, &fresh, PRIVATE).await;
        assert_eq!(status, StatusCode::CONFLICT);
        if submission == 2 {
            receipt = failure["error"]["credentialContinuation"].clone();
            assert!(receipt.is_object());
        } else {
            assert!(failure["error"].get("credentialContinuation").is_none());
        }
    }
    assert_eq!(
        mutations(&state.lock().unwrap())
            .iter()
            .filter(|(method, _)| *method == "POST")
            .count(),
        1
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn credential_review_rejects_expired_wrong_purpose_and_wrong_audience_signed_tokens() {
    use sha2::{Digest, Sha256};
    let (cluster, state, server) = fixture().await;
    prepare(&mut state.lock().unwrap());
    let (_, first) = preview(&cluster, None).await;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(first["token"].as_str().unwrap().split('.').nth(1).unwrap())
        .unwrap();
    let original: Value = serde_json::from_slice(&payload).unwrap();
    let mut hash = Sha256::new();
    hash.update(b"kars-bridge/credential-review-signing-key/v1\0");
    hash.update(SIGNING.as_bytes());
    let key = hash.finalize();
    for change in ["expired", "extended", "purpose", "audience"] {
        let mut claims = original.clone();
        match change {
            "expired" => claims["exp"] = (chrono::Utc::now().timestamp() - 1).into(),
            "extended" => claims["exp"] = (chrono::Utc::now().timestamp() + 3600).into(),
            "purpose" => claims["purpose"] = "Continuation".into(),
            "audience" => claims["aud"] = "other-protocol".into(),
            _ => unreachable!(),
        }
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(&key),
        )
        .unwrap();
        let mut review = first.clone();
        review["token"] = token.into();
        let (status, _) = submit(&cluster, &review, PRIVATE).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(mutations(&state.lock().unwrap()).is_empty());
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn credential_review_status_managed_fields_may_advance_but_other_metadata_cannot() {
    let (cluster, state, server, _, receipt) = stored_conflict().await;
    {
        let mut state = state.lock().unwrap();
        state.objects.get_mut(TARGET).unwrap()["metadata"]["managedFields"] = json!([{
            "manager":"controller", "operation":"Update", "apiVersion":"kars.azure.com/v1alpha1",
            "fieldsType":"FieldsV1", "fieldsV1":{"f:status":{}}, "subresource":"status"
        }]);
    }
    let (status, _) = preview(&cluster, receipt["token"].as_str()).await;
    assert_eq!(status, StatusCode::OK);
    state.lock().unwrap().objects.get_mut(TARGET).unwrap()["metadata"]["labels"] =
        json!({"authority":"changed"});
    let (status, _) = preview(&cluster, receipt["token"].as_str()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    server.abort();
    let _ = server.await;
}
