// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget::account::AccountStatusPhase as Phase;
use crate::inference_budget_contract::{Settlement, Usage};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResourceExt;

fn snapshot(state: &Arc<Mutex<State>>) -> KarsBudgetAccount {
    state.lock().unwrap().account.clone().unwrap()
}

fn ready(account: &KarsBudgetAccount) -> &Condition {
    account
        .status
        .as_ref()
        .unwrap()
        .conditions
        .iter()
        .find(|condition| condition.type_ == "Ready")
        .unwrap()
}

fn check(account: &KarsBudgetAccount, phase: Phase, value: &str, reason: &str) {
    let status = account.status.as_ref().unwrap();
    assert_eq!(status.phase, Some(phase));
    assert_eq!(status.observed_generation, account.metadata.generation);
    assert_eq!(status.conditions.len(), 2);
    assert_eq!(ready(account).status, value);
    assert_eq!(ready(account).reason, reason);
    for condition in &status.conditions {
        assert_eq!(condition.observed_generation, account.metadata.generation);
        assert!(!condition.message.contains("private-request-body"));
        assert!(!condition.reason.is_empty());
    }
}

fn command(request: &ReserveRequest) -> AttemptCommand {
    AttemptCommand {
        account_uid: request.account_uid.clone(),
        key: AttemptKey {
            pod_uid: request.identity.pod_uid.clone(),
            sequence: request.sequence,
        },
        identity: request.identity.clone(),
        wire_digest: request.wire_digest.clone(),
    }
}

#[tokio::test]
async fn reserved_headroom_is_not_revocation_and_settlement_reports_durable_exhaustion() {
    let (_server, store, state) = setup(Some(account()), Fault::None).await;
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.narrow_root_limits(Limits {
                tokens: Some(30),
                usd_micros: Some(10),
            })
        })
        .await
        .unwrap();
    let request = reserve("a");
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.reserve(&request, 100)
        })
        .await
        .unwrap();
    let reserved = snapshot(&state);
    check(&reserved, Phase::Blocked, "False", "BudgetReserved");
    let ledger = reserved.status.as_ref().unwrap().ledger.as_ref().unwrap();
    assert!(ledger.nodes["root"].active);
    assert!(!ledger.sessions["a"].closed);
    assert_eq!(ledger.meters.reserved.tokens, 30);
    let writes = state.lock().unwrap().writes;
    assert!(matches!(
        store
            .transact(&root(), "account-uid", |ledger| ledger
                .reserve(&reserve("b"), 100))
            .await,
        Err(StoreError::Ledger(BudgetError::Exhausted))
    ));
    assert_eq!(state.lock().unwrap().writes, writes);
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.begin_dispatch(&command(&request), 101)
        })
        .await
        .unwrap();
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.settle(&Settlement {
                attempt: command(&request),
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    reasoning_output_tokens: 0,
                }),
            })
        })
        .await
        .unwrap();
    let settled = snapshot(&state);
    check(&settled, Phase::Blocked, "False", "BudgetExhausted");
    assert_eq!(
        ready(&settled).last_transition_time,
        ready(&reserved).last_transition_time
    );
    let ledger = settled.status.as_ref().unwrap().ledger.as_ref().unwrap();
    assert_eq!(ledger.meters.settled.tokens, 30);
    assert_eq!(ledger.meters.reserved.tokens, 0);
    assert!(ledger.attempts.is_empty());
    assert!(ledger.nodes["root"].active);
}

#[tokio::test]
async fn expiry_restores_headroom_without_resetting_uid_or_replay_fences() {
    let (_server, store, state) = setup(Some(account()), Fault::None).await;
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.narrow_root_limits(Limits {
                tokens: Some(30),
                usd_micros: Some(10),
            })
        })
        .await
        .unwrap();
    let request = reserve("a");
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.reserve(&request, 100)
        })
        .await
        .unwrap();
    check(&snapshot(&state), Phase::Blocked, "False", "BudgetReserved");
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.expire_undispatched(200)
        })
        .await
        .unwrap();
    let current = snapshot(&state);
    check(&current, Phase::Active, "True", "LedgerAvailable");
    let ledger = current.status.as_ref().unwrap().ledger.as_ref().unwrap();
    assert_eq!(ledger.account_uid, "account-uid");
    assert_eq!(ledger.sessions["a"].closed_through, 1);
    assert!(
        store
            .transact(&root(), "account-uid", |ledger| ledger
                .reserve(&request, 201))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn revoked_and_retired_authority_retains_inflight_liability() {
    let (_server, store, state) = setup(Some(account()), Fault::None).await;
    let request = reserve("a");
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.reserve(&request, 100)
        })
        .await
        .unwrap();
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.begin_dispatch(&command(&request), 101)
        })
        .await
        .unwrap();
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.close_subtree("root")
        })
        .await
        .unwrap();
    let revoked = snapshot(&state);
    check(&revoked, Phase::Blocked, "False", "AuthorityRevoked");
    let ledger = revoked.status.as_ref().unwrap().ledger.as_ref().unwrap();
    assert_eq!(ledger.meters.uncertain.tokens, 30);
    assert!(ledger.sessions.values().all(|session| session.closed));
    store
        .transact(&root(), "account-uid", |ledger| ledger.close_account())
        .await
        .unwrap();
    let closed = snapshot(&state);
    check(&closed, Phase::Closed, "False", "AccountRetired");
    assert_eq!(
        closed
            .status
            .as_ref()
            .unwrap()
            .ledger
            .as_ref()
            .unwrap()
            .meters,
        ledger.meters
    );
}

#[tokio::test]
async fn provider_breach_freezes_even_if_observational_ready_is_forged() {
    let (_server, store, state) = setup(Some(account()), Fault::None).await;
    let request = reserve("a");
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.reserve(&request, 100)
        })
        .await
        .unwrap();
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.begin_dispatch(&command(&request), 101)
        })
        .await
        .unwrap();
    store
        .transact(&root(), "account-uid", |ledger| {
            ledger.settle(&Settlement {
                attempt: command(&request),
                usage: Some(Usage {
                    input_tokens: 11,
                    output_tokens: 0,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    reasoning_output_tokens: 0,
                }),
            })
        })
        .await
        .unwrap();
    check(&snapshot(&state), Phase::Frozen, "False", "ContractBreach");
    {
        let mut state = state.lock().unwrap();
        let status = state.account.as_mut().unwrap().status.as_mut().unwrap();
        status.phase = Some(Phase::Active);
        status
            .conditions
            .iter_mut()
            .find(|condition| condition.type_ == "Ready")
            .unwrap()
            .status = "True".into();
    }
    assert!(matches!(
        store
            .transact(&root(), "account-uid", |ledger| ledger
                .reserve(&reserve("b"), 102))
            .await,
        Err(StoreError::Ledger(BudgetError::Closed))
    ));
    let reported = store
        .refresh_status(&root(), "account-uid", None)
        .await
        .unwrap();
    check(&reported, Phase::Frozen, "False", "ContractBreach");
    assert_eq!(
        reported
            .status
            .unwrap()
            .ledger
            .unwrap()
            .meters
            .uncertain
            .tokens,
        30
    );
}

#[tokio::test]
async fn recovery_reports_unknown_on_live_api_failure_without_churn_or_private_errors() {
    let original = account();
    let (server, store, state) = setup(Some(original.clone()), Fault::None).await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    assert!(
        super::super::super::recovery::reconcile_account(&client, &store, &original)
            .await
            .is_err()
    );
    let unknown = snapshot(&state);
    check(
        &unknown,
        Phase::Unknown,
        "Unknown",
        "ReconciliationUnavailable",
    );
    assert_eq!(
        unknown.status.as_ref().unwrap().ledger,
        original.status.as_ref().unwrap().ledger
    );
    let writes = state.lock().unwrap().writes;
    assert!(
        super::super::super::recovery::reconcile_account(&client, &store, &unknown)
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().writes, writes);
    assert_eq!(snapshot(&state).status, unknown.status);
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/namespaces/workspace"))
        .respond_with(failure(404))
        .with_priority(1)
        .mount(&server)
        .await;
    super::super::super::recovery::reconcile_account(&client, &store, &unknown)
        .await
        .unwrap();
    check(&snapshot(&state), Phase::Closed, "False", "AccountRetired");
}

#[tokio::test]
async fn reporting_corruption_and_missing_ledger_never_initializes_or_repairs_money() {
    for missing in [false, true] {
        let mut original = account();
        if missing {
            original.status.as_mut().unwrap().ledger = None;
        } else {
            original
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
        let (_server, store, state) = setup(Some(original.clone()), Fault::None).await;
        let reported = store
            .refresh_status(&root(), "account-uid", None)
            .await
            .unwrap();
        check(&reported, Phase::Corrupt, "False", "LedgerInvalid");
        assert_eq!(
            reported.status.as_ref().unwrap().ledger,
            original.status.as_ref().unwrap().ledger
        );
        assert!(store.read(&root(), "account-uid").await.is_err());
        assert!(store.initialize(&root(), "account-uid").await.is_err());
        assert_eq!(snapshot(&state).status, reported.status);
    }
}

#[tokio::test]
async fn observation_write_failures_and_replacements_never_fake_success() {
    for fault in [
        Fault::FailRead,
        Fault::FailWrite,
        Fault::RecreateOnWrite,
        Fault::CommitThenFail,
        Fault::ConflictOnce,
    ] {
        let original = account();
        let (_server, store, state) = setup(Some(original.clone()), fault).await;
        let error = StoreError::Api {
            stage: "private-request-body",
            code: Some(503),
        };
        let result = store
            .refresh_status(&root(), "account-uid", Some(&error))
            .await;
        assert_eq!(result.is_ok(), matches!(fault, Fault::ConflictOnce));
        let stored = snapshot(&state);
        assert_eq!(
            stored.status.as_ref().unwrap().ledger,
            original.status.as_ref().unwrap().ledger
        );
        if matches!(fault, Fault::ConflictOnce | Fault::CommitThenFail) {
            check(
                &stored,
                Phase::Unknown,
                "Unknown",
                "ReconciliationUnavailable",
            );
        } else {
            assert_eq!(stored.status, original.status);
        }
    }
}

#[tokio::test]
async fn observation_backfill_and_generation_update_preserve_stable_transition_time() {
    let mut original = account();
    let ledger = original.status.as_ref().unwrap().ledger.clone();
    original.status = Some(KarsBudgetAccountStatus {
        ledger,
        ..Default::default()
    });
    let (_server, store, state) = setup(Some(original), Fault::None).await;
    let first = store
        .refresh_status(&root(), "account-uid", None)
        .await
        .unwrap();
    check(&first, Phase::Active, "True", "LedgerAvailable");
    state
        .lock()
        .unwrap()
        .account
        .as_mut()
        .unwrap()
        .metadata
        .generation = Some(2);
    let second = store
        .refresh_status(&root(), "account-uid", None)
        .await
        .unwrap();
    check(&second, Phase::Active, "True", "LedgerAvailable");
    assert_eq!(
        ready(&first).last_transition_time,
        ready(&second).last_transition_time
    );
    assert_eq!(ready(&second).observed_generation, Some(2));
    let writes = state.lock().unwrap().writes;
    store
        .refresh_status(&root(), "account-uid", None)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().writes, writes);
}

#[test]
fn helm_budget_account_reporting_matches_generated_schema() {
    fn canonical_schema(mut value: serde_json::Value) -> serde_json::Value {
        match &mut value {
            serde_json::Value::Object(fields) => {
                fields.remove("description");
                if let Some(serde_json::Value::Array(required)) = fields.get_mut("required") {
                    required.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                }
                if let Some(minimum) = fields.get_mut("minimum") {
                    *minimum = json!(minimum.as_f64().unwrap());
                }
                for field in fields.values_mut() {
                    *field = canonical_schema(field.take());
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    *item = canonical_schema(item.take());
                }
            }
            _ => {}
        }
        value
    }
    let helm: serde_json::Value = serde_yaml::from_str(include_str!(
        "../../../deploy/helm/kars/templates/crd-karsbudgetaccount.yaml"
    ))
    .unwrap();
    let generated = serde_json::to_value(KarsBudgetAccount::crd()).unwrap();
    let helm = &helm["spec"]["versions"][0];
    let generated = &generated["spec"]["versions"][0];
    assert_eq!(
        helm["additionalPrinterColumns"],
        generated["additionalPrinterColumns"]
    );
    for field in ["phase", "observedGeneration", "conditions", "ledger"] {
        let path = |schema: &serde_json::Value| {
            schema["schema"]["openAPIV3Schema"]["properties"]["status"]["properties"][field].clone()
        };
        assert_eq!(
            canonical_schema(path(helm)),
            canonical_schema(path(generated)),
            "{field}"
        );
    }
}

#[tokio::test]
async fn observation_conflict_reloads_concurrently_reserved_money() {
    let (_server, store, state) = setup(Some(account()), Fault::ReserveOnConflict).await;
    let error = StoreError::Api {
        stage: "recovery",
        code: Some(503),
    };
    let reported = store
        .refresh_status(&root(), "account-uid", Some(&error))
        .await
        .unwrap();
    check(
        &reported,
        Phase::Unknown,
        "Unknown",
        "ReconciliationUnavailable",
    );
    let ledger = reported.status.as_ref().unwrap().ledger.as_ref().unwrap();
    assert_eq!(ledger.meters.reserved.tokens, 30);
    assert_eq!(ledger.attempts.len(), 1);
    assert_eq!(ledger.sessions["a"].next_sequence, 2);
    assert_eq!(snapshot(&state).status, reported.status);
}

#[tokio::test]
async fn lost_bootstrap_status_ack_reuses_the_same_ledger_before_sealing() {
    let (_server, store, state) = setup(None, Fault::None).await;
    store
        .create_anchor(
            account().spec,
            &crate::providers::signing::ReceiptSigner::from_bytes(&[7; 32]),
        )
        .await
        .unwrap();
    state.lock().unwrap().fault = Fault::CommitThenFail;
    assert!(store.initialize(&root(), "account-uid").await.is_err());
    let pending = snapshot(&state);
    check(&pending, Phase::Bootstrap, "False", "BootstrapPending");
    assert!(store.read(&root(), "account-uid").await.is_err());
    let initialized = store.initialize(&root(), "account-uid").await.unwrap();
    check(&initialized, Phase::Active, "True", "LedgerAvailable");
    assert_eq!(
        initialized.status.as_ref().unwrap().ledger,
        pending.status.as_ref().unwrap().ledger
    );
    assert_eq!(initialized.metadata.uid, pending.metadata.uid);
}
