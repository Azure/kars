// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn team(limit: u64) -> Ledger {
    let mut value = ledger(limit);
    value.root.kind = RootKind::KarsTeam;
    value
}

fn grant(ledger: &Ledger) -> TokenTopUp {
    let uid = ledger.nodes.keys().next().unwrap().clone();
    TokenTopUp {
        id: "topup-1".into(),
        funding_renewal: None,
        config_uid: "config-uid".into(),
        account_uid: ledger.account_uid.clone(),
        root: ledger.root.clone(),
        original_limits: ledger.limits,
        tokens: 100,
        expires_at: "2030-01-01T00:00:00Z".into(),
        nodes: BTreeMap::from([(
            uid.clone(),
            ledger.nodes[&uid].authority.authorization_digest.clone(),
        )]),
    }
}

#[test]
fn token_top_up_preserves_all_liability_and_replay_fences() {
    let request = request("root", "pod", 1);
    let ledger = team(30)
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    let ledger = ledger.reserve(&request, 100).unwrap().next;
    let ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
    let ledger = ledger.commit_uncertain_before(200).unwrap().next;
    assert_eq!(ledger.meters.uncertain.tokens, 30);
    let grant = grant(&ledger);
    let updated = ledger.apply_token_top_up(&grant, 201).unwrap().next;
    assert_eq!(updated.limits, ledger.limits);
    assert_eq!(updated.nodes, ledger.nodes);
    assert_eq!(updated.meters, ledger.meters);
    assert_eq!(updated.sessions, ledger.sessions);
    assert_eq!(updated.attempts, ledger.attempts);
    assert_eq!(updated.effective_limits().unwrap().tokens, Some(130));
    assert_eq!(
        updated.effective_node_limits("root").unwrap().tokens,
        Some(130)
    );
    assert!(!updated.apply_token_top_up(&grant, 202).unwrap().changed);
    assert!(updated.begin_dispatch(&command(&request), 202).is_err());
    let next_request = ReserveRequest {
        sequence: 2,
        ..request
    };
    let reserved = updated.reserve(&next_request, 202).unwrap().next;
    assert_eq!(reserved.meters.uncertain.tokens, 30);
    assert_eq!(reserved.meters.reserved.tokens, 30);
    assert_eq!(
        reserved.effective_limits().unwrap().usd_micros,
        ledger.limits.usd_micros
    );
}

#[test]
fn token_top_up_does_not_widen_descendant_or_narrow_root_caps() {
    let ledger = team(30);
    let grant = grant(&ledger);
    let ledger = ledger
        .register_task(task("child", Some("root"), 30))
        .unwrap()
        .next;
    let ledger = ledger.apply_token_top_up(&grant, 100).unwrap().next;
    assert_eq!(
        ledger.effective_node_limits("child").unwrap().tokens,
        Some(30)
    );
    let request = request("child", "pod", 1);
    let ledger = ledger
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    let ledger = ledger.reserve(&request, 100).unwrap().next;
    assert!(matches!(
        ledger.reserve(
            &ReserveRequest {
                sequence: 2,
                ..request
            },
            101
        ),
        Err(BudgetError::Exhausted)
    ));
    let mut child_grant = grant.clone();
    child_grant.id = "child-topup".into();
    child_grant.nodes = BTreeMap::from([("child".into(), digest())]);
    assert!(matches!(
        ledger.apply_token_top_up(&child_grant, 100),
        Err(BudgetError::Authorization)
    ));
    let mut narrowed = team(100);
    narrowed
        .nodes
        .get_mut("root")
        .unwrap()
        .authority
        .limits
        .tokens = Some(50);
    assert!(matches!(
        narrowed.apply_token_top_up(&grant_for(&narrowed), 100),
        Err(BudgetError::Authorization)
    ));
}

fn grant_for(ledger: &Ledger) -> TokenTopUp {
    grant(ledger)
}

#[test]
fn token_top_up_rejects_changed_identity_limits_digest_expiry_and_overflow() {
    let ledger = team(30);
    let valid = grant(&ledger);
    for field in [
        "id",
        "config",
        "account",
        "cluster",
        "workspace",
        "root",
        "limits",
        "digest",
        "zero",
        "expired",
    ] {
        let mut invalid = valid.clone();
        match field {
            "id" => invalid.id = String::new(),
            "config" => invalid.config_uid = String::new(),
            "account" => invalid.account_uid = "replaced".into(),
            "cluster" => invalid.root.cluster_uid = "replaced".into(),
            "workspace" => invalid.root.workspace_uid = "replaced".into(),
            "root" => invalid.root.resource.uid = "replaced".into(),
            "limits" => invalid.original_limits.tokens = Some(31),
            "digest" => {
                invalid
                    .nodes
                    .insert("root".into(), format!("sha256:{}", "b".repeat(64)));
            }
            "zero" => invalid.tokens = 0,
            "expired" => invalid.expires_at = "1970-01-01T00:00:00Z".into(),
            _ => unreachable!(),
        }
        assert!(ledger.apply_token_top_up(&invalid, 100).is_err(), "{field}");
    }
    let funded = ledger.apply_token_top_up(&valid, 100).unwrap().next;
    let mut changed = valid;
    changed.tokens += 1;
    assert!(funded.apply_token_top_up(&changed, 100).is_err());
    let large = team(MAX_LEDGER_INTEGER);
    assert!(matches!(
        large.apply_token_top_up(&grant(&large), 100),
        Err(BudgetError::Overflow)
    ));
    for phase in [
        AccountPhase::Closing,
        AccountPhase::Closed,
        AccountPhase::Frozen,
    ] {
        let mut closed = ledger.clone();
        closed.phase = phase;
        assert!(matches!(
            closed.apply_token_top_up(&grant(&closed), 100),
            Err(BudgetError::Closed)
        ));
    }
    let mut corrupt = ledger.clone();
    corrupt.meters.reserved.tokens = 1;
    assert!(corrupt.apply_token_top_up(&grant(&corrupt), 100).is_err());
}

#[test]
fn token_top_up_settles_old_attempts_and_retains_bounded_receipts() {
    let request = request("root", "pod", 1);
    let original = team(30)
        .register_session(request.identity.clone())
        .unwrap()
        .next
        .reserve(&request, 100)
        .unwrap()
        .next
        .begin_dispatch(&command(&request), 101)
        .unwrap()
        .next;
    let mut funded = original
        .apply_token_top_up(&grant(&original), 102)
        .unwrap()
        .next;
    funded = funded
        .settle(&Settlement {
            attempt: command(&request),
            usage: Some(usage()),
        })
        .unwrap()
        .next;
    assert_eq!(funded.meters.settled.tokens, 8);
    assert_eq!(funded.meters.reserved.tokens, 0);
    assert_eq!(funded.sessions["pod"].closed_through, 1);
    assert!(funded.reserve(&request, 103).is_err());
    for index in 1..MAX_TOKEN_TOP_UPS {
        let mut additional = grant(&funded);
        additional.id = format!("additional-{index}");
        additional.tokens = 1;
        funded = funded.apply_token_top_up(&additional, 104).unwrap().next;
    }
    let mut excess = grant(&funded);
    excess.id = "excess".into();
    assert!(matches!(
        funded.apply_token_top_up(&excess, 105),
        Err(BudgetError::Capacity)
    ));
    assert_eq!(funded.meters.settled.tokens, 8);
    assert_eq!(funded.limits, original.limits);
    assert_eq!(
        funded.effective_limits().unwrap().usd_micros,
        original.limits.usd_micros
    );
    let decoded: Ledger = serde_json::from_slice(&serde_json::to_vec(&funded).unwrap()).unwrap();
    assert_eq!(decoded, funded);
    decoded.validate().unwrap();
}

#[test]
fn orchestrator_top_up_restores_capacity_without_resetting_usage_or_renewing_expiry() {
    let (mut ledger, mut request, original) = builtin_ledger("bridge-orchestrator");
    for sequence in 1..=2 {
        request.sequence = sequence;
        ledger = ledger.reserve(&request, 100).unwrap().next;
        ledger = ledger.begin_dispatch(&command(&request), 101).unwrap().next;
        ledger = ledger.commit_uncertain_before(200).unwrap().next;
    }
    request.sequence = 3;
    assert!(matches!(
        ledger.reserve(&request, 201),
        Err(BudgetError::Exhausted)
    ));
    let authorization = grant(&ledger);
    let funded = ledger.apply_token_top_up(&authorization, 201).unwrap().next;
    assert_eq!(funded.limits, ledger.limits);
    assert_eq!(funded.nodes, ledger.nodes);
    assert_eq!(funded.meters, ledger.meters);
    assert_eq!(funded.sessions, ledger.sessions);
    assert_eq!(funded.attempts, ledger.attempts);
    assert_eq!(funded.effective_limits().unwrap().tokens, Some(165));
    assert!(
        !funded
            .apply_token_top_up(&authorization, 202)
            .unwrap()
            .changed
    );
    let reserved = funded.reserve(&request, 202).unwrap().next;
    assert_eq!(reserved.meters.uncertain.tokens, 60);
    assert_eq!(reserved.meters.reserved.tokens, 30);
    assert!(funded.reserve(&request, 500).is_err());
    assert!(ledger.apply_token_top_up(&authorization, 500).is_err());
    assert!(funded.apply_token_top_up(&renewal(&funded), 500).is_err());
    assert!(funded.validate_funding_expiry(&original, 500).is_err());
    let decoded: Ledger = serde_json::from_slice(&serde_json::to_vec(&funded).unwrap()).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, funded);
    let mut foreign = authorization.clone();
    foreign.root.resource.uid = "replacement".into();
    assert!(ledger.apply_token_top_up(&foreign, 202).is_err());
    foreign.root.resource.name = "ordinary-sandbox".into();
    assert!(foreign.validate(202).is_err());
}

fn renewal(ledger: &Ledger) -> TokenTopUp {
    let mut value = grant(ledger);
    value.id = "renewal-1".into();
    value.expires_at = "1970-01-01T00:16:40Z".into();
    value.funding_renewal = Some(FundingRenewal {
        bridge_reactivation: None,
        registration_uid: "registration-uid".into(),
        intent_uid: "intent-uid".into(),
        intent_digest: "a".repeat(64),
        original_expires_at: "1970-01-01T00:08:20Z".into(),
        previous_top_up_ids: ledger.token_top_ups.keys().cloned().collect(),
    });
    value
}

#[test]
fn explicit_sre_renewal_preserves_accounting_and_covers_exact_expired_receipts() {
    let (ledger, mut request, original) = builtin_ledger("sre");
    let mut old = grant(&ledger);
    old.expires_at = original.expires_at.clone();
    let before = ledger
        .apply_token_top_up(&old, 100)
        .unwrap()
        .next
        .reserve(&request, 101)
        .unwrap()
        .next
        .begin_dispatch(&command(&request), 102)
        .unwrap()
        .next
        .commit_uncertain_before(400)
        .unwrap()
        .next;
    assert_eq!(before.meters.uncertain.tokens, 30);
    assert!(before.reserve(&request, 500).is_err());
    let authorization = renewal(&before);
    let after = before.apply_token_top_up(&authorization, 500).unwrap().next;
    assert_eq!(after.nodes, before.nodes);
    assert_eq!(after.sessions, before.sessions);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.meters, before.meters);
    assert_eq!(after.limits, before.limits);
    assert_eq!(after.token_top_ups[&old.id], old);
    assert_eq!(after.effective_limits().unwrap().tokens, Some(265));
    assert!(!after.apply_token_top_up(&old, 501).unwrap().changed);
    assert!(
        !after
            .apply_token_top_up(&authorization, 501)
            .unwrap()
            .changed
    );
    let replay = after.reserve(&request, 501).unwrap();
    assert!(!replay.changed);
    assert_eq!(replay.next, after);
    assert_eq!(replay.value.phase, AttemptPhase::Uncertain);
    assert!(matches!(
        after.begin_dispatch(&command(&request), 501),
        Err(BudgetError::AlreadyDispatched)
    ));
    request.sequence = 2;
    let reserved = after.reserve(&request, 501).unwrap().next;
    let begun = reserved
        .begin_dispatch(&command(&request), 502)
        .unwrap()
        .next;
    assert!(reserved.begin_dispatch(&command(&request), 1000).is_err());
    assert!(after.reserve(&request, 1000).is_err());
    let settled = begun
        .settle(&Settlement {
            attempt: command(&request),
            usage: Some(usage()),
        })
        .unwrap()
        .next;
    assert_eq!(settled.meters.uncertain.tokens, 30);
    assert_eq!(settled.meters.settled.tokens, 8);
    settled.validate().unwrap();
    let decoded: Ledger = serde_json::from_slice(&serde_json::to_vec(&settled).unwrap()).unwrap();
    assert_eq!(decoded, settled);
    assert!(
        serde_json::to_value(&old)
            .unwrap()
            .get("fundingRenewal")
            .is_none()
    );
}

#[test]
fn renewal_rejects_partial_snapshots_conflicts_branches_and_invalid_expiries() {
    let (ledger, _, _) = builtin_ledger("sre");
    let mut old = grant(&ledger);
    old.expires_at = "1970-01-01T00:08:20Z".into();
    let ledger = ledger.apply_token_top_up(&old, 100).unwrap().next;
    let valid = renewal(&ledger);
    for field in [
        "missing-prior",
        "extra-prior",
        "duplicate",
        "self",
        "original",
        "expired",
        "registration",
        "intent",
        "digest",
    ] {
        let mut bad = valid.clone();
        let pins = bad.funding_renewal.as_mut().unwrap();
        match field {
            "missing-prior" => pins.previous_top_up_ids.clear(),
            "extra-prior" => pins.previous_top_up_ids.push("unknown".into()),
            "duplicate" => pins.previous_top_up_ids.push(old.id.clone()),
            "self" => pins.previous_top_up_ids.push(bad.id.clone()),
            "original" => pins.original_expires_at = "1970-01-01T00:08:19Z".into(),
            "expired" => bad.expires_at = "1970-01-01T00:08:20Z".into(),
            "registration" => pins.registration_uid.clear(),
            "intent" => pins.intent_uid.clear(),
            "digest" => pins.intent_digest = "not-a-digest".into(),
            _ => unreachable!(),
        }
        assert!(ledger.apply_token_top_up(&bad, 500).is_err(), "{field}");
    }
    let funded = ledger.apply_token_top_up(&valid, 500).unwrap().next;
    let mut changed = valid.clone();
    changed.tokens += 1;
    assert!(funded.apply_token_top_up(&changed, 600).is_err());
    let mut next = renewal(&funded);
    next.id = "renewal-2".into();
    assert!(funded.apply_token_top_up(&next, 600).is_err());
    next.expires_at = "1970-01-01T00:25:00Z".into();
    let again = funded.apply_token_top_up(&next, 1000).unwrap().next;
    assert_eq!(again.latest_funding_renewal().unwrap(), Some(&next));
    let mut branch = funded.clone();
    changed.id = "competing-renewal".into();
    branch.token_top_ups.insert(changed.id.clone(), changed);
    assert!(branch.validate().is_err());
    let mut unrelated = old;
    unrelated.id = "unapplied-expired".into();
    assert!(again.apply_token_top_up(&unrelated, 1001).is_err());
}

#[test]
fn renewal_never_reopens_closed_frozen_or_inactive_authority() {
    let (ledger, _, _) = builtin_ledger("sre");
    for phase in [
        AccountPhase::Closing,
        AccountPhase::Closed,
        AccountPhase::Frozen,
    ] {
        let mut stopped = ledger.clone();
        stopped.phase = phase;
        assert!(matches!(
            stopped.apply_token_top_up(&renewal(&ledger), 500),
            Err(BudgetError::Closed)
        ));
    }
    let mut stopped = ledger.clone();
    stopped
        .nodes
        .get_mut(&ledger.root.resource.uid)
        .unwrap()
        .active = false;
    assert!(stopped.apply_token_top_up(&renewal(&ledger), 500).is_err());
    let team = team(100);
    assert!(team.apply_token_top_up(&renewal(&team), 500).is_err());
}

#[test]
fn sre_top_up_keeps_original_funding_expiry_and_unpriced_accounting() {
    let (ledger, request, original) = builtin_ledger("sre");
    let extension = grant(&ledger);
    let funded = ledger.apply_token_top_up(&extension, 100).unwrap().next;
    assert_eq!(funded.effective_limits().unwrap().tokens, Some(165));
    assert_eq!(funded.effective_limits().unwrap().usd_micros, None);
    assert_eq!(funded.nodes, ledger.nodes);
    assert_eq!(
        funded.nodes[&ledger.root.resource.uid]
            .authority
            .effective_authorization,
        serde_json::to_value(&original).unwrap()
    );
    let expiry = chrono::DateTime::parse_from_rfc3339(&original.expires_at)
        .unwrap()
        .timestamp();
    assert!(matches!(
        funded.reserve(&request, expiry),
        Err(BudgetError::Expired)
    ));
    assert!(matches!(
        funded.apply_token_top_up(&extension, expiry),
        Err(BudgetError::Expired)
    ));
    funded.validate().unwrap();
}
