// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::crd::KarsSandboxSpec;

pub const SOURCE_PATH: &str = "/api/v1/namespaces/workspace-a/secrets/kars-credential-source-demo";
pub const TARGETS_PATH: &str = "/api/v1/namespaces/kars-demo/secrets";
pub const TARGET_PATH: &str = "/api/v1/namespaces/kars-demo/secrets/demo-credential-projection";
pub const NS_PATH: &str = "/api/v1/namespaces/kars-demo";
const SANDBOX_PATH: &str =
    "/apis/kars.azure.com/v1alpha1/namespaces/workspace-a/karssandboxes/demo";
const DEPLOYMENT_PATH: &str = "/apis/apps/v1/namespaces/kars-demo/deployments/demo";

pub fn sandbox() -> KarsSandbox {
    let mut sandbox = KarsSandbox::new("demo", KarsSandboxSpec::default());
    sandbox.metadata.namespace = Some("workspace-a".into());
    sandbox.metadata.uid = Some("sandbox-a".into());
    sandbox.metadata.resource_version = Some("10".into());
    sandbox.metadata.generation = Some(1);
    sandbox.spec.credentials_ref = Some(CredentialSourceRef {
        name: source_name("demo"),
        uid: "source-a".into(),
    });
    sandbox.annotations_mut().insert(
        super::super::super::namespace_ownership::NAMESPACE_UID.into(),
        "namespace-a".into(),
    );
    sandbox
}

pub fn namespace() -> Namespace {
    use crate::reconciler::namespace_ownership as ownership;
    serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Namespace",
        "metadata": {
            "name": "kars-demo", "uid": "namespace-a", "resourceVersion": "20",
            "annotations": {
                (ownership::VERSION): "v1", (ownership::SOURCE_NAME): "demo",
                (ownership::SOURCE_NAMESPACE): "workspace-a", (ownership::SOURCE_UID): "sandbox-a"
            }
        }
    }))
    .unwrap()
}

pub fn source() -> Secret {
    serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
        "metadata": {
            "name": source_name("demo"), "namespace": "workspace-a",
            "uid": "source-a", "resourceVersion": "30",
            "annotations": {
                PURPOSE: SOURCE_PURPOSE, TARGET: "demo", WORKSPACE: "workspace-a", INTENT: BINDING_INTENT
            }
        },
        "data": {"TELEGRAM_BOT_TOKEN": ByteString(b"initial".to_vec())}
    })).unwrap()
}

pub fn deployment() -> Deployment {
    serde_json::from_value(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {
            "name": "demo", "namespace": "kars-demo", "uid": "deployment-a", "resourceVersion": "40",
            "labels": {
                "kars.azure.com/sandbox": "demo", "kars.azure.com/component": "sandbox",
                "kars.azure.com/parent-namespace": "workspace-a"
            },
            "managedFields": [{
                "manager": crate::field_managers::CLAWSANDBOX, "operation": "Apply", "apiVersion": "apps/v1",
                "fieldsType": "FieldsV1", "fieldsV1": {"f:spec": {}}
            }]
        },
        "spec": {
            "replicas": 1, "selector": {"matchLabels": {"kars.azure.com/sandbox": "demo"}},
            "template": {"metadata": {}, "spec": {"containers": [{
                "name": "openclaw", "image": "agent:latest",
                "envFrom": [{"secretRef": {"name": "demo-credentials", "optional": true}}]
            }]}}
        }
    })).unwrap()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    ReplaceNamespaceAfterAnchor,
    ReplaceProjectionOnWrite,
    ChangeSourceAfterAnchor,
    FailValueWrite,
    CreateConflict,
    SourceReadForbidden,
    FailSealOnce,
}

pub struct State {
    pub sandbox: KarsSandbox,
    pub namespace: Namespace,
    pub source: Option<Secret>,
    pub projection: Option<Secret>,
    pub deployment: Option<Deployment>,
    pub fault: Fault,
    pub successful_value_writes: usize,
    revision: usize,
}

impl State {
    pub fn new() -> Self {
        Self {
            sandbox: sandbox(),
            namespace: namespace(),
            source: Some(source()),
            projection: None,
            deployment: Some(deployment()),
            fault: Fault::None,
            successful_value_writes: 0,
            revision: 100,
        }
    }
}

fn response(code: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(body)
}

fn failure(code: u16) -> ResponseTemplate {
    response(
        code,
        json!({
            "apiVersion": "v1", "kind": "Status", "status": "Failure", "reason": "Failure", "code": code,
            "message": "admission body contains secret-never-log-this"
        }),
    )
}

fn output(request: &Request, value: Value) -> ResponseTemplate {
    if request
        .headers
        .get("accept")
        .is_some_and(|header| header.to_str().unwrap().contains("PartialObjectMetadata"))
    {
        response(
            200,
            json!({"apiVersion": "meta.k8s.io/v1", "kind": "PartialObjectMetadata", "metadata": value["metadata"]}),
        )
    } else {
        response(200, value)
    }
}

fn merge(target: &mut Value, patch: Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = json!({});
        }
        for (key, value) in patch {
            if value.is_null() {
                target.as_object_mut().unwrap().remove(&key);
            } else {
                merge(
                    target
                        .as_object_mut()
                        .unwrap()
                        .entry(key)
                        .or_insert(Value::Null),
                    value,
                );
            }
        }
    } else {
        *target = patch;
    }
}

pub fn nonempty_value_write(request: &Request) -> bool {
    if request.method != "PATCH" || request.url.path() != TARGET_PATH {
        return false;
    }
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    body.get("data")
        .and_then(Value::as_object)
        .is_some_and(|values| values.values().any(|value| !value.is_null()))
}

#[derive(Clone)]
struct Server(Arc<Mutex<State>>);

impl Respond for Server {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.0.lock().unwrap();
        let path = request.url.path();
        if request.method == "GET" {
            let value = match path {
                SANDBOX_PATH => Some(serde_json::to_value(&state.sandbox).unwrap()),
                NS_PATH => Some(serde_json::to_value(&state.namespace).unwrap()),
                SOURCE_PATH => {
                    if state.fault == Fault::SourceReadForbidden {
                        return failure(403);
                    }
                    state
                        .source
                        .as_ref()
                        .map(|value| serde_json::to_value(value).unwrap())
                }
                TARGET_PATH => state
                    .projection
                    .as_ref()
                    .map(|value| serde_json::to_value(value).unwrap()),
                DEPLOYMENT_PATH => state
                    .deployment
                    .as_ref()
                    .map(|value| serde_json::to_value(value).unwrap()),
                _ => None,
            };
            return value.map_or_else(|| failure(404), |value| output(request, value));
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        if request.method == "POST" && path == TARGETS_PATH {
            if state.fault == Fault::CreateConflict || state.projection.is_some() {
                return failure(409);
            }
            assert!(body.get("data").is_none() && body.get("stringData").is_none());
            let mut value = body;
            value["metadata"]["uid"] = json!("projection-uid");
            value["metadata"]["resourceVersion"] = json!("100");
            value["metadata"]["managedFields"] = json!([{
                "manager": crate::field_managers::CLAWSANDBOX, "operation": "Update",
                "apiVersion": "v1", "fieldsType": "FieldsV1", "fieldsV1": {"f:metadata": {}}
            }]);
            state.projection = Some(serde_json::from_value(value.clone()).unwrap());
            match state.fault {
                Fault::ReplaceNamespaceAfterAnchor => {
                    state.namespace.metadata.uid = Some("replacement".into())
                }
                Fault::ChangeSourceAfterAnchor => {
                    state.source.as_mut().unwrap().metadata.resource_version =
                        Some("changed".into())
                }
                _ => {}
            }
            return output(request, value);
        }
        if request.method == "DELETE" && path == TARGET_PATH {
            let Some(existing) = state.projection.as_ref() else {
                return failure(404);
            };
            if body["preconditions"]["uid"] != json!(existing.metadata.uid)
                || body["preconditions"]["resourceVersion"]
                    != json!(existing.metadata.resource_version)
            {
                return failure(409);
            }
            return response(
                200,
                serde_json::to_value(state.projection.take().unwrap()).unwrap(),
            );
        }
        if request.method == "PATCH" {
            let value_write = nonempty_value_write(request);
            if path == TARGET_PATH
                && body["metadata"]["annotations"]
                    .get(PROJECTION_UID)
                    .is_some()
                && state.fault == Fault::FailSealOnce
            {
                state.fault = Fault::None;
                return failure(500);
            }
            if value_write && state.fault == Fault::ReplaceProjectionOnWrite {
                state.projection.as_mut().unwrap().metadata.uid = Some("foreign-projection".into());
                state.projection.as_mut().unwrap().metadata.annotations = None;
            }
            if value_write && state.fault == Fault::FailValueWrite {
                return failure(409);
            }
            let mut existing = match path {
                SOURCE_PATH => state
                    .source
                    .as_ref()
                    .map(|value| serde_json::to_value(value).unwrap()),
                TARGET_PATH => state
                    .projection
                    .as_ref()
                    .map(|value| serde_json::to_value(value).unwrap()),
                DEPLOYMENT_PATH => state
                    .deployment
                    .as_ref()
                    .map(|value| serde_json::to_value(value).unwrap()),
                _ if path == format!("{SANDBOX_PATH}/status") => {
                    Some(serde_json::to_value(&state.sandbox).unwrap())
                }
                _ => None,
            };
            let Some(ref mut value) = existing else {
                return failure(404);
            };
            if body["metadata"]["uid"] != value["metadata"]["uid"]
                || body["metadata"]["resourceVersion"] != value["metadata"]["resourceVersion"]
            {
                return failure(409);
            }
            merge(value, body);
            state.revision += 1;
            value["metadata"]["resourceVersion"] = json!(state.revision.to_string());
            match path {
                SOURCE_PATH => state.source = Some(serde_json::from_value(value.clone()).unwrap()),
                TARGET_PATH => {
                    if value_write {
                        state.successful_value_writes += 1;
                    }
                    state.projection = Some(serde_json::from_value(value.clone()).unwrap());
                }
                DEPLOYMENT_PATH => {
                    state.deployment = Some(serde_json::from_value(value.clone()).unwrap())
                }
                _ => state.sandbox = serde_json::from_value(value.clone()).unwrap(),
            }
            return output(request, value.clone());
        }
        failure(500)
    }
}

pub async fn setup(state: State) -> (MockServer, Client, Arc<Mutex<State>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(state));
    Mock::given(wiremock::matchers::any())
        .respond_with(Server(state.clone()))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state)
}

pub fn resume(state: &Arc<Mutex<State>>, mode: &Mode) {
    let mut state = state.lock().unwrap();
    let sandbox = state.sandbox.clone();
    let ns = state.namespace.clone();
    let deployment = state.deployment.as_mut().unwrap();
    deployment.spec.as_mut().unwrap().replicas = Some(1);
    mode.decorate(deployment, &sandbox, &ns);
}
