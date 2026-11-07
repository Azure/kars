// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::json;

fn fixture() -> (Ledger, UnboundedTokenAllowance, ConfigMap) {
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../shared/inference_budget/bridge-renewal-fixture.json"
    ))
    .unwrap();
    let ledger: Ledger = serde_json::from_value(value["ledger"].clone()).unwrap();
    let grant = UnboundedTokenAllowance {
        id: "unbounded-beta".into(),
        config_uid: "catalog-uid".into(),
        account_uid: ledger.account_uid.clone(),
        root: ledger.root.clone(),
        original_limits: ledger.limits,
        ledger_digest: ledger.funding_renewal_state_digest().unwrap(),
        previous_token_top_up_ids: ledger.token_top_ups.keys().cloned().collect(),
    };
    let map = serde_json::from_value(json!({"metadata":{"uid":"catalog-uid"},
        "data":{CONFIG_KEY:serde_json::to_string(&vec![&grant]).unwrap()}}))
    .unwrap();
    (ledger, grant, map)
}

#[test]
fn unbounded_catalog_requires_unique_account_and_receipt_ids_and_catalog_uid() {
    let (_, grant, mut map) = fixture();
    assert_eq!(
        configured(&map).unwrap().get(&grant.account_uid),
        Some(&grant)
    );
    for case in [
        "same-account",
        "same-id",
        "wrong-catalog",
        "expiry",
        "unknown",
    ] {
        let mut other = grant.clone();
        let entries = match case {
            "same-account" => {
                other.id = "other".into();
                json!([grant, other])
            }
            "same-id" => {
                other.account_uid = "other".into();
                json!([grant, other])
            }
            "wrong-catalog" => {
                other.config_uid = "replacement".into();
                json!([other])
            }
            _ => {
                let mut value = json!(grant);
                value[case] = json!("not-supported");
                json!([value])
            }
        };
        map.data
            .as_mut()
            .unwrap()
            .insert(CONFIG_KEY.into(), entries.to_string());
        assert!(configured(&map).is_err(), "{case}");
    }
}

#[test]
fn unbounded_operator_receipt_is_non_expiring_but_revocation_and_mutation_deny_access() {
    let (ledger, grant, map) = fixture();
    let grants = configured(&map).unwrap();
    let allowed = apply(&ledger, &grants, 1_792_454_400).unwrap();
    assert_eq!(allowed.effective_limits().unwrap().tokens, None);
    assert_eq!(allowed.meters, ledger.meters);
    assert_eq!(allowed.sessions, ledger.sessions);
    assert_eq!(apply(&allowed, &grants, i64::MAX).unwrap(), allowed);
    assert!(validate_receipt(&allowed, &BTreeMap::new()).is_err());
    assert!(apply(&allowed, &BTreeMap::new(), i64::MAX).is_err());
    let mut changed = grants;
    changed.get_mut(&grant.account_uid).unwrap().id = "changed".into();
    assert!(validate_receipt(&allowed, &changed).is_err());
    assert!(apply(&allowed, &changed, i64::MAX).is_err());
    assert_eq!(apply(&ledger, &BTreeMap::new(), i64::MAX).unwrap(), ledger);
}

#[test]
fn record_only_reserve_and_begin_require_the_current_operator_receipt() {
    use crate::inference_budget_contract::{
        AttemptCommand, AttemptKey, ReserveRequest,
        tariffs::{AttemptQuote, Operation},
    };
    let (ledger, _, map) = fixture();
    let grants = configured(&map).unwrap();
    let now = 1_792_454_400;
    let allowed = apply(&ledger, &grants, now).unwrap();
    let mut identity = allowed.sessions.values().next().unwrap().identity.clone();
    identity.pod_uid = "record-only-pod".into();
    identity.pod_name = "record-only-pod".into();
    let allowed = allowed.register_session(identity.clone()).unwrap().next;
    let request = ReserveRequest {
        account_uid: allowed.account_uid.clone(),
        identity: identity.clone(),
        sequence: 1,
        wire_digest: format!("sha256:{}", "a".repeat(64)),
        quote: AttemptQuote::record_only(
            "provider",
            "https://models.example",
            "uncontracted",
            Operation::Responses,
        ),
    };
    assert!(
        apply(&allowed, &BTreeMap::new(), now)
            .and_then(|current| current.reserve(&request, now))
            .is_err()
    );
    let reserved = apply(&allowed, &grants, now)
        .unwrap()
        .reserve(&request, now)
        .unwrap()
        .next;
    let command = AttemptCommand {
        account_uid: allowed.account_uid.clone(),
        identity,
        key: AttemptKey {
            pod_uid: "record-only-pod".into(),
            sequence: 1,
        },
        wire_digest: request.wire_digest.clone(),
    };
    assert!(
        apply(&reserved, &BTreeMap::new(), now)
            .and_then(|current| current.begin_dispatch(&command, now))
            .is_err()
    );
    assert!(
        apply(&reserved, &grants, now)
            .unwrap()
            .begin_dispatch(&command, now)
            .is_ok()
    );
}

#[test]
fn unbounded_original_funding_must_match_the_retained_ledger_authority() {
    let (ledger, _, _) = fixture();
    let mut original: BuiltinFunding = serde_json::from_value(
        ledger.nodes[&ledger.root.resource.uid]
            .authority
            .effective_authorization
            .clone(),
    )
    .unwrap();
    // The fixture deliberately uses a synthetic authority digest; make the
    // source-bound ledger explicit before checking exact original identity.
    let mut bound = ledger;
    bound
        .nodes
        .get_mut(&bound.root.resource.uid)
        .unwrap()
        .authority = super::super::builtin::authority(&original).unwrap();
    verify_original(&bound, Some(&original)).unwrap();
    original.expires_at = "2035-01-01T00:00:00Z".into();
    assert!(verify_original(&bound, Some(&original)).is_err());
}
