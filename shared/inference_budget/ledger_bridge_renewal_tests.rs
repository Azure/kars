// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn fixture() -> (Ledger, TokenTopUp) {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("bridge-renewal-fixture.json")).unwrap();
    (
        serde_json::from_value(value["ledger"].clone()).unwrap(),
        serde_json::from_value(value["grant"].clone()).unwrap(),
    )
}

fn renewal(ledger: &Ledger, id: &str, expires_at: &str) -> TokenTopUp {
    let node = &ledger.nodes[&ledger.root.resource.uid];
    TokenTopUp {
        id: id.into(),
        config_uid: "config-uid".into(),
        account_uid: ledger.account_uid.clone(),
        root: ledger.root.clone(),
        original_limits: ledger.limits,
        tokens: 100,
        expires_at: expires_at.into(),
        nodes: BTreeMap::from([(
            ledger.root.resource.uid.clone(),
            node.authority.authorization_digest.clone(),
        )]),
        funding_renewal: Some(FundingRenewal {
            registration_uid: String::new(),
            intent_uid: String::new(),
            intent_digest: String::new(),
            original_expires_at: node.authority.effective_authorization["expiresAt"]
                .as_str()
                .unwrap()
                .into(),
            previous_top_up_ids: ledger.token_top_ups.keys().cloned().collect(),
            bridge_reactivation: Some(BridgeReactivation {
                closed_ledger_digest: ledger.funding_renewal_state_digest().unwrap(),
            }),
        }),
    }
}

#[test]
fn bridge_renewal_wire_fixture_matches_cli_and_preserves_entire_closed_history() {
    let (before, grant) = fixture();
    before.validate().unwrap();
    assert_eq!(
        before.funding_renewal_state_digest().unwrap(),
        "sha256:2e5ceb147247116976f9bb2a9e7c669172ab68cdaeaed2f1eb43153e8eb9d6bd"
    );
    let now = 1_792_454_400;
    let mutation = before.apply_token_top_up(&grant, now).unwrap();
    let mut expected = before.clone();
    expected.phase = AccountPhase::Active;
    expected.nodes.get_mut("sandbox-uid").unwrap().active = true;
    expected
        .token_top_ups
        .insert(grant.id.clone(), grant.clone());
    assert_eq!(mutation.next, expected);
    assert!(mutation.changed);
    assert!(!expected.apply_token_top_up(&grant, now).unwrap().changed);
    let decoded: Ledger = serde_json::from_slice(&serde_json::to_vec(&expected).unwrap()).unwrap();
    assert_eq!(decoded, expected);
    decoded.validate().unwrap();
    assert!(
        expected
            .register_session(before.sessions["2"].identity.clone())
            .is_err()
    );
    let mut fresh = before.sessions["2"].identity.clone();
    fresh.pod_uid = "fresh-pod-uid".into();
    fresh.pod_name = "bridge-fresh".into();
    let resumed = expected.register_session(fresh).unwrap().next;
    assert_eq!(resumed.sessions["2"], before.sessions["2"]);
    assert_eq!(resumed.sessions["10"], before.sessions["10"]);
    let closed_again = expected.close_account().unwrap().next;
    assert!(closed_again.apply_token_top_up(&grant, now).is_err());
}

#[test]
fn bridge_expiry_reactivation_keeps_settled_uncertain_unpriced_and_replay_fences() {
    let (ledger, mut request, original) = builtin_ledger("bridge-orchestrator");
    let mut old = renewal(&ledger, "old-topup", &original.expires_at);
    old.funding_renewal = None;
    let ledger = ledger.apply_token_top_up(&old, 100).unwrap().next;
    let ledger = ledger
        .reserve(&request, 101)
        .unwrap()
        .next
        .begin_dispatch(&command(&request), 102)
        .unwrap()
        .next
        .settle(&Settlement {
            attempt: command(&request),
            usage: Some(usage()),
        })
        .unwrap()
        .next;
    request.sequence = 2;
    let mut contract = request.quote.bounded().unwrap().contract.clone();
    contract.maximum_price = None;
    request.quote = contract
        .normalize(
            br#"{"model":"model","messages":[{"role":"user","content":"hello"}]}"#,
            100,
            false,
        )
        .unwrap()
        .1
        .into();
    let before = ledger
        .reserve(&request, 103)
        .unwrap()
        .next
        .begin_dispatch(&command(&request), 104)
        .unwrap()
        .next
        .close_account()
        .unwrap()
        .next;
    assert_eq!(before.meters.settled.tokens, 8);
    assert_eq!(before.meters.uncertain.tokens, 30);
    assert_eq!(before.meters.unpriced_attempts, 1);
    let grant = renewal(&before, "reactivation-1", "1970-01-01T00:16:40Z");
    let after = before.apply_token_top_up(&grant, 500).unwrap().next;
    let mut expected = before.clone();
    expected.phase = AccountPhase::Active;
    expected
        .nodes
        .get_mut(&before.root.resource.uid)
        .unwrap()
        .active = true;
    expected
        .token_top_ups
        .insert(grant.id.clone(), grant.clone());
    assert_eq!(after, expected);
    assert_eq!(after.token_top_ups[&old.id], old);
    assert!(after.begin_dispatch(&command(&request), 501).is_err());
    assert!(after.register_session(request.identity.clone()).is_err());
    assert!(after.reserve(&request, 501).is_err());
    let expired = after.close_account().unwrap().next;
    let next = renewal(&expired, "reactivation-2", "1970-01-01T00:25:00Z");
    assert!(expired.apply_token_top_up(&next, 999).is_err());
    let renewed = expired.apply_token_top_up(&next, 1000).unwrap().next;
    assert_eq!(renewed.latest_funding_renewal().unwrap(), Some(&next));
    assert_eq!(renewed.sessions, before.sessions);
    assert_eq!(renewed.attempts, before.attempts);
    assert_eq!(renewed.meters, before.meters);
}

#[test]
fn bridge_reactivation_rejects_stale_or_wrong_authority_and_preserves_sre_rules() {
    let (before, valid) = fixture();
    let now = 1_792_454_400;
    for fault in [
        "phase-frozen",
        "phase-closing",
        "phase-active",
        "active-node",
        "open-session",
        "liability",
        "replay",
        "account",
        "root",
        "digest",
        "prior-missing",
        "prior-duplicate",
        "sre-pin",
        "no-consent",
        "wrong-original",
        "expired",
        "wrong-node",
        "changed-prior",
        "extra-field",
    ] {
        let mut ledger = before.clone();
        let mut grant = valid.clone();
        match fault {
            "phase-frozen" => ledger.phase = AccountPhase::Frozen,
            "phase-closing" => ledger.phase = AccountPhase::Closing,
            "phase-active" => ledger.phase = AccountPhase::Active,
            "active-node" => ledger.nodes.get_mut("sandbox-uid").unwrap().active = true,
            "open-session" => ledger.sessions.get_mut("2").unwrap().closed = false,
            "liability" => {
                ledger.meters.settled.tokens += 1;
                ledger
                    .nodes
                    .get_mut("sandbox-uid")
                    .unwrap()
                    .meters
                    .settled
                    .tokens += 1;
            }
            "replay" => {
                ledger.sessions.get_mut("2").unwrap().closed_through = 0;
            }
            "account" => grant.account_uid = "replacement".into(),
            "root" => grant.root.resource.uid = "replacement".into(),
            "digest" => {
                grant
                    .funding_renewal
                    .as_mut()
                    .unwrap()
                    .bridge_reactivation
                    .as_mut()
                    .unwrap()
                    .closed_ledger_digest = format!("sha256:{}", "a".repeat(64))
            }
            "prior-missing" => grant
                .funding_renewal
                .as_mut()
                .unwrap()
                .previous_top_up_ids
                .clear(),
            "prior-duplicate" => grant
                .funding_renewal
                .as_mut()
                .unwrap()
                .previous_top_up_ids
                .push("topup-1".into()),
            "sre-pin" => {
                grant.funding_renewal.as_mut().unwrap().registration_uid = "registration".into()
            }
            "no-consent" => grant.funding_renewal.as_mut().unwrap().bridge_reactivation = None,
            "wrong-original" => {
                grant.funding_renewal.as_mut().unwrap().original_expires_at =
                    "2021-01-01T00:00:00Z".into()
            }
            "expired" => grant.expires_at = "2020-01-01T00:00:00Z".into(),
            "wrong-node" => {
                grant
                    .nodes
                    .insert("sandbox-uid".into(), format!("sha256:{}", "a".repeat(64)));
            }
            "changed-prior" => ledger.token_top_ups.get_mut("topup-1").unwrap().tokens += 1,
            "extra-field" => {
                let mut encoded = serde_json::to_value(&grant).unwrap();
                encoded["fundingRenewal"]["bridgeReactivation"]["bypass"] = json!(true);
                assert!(serde_json::from_value::<TokenTopUp>(encoded).is_err());
                continue;
            }
            _ => unreachable!(),
        }
        // Recompute consent for semantic faults so rejection is not merely a stale hash.
        if [
            "phase-frozen",
            "phase-closing",
            "phase-active",
            "active-node",
            "open-session",
        ]
        .contains(&fault)
        {
            grant
                .funding_renewal
                .as_mut()
                .unwrap()
                .bridge_reactivation
                .as_mut()
                .unwrap()
                .closed_ledger_digest = ledger.funding_renewal_state_digest().unwrap();
        }
        assert!(ledger.apply_token_top_up(&grant, now).is_err(), "{fault}");
    }
    assert!(before.apply_token_top_up(&valid, 1_500_000_000).is_err());
    let (sre, _, _) = builtin_ledger("sre");
    let sre = sre.close_account().unwrap().next;
    assert!(
        sre.apply_token_top_up(&renewal(&sre, "invalid", "2030-01-01T00:00:00Z"), now)
            .is_err()
    );
}

#[test]
fn bridge_reactivation_rejects_pending_reserved_or_inflight_liability_even_with_fresh_consent() {
    for dispatched in [false, true] {
        let (ledger, request, _) = builtin_ledger("bridge-orchestrator");
        let mut pending = ledger.reserve(&request, 100).unwrap().next;
        if dispatched {
            pending = pending
                .begin_dispatch(&command(&request), 101)
                .unwrap()
                .next;
        }
        pending.phase = AccountPhase::Closed;
        pending.nodes.get_mut("builtin-uid").unwrap().active = false;
        pending
            .sessions
            .get_mut(&request.identity.pod_uid)
            .unwrap()
            .closed = true;
        pending.validate().unwrap();
        let grant = renewal(&pending, "invalid", "2030-01-01T00:00:00Z");
        assert!(pending.apply_token_top_up(&grant, 500).is_err());
    }
}

#[test]
fn bridge_reactivation_never_unfreezes_historical_contract_breach() {
    let (ledger, request, _) = builtin_ledger("bridge-orchestrator");
    let mut excess = usage();
    excess.output_tokens = 21;
    let frozen = ledger
        .reserve(&request, 100)
        .unwrap()
        .next
        .begin_dispatch(&command(&request), 101)
        .unwrap()
        .next
        .settle(&Settlement {
            attempt: command(&request),
            usage: Some(excess),
        })
        .unwrap()
        .next;
    assert_eq!(frozen.phase, AccountPhase::Frozen);
    let closed = frozen.close_account().unwrap().next;
    assert!(
        closed
            .attempts
            .values()
            .any(|attempt| attempt.observed_breach.is_some())
    );
    let grant = renewal(&closed, "invalid", "2030-01-01T00:00:00Z");
    assert!(closed.apply_token_top_up(&grant, 500).is_err());
}
