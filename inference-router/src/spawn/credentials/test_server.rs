// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

pub(super) const NS_PATH: &str = "/api/v1/namespaces/kars-demo";
pub(super) const SECRET_PATH: &str = "/api/v1/namespaces/kars-demo/secrets/demo-credentials";
pub(super) const SENTINEL: &str = "sensitive-handoff-value";

#[derive(Clone, Copy, Default)]
pub(super) enum Scenario {
    #[default]
    Stable,
    ConvergeMissing,
    ConvergePartial,
    ReplaceNamespaceWhileWaiting,
    ReplaceSandboxWhileWaiting,
    ReplaceNamespaceAtAnchor,
    ReplaceSandboxAtAnchor,
    TerminateNamespaceAtAnchor,
    TerminateSandboxAtAnchor,
    RemoveClaimAtAnchor,
    ReplaceSecretAtWrite,
    ReplaceNamespaceAtWrite,
    ConflictAtWrite,
    SandboxError(u16),
    NamespaceError(u16),
    AnchorError(u16),
    WriteError(u16),
}

pub(super) struct State {
    pub sandbox: Option<Value>,
    pub namespace: Option<Value>,
    pub secret: Option<Value>,
    pub scenario: Scenario,
    pub requests: Vec<Request>,
    pub namespace_reads: usize,
    pub value_attempts: usize,
    pub value_writes: usize,
}

pub(super) fn sandbox() -> Value {
    json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
        "metadata": {
            "name":"demo", "namespace":"workspace-a", "uid":"created-uid", "resourceVersion":"1",
            "annotations": { NAMESPACE_UID: "namespace-uid" },
        },
        "spec": {},
    })
}

pub(super) fn namespace() -> Value {
    json!({
        "apiVersion":"v1", "kind":"Namespace",
        "metadata": {
            "name":"kars-demo", "uid":"namespace-uid", "resourceVersion":"1",
            "annotations": {
                VERSION:"v1", SOURCE_NAMESPACE:"workspace-a", SOURCE_NAME:"demo", SOURCE_UID:"created-uid",
            },
        },
        "status": { "phase":"Active" },
    })
}

fn secret() -> Value {
    json!({
        "apiVersion":"v1", "kind":"Secret", "type":"Opaque",
        "metadata": {
            "name":"demo-credentials", "namespace":"kars-demo", "uid":"secret-uid", "resourceVersion":"1",
            "labels": {"existing":"preserve"},
        },
        "data": { "EXISTING":"preserve" },
    })
}

impl Default for State {
    fn default() -> Self {
        Self {
            sandbox: Some(sandbox()),
            namespace: Some(namespace()),
            secret: None,
            scenario: Scenario::Stable,
            requests: Vec::new(),
            namespace_reads: 0,
            value_attempts: 0,
            value_writes: 0,
        }
    }
}

fn api_error(code: u16) -> ResponseTemplate {
    let reason = match code {
        403 => "Forbidden",
        404 => "NotFound",
        409 => "Conflict",
        422 => "Invalid",
        500 => "InternalError",
        503 => "ServiceUnavailable",
        _ => "Failure",
    };
    ResponseTemplate::new(code).set_body_json(json!({
        "apiVersion":"v1", "kind":"Status", "status":"Failure", "code":code,
        "reason":reason, "message":SENTINEL,
    }))
}

fn metadata_response(object: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "apiVersion":"meta.k8s.io/v1", "kind":"PartialObjectMetadata", "metadata":object["metadata"],
    }))
}

fn replace_namespace(state: &mut State) {
    state.namespace = Some(namespace());
    state.namespace.as_mut().unwrap()["metadata"]["uid"] = "replacement-namespace".into();
    if let Some(sandbox) = &mut state.sandbox {
        sandbox["metadata"]["annotations"][NAMESPACE_UID] = "replacement-namespace".into();
    }
    state.secret = None;
}

impl State {
    pub fn with_existing_secret(mut self) -> Self {
        self.secret = Some(secret());
        self
    }

    fn respond(&mut self, request: &Request) -> ResponseTemplate {
        self.requests.push(request.clone());
        let path = request.url.path();
        if request.method == "GET" && path.starts_with("/apis/kars.azure.com/") {
            if let Scenario::SandboxError(code) = self.scenario {
                return api_error(code);
            }
            let Some(sandbox) = &self.sandbox else {
                return api_error(404);
            };
            let workspace = sandbox["metadata"]["namespace"].as_str().unwrap();
            if path
                != format!(
                    "/apis/kars.azure.com/v1alpha1/namespaces/{workspace}/karssandboxes/demo"
                )
            {
                return api_error(404);
            }
            assert!(
                request.headers["accept"]
                    .to_str()
                    .unwrap()
                    .contains("PartialObjectMetadata")
            );
            return metadata_response(sandbox);
        }
        if request.method == "GET" && path == NS_PATH {
            self.namespace_reads += 1;
            if let Scenario::NamespaceError(code) = self.scenario {
                return api_error(code);
            }
            let reply = self.namespace.as_ref().map_or_else(
                || api_error(404),
                |namespace| ResponseTemplate::new(200).set_body_json(namespace),
            );
            if self.namespace_reads == 1 {
                match self.scenario {
                    Scenario::ConvergeMissing | Scenario::ConvergePartial => {
                        self.namespace = Some(namespace());
                        self.sandbox = Some(sandbox());
                    }
                    Scenario::ReplaceNamespaceWhileWaiting => replace_namespace(self),
                    Scenario::ReplaceSandboxWhileWaiting => {
                        self.sandbox = Some(sandbox());
                        self.sandbox.as_mut().unwrap()["metadata"]["uid"] =
                            "recreated-sandbox".into();
                        self.namespace = Some(namespace());
                        self.namespace.as_mut().unwrap()["metadata"]["annotations"][SOURCE_UID] =
                            "recreated-sandbox".into();
                    }
                    _ => {}
                }
            }
            return reply;
        }
        if request.method != "PATCH" || path != SECRET_PATH {
            return api_error(404);
        }
        assert!(
            request.headers["accept"]
                .to_str()
                .unwrap()
                .contains("PartialObjectMetadata")
        );
        assert!(
            !request
                .url
                .query()
                .unwrap_or_default()
                .contains("force=true")
        );
        let body: Value = request.body_json().unwrap();
        if body.get("stringData").is_none() {
            assert!(body.get("data").is_none());
            assert!(
                request
                    .url
                    .query()
                    .unwrap_or_default()
                    .contains("fieldManager=kars-handoff-credential-anchor")
            );
            if let Scenario::AnchorError(code) = self.scenario {
                return api_error(code);
            }
            match self.scenario {
                Scenario::ReplaceNamespaceAtAnchor => replace_namespace(self),
                Scenario::ReplaceSandboxAtAnchor => {
                    self.sandbox.as_mut().unwrap()["metadata"]["uid"] = "recreated-sandbox".into();
                }
                Scenario::TerminateNamespaceAtAnchor => {
                    self.namespace.as_mut().unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-09-07T00:00:00Z".into();
                }
                Scenario::TerminateSandboxAtAnchor => {
                    self.sandbox.as_mut().unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-09-07T00:00:00Z".into();
                }
                Scenario::RemoveClaimAtAnchor => {
                    self.namespace.as_mut().unwrap()["metadata"]["annotations"] = json!({});
                }
                _ => {}
            }
            let secret = self.secret.get_or_insert_with(|| {
                let mut created = secret();
                created["data"] = json!({});
                created["metadata"]["labels"] = json!({});
                created
            });
            let version = secret["metadata"]["resourceVersion"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                + 1;
            secret["metadata"]["resourceVersion"] = version.to_string().into();
            return metadata_response(secret);
        }
        self.value_attempts += 1;
        assert_eq!(
            request.headers["content-type"],
            "application/merge-patch+json"
        );
        assert!(body["metadata"]["uid"].is_string());
        assert!(body["metadata"]["resourceVersion"].is_string());
        match self.scenario {
            Scenario::WriteError(code) => return api_error(code),
            Scenario::ReplaceNamespaceAtWrite => replace_namespace(self),
            Scenario::ReplaceSecretAtWrite => {
                self.secret = Some(secret());
                self.secret.as_mut().unwrap()["metadata"]["uid"] = "replacement-secret".into();
            }
            Scenario::ConflictAtWrite => {
                self.secret.as_mut().unwrap()["metadata"]["resourceVersion"] = "changed".into();
            }
            _ => {}
        }
        let Some(secret) = &mut self.secret else {
            return api_error(404);
        };
        if secret["metadata"]["uid"] != body["metadata"]["uid"]
            || secret["metadata"]["resourceVersion"] != body["metadata"]["resourceVersion"]
        {
            return api_error(409);
        }
        for (key, value) in body["stringData"].as_object().unwrap() {
            secret["data"][key] = value.clone();
        }
        for (key, value) in body["metadata"]["labels"].as_object().unwrap() {
            secret["metadata"]["labels"][key] = value.clone();
        }
        self.value_writes += 1;
        metadata_response(secret)
    }
}

pub(super) async fn start(state: State) -> (MockServer, Client, Arc<Mutex<State>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(state));
    let handler = state.clone();
    Mock::given(|_: &Request| true)
        .respond_with(move |request: &Request| handler.lock().unwrap().respond(request))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state)
}
