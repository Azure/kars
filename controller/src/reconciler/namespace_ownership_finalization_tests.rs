// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::reconciler::{Context, error_policy, reconcile};
use kube::runtime::controller::Action;
use std::sync::{Arc, Mutex as StdMutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const CR_PATH: &str = "/apis/kars.azure.com/v1alpha1/namespaces/kars-demo/karssandboxes/demo";
const NS_PATH: &str = "/api/v1/namespaces/kars-demo";
const OTHER_FINALIZER: &str = "customer.example/cleanup";
const CONCURRENT_FINALIZER: &str = "customer.example/concurrent-cleanup";

#[derive(Clone, Copy)]
enum Fault {
    None,
    SourceGet(u16),
    NamespaceGet(u16),
    Patch(u16),
    RecreateOnPatch,
    BeginDeletionOnNamespacePatch,
}

struct State {
    sandbox: Value,
    namespace: Value,
    fault: Fault,
}

impl State {
    fn legacy(fault: Fault) -> Self {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/compat/fixtures/namespace-legacy.json"
        ))
        .unwrap();
        let mut sandbox = fixture["sandbox"].clone();
        sandbox["metadata"]["namespace"] = json!("kars-demo");
        sandbox["metadata"]["deletionTimestamp"] = json!("2026-09-01T12:00:01Z");
        sandbox["metadata"]["finalizers"] = json!([FINALIZER, OTHER_FINALIZER]);
        sandbox["metadata"]["annotations"] = json!({"customer.example/setting": "keep"});
        let mut namespace = fixture["namespace"].clone();
        namespace["metadata"]["deletionTimestamp"] = json!("2026-09-01T12:00:00Z");
        namespace["status"] = json!({"phase": "Terminating"});
        Self {
            sandbox,
            namespace,
            fault,
        }
    }

    fn observed(&self) -> Arc<KarsSandbox> {
        Arc::new(serde_json::from_value(self.sandbox.clone()).unwrap())
    }

    fn prestaged(fault: Fault) -> Self {
        let mut state = Self::legacy(fault);
        state.sandbox["metadata"]["annotations"][NAMESPACE_UID] =
            state.namespace["metadata"]["uid"].clone();
        state.namespace["metadata"]["annotations"] = json!({
            VERSION: "v1", SOURCE_NAME: "demo", SOURCE_NAMESPACE: "kars-demo",
            PRESTAGE: "bind-next-sandbox", "customer.example/setting": "keep",
        });
        state
    }
}

fn context(client: Client) -> Arc<Context> {
    Arc::new(Context {
        client,
        wi_client_id: String::new(),
        inference_router_image: String::new(),
        sandbox_image: String::new(),
        openai_endpoint: String::new(),
        foundry_endpoint: String::new(),
        foundry_project_endpoint: String::new(),
        foundry_deployments: String::new(),
        imds_client_id: String::new(),
        content_safety_endpoint: String::new(),
        fedcred: None,
        byo_strict: false,
        dev_openai_api_key: String::new(),
        dev_provider: String::new(),
        dev_copilot_github_token: String::new(),
        anthropic_api_key: String::new(),
        anthropic_endpoint: String::new(),
        ollama_endpoint: String::new(),
        openai_moderation_api_key: String::new(),
        openai_moderation_endpoint: String::new(),
        dev_profile: false,
        cluster_name: None,
        cluster_uid: String::new(),
        agent_id_cache: Arc::new(crate::agent_id_provisioning::ProvisionerCache::new()),
    })
}

fn response(code: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(body)
}

fn failure(code: u16) -> ResponseTemplate {
    response(
        code,
        json!({
            "apiVersion": "v1", "kind": "Status", "status": "Failure",
            "reason": if code == 404 { "NotFound" } else { "Conflict" }, "code": code,
        }),
    )
}

#[derive(Clone)]
struct Server(Arc<StdMutex<State>>);

impl Respond for Server {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.0.lock().unwrap();
        let path = request.url.path();
        match (request.method.as_str(), path) {
            ("GET", CR_PATH) => {
                if let Fault::SourceGet(code) = state.fault {
                    state.fault = Fault::None;
                    return failure(code);
                }
                response(200, state.sandbox.clone())
            }
            ("GET", NS_PATH) => {
                if let Fault::NamespaceGet(code) = state.fault {
                    state.fault = Fault::None;
                    return failure(code);
                }
                response(200, state.namespace.clone())
            }
            ("GET", "/apis/kars.azure.com/v1alpha1/karssandboxes") => response(
                200,
                json!({
                    "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandboxList",
                    "metadata": {}, "items": [state.sandbox],
                }),
            ),
            ("GET", "/apis/apps/v1/namespaces/kars-demo/deployments/demo") => failure(404),
            (
                "DELETE",
                "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings/kars-spawner-demo",
            ) => failure(404),
            ("PATCH", NS_PATH) => {
                let patch: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(patch["metadata"]["uid"], state.namespace["metadata"]["uid"]);
                assert_eq!(
                    patch["metadata"]["resourceVersion"],
                    state.namespace["metadata"]["resourceVersion"]
                );
                if matches!(state.fault, Fault::BeginDeletionOnNamespacePatch) {
                    state.fault = Fault::None;
                    assert!(
                        state.sandbox["metadata"]["finalizers"]
                            .as_array()
                            .unwrap()
                            .contains(&json!(FINALIZER))
                    );
                    state.namespace["metadata"]["deletionTimestamp"] =
                        json!("2026-09-01T12:00:00Z");
                    state.namespace["metadata"]["resourceVersion"] = json!("31");
                    state.sandbox["metadata"]["deletionTimestamp"] = json!("2026-09-01T12:00:01Z");
                    state.sandbox["metadata"]["resourceVersion"] = json!("23");
                    return failure(409);
                }
                let annotations = state.namespace["metadata"]["annotations"]
                    .as_object_mut()
                    .unwrap();
                for (key, value) in patch["metadata"]["annotations"].as_object().unwrap() {
                    if value.is_null() {
                        annotations.remove(key);
                    } else {
                        annotations.insert(key.clone(), value.clone());
                    }
                }
                state.namespace["metadata"]["resourceVersion"] = json!("32");
                response(200, state.namespace.clone())
            }
            ("PATCH", CR_PATH) => {
                let patch: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    request.headers.get("content-type").unwrap(),
                    "application/merge-patch+json"
                );
                assert_eq!(patch.as_object().unwrap().len(), 1);
                assert_eq!(patch["metadata"].as_object().unwrap().len(), 3);
                match state.fault {
                    Fault::Patch(code) => {
                        state.fault = Fault::None;
                        if code == 409 {
                            state.sandbox["metadata"]["resourceVersion"] = json!("21");
                            state.sandbox["metadata"]["finalizers"]
                                .as_array_mut()
                                .unwrap()
                                .push(json!(CONCURRENT_FINALIZER));
                        }
                        return failure(code);
                    }
                    Fault::RecreateOnPatch => {
                        state.fault = Fault::None;
                        state.sandbox["metadata"]["uid"] = json!("recreated-sandbox");
                        state.sandbox["metadata"]["resourceVersion"] = json!("21");
                    }
                    _ => {}
                }
                if patch["metadata"]["uid"] != state.sandbox["metadata"]["uid"]
                    || patch["metadata"]["resourceVersion"]
                        != state.sandbox["metadata"]["resourceVersion"]
                {
                    return failure(409);
                }
                state.sandbox["metadata"]["finalizers"] = patch["metadata"]["finalizers"].clone();
                state.sandbox["metadata"]["resourceVersion"] = json!("22");
                response(200, state.sandbox.clone())
            }
            _ => failure(500),
        }
    }
}

async fn setup(state: State) -> (MockServer, Client, Arc<StdMutex<State>>) {
    let server = MockServer::start().await;
    let state = Arc::new(StdMutex::new(state));
    Mock::given(wiremock::matchers::any())
        .respond_with(Server(state.clone()))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state)
}

fn only_finalizer_io(requests: &[Request]) {
    assert!(requests.iter().all(|request| {
        matches!(
            (request.method.as_str(), request.url.path()),
            ("GET", CR_PATH) | ("GET", NS_PATH) | ("PATCH", CR_PATH)
        )
    }));
}

#[tokio::test]
async fn reconciler_entry_finalizes_terminating_legacy_self_host_without_deployment_or_adoption() {
    let state = State::legacy(Fault::None);
    let original_namespace = state.namespace.clone();
    let mut expected_sandbox = state.sandbox.clone();
    let mut observed = (*state.observed()).clone();
    observed.metadata.resource_version = Some("stale-cache-version".into());
    let (server, client, state) = setup(state).await;
    let result = reconcile(Arc::new(observed), context(client))
        .await
        .unwrap();
    assert_eq!(result, Action::await_change());
    let requests = server.received_requests().await.unwrap();
    only_finalizer_io(&requests);
    assert_eq!(requests.len(), 3);
    let patch: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(
        patch["metadata"]["uid"],
        expected_sandbox["metadata"]["uid"]
    );
    assert_eq!(
        patch["metadata"]["resourceVersion"],
        expected_sandbox["metadata"]["resourceVersion"]
    );
    expected_sandbox["metadata"]["finalizers"] = json!([OTHER_FINALIZER]);
    expected_sandbox["metadata"]["resourceVersion"] = json!("22");
    let state = state.lock().unwrap();
    assert_eq!(state.namespace, original_namespace);
    assert_eq!(state.sandbox, expected_sandbox);
}

#[tokio::test]
async fn reconciler_entry_cancels_prestage_when_deletion_races_binding_after_finalizer_creation() {
    let mut state = State::prestaged(Fault::BeginDeletionOnNamespacePatch);
    state.sandbox["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("deletionTimestamp");
    state.namespace["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("deletionTimestamp");
    state.namespace["status"]["phase"] = json!("Active");
    state.sandbox["metadata"]["finalizers"] = json!([OTHER_FINALIZER]);
    let observed = state.observed();
    let namespace_uid = state.namespace["metadata"]["uid"].clone();
    let sandbox_uid = state.sandbox["metadata"]["uid"].clone();
    let (server, client, state) = setup(state).await;
    let ctx = context(client);

    let error = reconcile(observed.clone(), ctx.clone()).await.unwrap_err();
    assert_ne!(
        error_policy(observed, &error, ctx.clone()),
        Action::await_change()
    );
    let deleting = {
        let state = state.lock().unwrap();
        assert!(
            state.sandbox["metadata"]["finalizers"]
                .as_array()
                .unwrap()
                .contains(&json!(FINALIZER))
        );
        assert!(state.sandbox["metadata"]["deletionTimestamp"].is_string());
        assert!(state.namespace["metadata"]["deletionTimestamp"].is_string());
        assert_eq!(
            state.namespace["metadata"]["annotations"][PRESTAGE],
            "bind-next-sandbox"
        );
        assert!(state.namespace["metadata"]["annotations"][SOURCE_UID].is_null());
        state.observed()
    };
    assert_eq!(
        reconcile(deleting, ctx).await.unwrap(),
        Action::await_change()
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(
            state.sandbox["metadata"]["finalizers"],
            json!([OTHER_FINALIZER])
        );
        assert_eq!(state.namespace["metadata"]["uid"], namespace_uid);
        assert_eq!(
            state.namespace["metadata"]["annotations"][SOURCE_UID],
            sandbox_uid
        );
        assert!(state.namespace["metadata"]["annotations"][PRESTAGE].is_null());
        assert_eq!(
            state.namespace["metadata"]["annotations"]["customer.example/setting"],
            "keep"
        );
    }
    let requests = server.received_requests().await.unwrap();
    assert!(requests.iter().all(|request| {
        !(request.url.path().contains("/deployments/")
            || request.method == "DELETE" && request.url.path() == NS_PATH)
    }));
}

#[tokio::test]
async fn reconciler_entry_retries_non404_and_cas_errors_without_losing_other_finalizers() {
    for fault in [
        Fault::SourceGet(403),
        Fault::NamespaceGet(500),
        Fault::Patch(403),
        Fault::Patch(500),
        Fault::Patch(409),
    ] {
        let state = State::legacy(fault);
        let observed = state.observed();
        let original_namespace = state.namespace.clone();
        let (server, client, state) = setup(state).await;
        let ctx = context(client);
        let error = reconcile(observed.clone(), ctx.clone()).await.unwrap_err();
        assert_ne!(
            error_policy(observed.clone(), &error, ctx.clone()),
            Action::await_change()
        );
        assert!(
            state.lock().unwrap().sandbox["metadata"]["finalizers"]
                .as_array()
                .unwrap()
                .contains(&json!(FINALIZER))
        );
        assert_eq!(
            reconcile(observed, ctx).await.unwrap(),
            Action::await_change()
        );
        let requests = server.received_requests().await.unwrap();
        only_finalizer_io(&requests);
        let state = state.lock().unwrap();
        assert_eq!(state.namespace, original_namespace);
        let expected = if matches!(fault, Fault::Patch(409)) {
            json!([OTHER_FINALIZER, CONCURRENT_FINALIZER])
        } else {
            json!([OTHER_FINALIZER])
        };
        assert_eq!(state.sandbox["metadata"]["finalizers"], expected);
    }
}

#[tokio::test]
async fn reconciler_entry_rejects_recreated_cr_before_read_and_between_read_and_patch() {
    for during_patch in [false, true] {
        let mut state = State::legacy(if during_patch {
            Fault::RecreateOnPatch
        } else {
            Fault::None
        });
        let observed = state.observed();
        if !during_patch {
            state.sandbox["metadata"]["uid"] = json!("recreated-sandbox");
        }
        let original_namespace = state.namespace.clone();
        let (server, client, state) = setup(state).await;
        let ctx = context(client);
        assert!(reconcile(observed.clone(), ctx.clone()).await.is_err());
        assert!(reconcile(observed, ctx).await.is_err());
        only_finalizer_io(&server.received_requests().await.unwrap());
        let state = state.lock().unwrap();
        assert_eq!(state.namespace, original_namespace);
        assert_eq!(state.sandbox["metadata"]["uid"], "recreated-sandbox");
        assert_eq!(
            state.sandbox["metadata"]["finalizers"],
            json!([FINALIZER, OTHER_FINALIZER])
        );
    }
}

#[tokio::test]
async fn reconciler_entry_rejects_foreign_claims_ownerrefs_and_namespace_uid_replacement() {
    for variant in [
        "foreign-workspace",
        "foreign-uid",
        "partial-claim",
        "owner-ref",
        "namespace-uid",
        "prestage-foreign-workspace",
        "prestage-namespace-uid",
        "prestage-missing-backlink",
    ] {
        let mut state = if variant.starts_with("prestage-") {
            State::prestaged(Fault::None)
        } else {
            State::legacy(Fault::None)
        };
        match variant {
            "foreign-workspace" | "foreign-uid" => {
                let sandbox = state.observed();
                let mut annotations = owner_annotations(&sandbox);
                annotations[if variant == "foreign-workspace" {
                    SOURCE_NAMESPACE
                } else {
                    SOURCE_UID
                }] = json!("foreign");
                state.namespace["metadata"]["annotations"] = annotations;
            }
            "partial-claim" => state.namespace["metadata"]["annotations"][VERSION] = json!("v1"),
            "prestage-foreign-workspace" => {
                state.namespace["metadata"]["annotations"][SOURCE_NAMESPACE] =
                    json!("other-workspace");
            }
            "prestage-missing-backlink" => {
                state.sandbox["metadata"]["annotations"]
                    .as_object_mut()
                    .unwrap()
                    .remove(NAMESPACE_UID);
            }
            "owner-ref" => {
                state.namespace["metadata"]["ownerReferences"] = json!([{
                    "apiVersion": "v1", "kind": "Namespace", "name": "foreign", "uid": "foreign",
                }])
            }
            _ => {
                state.sandbox["metadata"]["annotations"][NAMESPACE_UID] =
                    json!("previous-namespace")
            }
        }
        let observed = state.observed();
        let original_source = state.sandbox.clone();
        let original_namespace = state.namespace.clone();
        let (server, client, state) = setup(state).await;
        assert!(
            reconcile(observed, context(client)).await.is_err(),
            "{variant}"
        );
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests.iter().all(|request| request.method == "GET"),
            "{variant}"
        );
        only_finalizer_io(&requests);
        let state = state.lock().unwrap();
        assert_eq!(state.sandbox, original_source, "{variant}");
        assert_eq!(state.namespace, original_namespace, "{variant}");
    }
}

#[tokio::test]
async fn namespace_gc_path_never_handles_foreign_namespace_active_or_already_claimed_objects() {
    for variant in [
        "foreign-namespace",
        "active-source",
        "active-namespace",
        "claimed",
    ] {
        let mut state = State::legacy(Fault::None);
        match variant {
            "foreign-namespace" => state.sandbox["metadata"]["namespace"] = json!("workspace-a"),
            "active-source" => {
                let _ = state.sandbox["metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove("deletionTimestamp");
            }
            "active-namespace" => {
                let _ = state.namespace["metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove("deletionTimestamp");
            }
            _ => state.namespace["metadata"]["annotations"] = owner_annotations(&state.observed()),
        }
        let observed = state.observed();
        let original_source = state.sandbox.clone();
        let original_namespace = state.namespace.clone();
        let (server, client, state) = setup(state).await;
        assert!(
            !finalize_legacy_namespace_gc(&client, &observed)
                .await
                .unwrap(),
            "{variant}"
        );
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests.iter().all(|request| request.method == "GET"),
            "{variant}"
        );
        only_finalizer_io(&requests);
        let state = state.lock().unwrap();
        assert_eq!(state.sandbox, original_source, "{variant}");
        assert_eq!(state.namespace, original_namespace, "{variant}");
    }
}

#[tokio::test]
async fn reconciler_entry_does_not_rewrite_a_cr_whose_own_finalizer_is_already_gone() {
    let mut state = State::legacy(Fault::None);
    state.sandbox["metadata"]["finalizers"] = json!([OTHER_FINALIZER]);
    let observed = state.observed();
    let original_source = state.sandbox.clone();
    let (server, client, state) = setup(state).await;
    assert_eq!(
        reconcile(observed, context(client)).await.unwrap(),
        Action::await_change()
    );
    let requests = server.received_requests().await.unwrap();
    assert!(requests.iter().all(|request| request.method == "GET"));
    only_finalizer_io(&requests);
    assert_eq!(state.lock().unwrap().sandbox, original_source);
}
