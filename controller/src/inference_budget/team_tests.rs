// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget::{account::*, store::Store};
use crate::inference_budget_contract::{
    AccountReference, Amounts, ExecutionIdentity, Limits, ResourceIdentity, RootIdentity,
    TaskAuthority,
    ledger::{Meters, Node},
};
use crate::kars_team::{KarsTeamSpec, KarsTeamStatus};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn fixture() -> (KarsTeam, KarsTask, KarsBudgetAccount, Settings) {
    let mut team = KarsTeam::new("team", KarsTeamSpec::default());
    team.metadata.namespace = Some("workspace".into());
    team.metadata.uid = Some("team-uid".into());
    team.metadata.generation = Some(1);
    team.spec.envelope.budget = Some(
        serde_json::from_value(json!({
            "scope":"GovernedInference", "tokens":86793
        }))
        .unwrap(),
    );
    let root = RootIdentity {
        kind: RootKind::KarsTeam,
        resource: ResourceIdentity {
            namespace: "workspace".into(),
            name: "team".into(),
            uid: "team-uid".into(),
        },
        workspace_uid: "workspace-uid".into(),
        cluster_uid: "cluster-uid".into(),
    };
    let pin = AccountReference {
        namespace: "kars-system".into(),
        name: name_for_root(&root),
        uid: "account-uid".into(),
    };
    team.status = Some(KarsTeamStatus {
        inference_budget_account: Some(pin.clone()),
        ..Default::default()
    });
    let mut principal: KarsTask = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
        "metadata":{"name":"principal", "namespace":"workspace", "uid":"root", "generation":1},
        "spec":{"objective":"principal", "envelope":team.spec.envelope},
        "status":{"phase":"Ready", "observedGeneration":1,
            "conditions":[{"type":"Ready", "status":"True", "reason":"Reconciled", "message":"ready", "lastTransitionTime":"2026-10-15T00:00:00Z"}],
            "inferenceBudget":{"scope":"GovernedInference", "account":pin, "root":root,
                "taskUid":"root", "parentTaskUid":null, "rootTaskUid":"root", "authorizationDigest":format!("sha256:{}", "a".repeat(64))}}
    })).unwrap();
    principal.status.as_mut().unwrap().envelope_digest = Some(principal.envelope_digest());
    assert!(crate::kars_task_reconciler::task_is_ready(&principal));
    let limits = Limits {
        tokens: Some(86793),
        usd_micros: None,
    };
    let mut ledger = Ledger::new("account-uid".into(), root.clone(), limits).unwrap();
    let own_root_tokens = 37488;
    let children = [
        ("builder", 9614),
        ("hermes-reviewer", 14876),
        ("integrator", 24815),
    ];
    let total = own_root_tokens + children.iter().map(|(_, tokens)| tokens).sum::<u64>();
    // Node meters are subtree-inclusive; the root already contains these roles.
    for (uid, tokens) in [("root", total)].into_iter().chain(children) {
        let effective = json!({"task":uid});
        let digest = format!(
            "sha256:{}",
            crate::providers::signing::sha256_hex(&serde_json::to_vec(&effective).unwrap())
        );
        ledger.nodes.insert(
            uid.into(),
            Node {
                authority: TaskAuthority {
                    task: ResourceIdentity {
                        namespace: "workspace".into(),
                        name: uid.into(),
                        uid: uid.into(),
                    },
                    parent_uid: (uid != "root").then(|| "root".into()),
                    root_task_uid: "root".into(),
                    authorization_digest: digest,
                    effective_authorization: effective,
                    limits,
                },
                active: true,
                meters: Meters {
                    settled: Amounts {
                        tokens,
                        usd_micros: 0,
                    },
                    ..Default::default()
                },
            },
        );
    }
    ledger.meters.settled.tokens = total;
    ledger.meters.unpriced_attempts = 15;
    ledger.validate().unwrap();
    let mut account = KarsBudgetAccount::new(
        &pin.name,
        KarsBudgetAccountSpec {
            scope: BudgetScope::GovernedInference,
            root,
            limits,
        },
    );
    account.metadata.namespace = Some(pin.namespace.clone());
    account.metadata.uid = Some(pin.uid);
    account.metadata.resource_version = Some("1".into());
    account.labels_mut().insert(MANAGED_BY.into(), OWNER.into());
    account
        .annotations_mut()
        .insert(BOOTSTRAP.into(), "sealed".into());
    account.status = Some(KarsBudgetAccountStatus {
        ledger: Some(ledger),
        ..Default::default()
    });
    Store::ledger(&account).unwrap();
    let settings = Settings {
        accounting_namespace: "kars-system".into(),
        catalog_name: "catalog".into(),
        address: "0.0.0.0:9447".into(),
        tls_secret: "tls".into(),
        router_image_digest: format!("sha256:{}", "a".repeat(64)),
        first_install_router_digest: None,
    };
    (team, principal, account, settings)
}

#[test]
fn team_top_up_preserves_settlement_and_original_envelope_but_restores_admission() {
    let (team, _, account, _) = fixture();
    let ledger = Store::ledger(&account).unwrap();
    assert!(matches!(
        admission(ledger),
        Err(StoreError::Ledger(BudgetError::Exhausted))
    ));
    let extension = crate::inference_budget_contract::ledger::TokenTopUp {
        funding_renewal: None,
        id: "extension-1".into(),
        config_uid: "config-uid".into(),
        account_uid: ledger.account_uid.clone(),
        root: ledger.root.clone(),
        original_limits: ledger.limits,
        tokens: 1000,
        expires_at: "2030-01-01T00:00:00Z".into(),
        nodes: std::collections::BTreeMap::from([(
            "root".into(),
            ledger.nodes["root"].authority.authorization_digest.clone(),
        )]),
    };
    let next = ledger.apply_token_top_up(&extension, 100).unwrap().next;
    admission(&next).unwrap();
    assert_eq!(next.meters, ledger.meters);
    assert_eq!(next.meters.unpriced_attempts, 15);
    assert_eq!(binding::limits(&team.spec.envelope).unwrap(), next.limits);
    assert_eq!(next.effective_limits().unwrap().tokens, Some(87793));
    let observed = budget_observation(
        team.status
            .as_ref()
            .unwrap()
            .inference_budget_account
            .as_ref()
            .unwrap(),
        &next,
    )
    .unwrap();
    assert_eq!(observed.original_limits.tokens, Some(86793));
    assert_eq!(observed.effective_limits.tokens, Some(87793));
    assert!(!observed.unbounded_tokens);
    assert_eq!(observed.settled.tokens, 86793);
    assert!(!observed.price_coverage_complete);
    assert_eq!(
        next.effective_node_limits("builder").unwrap(),
        ledger.nodes["builder"].authority.limits
    );
}

async fn mock_reads(
    principal: &KarsTask,
    account: &KarsBudgetAccount,
    account_code: u16,
) -> (MockServer, Client) {
    let server = MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks/principal",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(principal))
        .mount(&server)
        .await;
    let response = if account_code == 200 {
        ResponseTemplate::new(200).set_body_json(account)
    } else {
        ResponseTemplate::new(account_code).set_body_json(json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "message":"unavailable", "reason":"Unavailable", "code":account_code}))
    };
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karsbudgetaccounts/{}",
            account.name_any()
        )))
        .respond_with(response)
        .mount(&server)
        .await;
    (server, client)
}

#[tokio::test]
async fn lifetime_observation_counts_roles_and_root_once_even_when_exhausted_or_closed() {
    let (team, principal, mut account, settings) = fixture();
    for phase in [
        AccountPhase::Active,
        AccountPhase::Closing,
        AccountPhase::Closed,
        AccountPhase::Frozen,
    ] {
        account
            .status
            .as_mut()
            .unwrap()
            .ledger
            .as_mut()
            .unwrap()
            .phase = phase;
        let (_server, client) = mock_reads(&principal, &account, 200).await;
        let observed = read_observation(&client, &team, "principal", &settings)
            .await
            .unwrap();
        assert_eq!(observed.settled_tokens, 86793);
        assert_eq!(observed.budget.phase, phase);
        assert_eq!(observed.budget.settled.tokens, 86793);
        assert_eq!(observed.budget.effective_limits.tokens, Some(86793));
        assert!(!observed.budget.unbounded_tokens);
        assert!(matches!(
            observed.admission,
            Err(StoreError::Ledger(
                BudgetError::Exhausted | BudgetError::Closed
            ))
        ));
    }
}

#[tokio::test]
async fn unbounded_observation_preserves_original_money_cap_and_exact_usage() {
    let (mut team, mut principal, mut account, settings) = fixture();
    team.spec.envelope.budget.as_mut().unwrap().usd_micros = Some(10000);
    principal.spec.envelope = team.spec.envelope.clone();
    principal.status.as_mut().unwrap().envelope_digest = Some(principal.envelope_digest());
    account.spec.limits.usd_micros = Some(10000);
    let ledger = account.status.as_mut().unwrap().ledger.as_mut().unwrap();
    ledger.limits.usd_micros = Some(10000);
    ledger.meters.unpriced_attempts = 0;
    let grant = crate::inference_budget_contract::ledger::UnboundedTokenAllowance {
        id: "beta".into(),
        config_uid: "catalog-uid".into(),
        account_uid: ledger.account_uid.clone(),
        root: ledger.root.clone(),
        original_limits: ledger.limits,
        ledger_digest: ledger.funding_renewal_state_digest().unwrap(),
        previous_token_top_up_ids: vec![],
    };
    *ledger = ledger.authorize_unbounded_tokens(&grant, 100).unwrap().next;
    ledger.meters.settled = Amounts {
        tokens: 9007199254740993,
        usd_micros: 50,
    };
    ledger.meters.uncertain = Amounts {
        tokens: 7,
        usd_micros: 2,
    };
    Store::ledger(&account).unwrap();
    let (_server, client) = mock_reads(&principal, &account, 200).await;
    let observed = read_observation(&client, &team, "principal", &settings)
        .await
        .unwrap();
    assert_eq!(observed.settled_tokens, 9007199254740993);
    assert!(observed.budget.unbounded_tokens);
    assert_eq!(observed.budget.original_limits.tokens, Some(86793));
    assert_eq!(observed.budget.effective_limits.tokens, None);
    assert_eq!(observed.budget.effective_limits.usd_micros, Some(10000));
    assert_eq!(observed.budget.settled.tokens, 9007199254740993);
    assert_eq!(observed.budget.uncertain.tokens, 7);
    assert_eq!(observed.budget.reserved.tokens, 0);
    assert!(observed.budget.price_coverage_complete);
    let projected = serde_json::to_value(&observed.budget).unwrap();
    for private in [
        "nodes",
        "sessions",
        "attempts",
        "unboundedTokensReceipt",
        "expiresAt",
    ] {
        assert!(projected.get(private).is_none());
    }
}

#[tokio::test]
async fn unsettled_charges_affect_admission_not_observed_settlement() {
    let (mut team, principal, mut account, settings) = fixture();
    team.spec.envelope.budget.as_mut().unwrap().tokens = Some(100000);
    account.spec.limits.tokens = Some(100000);
    account
        .status
        .as_mut()
        .unwrap()
        .ledger
        .as_mut()
        .unwrap()
        .limits
        .tokens = Some(100000);
    for uncertain in [0, 13207] {
        account
            .status
            .as_mut()
            .unwrap()
            .ledger
            .as_mut()
            .unwrap()
            .meters
            .uncertain
            .tokens = uncertain;
        let (_server, client) = mock_reads(&principal, &account, 200).await;
        let observed = read_observation(&client, &team, "principal", &settings)
            .await
            .unwrap();
        assert_eq!(observed.settled_tokens, 86793);
        assert_eq!(observed.budget.uncertain.tokens, uncertain);
        assert_eq!(observed.budget.reserved.tokens, 0);
        if uncertain == 0 {
            assert!(observed.admission.is_ok());
        } else {
            assert!(matches!(
                observed.admission,
                Err(StoreError::Ledger(BudgetError::Exhausted))
            ));
        }
    }
}

#[tokio::test]
async fn observation_requires_current_owned_principal_and_matching_protected_pin() {
    let (team, principal, account, settings) = fixture();
    for pointer in [
        "/status/inferenceBudget/root/resource/uid",
        "/status/inferenceBudget/root/resource/name",
        "/status/inferenceBudget/root/resource/namespace",
        "/status/inferenceBudget/account/namespace",
        "/status/inferenceBudget/account/name",
        "/status/inferenceBudget/account/uid",
        "/status/inferenceBudget/taskUid",
        "/status/envelopeDigest",
    ] {
        let mut stale = serde_json::to_value(&principal).unwrap();
        *stale.pointer_mut(pointer).unwrap() = json!("foreign");
        let stale = serde_json::from_value(stale).unwrap();
        let (_server, client) = mock_reads(&stale, &account, 200).await;
        assert!(
            read_observation(&client, &team, "principal", &settings)
                .await
                .is_err(),
            "{pointer}"
        );
    }
    let mut stale = principal.clone();
    stale.metadata.generation = Some(2);
    let (_server, client) = mock_reads(&stale, &account, 200).await;
    assert!(
        read_observation(&client, &team, "principal", &settings)
            .await
            .is_err()
    );
    let mut unpinned = team.clone();
    unpinned.status.as_mut().unwrap().inference_budget_account = None;
    let (_server, client) = mock_reads(&principal, &account, 200).await;
    assert!(
        read_observation(&client, &unpinned, "principal", &settings)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unavailable_foreign_unsealed_or_mismatched_account_never_produces_usage() {
    let (team, principal, account, settings) = fixture();
    for code in [404, 403, 503] {
        let (_server, client) = mock_reads(&principal, &account, code).await;
        assert!(
            read_observation(&client, &team, "principal", &settings)
                .await
                .is_err()
        );
    }
    for pointer in [
        "/metadata/uid",
        "/metadata/annotations/kars.azure.com~1inference-budget-bootstrap",
        "/spec/root/resource/uid",
        "/status/ledger/accountUid",
    ] {
        let mut invalid = serde_json::to_value(&account).unwrap();
        *invalid.pointer_mut(pointer).unwrap() = json!("foreign");
        let invalid = serde_json::from_value(invalid).unwrap();
        let (_server, client) = mock_reads(&principal, &invalid, 200).await;
        assert!(
            read_observation(&client, &team, "principal", &settings)
                .await
                .is_err(),
            "{pointer}"
        );
    }
    let mut changed = team.clone();
    changed.spec.envelope.budget.as_mut().unwrap().tokens = Some(100000);
    let (_server, client) = mock_reads(&principal, &account, 200).await;
    assert!(
        read_observation(&client, &changed, "principal", &settings)
            .await
            .is_err()
    );
    // No catalog/admission proof endpoint is served: observation cannot bypass it.
    assert!(
        observe_with_settings(&client, &team, "principal", &settings)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn session_capacity_blocks_admission_without_hiding_settlement() {
    let (mut team, principal, mut account, settings) = fixture();
    team.spec.envelope.budget.as_mut().unwrap().tokens = Some(100000);
    account.spec.limits.tokens = Some(100000);
    let ledger = account.status.as_mut().unwrap().ledger.as_mut().unwrap();
    ledger.limits.tokens = Some(100000);
    for index in 0..crate::inference_budget_contract::MAX_SESSIONS {
        let identity = ExecutionIdentity {
            task_uid: "root".into(),
            authorization_digest: ledger.nodes["root"].authority.authorization_digest.clone(),
            sandbox: ResourceIdentity {
                namespace: "workspace".into(),
                name: "sandbox".into(),
                uid: "sandbox-uid".into(),
            },
            runtime_namespace_uid: "runtime-uid".into(),
            pod_name: format!("pod-{index}"),
            pod_uid: format!("pod-{index}"),
        };
        *ledger = ledger.register_session(identity).unwrap().next;
    }
    let (_server, client) = mock_reads(&principal, &account, 200).await;
    let observed = read_observation(&client, &team, "principal", &settings)
        .await
        .unwrap();
    assert_eq!(observed.settled_tokens, 86793);
    assert!(matches!(
        observed.admission,
        Err(StoreError::Ledger(BudgetError::Capacity))
    ));
}
