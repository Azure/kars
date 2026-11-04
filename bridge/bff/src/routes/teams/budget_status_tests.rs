// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};

fn fixture(unbounded: bool) -> (KarsTeam, chrono::DateTime<chrono::Utc>) {
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-20T00:00:00Z").unwrap().to_utc();
    let account = json!({"namespace":"kars-system","name":"inference-budget-team-uid","uid":"account-uid"});
    let settled = if unbounded { 9007199254740993_i64 } else { 123 };
    let mut effective = json!({"usdMicros":10000});
    if !unbounded { effective["tokens"] = json!(6000000); }
    let team = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTeam",
        "metadata":{"name":"team", "namespace":"work", "uid":"team-uid", "generation":1,
            "annotations":{"kars.azure.com/owner-sub":"owner"}},
        "spec":{"charter":"Review", "envelope":{"tier":1,"authorityCeiling":1,
            "budget":{"scope":"GovernedInference","tokens":5000000,"usdMicros":10000}}},
        "status":{"inferenceBudgetAccount":account, "observedGeneration":1,
            "tokensObservedAt":now.to_rfc3339(), "tokensSpentScope":"GovernedInferenceLifetime",
            "tokensSpentTotal":settled,
            "inferenceBudgetObservation":{
                "account":account,"team":{"namespace":"work","name":"team","uid":"team-uid"},
                "originalLimits":{"tokens":5000000,"usdMicros":10000},"effectiveLimits":effective,
                "phase":"Active","unboundedTokens":unbounded,"priceCoverageComplete":true,
                "settled":{"tokens":settled,"usdMicros":50},
                "reserved":{"tokens":0,"usdMicros":0},"uncertain":{"tokens":7,"usdMicros":2}
            }}
    })).unwrap();
    (team, now)
}

#[test]
fn projects_unbounded_receipt_without_erasing_history_currency_or_usage() {
    let (team, now) = fixture(true);
    let view = serde_json::to_value(project(&team, now).unwrap()).unwrap();
    assert_eq!(view["observed_at"], now.to_rfc3339());
    assert_eq!(view["state"], "available");
    assert_eq!(view["unbounded_tokens"], true);
    assert!(view["effective_tokens"].is_null());
    assert_eq!(view["effective_usd_micros"], "10000");
    assert_eq!(view["settled"]["tokens"], "9007199254740993");
    assert_eq!(view["uncertain"]["tokens"], "7");
    assert_eq!(view["reserved"]["tokens"], "0");
    assert_eq!(team.spec.envelope.budget.unwrap().tokens, Some(5000000));
    assert!(view.get("nodes").is_none());
    assert!(view.get("unboundedTokens").is_none());
}

#[test]
fn finite_and_currency_only_accounts_do_not_claim_an_unbounded_allowance() {
    let (mut team, now) = fixture(false);
    let view = serde_json::to_value(project(&team, now).unwrap()).unwrap();
    assert_eq!(view["effective_tokens"], "6000000");
    assert_eq!(view["unbounded_tokens"], false);
    team.spec.envelope.budget.as_mut().unwrap().tokens = Some(0);
    let observation = team.status.as_mut().unwrap().inference_budget_observation.as_mut().unwrap();
    observation["originalLimits"] = json!({"usdMicros":10000});
    observation["effectiveLimits"] = json!({"usdMicros":10000});
    let view = serde_json::to_value(project(&team, now).unwrap()).unwrap();
    assert!(view["effective_tokens"].is_null());
    assert_eq!(view["unbounded_tokens"], false);
    assert_eq!(view["effective_usd_micros"], "10000");
    team.spec.envelope.budget.as_mut().unwrap().scope = None;
    assert!(project(&team, now).is_err());
}

#[test]
fn rejects_replaced_cross_team_stale_missing_or_corrupt_observations() {
    let (team, now) = fixture(true);
    for (pointer, value) in [
        ("/metadata/uid", json!("replacement")),
        ("/metadata/namespace", json!("other")),
        ("/metadata/name", json!("other")),
        ("/metadata/generation", json!(2)),
        ("/metadata/deletionTimestamp", json!("2026-10-20T00:00:00Z")),
        ("/status/observedGeneration", Value::Null),
        ("/status/tokensObservedAt", json!("2026-10-19T23:54:59Z")),
        ("/status/tokensObservedAt", json!("2026-10-20T00:00:01Z")),
        ("/status/tokensObservedAt", json!("invalid")),
        ("/status/tokensSpentScope", json!("RetainedRootSynthesis")),
        ("/status/tokensSpentTotal", Value::Null),
        ("/status/inferenceBudgetAccount", Value::Null),
        ("/status/inferenceBudgetObservation", Value::Null),
        ("/status/inferenceBudgetObservation", json!("malformed")),
        ("/status/inferenceBudgetObservation/account/uid", json!("replacement")),
        ("/status/inferenceBudgetObservation/account/namespace", json!("other")),
        ("/status/inferenceBudgetObservation/account/name", json!("other")),
        ("/status/inferenceBudgetObservation/team/uid", json!("foreign-team")),
        ("/status/inferenceBudgetObservation/team/name", json!("foreign-team")),
        ("/status/inferenceBudgetObservation/team/namespace", json!("other")),
        ("/status/inferenceBudgetObservation/settled/tokens", json!(-1)),
        ("/status/inferenceBudgetObservation/settled/tokens", json!(10)),
        ("/status/inferenceBudgetObservation/effectiveLimits/tokens", json!(5000000)),
        ("/status/inferenceBudgetObservation/effectiveLimits/usdMicros", Value::Null),
        ("/status/inferenceBudgetObservation/originalLimits/tokens", json!(4000000)),
        ("/status/inferenceBudgetObservation/unboundedTokens", json!(false)),
        ("/spec/envelope/budget/tokens", json!(4000000)),
    ] {
        let mut changed = serde_json::to_value(&team).unwrap();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        changed.pointer_mut(parent).unwrap()[key] = value;
        let changed = serde_json::from_value(changed).unwrap();
        assert!(project(&changed, now).is_err(), "{pointer}");
        let view = serde_json::to_value(read(&changed, now)).unwrap();
        assert_eq!(view["state"], "unavailable", "{pointer}");
        assert!(view.get("settled").is_none());
    }
    assert!(project(&team, now + chrono::Duration::seconds(300)).is_ok());
    assert!(project(&team, now + chrono::Duration::seconds(301)).is_err());
}

#[test]
fn closed_accounts_and_unpriced_costs_are_reported_without_readiness_claims() {
    let (mut team, now) = fixture(true);
    team.spec.envelope.budget.as_mut().unwrap().usd_micros = None;
    let observation = team.status.as_mut().unwrap().inference_budget_observation.as_mut().unwrap();
    for field in ["originalLimits", "effectiveLimits"] {
        observation[field].as_object_mut().unwrap().remove("usdMicros");
    }
    observation["phase"] = json!("Closed");
    observation["priceCoverageComplete"] = json!(false);
    let view = serde_json::to_value(project(&team, now).unwrap()).unwrap();
    assert_eq!(view["account_phase"], "Closed");
    assert_eq!(view["price_coverage_complete"], false);
    assert!(view.get("ready").is_none());
}

#[tokio::test]
async fn owned_team_detail_uses_status_without_any_budget_account_permissions() {
    use axum::{body::Body, extract::{Extension, Path, State}, http::Request, response::Response};
    use std::sync::{Arc, Mutex};

    let (mut team, _) = fixture(true);
    team.spec.blueprint = Some(serde_json::from_value(json!({
        "githubBinding": {
            "grant": {"name": "workspace", "uid": "grant"},
            "connection": {"name": "connection", "uid": "connection-uid"},
            "repositories": ["owner/demo"], "write": true
        }
    })).unwrap());
    team.status.as_mut().unwrap().tokens_observed_at = Some(chrono::Utc::now().to_rfc3339());
    let team = serde_json::to_value(team).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let service = tower::service_fn(move |request: Request<kube::client::Body>| {
        assert_eq!(request.method(), "GET");
        let path = request.uri().path();
        recorded.lock().unwrap().push(path.to_string());
        let (code, response) = if path.contains("karsbudgetaccounts") {
            (403, json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","message":"No account read permission","code":403}))
        } else if path == "/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/team" {
            (200, team.clone())
        } else if path == "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace" {
            (200, json!({
                "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
                "metadata":{"name":"workspace","namespace":"work","uid":"grant","generation":1,"resourceVersion":"1"},
                "spec":{"enabled":true,"workspaceUid":"workspace","agentKeys":[],"integrationStores":[]},
                "status":{"phase":"Ready","observedGeneration":1,"sources":[],"legacySources":[]}
            }))
        } else if path == "/api/v1/namespaces/work" {
            (200, json!({
                "apiVersion":"v1","kind":"Namespace",
                "metadata":{"name":"work","uid":"workspace","resourceVersion":"1"}
            }))
        } else if ["/karstasks", "/karssandboxes", "/configmaps"].iter().any(|suffix| path.ends_with(suffix)) {
            (200, json!({"apiVersion":"v1","kind":"List","metadata":{},"items":[]}))
        } else {
            (404, json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"Not found","code":404}))
        };
        async move {
            Ok::<_, std::convert::Infallible>(Response::builder().status(code)
                .header("content-type", "application/json")
                .body(Body::from(response.to_string())).unwrap())
        }
    });
    let state = crate::state::AppState::for_test_client(kube::Client::new(service, "work"), "work");
    let result = super::super::queries::get_team(State(state), Extension(crate::auth::Principal {
        sub: "owner".into(), name: "Owner".into(), roles: vec![],
    }), Path(("work".into(), "team".into()))).await.unwrap();
    let view = serde_json::to_value(result.0).unwrap();
    assert_eq!(view["egress_mode"], "Strict", "Core governs Team tasks as Strict even when the blueprint field is absent");
    assert_eq!(view["git_write_repos"], json!(["owner/demo"]));
    assert_eq!(view["budget_status"]["state"], "available");
    assert_eq!(view["budget_status"]["unbounded_tokens"], true);
    assert_eq!(view["budget_status"]["settled"]["tokens"], "9007199254740993");
    assert!(!requests.lock().unwrap().iter().any(|path| path.contains("karsbudgetaccounts")));
}

#[tokio::test]
async fn team_authorization_precedes_any_budget_account_read() {
    use axum::{body::Body, extract::{Extension, Path, State}, http::Request, response::Response};
    let team = serde_json::to_value(fixture(true).0).unwrap();
    let service = tower::service_fn(move |request: Request<kube::client::Body>| {
        let team = team.clone();
        async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.uri().path(), "/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/team");
            Ok::<_, std::convert::Infallible>(Response::new(Body::from(team.to_string())))
        }
    });
    let state = crate::state::AppState::for_test_client(kube::Client::new(service, "work"), "work");
    let result = super::super::queries::get_team(State(state), Extension(crate::auth::Principal {
        sub: "not-owner".into(), name: "Other operator".into(), roles: vec!["operator".into()],
    }), Path(("work".into(), "team".into()))).await;
    assert!(matches!(result, Err(crate::error::AppError::NotFound)));
}
