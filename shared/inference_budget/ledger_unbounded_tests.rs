// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn allowance(ledger: &Ledger) -> UnboundedTokenAllowance {
    UnboundedTokenAllowance {
        id: "unbounded-beta".into(),
        config_uid: "config-uid".into(),
        account_uid: ledger.account_uid.clone(),
        root: ledger.root.clone(),
        original_limits: ledger.limits,
        ledger_digest: ledger.funding_renewal_state_digest().unwrap(),
        previous_token_top_up_ids: ledger.token_top_ups.keys().cloned().collect(),
    }
}

#[test]
fn cold_unbounded_builtin_grant_uses_existing_receipt_and_keeps_accounting() {
    let (mut current, mut request, mut funding) = builtin_ledger("sre");
    assert!(
        serde_json::to_value(&funding)
            .unwrap()
            .get("unboundedTokens")
            .is_none()
    );
    funding.limits = Limits::default();
    funding.expires_at.clear();
    funding.unbounded_tokens = true;
    funding.validate(i64::MAX).unwrap();
    let encoded = serde_json::to_value(&funding).unwrap();
    assert_eq!(encoded["unboundedTokens"], true);
    assert!(encoded.get("expiresAt").is_none());
    let mut authority = current.nodes["builtin-uid"].authority.clone();
    authority.limits = funding.limits;
    authority.effective_authorization = encoded;
    current = Ledger::new(
        current.account_uid.clone(),
        current.root.clone(),
        funding.limits,
    )
    .unwrap()
    .register_task(authority)
    .unwrap()
    .next
    .register_session(request.identity.clone())
    .unwrap()
    .next;
    let receipt = allowance(&current);
    current = current
        .authorize_unbounded_tokens(&receipt, 100)
        .unwrap()
        .next;
    assert!(
        current
            .allows_memory_service(&request.identity, i64::MAX)
            .unwrap()
    );
    for sequence in 1..=12 {
        request.sequence = sequence;
        current = current
            .reserve(&request, 100)
            .unwrap()
            .next
            .begin_dispatch(&command(&request), 101)
            .unwrap()
            .next
            .settle(&Settlement {
                attempt: command(&request),
                usage: Some(usage()),
            })
            .unwrap()
            .next;
    }
    assert_eq!(current.meters.settled.tokens, 96);
    assert_eq!(current.unbounded_tokens, Some(receipt));
    current.validate_funding_expiry(&funding, i64::MAX).unwrap();
    current.validate().unwrap();
    funding.limits.tokens = Some(1);
    assert!(funding.validate(100).is_err());
    funding.limits.tokens = None;
    funding.expires_at = "2999-01-01T00:00:00Z".into();
    assert!(funding.validate(100).is_err());
}

#[test]
fn cold_unbounded_builtin_requires_matching_explicit_funding() {
    for name in ["sre", "bridge-orchestrator"] {
        let (before, request, mut funding) = builtin_ledger(name);
        funding.limits = Limits::default();
        funding.expires_at.clear();
        funding.unbounded_tokens = true;
        let empty = Ledger::new(
            before.account_uid.clone(),
            before.root.clone(),
            funding.limits,
        )
        .unwrap();
        assert!(empty.register_session(request.identity.clone()).is_err());
        assert!(empty.reserve(&request, 100).is_err());
        assert!(empty.allows_memory_service(&request.identity, 100).is_err());
        let mut authority = before.nodes["builtin-uid"].authority.clone();
        authority.limits = funding.limits;
        authority.effective_authorization = serde_json::to_value(&funding).unwrap();
        let current = empty.register_task(authority.clone()).unwrap().next;
        let restored: Ledger =
            serde_json::from_value(serde_json::to_value(current).unwrap()).unwrap();
        restored.validate().unwrap();
        for invalid in [
            serde_json::json!({}),
            serde_json::to_value(BuiltinFunding {
                unbounded_tokens: false,
                ..funding.clone()
            })
            .unwrap(),
            serde_json::to_value(BuiltinFunding {
                limits: Limits {
                    tokens: Some(1),
                    usd_micros: None,
                },
                ..funding.clone()
            })
            .unwrap(),
            serde_json::to_value(BuiltinFunding {
                expires_at: "2999-01-01T00:00:00Z".into(),
                ..funding.clone()
            })
            .unwrap(),
            serde_json::to_value(BuiltinFunding {
                root: RootIdentity {
                    resource: resource(name, "different-uid"),
                    ..funding.root.clone()
                },
                ..funding.clone()
            })
            .unwrap(),
        ] {
            authority.effective_authorization = invalid.clone();
            assert!(empty.register_task(authority.clone()).is_err());
            let mut corrupted = restored.clone();
            corrupted
                .nodes
                .get_mut("builtin-uid")
                .unwrap()
                .authority
                .effective_authorization = invalid;
            assert!(corrupted.validate().is_err());
        }
    }
}

#[test]
fn scope_only_tasks_require_accounting_without_finite_caps_or_allowance_receipts() {
    let mut current = ledger(30);
    current.limits = Limits::default();
    current.nodes.get_mut("root").unwrap().authority.limits = Limits::default();
    let mut child = task("child", Some("root"), 10);
    child.limits = Limits::default();
    current = current.register_task(child).unwrap().next;
    assert!(current.unbounded_tokens.is_none());
    assert!(current.token_top_ups.is_empty());
    assert!(!current.effective_limits().unwrap().finite());
    for uid in ["root", "child"] {
        assert!(current.requires_enforcement(uid).unwrap());
        assert!(!current.effective_node_limits(uid).unwrap().finite());
    }
    assert!(current.requires_enforcement("unknown").is_err());
    let mut req = request("child", "pod", 1);
    current = current.register_session(req.identity.clone()).unwrap().next;
    for sequence in 1..=12 {
        req.sequence = sequence;
        current = current
            .reserve(&req, 100)
            .unwrap()
            .next
            .begin_dispatch(&command(&req), 101)
            .unwrap()
            .next
            .settle(&Settlement {
                attempt: command(&req),
                usage: Some(usage()),
            })
            .unwrap()
            .next;
    }
    assert_eq!(current.meters.settled.tokens, 96);
    assert_eq!(current.nodes["root"].meters.settled.tokens, 96);
    assert_eq!(current.nodes["child"].meters.settled.tokens, 96);
    assert_eq!(current.limits, Limits::default());
    current.validate().unwrap();
}

#[test]
fn scope_only_root_does_not_remove_descendant_token_or_currency_caps() {
    for limits in [
        Limits {
            tokens: Some(29),
            usd_micros: None,
        },
        Limits {
            tokens: None,
            usd_micros: Some(4),
        },
    ] {
        let mut current = ledger(30);
        current.limits = Limits::default();
        current.nodes.get_mut("root").unwrap().authority.limits = Limits::default();
        let mut child = task("child", Some("root"), 10);
        child.limits = limits;
        current = current.register_task(child).unwrap().next;
        assert!(current.requires_enforcement("child").unwrap());
        assert_eq!(current.effective_node_limits("child").unwrap(), limits);
        let req = request("child", "pod", 1);
        current = current.register_session(req.identity.clone()).unwrap().next;
        assert!(matches!(
            current.reserve(&req, 100),
            Err(BudgetError::Exhausted)
        ));
    }
}

#[test]
fn unbounded_tokens_meter_repeated_calls_above_original_caps_for_all_descendants() {
    let before = ledger(30)
        .register_task(task("child", Some("root"), 10))
        .unwrap()
        .next;
    let grant = allowance(&before);
    let mut current = before.authorize_unbounded_tokens(&grant, 100).unwrap().next;
    assert_eq!(current.nodes, before.nodes);
    assert_eq!(current.limits, before.limits);
    assert_eq!(current.effective_limits().unwrap().tokens, None);
    assert_eq!(current.effective_node_limits("child").unwrap().tokens, None);
    assert!(current.requires_enforcement("child").unwrap());
    let mut req = request("child", "pod", 1);
    current = current.register_session(req.identity.clone()).unwrap().next;
    for sequence in 1..=12 {
        req.sequence = sequence;
        current = current
            .reserve(&req, 100)
            .unwrap()
            .next
            .begin_dispatch(&command(&req), 101)
            .unwrap()
            .next
            .settle(&Settlement {
                attempt: command(&req),
                usage: Some(usage()),
            })
            .unwrap()
            .next;
    }
    assert_eq!(current.meters.settled.tokens, 96);
    assert_eq!(current.nodes["root"].meters.settled.tokens, 96);
    assert_eq!(current.nodes["child"].meters.settled.tokens, 96);
    assert_eq!(current.effective_limits().unwrap().usd_micros, Some(1000));
    current.validate().unwrap();
    let decoded: Ledger = serde_json::from_slice(&serde_json::to_vec(&current).unwrap()).unwrap();
    assert_eq!(decoded, current);
    assert!(
        !decoded
            .authorize_unbounded_tokens(&grant, i64::MAX)
            .unwrap()
            .changed
    );
    assert!(
        before
            .register_session(req.identity.clone())
            .unwrap()
            .next
            .reserve(&req, 100)
            .is_err()
    );
}

#[test]
fn unbounded_tokens_still_require_currency_enforcement_and_accounting() {
    let mut before = ledger(30);
    before.limits.usd_micros = Some(5);
    before
        .nodes
        .get_mut("root")
        .unwrap()
        .authority
        .limits
        .usd_micros = Some(5);
    let allowed = before
        .authorize_unbounded_tokens(&allowance(&before), 100)
        .unwrap()
        .next;
    let req = request("root", "pod", 1);
    let reserved = allowed
        .register_session(req.identity.clone())
        .unwrap()
        .next
        .reserve(&req, 100)
        .unwrap()
        .next;
    assert!(matches!(
        reserved.reserve(&ReserveRequest { sequence: 2, ..req }, 100),
        Err(BudgetError::Exhausted)
    ));
    assert_eq!(reserved.meters.reserved.usd_micros, 5);
    let mut token_only = ledger(30);
    token_only.limits.usd_micros = None;
    token_only
        .nodes
        .get_mut("root")
        .unwrap()
        .authority
        .limits
        .usd_micros = None;
    let token_only = token_only
        .authorize_unbounded_tokens(&allowance(&token_only), 100)
        .unwrap()
        .next;
    assert!(!token_only.effective_limits().unwrap().finite());
    assert!(token_only.requires_enforcement("root").unwrap());
}

#[test]
fn unbounded_funding_does_not_expire_or_erase_historical_topups() {
    for name in ["sre", "bridge-orchestrator"] {
        let (before, req, original) = builtin_ledger(name);
        let prior = TokenTopUp {
            id: "prior".into(),
            config_uid: "config-uid".into(),
            account_uid: before.account_uid.clone(),
            root: before.root.clone(),
            original_limits: before.limits,
            tokens: 10,
            expires_at: original.expires_at.clone(),
            funding_renewal: None,
            nodes: BTreeMap::from([(before.root.resource.uid.clone(), digest())]),
        };
        let before = before.apply_token_top_up(&prior, 100).unwrap().next;
        let allowed = before
            .authorize_unbounded_tokens(&allowance(&before), 600)
            .unwrap()
            .next;
        assert_eq!(allowed.token_top_ups, before.token_top_ups);
        assert_eq!(allowed.meters, before.meters);
        allowed
            .validate_funding_expiry(&original, i64::MAX)
            .unwrap();
        assert!(
            !allowed
                .apply_token_top_up(&prior, i64::MAX)
                .unwrap()
                .changed
        );
        let mut fresh = prior.clone();
        fresh.id = "new-finite".into();
        assert!(allowed.apply_token_top_up(&fresh, 100).is_err());
        assert!(before.reserve(&req, 600).is_err());
        assert!(allowed.reserve(&req, 600).is_ok());
    }
}

#[test]
fn unbounded_bridge_reactivation_preserves_closed_sessions_and_liabilities() {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("bridge-renewal-fixture.json")).unwrap();
    let before: Ledger = serde_json::from_value(value["ledger"].clone()).unwrap();
    let grant = allowance(&before);
    let after = before
        .authorize_unbounded_tokens(&grant, 1_792_454_400)
        .unwrap()
        .next;
    assert_eq!(after.phase, AccountPhase::Active);
    assert_eq!(after.meters, before.meters);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.sessions, before.sessions);
    assert!(
        after
            .register_session(before.sessions["2"].identity.clone())
            .is_err()
    );
    let closed = after.close_account().unwrap().next;
    assert!(closed.authorize_unbounded_tokens(&grant, i64::MAX).is_err());
}

#[test]
fn unbounded_allowance_rejects_stale_or_changed_pins_and_closed_non_bridge_accounts() {
    let before = ledger(30);
    let grant = allowance(&before);
    for field in [
        "account",
        "root",
        "workspace",
        "cluster",
        "limits",
        "digest",
        "prior",
        "config",
    ] {
        let mut bad = grant.clone();
        match field {
            "account" => bad.account_uid = "replaced".into(),
            "root" => bad.root.resource.uid = "replaced".into(),
            "workspace" => bad.root.workspace_uid = "replaced".into(),
            "cluster" => bad.root.cluster_uid = "replaced".into(),
            "limits" => bad.original_limits.tokens = Some(31),
            "digest" => bad.ledger_digest = format!("sha256:{}", "0".repeat(64)),
            "prior" => bad.previous_token_top_up_ids.push("missing".into()),
            "config" => bad.config_uid.clear(),
            _ => unreachable!(),
        }
        assert!(
            before.authorize_unbounded_tokens(&bad, 100).is_err(),
            "{field}"
        );
    }
    let changed = before
        .register_session(identity("root", "new-pod"))
        .unwrap()
        .next;
    assert!(changed.authorize_unbounded_tokens(&grant, 100).is_err());
    let after = before.authorize_unbounded_tokens(&grant, 100).unwrap().next;
    let mut mutated = grant;
    mutated.id = "replacement".into();
    assert!(after.authorize_unbounded_tokens(&mutated, 100).is_err());
    for before in [before, builtin_ledger("sre").0] {
        let closed = before.close_account().unwrap().next;
        assert!(
            closed
                .authorize_unbounded_tokens(&allowance(&closed), i64::MAX)
                .is_err()
        );
    }
}
