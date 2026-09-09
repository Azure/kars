// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget_contract::{
    Amounts, AttemptCommand, AttemptKey, BudgetScope, ExecutionIdentity, Limits, ReserveRequest,
    ResourceIdentity, RootKind, TaskAuthority,
    ledger::Ledger,
    tariffs::{MaximumPrice, ModelContract, Operation, OutputField},
};
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const COLLECTION: &str = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karsbudgetaccounts";
const OBJECT: &str =
    "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karsbudgetaccounts/inference-budget-root";

fn root() -> RootIdentity {
    RootIdentity {
        kind: RootKind::KarsTask,
        resource: ResourceIdentity {
            namespace: "workspace".into(),
            name: "root".into(),
            uid: "root".into(),
        },
        workspace_uid: "workspace-uid".into(),
        cluster_uid: "cluster-uid".into(),
    }
}

fn identity(pod: &str) -> ExecutionIdentity {
    ExecutionIdentity {
        task_uid: "root".into(),
        authorization_digest: authorization().authorization_digest,
        sandbox: ResourceIdentity {
            namespace: "workspace".into(),
            name: "runtime".into(),
            uid: "sandbox-uid".into(),
        },
        runtime_namespace_uid: "runtime-namespace-uid".into(),
        pod_name: format!("pod-{pod}"),
        pod_uid: pod.into(),
    }
}

fn authorization() -> TaskAuthority {
    let mut effective = json!({"domain": "test", "taskUid": "root"});
    effective.sort_all_objects();
    let digest = format!(
        "sha256:{}",
        crate::providers::signing::sha256_hex(&serde_json::to_vec(&effective).unwrap())
    );
    TaskAuthority {
        task: root().resource,
        parent_uid: None,
        root_task_uid: "root".into(),
        authorization_digest: digest,
        effective_authorization: effective,
        limits: Limits {
            tokens: Some(50),
            usd_micros: Some(10),
        },
    }
}

fn account() -> KarsBudgetAccount {
    let spec = KarsBudgetAccountSpec {
        scope: BudgetScope::GovernedInference,
        root: root(),
        limits: Limits {
            tokens: Some(50),
            usd_micros: Some(10),
        },
    };
    let mut account = KarsBudgetAccount::new("inference-budget-root", spec);
    account.metadata.namespace = Some("kars-system".into());
    account.metadata.uid = Some("account-uid".into());
    account.metadata.resource_version = Some("1".into());
    account.labels_mut().insert(MANAGED_BY.into(), OWNER.into());
    account
        .annotations_mut()
        .insert(BOOTSTRAP.into(), "sealed".into());
    let ledger = Ledger::new("account-uid".into(), root(), account.spec.limits).unwrap();
    let ledger = ledger.register_task(authorization()).unwrap().next;
    let ledger = ledger.register_session(identity("a")).unwrap().next;
    let ledger = ledger.register_session(identity("b")).unwrap().next;
    account.status = Some(KarsBudgetAccountStatus {
        ledger: Some(ledger),
    });
    account
}

fn reserve(pod: &str) -> ReserveRequest {
    let contract = ModelContract {
        id: "model".into(),
        version: "v1".into(),
        valid_until: "2030-01-01T00:00:00Z".into(),
        provider_id: "configured".into(),
        endpoint: "https://configured.example".into(),
        model: "model".into(),
        operation: Operation::ChatCompletions,
        output_field: OutputField::Tokens,
        maximum_input_tokens: 10,
        maximum_output_tokens: 20,
        maximum_wire_bytes: 4096,
        output_bound_includes_reasoning: true,
        maximum_price: Some(MaximumPrice::PerRequest { maximum_micros: 5 }),
    };
    let (_, quote) = contract
        .normalize(
            br#"{"messages":[{"role":"user","content":"hello"}]}"#,
            100,
            true,
        )
        .unwrap();
    ReserveRequest {
        account_uid: "account-uid".into(),
        identity: identity(pod),
        sequence: 1,
        wire_digest: format!("sha256:{}", "a".repeat(64)),
        quote,
    }
}

#[derive(Clone, Copy)]
enum Fault {
    None,
    ConflictOnce,
    CommitThenFail,
    RecreateOnWrite,
}

struct State {
    account: Option<KarsBudgetAccount>,
    fault: Fault,
    version: u64,
    writes: usize,
}

#[derive(Clone)]
struct Server(Arc<Mutex<State>>);

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({
        "apiVersion": "v1", "kind": "Status", "status": "Failure",
        "reason": if code == 404 { "NotFound" } else { "Conflict" }, "code": code,
        "message": "private-request-body-must-not-appear"
    }))
}

fn response(value: &KarsBudgetAccount) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(value)
}

impl Respond for Server {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.0.lock().unwrap();
        match (request.method.as_str(), request.url.path()) {
            ("GET", OBJECT) => state
                .account
                .as_ref()
                .map_or_else(|| failure(404), response),
            ("POST", COLLECTION) => {
                if state.account.is_some() {
                    return failure(409);
                }
                let mut account: KarsBudgetAccount = serde_json::from_slice(&request.body).unwrap();
                // Kubernetes ignores status on the CRD main-resource CREATE.
                account.status = None;
                account.metadata.uid = Some("account-uid".into());
                account.metadata.namespace = Some("kars-system".into());
                account.metadata.resource_version = Some(state.version.to_string());
                let output = response(&account);
                state.account = Some(account);
                state.writes += 1;
                output
            }
            ("PUT", path) if path == format!("{OBJECT}/status") => {
                let mut incoming: KarsBudgetAccount =
                    serde_json::from_slice(&request.body).unwrap();
                match state.fault {
                    Fault::ConflictOnce => {
                        state.fault = Fault::None;
                        state.version += 1;
                        let version = state.version.to_string();
                        state.account.as_mut().unwrap().metadata.resource_version = Some(version);
                        return failure(409);
                    }
                    Fault::RecreateOnWrite => {
                        state.account.as_mut().unwrap().metadata.uid = Some("replacement".into());
                    }
                    _ => {}
                }
                let Some(existing) = state.account.as_ref() else {
                    return failure(404);
                };
                if incoming.metadata.uid != existing.metadata.uid
                    || incoming.metadata.resource_version != existing.metadata.resource_version
                {
                    return failure(409);
                }
                state.version += 1;
                state.writes += 1;
                incoming.metadata.resource_version = Some(state.version.to_string());
                state.account = Some(incoming.clone());
                if matches!(state.fault, Fault::CommitThenFail) {
                    state.fault = Fault::None;
                    failure(500)
                } else {
                    response(&incoming)
                }
            }
            ("PATCH", path) if path == OBJECT || path == format!("{OBJECT}/status") => {
                let patch: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let Some(existing) = state.account.as_ref() else {
                    return failure(404);
                };
                if patch["metadata"]["uid"] != json!(existing.metadata.uid)
                    || patch["metadata"]["resourceVersion"]
                        != json!(existing.metadata.resource_version)
                {
                    return failure(409);
                }
                state.version += 1;
                state.writes += 1;
                let version = state.version.to_string();
                let account = state.account.as_mut().unwrap();
                if path.ends_with("/status") {
                    account.status = Some(serde_json::from_value(patch["status"].clone()).unwrap());
                } else {
                    account.annotations_mut().insert(
                        BOOTSTRAP.into(),
                        patch["metadata"]["annotations"][BOOTSTRAP]
                            .as_str()
                            .unwrap()
                            .into(),
                    );
                }
                account.metadata.resource_version = Some(version);
                response(account)
            }
            _ => failure(500),
        }
    }
}

async fn setup(
    account: Option<KarsBudgetAccount>,
    fault: Fault,
) -> (MockServer, Store, Arc<Mutex<State>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(State {
        account,
        fault,
        version: 100,
        writes: 0,
    }));
    Mock::given(wiremock::matchers::any())
        .respond_with(Server(state.clone()))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, Store::new(client, "kars-system"), state)
}

#[tokio::test]
async fn concurrent_siblings_cannot_both_reserve_past_a_shared_ceiling() {
    let (_server, store, state) = setup(Some(account()), Fault::None).await;
    let a = reserve("a");
    let b = reserve("b");
    let root = root();
    let (first, second) = tokio::join!(
        store.transact(&root, "account-uid", |ledger| ledger.reserve(&a, 100)),
        store.transact(&root, "account-uid", |ledger| ledger.reserve(&b, 100)),
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .account
            .as_ref()
            .unwrap()
            .status
            .as_ref()
            .unwrap()
            .ledger
            .as_ref()
            .unwrap()
            .meters
            .reserved
            .tokens,
        30
    );
}

#[tokio::test]
async fn resource_version_conflicts_retry_from_authoritative_state() {
    let (_server, store, state) = setup(Some(account()), Fault::ConflictOnce).await;
    let request = reserve("a");
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.reserve(&request, 100)
        })
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().writes, 1);
}

#[tokio::test]
async fn lost_reserve_ack_recovers_without_allocating_twice() {
    let (_server, store, state) = setup(Some(account()), Fault::CommitThenFail).await;
    let request = reserve("a");
    assert!(
        store
            .transact(&root(), "account-uid", |ledger| ledger
                .reserve(&request, 100))
            .await
            .is_err()
    );
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.reserve(&request, 100)
        })
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().writes, 1);
}

#[tokio::test]
async fn lost_begin_ack_never_reissues_permission_to_send() {
    let (_server, store, state) = setup(Some(account()), Fault::None).await;
    let request = reserve("a");
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.reserve(&request, 100)
        })
        .await
        .unwrap();
    state.lock().unwrap().fault = Fault::CommitThenFail;
    let command = AttemptCommand {
        account_uid: "account-uid".into(),
        key: AttemptKey {
            pod_uid: "a".into(),
            sequence: 1,
        },
        identity: request.identity,
        wire_digest: request.wire_digest,
    };
    assert!(
        store
            .transact(&root(), "account-uid", |ledger| ledger
                .begin_dispatch(&command, 101))
            .await
            .is_err()
    );
    assert!(matches!(
        store
            .transact(&root(), "account-uid", |ledger| ledger
                .begin_dispatch(&command, 101))
            .await,
        Err(StoreError::Ledger(BudgetError::AlreadyDispatched))
    ));
    assert_eq!(
        state
            .lock()
            .unwrap()
            .account
            .as_ref()
            .unwrap()
            .status
            .as_ref()
            .unwrap()
            .ledger
            .as_ref()
            .unwrap()
            .meters
            .reserved,
        Amounts {
            tokens: 30,
            usd_micros: 5
        }
    );
}

#[tokio::test]
async fn missing_recreated_and_corrupt_accounts_are_never_reinitialized_by_requests() {
    for case in ["missing", "uid", "status", "counters"] {
        let mut account = account();
        if case == "uid" {
            account.metadata.uid = Some("replacement".into());
        }
        if case == "status" {
            account.status = None;
        }
        if case == "counters" {
            account
                .status
                .as_mut()
                .unwrap()
                .ledger
                .as_mut()
                .unwrap()
                .meters
                .reserved
                .tokens = 1;
        }
        let (server, store, state) = setup(
            if case == "missing" {
                None
            } else {
                Some(account)
            },
            Fault::None,
        )
        .await;
        assert!(store.read(&root(), "account-uid").await.is_err(), "{case}");
        assert_eq!(state.lock().unwrap().writes, 0);
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.method == "GET")
        );
    }
}

#[tokio::test]
async fn bootstrap_is_metadata_first_and_sealed_only_after_uid_bound_status() {
    let (_server, store, _) = setup(None, Fault::None).await;
    let spec = account().spec;
    let anchor = store
        .create_anchor(
            spec,
            &crate::providers::signing::ReceiptSigner::from_bytes(&[7; 32]),
        )
        .await
        .unwrap();
    assert!(store.read(&root(), "account-uid").await.is_err());
    let initialized = store
        .initialize(&root(), anchor.metadata.uid.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(
        initialized.annotations().get(BOOTSTRAP).map(String::as_str),
        Some("sealed")
    );
    assert!(
        initialized
            .status
            .as_ref()
            .unwrap()
            .ledger
            .as_ref()
            .unwrap()
            .nodes
            .is_empty()
    );
}

#[tokio::test]
async fn replacement_between_read_and_commit_cannot_receive_a_grant() {
    let (_server, store, state) = setup(Some(account()), Fault::RecreateOnWrite).await;
    let request = reserve("a");
    assert!(
        store
            .transact(&root(), "account-uid", |ledger| ledger
                .reserve(&request, 100))
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().writes, 0);
}
