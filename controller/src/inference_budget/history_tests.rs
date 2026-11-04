// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget_contract::{MAX_NODES, Settlement, Usage, tariffs::AttemptQuote};

fn child(uid: &str) -> TaskAuthority {
    let mut authority = authorization();
    authority.task.uid = uid.into();
    authority.task.name = uid.into();
    authority.parent_uid = Some("root".into());
    authority.effective_authorization["taskUid"] = json!(uid);
    authority.authorization_digest = format!(
        "sha256:{}",
        crate::providers::signing::sha256_hex(
            &serde_json::to_vec(&authority.effective_authorization).unwrap()
        )
    );
    authority
}

fn ledger_of(account: &KarsBudgetAccount) -> Ledger {
    Store::ledger(account).unwrap().clone()
}

async fn compact(store: &Store) {
    store
        .for_task("root")
        .transact(&root(), "account-uid", |ledger| {
            Ok(Mutation {
                next: ledger.clone(),
                value: (),
                changed: false,
            })
        })
        .await
        .unwrap();
}

fn closed_account() -> KarsBudgetAccount {
    let mut account = account();
    let mut ledger = ledger_of(&account);
    ledger = ledger.register_task(child("retained")).unwrap().next;
    ledger = ledger.close_subtree("retained").unwrap().next;
    account.status.as_mut().unwrap().ledger = Some(ledger);
    account
}

#[tokio::test]
async fn history_migrates_full_legacy_account_without_resetting_authority_or_meters() {
    let mut account = account();
    let mut ledger = ledger_of(&account);
    for index in 1..MAX_NODES {
        let uid = format!("old-{index}");
        ledger = ledger.register_task(child(&uid)).unwrap().next;
        ledger = ledger.close_subtree(&uid).unwrap().next;
    }
    let before = ledger.clone();
    account.status.as_mut().unwrap().ledger = Some(ledger);
    let (_server, store, state) = setup(Some(account), Fault::None).await;
    store
        .for_task("new")
        .transact(&root(), "account-uid", |ledger| {
            ledger.register_task(child("new"))
        })
        .await
        .unwrap();
    let current = ledger_of(state.lock().unwrap().account.as_ref().unwrap());
    assert_eq!(current.account_uid, before.account_uid);
    assert_eq!(current.meters, before.meters);
    assert_eq!(current.nodes["root"], before.nodes["root"]);
    assert!(current.nodes["new"].active);
    assert!(current.nodes.len() < MAX_NODES);
    assert_eq!(
        current.nodes.len() as u64 + current.history.as_ref().unwrap().nodes,
        MAX_NODES as u64 + 1
    );
    let restored = store
        .for_task("old-1")
        .read(&root(), "account-uid")
        .await
        .unwrap();
    assert_eq!(ledger_of(&restored).nodes["old-1"], before.nodes["old-1"]);
}

#[tokio::test]
async fn history_retains_more_than_lifetime_node_and_session_caps_with_bounded_residency() {
    let (_server, store, state) = setup(Some(account()), Fault::None).await;
    for index in 0..270 {
        let uid = format!("retained-{index}");
        let mut identity = identity(&format!("history-pod-{index}"));
        identity.task_uid = uid.clone();
        identity.authorization_digest = child(&uid).authorization_digest;
        store
            .for_identity(&identity)
            .transact(&root(), "account-uid", |ledger| {
                ledger
                    .register_task(child(&uid))?
                    .next
                    .register_session(identity.clone())?
                    .next
                    .close_subtree(&uid)
            })
            .await
            .unwrap();
    }
    let current = ledger_of(state.lock().unwrap().account.as_ref().unwrap());
    assert!(current.nodes.len() <= 3);
    assert!(current.sessions.len() <= 4);
    assert_eq!(
        current.history.as_ref().unwrap().nodes + current.nodes.len() as u64,
        271
    );
    assert_eq!(
        current.history.as_ref().unwrap().sessions + current.sessions.len() as u64,
        272
    );
    assert!(serde_json::to_vec(&current).unwrap().len() < 20_000);
    let mut old = identity("history-pod-0");
    old.task_uid = "retained-0".into();
    old.authorization_digest = child("retained-0").authorization_digest;
    let restored = store
        .for_identity(&old)
        .read(&root(), "account-uid")
        .await
        .unwrap();
    let ledger = ledger_of(&restored);
    assert!(ledger.sessions[&old.pod_uid].closed);
    assert!(!ledger.nodes[&old.task_uid].active);
    assert!(ledger.register_session(old).is_err());
    assert_eq!(ledger.nodes["retained-0"].authority, child("retained-0"));
    store
        .for_task("retained-0")
        .transact(&root(), "account-uid", |ledger| {
            ledger.resume_task("retained-0", &child("retained-0").authorization_digest)
        })
        .await
        .unwrap();
    assert!(ledger_of(state.lock().unwrap().account.as_ref().unwrap()).nodes["retained-0"].active);
}

#[tokio::test]
async fn history_preserves_unknown_evidence_and_settles_late_usage_exactly_once() {
    let mut account = account();
    let mut ledger = ledger_of(&account);
    account.spec.limits = Limits::default();
    ledger.limits = Limits::default();
    ledger.nodes.get_mut("root").unwrap().authority.limits = Limits::default();
    let mut request = reserve("a");
    request.quote = AttemptQuote::record_only(
        "provider",
        "https://provider.example/v1",
        "model",
        Operation::Responses,
    );
    let command = AttemptCommand {
        account_uid: request.account_uid.clone(),
        identity: request.identity.clone(),
        key: AttemptKey {
            pod_uid: "a".into(),
            sequence: 1,
        },
        wire_digest: request.wire_digest.clone(),
    };
    ledger = ledger
        .reserve(&request, 100)
        .unwrap()
        .next
        .begin_dispatch(&command, 101)
        .unwrap()
        .next
        .close_session("a")
        .unwrap()
        .next;
    let original = ledger.clone();
    account.status.as_mut().unwrap().ledger = Some(ledger);
    let (_server, store, state) = setup(Some(account), Fault::None).await;
    compact(&store).await;
    let archived = ledger_of(state.lock().unwrap().account.as_ref().unwrap());
    assert_eq!(archived.meters, original.meters);
    assert_eq!(archived.nodes["root"].meters, original.nodes["root"].meters);
    assert_eq!(archived.history.as_ref().unwrap().unknown_usage_attempts, 1);
    assert_eq!(archived.history.as_ref().unwrap().node_unknown["root"], 1);
    assert!(!archived.sessions.contains_key("a"));
    let settlement = Settlement {
        attempt: command,
        usage: Some(Usage {
            input_tokens: 1_000_000,
            output_tokens: 2_000_000,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
        }),
    };
    let first = store
        .for_identity(&identity("a"))
        .transact(&root(), "account-uid", |ledger| ledger.settle(&settlement))
        .await
        .unwrap();
    assert_eq!(first.charged.unwrap().tokens, 3_000_000);
    compact(&store).await;
    store
        .for_identity(&identity("a"))
        .transact(&root(), "account-uid", |ledger| ledger.settle(&settlement))
        .await
        .unwrap();
    let final_ledger = ledger_of(state.lock().unwrap().account.as_ref().unwrap());
    assert_eq!(final_ledger.meters.settled.tokens, 3_000_000);
    assert_eq!(final_ledger.meters.unknown_usage_attempts, 0);
    assert_eq!(final_ledger.nodes["root"].meters, final_ledger.meters);
    assert!(final_ledger.reserve(&request, 102).is_err());
    assert!(final_ledger.register_session(identity("a")).is_err());
}

#[tokio::test]
async fn history_pages_are_durable_before_root_and_cas_conflicts_do_not_lose_records() {
    for fault in [Fault::ConflictOnce, Fault::CommitThenFail] {
        let (_server, store, state) = setup(Some(closed_account()), fault).await;
        let first = store
            .for_task("root")
            .transact(&root(), "account-uid", |ledger| {
                Ok(Mutation {
                    next: ledger.clone(),
                    value: (),
                    changed: false,
                })
            })
            .await;
        assert_eq!(first.is_err(), matches!(fault, Fault::CommitThenFail));
        compact(&store).await;
        let account = store
            .for_task("retained")
            .read(&root(), "account-uid")
            .await
            .unwrap();
        assert_eq!(
            ledger_of(&account).nodes["retained"].authority,
            child("retained")
        );
        assert!(!ledger_of(&account).nodes["retained"].active);
        assert!(!state.lock().unwrap().history.is_empty());
    }
}

#[tokio::test]
async fn history_failed_page_write_keeps_original_account_authoritative() {
    let original = closed_account();
    let (_server, store, state) = setup(Some(original.clone()), Fault::None).await;
    state.lock().unwrap().fail_history = true;
    assert!(
        store
            .for_task("root")
            .transact(&root(), "account-uid", |ledger| Ok(Mutation {
                next: ledger.clone(),
                value: (),
                changed: false,
            }))
            .await
            .is_err()
    );
    assert_eq!(
        ledger_of(state.lock().unwrap().account.as_ref().unwrap()),
        ledger_of(&original)
    );
    assert_eq!(state.lock().unwrap().writes, 0);
    state.lock().unwrap().fail_history = false;
    compact(&store).await;
}

#[tokio::test]
async fn history_prepared_pages_do_not_commit_an_account_after_failed_cas() {
    let original = closed_account();
    let (_server, store, state) = setup(Some(original.clone()), Fault::FailWrite).await;
    assert!(
        store
            .for_task("root")
            .transact(&root(), "account-uid", |ledger| Ok(Mutation {
                next: ledger.clone(),
                value: (),
                changed: false,
            }))
            .await
            .is_err()
    );
    assert!(!state.lock().unwrap().history.is_empty());
    assert_eq!(
        ledger_of(state.lock().unwrap().account.as_ref().unwrap()),
        ledger_of(&original)
    );
    state.lock().unwrap().fault = Fault::None;
    compact(&store).await;
    assert!(
        ledger_of(state.lock().unwrap().account.as_ref().unwrap())
            .history
            .is_some()
    );
}

#[tokio::test]
async fn history_rejects_unauthenticated_and_foreign_routers_before_restoration_writes() {
    use crate::inference_budget::{
        config::Settings,
        service::{Broker, authenticated_account},
    };
    use axum::http::HeaderMap;
    use wiremock::matchers::{method, path};

    for mode in [
        "missing-token",
        "rejected-token",
        "foreign-runtime",
        "foreign-pod",
    ] {
        let (server, store, state) = setup(Some(closed_account()), Fault::None).await;
        compact(&store).await;
        let before = state.lock().unwrap().account.clone().unwrap();
        let pages = state.lock().unwrap().history.clone();
        let request_count = server.received_requests().await.unwrap().len();
        let mut identity = identity("historical-pod");
        identity.task_uid = "retained".into();
        identity.authorization_digest = child("retained").authorization_digest;
        let mut headers = HeaderMap::new();
        if mode != "missing-token" {
            headers.insert("authorization", "Bearer test-only-token".parse().unwrap());
        }
        Mock::given(method("POST"))
            .and(path("/apis/authentication.k8s.io/v1/tokenreviews"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion":"authentication.k8s.io/v1", "kind":"TokenReview",
                "metadata":{}, "spec":{},
                "status":{
                    "authenticated":mode != "rejected-token",
                    "audiences":[crate::inference_budget::config::AUDIENCE],
                    "user":{
                        "username":if mode == "foreign-runtime" {
                            "system:serviceaccount:kars-foreign:sandbox"
                        } else { "system:serviceaccount:kars-runtime:sandbox" },
                        "extra":{
                            "authentication.kubernetes.io/pod-name":[identity.pod_name],
                            "authentication.kubernetes.io/pod-uid":[if mode == "foreign-pod" {
                                "foreign-pod"
                            } else { identity.pod_uid.as_str() }]
                        }
                    }
                }
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let broker = Broker {
            client,
            store: store.clone(),
            settings: Settings {
                accounting_namespace: "kars-system".into(),
                catalog_name: "catalog".into(),
                address: "127.0.0.1:0".into(),
                tls_secret: "tls".into(),
                router_image_digest: format!("sha256:{}", "a".repeat(64)),
                first_install_router_digest: None,
            },
        };
        assert!(
            matches!(
                authenticated_account(&broker, &headers, &root(), "account-uid", &identity, false)
                    .await,
                Err(StoreError::Ledger(BudgetError::Authorization))
            ),
            "{mode}"
        );
        assert_eq!(
            serde_json::to_value(&state.lock().unwrap().account).unwrap(),
            serde_json::to_value(&before).unwrap(),
            "{mode}"
        );
        assert_eq!(state.lock().unwrap().history, pages, "{mode}");
        for request in &server.received_requests().await.unwrap()[request_count..] {
            assert!(
                request.method == "GET" || request.url.path().ends_with("/tokenreviews"),
                "{mode}: {request:?}"
            );
        }
    }
}

#[tokio::test]
async fn history_keeps_fully_archived_team_authority_revoked() {
    let mut account = account();
    let mut ledger = ledger_of(&account);
    account.spec.root.kind = RootKind::KarsTeam;
    ledger.root.kind = RootKind::KarsTeam;
    ledger = ledger.close_subtree("root").unwrap().next;
    let team_root = ledger.root.clone();
    account.status.as_mut().unwrap().ledger = Some(ledger);
    let (_server, store, state) = setup(Some(account), Fault::None).await;
    for _ in 0..2 {
        store
            .for_task("future-task")
            .transact(&team_root, "account-uid", |ledger| {
                Ok(Mutation {
                    next: ledger.clone(),
                    value: (),
                    changed: false,
                })
            })
            .await
            .unwrap();
    }
    let current = state.lock().unwrap().account.clone().unwrap();
    assert!(ledger_of(&current).nodes.is_empty());
    assert_eq!(ledger_of(&current).history.unwrap().nodes, 1);
    assert!(current.status.unwrap().conditions.iter().any(|condition| {
        condition.type_ == "Ready"
            && condition.status == "False"
            && condition.reason == "AuthorityRevoked"
    }));
}

#[tokio::test]
async fn history_enrolls_a_new_dormant_child_with_its_archived_parent() {
    let (_server, store, state) = setup(Some(closed_account()), Fault::None).await;
    compact(&store).await;
    assert!(
        !ledger_of(state.lock().unwrap().account.as_ref().unwrap())
            .nodes
            .contains_key("retained")
    );
    let mut authority = child("new-dormant-child");
    authority.parent_uid = Some("retained".into());
    store
        .for_authority(&authority)
        .transact(&root(), "account-uid", |ledger| {
            crate::inference_budget::binding::bind_authority(ledger, &authority, true, false)
        })
        .await
        .unwrap();
    let current = ledger_of(state.lock().unwrap().account.as_ref().unwrap());
    assert!(!current.nodes["retained"].active);
    assert!(!current.nodes["new-dormant-child"].active);
    assert_eq!(current.nodes["retained"].authority, child("retained"));
    assert_eq!(current.nodes["new-dormant-child"].authority, authority);
}

#[tokio::test]
async fn history_preserves_finite_uncertainty_without_refunding_or_double_charging() {
    let mut account = account();
    let request = reserve("a");
    let command = AttemptCommand {
        account_uid: request.account_uid.clone(),
        identity: request.identity.clone(),
        key: AttemptKey {
            pod_uid: "a".into(),
            sequence: 1,
        },
        wire_digest: request.wire_digest.clone(),
    };
    let original = ledger_of(&account)
        .reserve(&request, 100)
        .unwrap()
        .next
        .begin_dispatch(&command, 101)
        .unwrap()
        .next
        .close_session("a")
        .unwrap()
        .next;
    account.status.as_mut().unwrap().ledger = Some(original.clone());
    let (_server, store, state) = setup(Some(account), Fault::None).await;
    compact(&store).await;
    let current = ledger_of(state.lock().unwrap().account.as_ref().unwrap());
    assert!(!current.sessions.contains_key("a"));
    assert_eq!(current.meters, original.meters);
    assert_eq!(current.effective_limits().unwrap(), original.limits);
    let settlement = Settlement {
        attempt: command,
        usage: Some(Usage {
            input_tokens: 2,
            output_tokens: 1,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
        }),
    };
    for _ in 0..2 {
        let result = store
            .for_identity(&request.identity)
            .transact(&root(), "account-uid", |ledger| ledger.settle(&settlement))
            .await
            .unwrap();
        assert!(result.finalized && result.uncertain && !result.breach);
        assert_eq!(result.charged, Some(request.quote.maximum()));
        compact(&store).await;
    }
    let current = store
        .for_identity(&request.identity)
        .read(&root(), "account-uid")
        .await
        .unwrap();
    let current = ledger_of(&current);
    assert_eq!(current.meters, original.meters);
    assert_eq!(current.meters.uncertain, request.quote.maximum());
    assert_eq!(current.attempts, original.attempts);
    assert_eq!(current.effective_limits().unwrap(), original.limits);
    assert!(current.register_session(request.identity.clone()).is_err());
    compact(&store).await;
    let mut breach = settlement;
    breach.usage.as_mut().unwrap().output_tokens = 21;
    let result = store
        .for_identity(&request.identity)
        .transact(&root(), "account-uid", |ledger| ledger.settle(&breach))
        .await
        .unwrap();
    assert!(result.breach && result.uncertain);
    let frozen = ledger_of(state.lock().unwrap().account.as_ref().unwrap());
    assert_eq!(
        frozen.phase,
        crate::inference_budget_contract::AccountPhase::Frozen
    );
    assert_eq!(frozen.meters, original.meters);
}

#[tokio::test]
async fn history_restoration_preserves_unbounded_tokens_and_finite_money_policy() {
    use crate::inference_budget_contract::ledger::UnboundedTokenAllowance;
    let mut account = closed_account();
    let ledger = ledger_of(&account);
    let grant = UnboundedTokenAllowance {
        id: "beta-unbounded".into(),
        config_uid: "operator-config".into(),
        account_uid: ledger.account_uid.clone(),
        root: ledger.root.clone(),
        original_limits: ledger.limits,
        ledger_digest: ledger.funding_renewal_state_digest().unwrap(),
        previous_token_top_up_ids: vec![],
    };
    account.status.as_mut().unwrap().ledger =
        Some(ledger.authorize_unbounded_tokens(&grant, 100).unwrap().next);
    let (_server, store, _) = setup(Some(account), Fault::None).await;
    compact(&store).await;
    let restored = store
        .for_task("retained")
        .read(&root(), "account-uid")
        .await
        .unwrap();
    let ledger = ledger_of(&restored);
    assert_eq!(ledger.unbounded_tokens.as_ref(), Some(&grant));
    assert_eq!(ledger.nodes["retained"].authority, child("retained"));
    assert_eq!(
        ledger.effective_node_limits("retained").unwrap(),
        Limits {
            tokens: None,
            usd_micros: Some(10)
        }
    );
    assert_eq!(
        ledger.effective_limits().unwrap(),
        Limits {
            tokens: None,
            usd_micros: Some(10)
        }
    );
}

#[tokio::test]
async fn history_keeps_finite_top_up_targets_and_receipts_unchanged() {
    let (mut account, grant) = top_up_fixture();
    let original = ledger_of(&account)
        .register_task(child("retained"))
        .unwrap()
        .next
        .close_subtree("retained")
        .unwrap()
        .next;
    account.status.as_mut().unwrap().ledger = Some(original.clone());
    let (_server, store, state) = setup(Some(account), Fault::None).await;
    store
        .for_task("root")
        .transact(&grant.root, "account-uid", |ledger| {
            ledger.apply_token_top_up(&grant, 100)
        })
        .await
        .unwrap();
    let current = ledger_of(state.lock().unwrap().account.as_ref().unwrap());
    assert!(current.nodes.contains_key("root"));
    assert!(!current.nodes.contains_key("retained"));
    assert_eq!(current.token_top_ups.get(&grant.id), Some(&grant));
    assert_eq!(current.effective_limits().unwrap().tokens, Some(150));
    let restored = store
        .for_task("retained")
        .read(&grant.root, "account-uid")
        .await
        .unwrap();
    let restored = ledger_of(&restored);
    assert_eq!(restored.nodes["retained"], original.nodes["retained"]);
    assert_eq!(restored.token_top_ups, current.token_top_ups);
    assert_eq!(restored.meters, original.meters);
}

#[tokio::test]
async fn history_missing_corrupt_or_foreign_pages_fail_closed() {
    for mode in ["missing", "body", "account", "mutable"] {
        let (_server, store, state) = setup(Some(closed_account()), Fault::None).await;
        compact(&store).await;
        {
            let mut state = state.lock().unwrap();
            let name = state.history.keys().next().unwrap().clone();
            let page = state.history.get_mut(&name).unwrap();
            match mode {
                "missing" => {
                    state.history.remove(&name);
                }
                "body" => {
                    page.data
                        .as_mut()
                        .unwrap()
                        .values_mut()
                        .next()
                        .unwrap()
                        .push(' ');
                }
                "account" => {
                    page.annotations_mut()
                        .values_mut()
                        .for_each(|value| *value = "foreign-account".into());
                }
                "mutable" => {
                    page.immutable = Some(false);
                }
                _ => unreachable!(),
            }
        }
        assert!(
            store
                .for_task("retained")
                .read(&root(), "account-uid")
                .await
                .is_err(),
            "{mode}"
        );
    }
}
