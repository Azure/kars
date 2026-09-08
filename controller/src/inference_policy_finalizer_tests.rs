// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::Value;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const POLICY_PATH: &str =
    "/apis/kars.azure.com/v1alpha1/namespaces/workspace/inferencepolicies/example";
const CONFIGMAP_PATH: &str =
    "/api/v1/namespaces/workspace/configmaps/inferencepolicy-example-profile";
const CUSTOMER_FINALIZER: &str = "customer.example/cleanup";

fn policy(deleting: bool) -> InferencePolicy {
    let mut value = json!({
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "InferencePolicy",
        "metadata": {
            "name": "example", "namespace": "workspace", "uid": "policy-uid",
            "resourceVersion": "12", "finalizers": [CUSTOMER_FINALIZER],
        },
        "spec": { "appliesTo": { "sandboxName": "agent" } },
    });
    if deleting {
        value["metadata"]["deletionTimestamp"] = json!("2026-09-07T22:52:35Z");
        value["metadata"]["finalizers"] = json!([FINALIZER, CUSTOMER_FINALIZER]);
    }
    serde_json::from_value(value).unwrap()
}

fn status(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({
        "apiVersion": "v1", "kind": "Status",
        "status": if code < 400 { "Success" } else { "Failure" },
        "reason": match code {
            404 => "NotFound", 409 => "Conflict", 403 => "Forbidden", _ => "InternalError"
        },
        "code": code,
    }))
}

async fn setup() -> (MockServer, Arc<Ctx>) {
    let server = MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        http: reqwest::Client::new(),
        phase_reporter: PhaseEventReporter::new(client, "InferencePolicy"),
    });
    (server, ctx)
}

async fn expect_patch(server: &MockServer, policy: &InferencePolicy, finalizers: Vec<String>) {
    let expected = json!({"metadata": {
        "name": "example", "namespace": "workspace", "uid": "policy-uid",
        "resourceVersion": "12", "finalizers": finalizers,
    }});
    let mut response = policy.clone();
    response.metadata.resource_version = Some("13".into());
    response.metadata.finalizers = Some(finalizers);
    Mock::given(method("PATCH"))
        .and(path(POLICY_PATH))
        .and(wiremock::matchers::header(
            "content-type",
            "application/merge-patch+json",
        ))
        .and(wiremock::matchers::body_json(expected))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn finalizer_registration_preserves_customer_metadata_without_partial_policy_apply() {
    let policy = policy(false);
    let (server, ctx) = setup().await;
    expect_patch(
        &server,
        &policy,
        vec![CUSTOMER_FINALIZER.into(), FINALIZER.into()],
    )
    .await;
    assert_eq!(
        reconcile(Arc::new(policy), ctx).await.unwrap(),
        Action::requeue(Duration::from_secs(1))
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn deletion_removes_only_own_finalizer_after_profile_cleanup_or_notfound() {
    for code in [200, 404] {
        let policy = policy(true);
        let (server, ctx) = setup().await;
        Mock::given(method("DELETE"))
            .and(path(CONFIGMAP_PATH))
            .respond_with(status(code))
            .expect(1)
            .mount(&server)
            .await;
        expect_patch(&server, &policy, vec![CUSTOMER_FINALIZER.into()]).await;
        assert_eq!(
            reconcile(Arc::new(policy), ctx).await.unwrap(),
            Action::await_change()
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(requests[1].method, "PATCH");
        assert!(
            requests[1]
                .url
                .query_pairs()
                .all(|(key, value)| key != "force" || value != "true")
        );
    }
}

#[tokio::test]
async fn profile_cleanup_errors_keep_finalizer_and_use_error_retry() {
    for code in [403, 409, 500] {
        let policy = Arc::new(policy(true));
        let (server, ctx) = setup().await;
        Mock::given(method("DELETE"))
            .and(path(CONFIGMAP_PATH))
            .respond_with(status(code))
            .expect(1)
            .mount(&server)
            .await;
        let error = reconcile(policy.clone(), ctx.clone()).await.unwrap_err();
        assert!(matches!(&error, ReconcileError::Kube(kube::Error::Api(s)) if s.code == code));
        assert_ne!(
            error_policy(policy.clone(), &error, ctx),
            Action::await_change()
        );
        assert!(
            policy
                .metadata
                .finalizers
                .as_ref()
                .unwrap()
                .contains(&FINALIZER.into())
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn finalizer_update_conflicts_do_not_force_or_retry_stale_identity() {
    for deleting in [false, true] {
        let policy = Arc::new(policy(deleting));
        let (server, ctx) = setup().await;
        if deleting {
            Mock::given(method("DELETE"))
                .and(path(CONFIGMAP_PATH))
                .respond_with(status(404))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("PATCH"))
            .and(path(POLICY_PATH))
            .respond_with(status(409))
            .expect(1)
            .mount(&server)
            .await;
        let error = reconcile(policy.clone(), ctx.clone()).await.unwrap_err();
        assert_ne!(error_policy(policy, &error, ctx), Action::await_change());
        let requests = server.received_requests().await.unwrap();
        let patch = requests.last().unwrap();
        assert_eq!(
            patch.headers.get("content-type").unwrap(),
            "application/merge-patch+json"
        );
        let body: Value = serde_json::from_slice(&patch.body).unwrap();
        assert_eq!(body["metadata"]["uid"], "policy-uid");
        assert_eq!(body["metadata"]["resourceVersion"], "12");
        assert!(
            patch
                .url
                .query_pairs()
                .all(|(key, value)| key != "force" || value != "true")
        );
        assert_eq!(requests.len(), if deleting { 2 } else { 1 });
    }
}

#[tokio::test]
async fn finalizer_patch_refuses_missing_api_identity_without_writing() {
    for field in ["name", "namespace", "uid", "resourceVersion"] {
        let (server, ctx) = setup().await;
        let mut value = serde_json::to_value(policy(false)).unwrap();
        value["metadata"].as_object_mut().unwrap().remove(field);
        let policy: InferencePolicy = serde_json::from_value(value).unwrap();
        let api = Api::namespaced(ctx.client.clone(), "workspace");
        let result = patch_finalizers(&api, &policy, vec![FINALIZER.into()]).await;
        assert!(matches!(result, Err(ReconcileError::MissingIdentity(_))));
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
