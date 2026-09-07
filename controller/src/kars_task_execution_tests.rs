// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OBJECT_PATH: &str = "/apis/kars.azure.com/v1alpha1/namespaces/default/karssandboxes/demo";
const COLLECTION_PATH: &str = "/apis/kars.azure.com/v1alpha1/namespaces/default/karssandboxes";

fn task() -> KarsTask {
    let mut task = KarsTask::new("demo", Default::default());
    task.metadata.uid = Some("task-uid".into());
    task
}

fn object(task: &KarsTask) -> DynamicObject {
    serde_json::from_value(json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSandbox",
        "metadata": {
            "name": "demo", "namespace": "default",
            "uid": "sandbox-uid", "resourceVersion": "42",
            "ownerReferences": owner_ref(task),
        },
        "spec": { "oldField": "remove-me" },
        "status": { "phase": "Running" },
    }))
    .unwrap()
}

fn client(server: &MockServer) -> Client {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap()
}

fn api_error(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({
        "apiVersion": "v1", "kind": "Status", "status": "Failure",
        "message": "test API failure", "reason": "Failure", "code": code,
    }))
}

async fn apply(server: &MockServer) -> Result<(), kube::Error> {
    apply_dynamic(
        &client(server),
        "default",
        &sandbox_api_resource(),
        "demo",
        &task(),
        json!({ "networkPolicy": { "egressMode": "Strict" } }),
        None,
    )
    .await
}

#[tokio::test]
async fn unowned_and_previous_task_uid_objects_are_never_modified_or_deleted() {
    for old_uid in [None, Some("previous-task-uid")] {
        let server = MockServer::start().await;
        let mut existing = object(&task());
        if let Some(uid) = old_uid {
            existing.metadata.owner_references.as_mut().unwrap()[0].uid = uid.into();
        } else {
            existing.metadata.owner_references = None;
        }
        Mock::given(method("GET"))
            .and(path(OBJECT_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(&existing))
            .mount(&server)
            .await;
        assert!(apply(&server).await.is_err());
        let api = Api::namespaced_with(client(&server), "default", &sandbox_api_resource());
        assert!(delete_owned(&api, "demo", &task()).await.unwrap());
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method == "GET")
        );
    }
}

#[tokio::test]
async fn creation_collision_is_not_retried_as_an_adoption() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(api_error(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(COLLECTION_PATH))
        .respond_with(api_error(409))
        .expect(1)
        .mount(&server)
        .await;
    assert!(matches!(apply(&server).await, Err(kube::Error::Api(e)) if e.code == 409));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn owned_update_has_uid_and_version_and_does_not_force_apply() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(object(&task())))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .respond_with(api_error(409))
        .expect(1)
        .mount(&server)
        .await;
    assert!(matches!(apply(&server).await, Err(kube::Error::Api(e)) if e.code == 409));
    let requests = server.received_requests().await.unwrap();
    let update: serde_json::Value = requests[1].body_json().unwrap();
    assert_eq!(update["metadata"]["uid"], "sandbox-uid");
    assert_eq!(update["metadata"]["resourceVersion"], "42");
    assert_eq!(update["status"]["phase"], "Running");
    assert!(update["spec"].get("oldField").is_none());
    assert!(!requests[1].url.query().unwrap_or("").contains("force"));
}

#[tokio::test]
async fn delete_failures_propagate_with_race_preconditions() {
    for code in [403, 409, 500] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(OBJECT_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(object(&task())))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(OBJECT_PATH))
            .respond_with(api_error(code))
            .mount(&server)
            .await;
        let api = Api::namespaced_with(client(&server), "default", &sandbox_api_resource());
        assert!(
            matches!(delete_owned(&api, "demo", &task()).await, Err(kube::Error::Api(e)) if e.code == code)
        );
        let requests = server.received_requests().await.unwrap();
        let deletion: serde_json::Value = requests[1].body_json().unwrap();
        assert_eq!(deletion["preconditions"]["uid"], "sandbox-uid");
        assert_eq!(deletion["preconditions"]["resourceVersion"], "42");
    }
}

#[tokio::test]
async fn teardown_discovers_resources_without_task_status_and_waits_for_finalizers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(object(&task())))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(object(&task())))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/default/inferencepolicies/demo-inference",
        ))
        .respond_with(api_error(404))
        .mount(&server)
        .await;
    assert!(task().status.is_none());
    assert!(
        !teardown(&client(&server), "default", &task())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn deleting_an_already_absent_object_is_successful() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(api_error(404))
        .mount(&server)
        .await;
    let api = Api::namespaced_with(client(&server), "default", &sandbox_api_resource());
    assert!(delete_owned(&api, "demo", &task()).await.unwrap());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn materialized_resources_match_the_authorization_blueprint() {
    use crate::kars_task::{TaskEgress, TaskModel};
    let server = MockServer::start().await;
    let mut task = task();
    task.spec.objective = "Review the patch".into();
    task.spec.blueprint = Some(TaskBlueprint {
        runtime: Some("MAF".into()),
        model: Some(TaskModel {
            deployment: "reviewed-model".into(),
            provider: String::new(),
        }),
        instructions: Some(" Cite evidence. ".into()),
        tool_policy: Some("read-only".into()),
        mcp_servers: vec!["docs".into()],
        memory: Some(" team-memory ".into()),
        isolation: Some("enhanced".into()),
        egress: vec![TaskEgress {
            host: "docs.example.com".into(),
            port: Some(443),
        }],
    });
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(api_error(404))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(object(&task)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/default/inferencepolicies/demo-inference",
        ))
        .respond_with(api_error(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(|request: &wiremock::Request| {
            let mut resource: serde_json::Value = request.body_json().unwrap();
            resource["metadata"]["uid"] = json!("created-resource");
            resource["metadata"]["resourceVersion"] = json!("1");
            ResponseTemplate::new(201).set_body_json(resource)
        })
        .expect(2)
        .mount(&server)
        .await;
    let effective = crate::kars_task::blueprint::effective_blueprint(&task.spec);
    let outcome = materialize(&client(&server), "default", &task)
        .await
        .unwrap();
    assert_eq!(outcome.phase, "Running");
    let requests = server.received_requests().await.unwrap();
    let specs: Vec<serde_json::Value> = requests
        .iter()
        .filter(|r| r.method == "POST")
        .map(|r| r.body_json::<serde_json::Value>().unwrap()["spec"].clone())
        .collect();
    assert_eq!(
        specs[0]["modelPreference"]["primary"],
        json!(effective.model)
    );
    assert_eq!(specs[1]["runtime"]["kind"], "MicrosoftAgentFramework");
    assert_eq!(specs[1]["sandbox"]["isolation"], json!(effective.isolation));
    assert_eq!(
        specs[1]["agent"]["instructions"],
        json!(effective.instructions)
    );
    assert_eq!(
        specs[1]["networkPolicy"]["allowedEndpoints"],
        json!(effective.egress)
    );
    assert_eq!(specs[1]["networkPolicy"]["egressMode"], "Strict");
    assert_eq!(
        specs[1]["governance"]["toolPolicyRef"]["name"],
        json!(effective.tool_policy)
    );
    assert_eq!(specs[1]["governance"]["mcpServerRefs"][0]["name"], "docs");
    assert_eq!(specs[1]["memoryRef"]["name"], json!(effective.memory));
}
