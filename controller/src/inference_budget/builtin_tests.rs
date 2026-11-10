// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget_contract::{Limits, ResourceIdentity, RootIdentity};

fn source(name: &str) -> (BuiltinFunding, KarsSandbox) {
    let mut sandbox = KarsSandbox::new(name, Default::default());
    sandbox.metadata.namespace = Some("workspace".into());
    sandbox.metadata.uid = Some("sandbox-uid".into());
    sandbox.metadata.resource_version = Some("1".into());
    sandbox.spec.runtime.kind = RuntimeKind::Hermes;
    let grant = BuiltinFunding {
        root: RootIdentity {
            kind: RootKind::KarsSandbox,
            resource: ResourceIdentity {
                namespace: "workspace".into(),
                name: name.into(),
                uid: "sandbox-uid".into(),
            },
            workspace_uid: "workspace-uid".into(),
            cluster_uid: "cluster-uid".into(),
        },
        limits: Limits {
            tokens: Some(100),
            usd_micros: None,
        },
        expires_at: "2030-01-01T00:00:00Z".into(),
        unbounded_tokens: false,
    };
    (grant, sandbox)
}

fn bridge_renewal_fixture() -> (
    crate::inference_budget_contract::ledger::TokenTopUp,
    k8s_openapi::api::core::v1::ConfigMap,
    KarsSandbox,
) {
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../shared/inference_budget/bridge-renewal-fixture.json"
    ))
    .unwrap();
    let grant: crate::inference_budget_contract::ledger::TokenTopUp =
        serde_json::from_value(value["grant"].clone()).unwrap();
    let original: BuiltinFunding = serde_json::from_value(
        value["ledger"]["nodes"]["sandbox-uid"]["authority"]["effectiveAuthorization"].clone(),
    )
    .unwrap();
    let (_, mut sandbox) = source("bridge-orchestrator");
    sandbox.spec.inference_budget_ref = Some(
        binding(
            &original,
            AccountReference {
                namespace: "workspace".into(),
                name: super::super::account::name_for_root(&grant.root),
                uid: grant.account_uid.clone(),
            },
        )
        .unwrap(),
    );
    let catalog = serde_json::from_value(json!({
        "metadata": {"name":"contracts", "namespace":"workspace", "uid":"config-uid", "resourceVersion":"1",
            "annotations":{"kars.azure.com/inference-budget-contracts":"v1"}},
        "data": {CONFIG_KEY: serde_json::to_string(&vec![original]).unwrap()}
    })).unwrap();
    (grant, catalog, sandbox)
}

#[test]
fn bridge_renewal_requires_unchanged_original_catalog_source_and_account_bindings() {
    let (grant, catalog, sandbox) = bridge_renewal_fixture();
    verify_bridge_renewal(&grant, &catalog, &sandbox, "workspace").unwrap();
    for fault in [
        "catalog-uid",
        "catalog-delete",
        "catalog-rv",
        "revoked",
        "changed-grant",
        "duplicate-grant",
        "source-uid",
        "source-delete",
        "source-owner",
        "source-name",
        "source-namespace",
        "source-rv",
        "sre-intent",
        "account-uid",
        "account-namespace",
        "account-name",
        "binding-digest",
        "grant-digest",
        "root",
        "expiry",
        "no-consent",
    ] {
        let mut proposed = grant.clone();
        let mut map = serde_json::to_value(&catalog).unwrap();
        let mut source = serde_json::to_value(&sandbox).unwrap();
        match fault {
            "catalog-uid" => map["metadata"]["uid"] = json!("replacement"),
            "catalog-delete" => {
                map["metadata"]["deletionTimestamp"] = json!("2026-10-20T00:00:00Z")
            }
            "catalog-rv" => map["metadata"]["resourceVersion"] = json!(""),
            "revoked" => map["data"][CONFIG_KEY] = json!("[]"),
            "changed-grant" | "duplicate-grant" => {
                let mut originals: Vec<serde_json::Value> =
                    serde_json::from_str(map["data"][CONFIG_KEY].as_str().unwrap()).unwrap();
                if fault == "changed-grant" {
                    originals[0]["limits"]["tokens"] = json!(101);
                } else {
                    originals.push(originals[0].clone());
                }
                map["data"][CONFIG_KEY] = json!(serde_json::to_string(&originals).unwrap());
            }
            "source-uid" => source["metadata"]["uid"] = json!("replacement"),
            "source-delete" => {
                source["metadata"]["deletionTimestamp"] = json!("2026-10-20T00:00:00Z")
            }
            "source-owner" => {
                source["metadata"]["ownerReferences"] = json!([{
                    "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask", "name":"owner", "uid":"owner-uid"
                }])
            }
            "source-name" => source["metadata"]["name"] = json!("another"),
            "source-namespace" => source["metadata"]["namespace"] = json!("another"),
            "source-rv" => source["metadata"]["resourceVersion"] = json!(""),
            "sre-intent" => {
                source["metadata"]["annotations"] =
                    json!({(super::super::first_install::ANNOTATION): "sre-intent"})
            }
            "account-uid" => {
                source["spec"]["inferenceBudgetRef"]["account"]["uid"] = json!("replacement")
            }
            "account-namespace" => {
                source["spec"]["inferenceBudgetRef"]["account"]["namespace"] = json!("another")
            }
            "account-name" => {
                source["spec"]["inferenceBudgetRef"]["account"]["name"] = json!("another")
            }
            "binding-digest" => {
                source["spec"]["inferenceBudgetRef"]["authorizationDigest"] =
                    json!(format!("sha256:{}", "a".repeat(64)))
            }
            "grant-digest" => {
                proposed
                    .nodes
                    .insert("sandbox-uid".into(), format!("sha256:{}", "a".repeat(64)));
            }
            "root" => proposed.root.cluster_uid = "replacement".into(),
            "expiry" => {
                proposed
                    .funding_renewal
                    .as_mut()
                    .unwrap()
                    .original_expires_at = "2021-01-01T00:00:00Z".into()
            }
            "no-consent" => {
                proposed
                    .funding_renewal
                    .as_mut()
                    .unwrap()
                    .bridge_reactivation = None
            }
            _ => unreachable!(),
        }
        assert!(
            verify_bridge_renewal(
                &proposed,
                &serde_json::from_value(map).unwrap(),
                &serde_json::from_value(source).unwrap(),
                "workspace"
            )
            .is_err(),
            "{fault}"
        );
    }
}

#[tokio::test]
async fn bridge_renewal_rechecks_live_source_without_sre_registration_on_each_call() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let server = MockServer::start().await;
    let config = crate::kube_client_config::preserve_request_semantics(kube::Config::new(
        server.uri().parse().unwrap(),
    ));
    let client = Client::try_from(config).unwrap();
    let settings = Settings {
        accounting_namespace: "workspace".into(),
        catalog_name: "contracts".into(),
        address: "0.0.0.0:9447".into(),
        tls_secret: "budget-tls".into(),
        router_image_digest: format!("sha256:{}", "a".repeat(64)),
        first_install_router_digest: None,
    };
    let (grant, catalog, source) = bridge_renewal_fixture();
    let grants = std::collections::BTreeMap::from([(grant.id.clone(), grant.clone())]);
    for change in [
        "none",
        "replacement",
        "deleting",
        "unavailable",
        "stale-root",
        "stale-account",
    ] {
        server.reset().await;
        let mut current = source.clone();
        if change == "replacement" {
            current.metadata.uid = Some("replacement".into());
        }
        if change == "deleting" {
            current.metadata.deletion_timestamp =
                Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                    k8s_openapi::jiff::Timestamp::now(),
                ));
        }
        let response = if change == "unavailable" {
            ResponseTemplate::new(503).set_body_json(json!({"apiVersion":"v1", "kind":"Status",
                "status":"Failure", "reason":"ServiceUnavailable", "message":"unavailable", "code":503}))
        } else {
            ResponseTemplate::new(200).set_body_json(&current)
        };
        Mock::given(method("GET")).and(path("/apis/kars.azure.com/v1alpha1/namespaces/workspace/karssandboxes/bridge-orchestrator"))
            .respond_with(response).expect(1).mount(&server).await;
        let mut root = grant.root.clone();
        if change == "stale-root" {
            root.resource.uid = "replacement".into();
        }
        let account = if change == "stale-account" {
            "replacement"
        } else {
            grant.account_uid.as_str()
        };
        let result = super::super::first_install::verify_renewal_intents(
            &client, &settings, &catalog, &grants, &root, account,
        )
        .await;
        assert_eq!(result.is_ok(), change == "none", "{change}");
        server.verify().await;
    }
}

#[test]
fn builtin_operator_intent_rejects_ambiguity_unbounded_limits_and_non_builtin_roots() {
    let (grant, _) = source("sre");
    assert_eq!(
        parse_config(&json!([grant]).to_string()).unwrap(),
        vec![grant.clone()]
    );
    assert!(parse_config(&json!([grant, grant]).to_string()).is_err());
    assert!(parse_config("{}").is_err());
    for change in [
        "unknown", "zero", "missing", "overflow", "kind", "name", "expiry",
    ] {
        let mut value = json!([grant]);
        match change {
            "unknown" => value[0]["dailyTokens"] = json!(100),
            "zero" => value[0]["limits"]["tokens"] = json!(0),
            "missing" => value[0]["limits"] = json!({"usdMicros": 100}),
            "overflow" => value[0]["limits"]["tokens"] = json!(u64::MAX),
            "kind" => value[0]["root"]["kind"] = json!("KarsTask"),
            "name" => value[0]["root"]["resource"]["name"] = json!("another-actor"),
            "expiry" => value[0]["expiresAt"] = json!("tomorrow"),
            _ => unreachable!(),
        }
        assert!(parse_config(&value.to_string()).is_err(), "{change}");
    }
}

#[tokio::test]
async fn builtin_funding_reads_current_operator_configuration_and_refuses_read_failures() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let server = MockServer::start().await;
    let config = crate::kube_client_config::preserve_request_semantics(kube::Config::new(
        server.uri().parse().unwrap(),
    ));
    let client = Client::try_from(config).unwrap();
    let settings = Settings {
        accounting_namespace: "workspace".into(),
        catalog_name: "contracts".into(),
        address: "0.0.0.0:9447".into(),
        tls_secret: "budget-tls".into(),
        router_image_digest: format!("sha256:{}", "a".repeat(64)),
        first_install_router_digest: None,
    };
    let (grant, sandbox) = source("sre");
    for mutation in [
        "initial",
        "changed",
        "removed",
        "absent",
        "invalid",
        "ambiguous",
        "uid",
        "resource-version",
        "annotation",
        "deleting",
        "unavailable",
    ] {
        server.reset().await;
        let mut current = grant.clone();
        if mutation == "changed" {
            current.limits.tokens = Some(200);
        }
        let mut config = json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {
                "name": "contracts", "namespace": "workspace", "uid": "config-uid",
                "resourceVersion": "7",
                "annotations": {"kars.azure.com/inference-budget-contracts": "v1"}
            },
            "data": {CONFIG_KEY: serde_json::to_string(&vec![current.clone()]).unwrap()}
        });
        match mutation {
            "removed" => config["data"][CONFIG_KEY] = json!("[]"),
            "absent" => config["data"] = json!({}),
            "invalid" => config["data"][CONFIG_KEY] = json!("{"),
            "ambiguous" => {
                config["data"][CONFIG_KEY] =
                    json!(serde_json::to_string(&vec![grant.clone(), grant.clone()]).unwrap());
            }
            "uid" => config["metadata"]["uid"] = json!(""),
            "resource-version" => config["metadata"]["resourceVersion"] = json!(""),
            "annotation" => config["metadata"]["annotations"] = json!({}),
            "deleting" => {
                config["metadata"]["deletionTimestamp"] = json!("2026-10-13T00:00:00Z");
            }
            _ => {}
        }
        let response = if mutation == "unavailable" {
            ResponseTemplate::new(503).set_body_json(json!({
                "apiVersion": "v1", "kind": "Status", "status": "Failure",
                "reason": "ServiceUnavailable", "message": "unavailable", "code": 503
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(config)
        };
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/workspace/configmaps/contracts"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let result = configured_from_store(&client, &settings, &sandbox).await;
        match mutation {
            "initial" | "changed" => assert_eq!(result.unwrap(), Some(current)),
            "removed" | "absent" => assert_eq!(result.unwrap(), None),
            _ => assert!(result.is_err(), "{mutation}"),
        }
        server.verify().await;
    }
}

#[test]
fn builtin_source_is_never_adopted_by_name_or_repurposed_as_a_task() {
    for name in ["sre", "bridge-orchestrator"] {
        let (grant, sandbox) = source(name);
        assert!(verify_source(&grant, &sandbox, 100).is_ok());
        for field in ["uid", "namespace", "name"] {
            let mut changed = sandbox.clone();
            match field {
                "uid" => changed.metadata.uid = Some("replacement".into()),
                "namespace" => changed.metadata.namespace = Some("foreign".into()),
                "name" => changed.metadata.name = Some("other".into()),
                _ => unreachable!(),
            }
            assert!(verify_source(&grant, &changed, 100).is_err());
        }
        let mut owned = sandbox;
        owned.metadata.owner_references = Some(vec![
            serde_json::from_value(json!({
                "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
                "name":"task", "uid":"real-task-uid", "controller":true
            }))
            .unwrap(),
        ]);
        assert!(verify_source(&grant, &owned, 100).is_err());
    }
    let (grant, mut sandbox) = source("sre");
    sandbox.spec.runtime.kind = RuntimeKind::OpenClaw;
    assert!(verify_source(&grant, &sandbox, 100).is_err());
}

#[tokio::test]
async fn builtin_sre_requires_the_current_registered_source_and_privacy_epoch() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let server = MockServer::start().await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let (_, sandbox) = source("sre");
    let registration: crate::sre_registration::KarsSRERegistration =
        serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSRERegistration",
            "metadata":{"name":"canonical","uid":"registration-uid","generation":1},
            "spec":{
                "controller":{
                    "namespace":{"name":"workspace","uid":"workspace-uid"},
                    "deployment":{"name":"kars-controller","uid":"controller-uid"},"release":"kars"
                },
                "sandbox":{"namespace":"workspace","name":"sre","uid":"sandbox-uid"},
                "runtimeNamespace":{"name":"kars-sre","uid":"runtime-uid"},
                "enabled":true
            }
        }))
        .unwrap();
    let epoch = Some(registration.epoch());
    for mutation in ["none", "uid", "disabled", "missing"] {
        server.reset().await;
        let mut changed = registration.clone();
        match mutation {
            "uid" => changed.spec.sandbox.uid = "replacement".into(),
            "disabled" => changed.spec.enabled = false,
            _ => {}
        }
        let response = if mutation == "missing" {
            ResponseTemplate::new(404).set_body_json(json!({
                "apiVersion":"v1","kind":"Status","status":"Failure",
                "reason":"NotFound","message":"missing","code":404
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(changed)
        };
        Mock::given(method("GET"))
            .and(path(
                "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical",
            ))
            .respond_with(response)
            .mount(&server)
            .await;
        assert_eq!(
            verify_registration(&client, &sandbox, "runtime-uid", &epoch)
                .await
                .is_ok(),
            mutation == "none",
        );
        assert!(
            verify_registration(&client, &sandbox, "runtime-uid", &None)
                .await
                .is_err()
        );
        assert!(
            verify_registration(&client, &sandbox, "replacement", &epoch)
                .await
                .is_err()
        );
    }
}

#[test]
fn builtin_migration_refuses_unknown_or_calendar_only_history() {
    let (_, sandbox) = source("sre");
    let mut namespace = Namespace::default();
    assert!(verify_fresh_history(&sandbox, &namespace, false).is_err());
    namespace.annotations_mut().insert(
        super::super::calendar::INITIALIZATION_ANNOTATION.into(),
        "sandbox-uid".into(),
    );
    assert!(verify_fresh_history(&sandbox, &namespace, false).is_ok());
    assert!(verify_fresh_history(&sandbox, &namespace, true).is_err());
    namespace.annotations_mut().insert(
        crate::inference_budget_contract::calendar::ACCOUNT_ANNOTATION.into(),
        "old-calendar".into(),
    );
    assert!(verify_fresh_history(&sandbox, &namespace, false).is_err());
}

#[test]
fn builtin_authorization_is_immutable_expiring_and_keeps_settlement_identity() {
    let (grant, mut sandbox) = source("sre");
    let authority = authority(&grant).unwrap();
    let mut ledger = Ledger::new("account-uid".into(), grant.root.clone(), grant.limits)
        .unwrap()
        .register_task(authority.clone())
        .unwrap()
        .next;
    assert!(verify_grant(&grant, &ledger, &sandbox, 100).is_ok());
    assert!(verify_grant(&grant, &ledger, &sandbox, 1_893_456_000).is_err());
    for field in ["expiry", "limit", "cluster", "workspace"] {
        let mut changed = grant.clone();
        match field {
            "expiry" => changed.expires_at = "2031-01-01T00:00:00Z".into(),
            "limit" => changed.limits.tokens = Some(200),
            "cluster" => changed.root.cluster_uid = "new-cluster".into(),
            "workspace" => changed.root.workspace_uid = "new-workspace".into(),
            _ => unreachable!(),
        }
        assert!(
            verify_grant(&changed, &ledger, &sandbox, 100).is_err(),
            "{field}"
        );
    }
    sandbox.spec.inference_budget_ref = Some(
        binding(
            &grant,
            AccountReference {
                namespace: "accounting".into(),
                name: super::super::account::name_for_root(&grant.root),
                uid: "account-uid".into(),
            },
        )
        .unwrap(),
    );
    let identity = ExecutionIdentity {
        task_uid: "sandbox-uid".into(),
        authorization_digest: authority.authorization_digest,
        sandbox: grant.root.resource.clone(),
        runtime_namespace_uid: "runtime-uid".into(),
        pod_name: "router".into(),
        pod_uid: "pod-uid".into(),
    };
    ledger = ledger
        .register_session(identity.clone())
        .unwrap()
        .next
        .close_account()
        .unwrap()
        .next;
    assert!(verify_binding(&ledger, &sandbox, &identity, false).is_ok());
    let mut replacement = identity.clone();
    replacement.pod_uid = "replacement".into();
    assert!(verify_binding(&ledger, &sandbox, &replacement, false).is_err());
    sandbox
        .spec
        .inference_budget_ref
        .as_mut()
        .unwrap()
        .account
        .uid = "new-account".into();
    assert!(verify_binding(&ledger, &sandbox, &identity, false).is_err());
}
