// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget_contract::{
    AccountReference, BudgetScope, Limits, ResourceIdentity, RootIdentity, RootKind, TaskAuthority,
    TaskBudgetBinding,
    ledger::Ledger,
    tariffs::{MaximumPrice, ModelContract, OutputField},
};
use crate::{auth::WorkloadIdentityAuth, provider::ProviderKind, proxy::UpstreamConfig};
use axum::http::{HeaderMap, Method};
use futures::TryStreamExt;
use serde_json::json;
use std::sync::{Mutex as StdMutex, atomic::AtomicUsize};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn finite_fence_cannot_be_bypassed_by_disabling_blocklist_or_enabling_learning() {
    let fixture = Fixture::new(100, false).await;
    let blocklist = crate::blocklist::Blocklist::disabled();
    assert!(blocklist.opaque_proxy_allowed());
    blocklist
        .bind_inference_budget(fixture.client.clone(), vec![])
        .unwrap();
    assert!(!blocklist.opaque_proxy_allowed());
    blocklist.set_learn_mode(true);
    assert!(!blocklist.opaque_proxy_allowed());
    assert!(
        blocklist
            .check_egress("unknown-model.example", "forged-agent-header")
            .await
            .is_err()
    );
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
}

#[derive(Clone)]
struct BrokerFixture {
    ledger: Arc<StdMutex<Ledger>>,
    catalog: Catalog,
    lose_begin_ack: bool,
}

impl Respond for BrokerFixture {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer private-test-token"
        );
        let now = chrono::Utc::now().timestamp();
        let mut ledger = self.ledger.lock().unwrap();
        let result = match request.url.path() {
            "/v1/catalog" => Ok(json!({"catalog":self.catalog,"moneyRequired":true})),
            "/v1/session" => {
                let request: BrokerRequest<SessionRequest> =
                    serde_json::from_slice(&request.body).unwrap();
                ledger
                    .register_session(request.payload.identity)
                    .map(|change| {
                        *ledger = change.next;
                        json!(change.value)
                    })
            }
            "/v1/reserve" => {
                let request: BrokerRequest<ReserveRequest> =
                    serde_json::from_slice(&request.body).unwrap();
                self.catalog
                    .accepts_quote(&request.payload.quote, now, true)
                    .unwrap();
                ledger.reserve(&request.payload, now).map(|change| {
                    *ledger = change.next;
                    json!(change.value)
                })
            }
            "/v1/begin" => {
                let request: BrokerRequest<AttemptCommand> =
                    serde_json::from_slice(&request.body).unwrap();
                ledger.begin_dispatch(&request.payload, now).map(|change| {
                    *ledger = change.next;
                    json!(change.value)
                })
            }
            "/v1/settle" => {
                let request: BrokerRequest<Settlement> =
                    serde_json::from_slice(&request.body).unwrap();
                ledger.settle(&request.payload).map(|change| {
                    *ledger = change.next;
                    json!(change.value)
                })
            }
            _ => panic!("unexpected broker route"),
        };
        if self.lose_begin_ack && request.url.path() == "/v1/begin" {
            return ResponseTemplate::new(503);
        }
        match result {
            Ok(response) => ResponseTemplate::new(200).set_body_json(response),
            Err(_) => ResponseTemplate::new(429),
        }
    }
}

struct Fixture {
    provider: MockServer,
    _broker: MockServer,
    client: Arc<Client>,
    ledger: Arc<StdMutex<Ledger>>,
    directory: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

impl Fixture {
    async fn new(limit: u64, lose_begin_ack: bool) -> Self {
        let provider = MockServer::start().await;
        let broker = MockServer::start().await;
        let task = ResourceIdentity {
            namespace: "workspace".into(),
            name: "task".into(),
            uid: "task-uid".into(),
        };
        let root = RootIdentity {
            kind: RootKind::KarsTask,
            resource: task.clone(),
            workspace_uid: "workspace-uid".into(),
            cluster_uid: "cluster-uid".into(),
        };
        let digest = format!("sha256:{}", "a".repeat(64));
        let authority = TaskAuthority {
            task,
            parent_uid: None,
            root_task_uid: "task-uid".into(),
            authorization_digest: digest.clone(),
            effective_authorization: json!({"test":true}),
            limits: Limits {
                tokens: Some(limit),
                usd_micros: Some(1000),
            },
        };
        let ledger = Ledger::new("account-uid".into(), root.clone(), authority.limits)
            .unwrap()
            .register_task(authority)
            .unwrap()
            .next;
        let ledger = Arc::new(StdMutex::new(ledger));
        let model = ModelContract {
            id: "model".into(),
            version: "v1".into(),
            valid_until: "2030-01-01T00:00:00Z".into(),
            provider_id: "ollama".into(),
            endpoint: provider.uri(),
            model: "model".into(),
            operation: Operation::ChatCompletions,
            output_field: OutputField::MaxTokens,
            maximum_input_tokens: 10,
            maximum_output_tokens: 20,
            maximum_wire_bytes: 4096,
            output_bound_includes_reasoning: true,
            maximum_price: Some(MaximumPrice::PerRequest { maximum_micros: 5 }),
        };
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(BrokerFixture {
                ledger: ledger.clone(),
                catalog: Catalog {
                    version: "v1".into(),
                    contracts: vec![model],
                    non_inference_egress_hosts: vec![],
                },
                lose_begin_ack,
            })
            .mount(&broker)
            .await;
        let identity = ExecutionIdentity {
            task_uid: "task-uid".into(),
            authorization_digest: digest.clone(),
            sandbox: ResourceIdentity {
                namespace: "workspace".into(),
                name: "task".into(),
                uid: "sandbox-uid".into(),
            },
            runtime_namespace_uid: "runtime-uid".into(),
            pod_name: "pod".into(),
            pod_uid: "pod-uid".into(),
        };
        let directory = std::env::current_dir().unwrap().join(format!(
            ".budget-client-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let token_path = directory.join("token");
        std::fs::write(&token_path, "private-test-token").unwrap();
        let client = Arc::new(Client {
            binding: RouterBinding {
                task: TaskBudgetBinding {
                    scope: BudgetScope::GovernedInference,
                    account: AccountReference {
                        namespace: "accounting".into(),
                        name: "account".into(),
                        uid: "account-uid".into(),
                    },
                    root,
                    task_uid: "task-uid".into(),
                    parent_task_uid: None,
                    root_task_uid: "task-uid".into(),
                    authorization_digest: digest,
                },
                sandbox: identity.sandbox.clone(),
                runtime_namespace: "kars-task".into(),
                runtime_namespace_uid: "runtime-uid".into(),
                privacy_epoch: None,
            },
            identity,
            endpoint: broker.uri(),
            token_path,
            http: reqwest::Client::new(),
            reservation_lock: Mutex::new(()),
        });
        Self {
            provider,
            _broker: broker,
            client,
            ledger,
            directory,
        }
    }

    fn upstream(&self) -> UpstreamConfig {
        let mut upstream =
            UpstreamConfig::azure(self.provider.uri(), "model".into(), "task".into());
        upstream.provider = ProviderKind::Ollama;
        upstream.inference_budget = Some(self.client.clone());
        upstream
    }

    async fn buffered(&self) -> anyhow::Result<(axum::http::StatusCode, HeaderMap, bytes::Bytes)> {
        crate::proxy::forward(
            &WorkloadIdentityAuth::new(),
            None,
            &reqwest::Client::new(),
            &self.upstream(),
            Method::POST,
            "chat/completions",
            &HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"messages":[{"role":"user","content":"text"}]}"#),
        )
        .await
    }
}

#[tokio::test]
async fn actual_buffered_send_injects_output_bound_and_settles_before_returning() {
    let fixture = Fixture::new(100, false).await;
    Mock::given(wiremock::matchers::path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "usage":{"prompt_tokens":3,"completion_tokens":5,"total_tokens":8}
        })))
        .mount(&fixture.provider)
        .await;
    assert!(fixture.buffered().await.unwrap().0.is_success());
    let sent = fixture.provider.received_requests().await.unwrap();
    assert_eq!(sent.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(body["max_tokens"], 20);
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.reserved.tokens, 0);
    assert_eq!(ledger.meters.settled.tokens, 8);
    assert_eq!(ledger.meters.settled.usd_micros, 5);
}

#[tokio::test]
async fn unknown_usage_consumes_full_maximum_and_second_send_is_denied_without_health_failover() {
    let fixture = Fixture::new(50, false).await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[]})))
        .mount(&fixture.provider)
        .await;
    fixture.buffered().await.unwrap();
    assert_eq!(fixture.ledger.lock().unwrap().meters.uncertain.tokens, 30);
    let error = fixture.buffered().await.err().unwrap();
    assert!(!crate::proxy::failure::retryable_failure(&error));
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn lost_begin_ack_funds_uncertain_work_but_never_sends_or_regrants_that_attempt() {
    let fixture = Fixture::new(50, true).await;
    assert!(fixture.buffered().await.is_err());
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 0);
    assert_eq!(fixture.ledger.lock().unwrap().meters.reserved.tokens, 30);
    assert!(fixture.buffered().await.is_err());
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn actual_stream_eof_requires_terminal_usage_and_charges_output_once() {
    let fixture = Fixture::new(100, false).await;
    Mock::given(wiremock::matchers::method("POST")).respond_with(
        ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
            .set_body_string("data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5}}\n\ndata: [DONE]\n\n"),
    ).mount(&fixture.provider).await;
    let (_, _, stream) = crate::proxy::forward_stream(
        Arc::new(WorkloadIdentityAuth::new()),
        None,
        reqwest::Client::new(),
        fixture.upstream(),
        "chat/completions",
        HeaderMap::new(),
        bytes::Bytes::from_static(
            br#"{"stream":true,"messages":[{"role":"user","content":"text"}]}"#,
        ),
    )
    .await
    .unwrap();
    let _: Vec<_> = stream.try_collect().await.unwrap();
    assert_eq!(fixture.ledger.lock().unwrap().meters.settled.tokens, 8);
}
