// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget_contract::tariffs::{
    MaximumPrice, ModelContract, Operation, OutputField,
};
use serde_json::json;

#[test]
fn integer_range_validation_applies_even_when_attempt_map_is_empty() {
    let mut ledger = ledger(100);
    assert!(ledger.attempts.is_empty());
    ledger.validate().unwrap();
    ledger.limits.tokens = Some(MAX_LEDGER_INTEGER + 1);
    assert!(matches!(ledger.validate(), Err(BudgetError::Overflow)));
}

fn resource(name: &str, uid: &str) -> ResourceIdentity {
    ResourceIdentity {
        namespace: "workspace".into(),
        name: name.into(),
        uid: uid.into(),
    }
}

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn task(uid: &str, parent: Option<&str>, limit: u64) -> TaskAuthority {
    TaskAuthority {
        task: resource(uid, uid),
        parent_uid: parent.map(str::to_string),
        root_task_uid: "root".into(),
        authorization_digest: digest(),
        effective_authorization: json!({"domain": "test", "uid": uid}),
        limits: Limits {
            tokens: Some(limit),
            usd_micros: Some(1000),
        },
    }
}

fn identity(task: &str, pod: &str) -> ExecutionIdentity {
    ExecutionIdentity {
        task_uid: task.into(),
        authorization_digest: digest(),
        sandbox: resource(&format!("sandbox-{task}"), &format!("sandbox-{task}")),
        runtime_namespace_uid: format!("namespace-{task}"),
        pod_name: format!("pod-{pod}"),
        pod_uid: pod.into(),
    }
}

fn ledger(limit: u64) -> Ledger {
    let root = RootIdentity {
        kind: RootKind::KarsTask,
        resource: resource("root", "root"),
        workspace_uid: "workspace-uid".into(),
        cluster_uid: "cluster-uid".into(),
    };
    let ledger = Ledger::new(
        "account-uid".into(),
        root,
        Limits {
            tokens: Some(limit),
            usd_micros: Some(1000),
        },
    )
    .unwrap();
    ledger
        .register_task(task("root", None, limit))
        .unwrap()
        .next
}

fn request(task: &str, pod: &str, sequence: u64) -> ReserveRequest {
    let contract = ModelContract {
        id: "model".into(),
        version: "v1".into(),
        valid_until: "2030-01-01T00:00:00Z".into(),
        provider_id: "provider".into(),
        endpoint: "https://provider.example".into(),
        model: "model".into(),
        operation: Operation::ChatCompletions,
        output_field: OutputField::MaxTokens,
        maximum_input_tokens: 10,
        maximum_output_tokens: 20,
        maximum_wire_bytes: 4096,
        output_bound_includes_reasoning: true,
        maximum_price: Some(MaximumPrice::PerRequest { maximum_micros: 5 }),
    };
    let (_, quote) = contract
        .normalize(
            br#"{"model":"model","messages":[{"role":"user","content":"hello"}]}"#,
            100,
            true,
        )
        .unwrap();
    ReserveRequest {
        account_uid: "account-uid".into(),
        identity: identity(task, pod),
        sequence,
        wire_digest: digest(),
        quote,
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

fn usage() -> Usage {
    Usage {
        input_tokens: 3,
        output_tokens: 5,
        cached_input_tokens: 0,
        cache_creation_input_tokens: 0,
        reasoning_output_tokens: 0,
    }
}

#[test]
fn uncertain_tombstones_retain_contracts_and_freeze_on_late_over_bound_usage() {
    let request = request("root", "pod-a", 1);
    let mut ledger = ledger(100)
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    ledger = ledger.reserve(&request, 100).unwrap().next;
    ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
    ledger = ledger.commit_uncertain_before(200).unwrap().next;
    assert_eq!(ledger.meters.uncertain.tokens, 30);
    assert_eq!(ledger.attempts.len(), 1);
    let settled = ledger
        .settle(&Settlement {
            attempt: command(&request),
            usage: Some(Usage {
                output_tokens: 21,
                ..usage()
            }),
        })
        .unwrap();
    assert!(settled.value.breach);
    assert_eq!(settled.next.phase, AccountPhase::Frozen);
    assert_eq!(settled.next.meters.uncertain.tokens, 30);
    assert_eq!(settled.next.sessions["pod-a"].next_sequence, 2);
}

#[test]
fn ordinary_completed_attempts_compact_without_reissuing_their_sequences() {
    let request = request("root", "pod-a", 1);
    let mut ledger = ledger(100)
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    ledger = ledger.reserve(&request, 100).unwrap().next;
    ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
    ledger = ledger
        .settle(&Settlement {
            attempt: command(&request),
            usage: Some(usage()),
        })
        .unwrap()
        .next;
    assert!(ledger.attempts.is_empty());
    assert_eq!(ledger.sessions["pod-a"].closed_through, 1);
    assert!(ledger.reserve(&request, 102).is_err());
    assert_eq!(ledger.meters.settled.tokens, 8);
}

#[test]
fn sibling_reservations_share_one_root_ceiling_instead_of_getting_daily_copies() {
    let ledger = ledger(50);
    let ledger = ledger
        .register_task(task("left", Some("root"), 50))
        .unwrap()
        .next;
    let ledger = ledger
        .register_task(task("right", Some("root"), 50))
        .unwrap()
        .next;
    let ledger = ledger
        .register_session(identity("left", "pod-left"))
        .unwrap()
        .next;
    let ledger = ledger
        .register_session(identity("right", "pod-right"))
        .unwrap()
        .next;
    let ledger = ledger
        .reserve(&request("left", "pod-left", 1), 100)
        .unwrap()
        .next;
    assert_eq!(ledger.meters.reserved.tokens, 30);
    assert!(matches!(
        ledger.reserve(&request("right", "pod-right", 1), 100),
        Err(BudgetError::Exhausted)
    ));
}

#[test]
fn all_ancestors_are_reserved_atomically_and_a_narrow_subtree_binds_grandchildren() {
    let ledger = ledger(100)
        .register_task(task("branch", Some("root"), 40))
        .unwrap()
        .next;
    let ledger = ledger
        .register_task(task("leaf", Some("branch"), 40))
        .unwrap()
        .next;
    let ledger = ledger
        .register_session(identity("leaf", "pod-leaf"))
        .unwrap()
        .next;
    let ledger = ledger
        .reserve(&request("leaf", "pod-leaf", 1), 100)
        .unwrap()
        .next;
    for uid in ["root", "branch", "leaf"] {
        assert_eq!(ledger.nodes[uid].meters.reserved.tokens, 30);
    }
    assert!(matches!(
        ledger.reserve(&request("leaf", "pod-leaf", 2), 100),
        Err(BudgetError::Exhausted)
    ));
}

#[test]
fn reserve_replay_is_idempotent_but_begin_dispatch_never_regrants() {
    let request = request("root", "pod", 1);
    let ledger = ledger(100)
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    let ledger = ledger.reserve(&request, 100).unwrap().next;
    assert!(!ledger.reserve(&request, 101).unwrap().changed);
    let ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
    assert!(matches!(
        ledger.begin_dispatch(&command(&request), 101),
        Err(BudgetError::AlreadyDispatched)
    ));
    assert_eq!(ledger.meters.reserved.tokens, 30);
}

#[test]
fn only_undispatched_reservations_expire_and_lost_begin_ack_remains_funded() {
    let one = request("root", "pod", 1);
    let two = request("root", "pod", 2);
    let ledger = ledger(100)
        .register_session(one.identity.clone())
        .unwrap()
        .next;
    let ledger = ledger.reserve(&one, 100).unwrap().next;
    let ledger = ledger.reserve(&two, 100).unwrap().next;
    let ledger = ledger.begin_dispatch(&command(&two), 101).unwrap().next;
    let expired = ledger.expire_undispatched(1000).unwrap();
    assert_eq!(expired.value, 1);
    assert_eq!(expired.next.meters.reserved.tokens, 30);
    assert!(
        expired
            .next
            .attempts
            .values()
            .any(|attempt| attempt.phase == AttemptPhase::InFlight)
    );
}

#[test]
fn settlement_refunds_only_unused_bound_once_and_compaction_retains_replay_fences() {
    let request = request("root", "pod", 1);
    let ledger = ledger(100)
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    let ledger = ledger.reserve(&request, 100).unwrap().next;
    let ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
    let settlement = Settlement {
        attempt: command(&request),
        usage: Some(usage()),
    };
    let next = ledger.settle(&settlement).unwrap();
    assert_eq!(next.next.meters.settled.tokens, 8);
    assert_eq!(next.next.meters.reserved.tokens, 0);
    assert_eq!(next.next.sessions["pod"].closed_through, 1);
    assert!(next.next.attempts.is_empty());
    assert!(!next.next.settle(&settlement).unwrap().changed);
    assert!(matches!(
        next.next.reserve(&request, 102),
        Err(BudgetError::Sequence)
    ));
}

#[test]
fn cancellation_charges_inflight_at_maximum_and_never_refunds_late_usage() {
    let request = request("root", "pod", 1);
    let ledger = ledger(100)
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    let ledger = ledger.reserve(&request, 100).unwrap().next;
    let ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
    let ledger = ledger.close_subtree("root").unwrap().next;
    assert_eq!(ledger.meters.uncertain.tokens, 30);
    assert_eq!(ledger.meters.reserved.tokens, 0);
    assert!(
        !ledger
            .settle(&Settlement {
                attempt: command(&request),
                usage: Some(usage())
            })
            .unwrap()
            .changed
    );
    assert!(
        ledger
            .register_session(identity("root", "new-pod"))
            .is_err()
    );
}

#[test]
fn missing_usage_commits_the_full_bound_and_overbound_usage_freezes_the_account() {
    for observed in [
        None,
        Some(Usage {
            input_tokens: 999,
            ..usage()
        }),
    ] {
        let request = request("root", "pod", 1);
        let ledger = ledger(100)
            .register_session(request.identity.clone())
            .unwrap()
            .next;
        let ledger = ledger.reserve(&request, 100).unwrap().next;
        let ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
        let breach = observed.is_some();
        let result = ledger
            .settle(&Settlement {
                attempt: command(&request),
                usage: observed,
            })
            .unwrap();
        assert_eq!(result.next.meters.uncertain.tokens, 30);
        assert_eq!(result.value.breach, breach);
        assert_eq!(result.next.phase == AccountPhase::Frozen, breach);
    }
}

#[test]
fn uid_reparenting_and_authorization_replays_are_not_new_budget_accounts() {
    let ledger = ledger(100);
    let mut different = task("root", None, 100);
    different.task.name = "same-uid-different-name".into();
    assert!(ledger.register_task(different).is_err());
    let mut child = task("child", Some("root"), 100);
    child.root_task_uid = "other-root".into();
    assert!(ledger.register_task(child).is_err());
    let request = request("root", "pod", 1);
    let ledger = ledger
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    let mut replay = request.clone();
    replay.account_uid = "new-account".into();
    assert!(ledger.reserve(&replay, 100).is_err());
    replay = request;
    replay.identity.authorization_digest = format!("sha256:{}", "b".repeat(64));
    assert!(ledger.reserve(&replay, 100).is_err());
}

#[test]
fn monetary_axis_is_separate_and_zero_remains_unbounded() {
    assert_eq!(
        Limits {
            tokens: Some(0),
            usd_micros: Some(0)
        }
        .normalized(),
        Limits::default()
    );
    let mut ledger = ledger(100);
    ledger.limits.usd_micros = Some(4);
    ledger
        .nodes
        .get_mut("root")
        .unwrap()
        .authority
        .limits
        .usd_micros = Some(4);
    let request = request("root", "pod", 1);
    let ledger = ledger
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    assert!(matches!(
        ledger.reserve(&request, 100),
        Err(BudgetError::Exhausted)
    ));
}

#[test]
fn corrupted_balances_or_missing_active_rows_fail_instead_of_resetting() {
    let request = request("root", "pod", 1);
    let ledger = ledger(100)
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    let mut ledger = ledger.reserve(&request, 100).unwrap().next;
    ledger.meters.reserved.tokens = 0;
    assert!(ledger.validate().is_err());
    ledger.meters.reserved.tokens = 30;
    ledger.attempts.clear();
    assert!(ledger.validate().is_err());
}

#[test]
fn team_uid_is_the_lifetime_account_even_for_new_principal_task_uids() {
    let root = RootIdentity {
        kind: RootKind::KarsTeam,
        resource: resource("team", "team-uid"),
        workspace_uid: "workspace-uid".into(),
        cluster_uid: "cluster-uid".into(),
    };
    let mut ledger = Ledger::new(
        "account-uid".into(),
        root,
        Limits {
            tokens: Some(50),
            usd_micros: Some(1000),
        },
    )
    .unwrap();
    for uid in ["principal-one", "principal-two"] {
        let mut authority = task(uid, None, 50);
        authority.root_task_uid = uid.into();
        ledger = ledger.register_task(authority).unwrap().next;
        let request = request(uid, uid, 1);
        ledger = ledger
            .register_session(request.identity.clone())
            .unwrap()
            .next;
        if uid == "principal-one" {
            ledger = ledger.reserve(&request, 100).unwrap().next;
            ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
            ledger = ledger.close_subtree(uid).unwrap().next;
        } else {
            assert!(matches!(
                ledger.reserve(&request, 100),
                Err(BudgetError::Exhausted)
            ));
        }
    }
}
