// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};

#[tokio::test]
async fn scheduling_pause_preserves_dispatch_without_weakening_team_authority() {
    use crate::inference_budget_contract::{BudgetScope, Limits, RootIdentity, TaskAuthority};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for scenario in [
        "active",
        "paused",
        "stopped",
        "replaced-team",
        "wrong-account",
        "narrowed-budget",
    ] {
        let server = MockServer::start().await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let root: RootIdentity = serde_json::from_value(json!({
            "kind":"KarsTeam", "resource":{"namespace":"workspace","name":"team","uid":"team-uid"},
            "workspaceUid":"workspace-uid", "clusterUid":"cluster-uid"
        }))
        .unwrap();
        let mut task: KarsTask = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
            "metadata":{"name":"task","namespace":"workspace","uid":"task-uid","resourceVersion":"1","generation":1,
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","name":"team","uid":"team-uid","controller":true}]},
            "spec":{"objective":"Finish authorized work", "envelope":{"tier":2,"authorityCeiling":2,"delegationDepth":1},
                "blueprint":{"model":{"provider":"azure-openai","deployment":"fixture"}}, "execution":{"launch":true}}
        })).unwrap();
        let model = crate::kars_task::blueprint::controller_default_model();
        let authority = TaskAuthority {
            task: serde_json::from_value(
                json!({"namespace":"workspace","name":"task","uid":"task-uid"}),
            )
            .unwrap(),
            parent_uid: None,
            root_task_uid: "task-uid".into(),
            authorization_digest: task.envelope_digest(),
            effective_authorization: task.spec.authorization_configuration_with_model(&model),
            limits: Limits::default(),
        };
        let ledger = Ledger::new("account-uid".into(), root.clone(), Limits::default())
            .unwrap()
            .register_task(authority.clone())
            .unwrap()
            .next;
        let account_ref = json!({"namespace":"kars-system","name":"account","uid":"account-uid"});
        let account: KarsBudgetAccount = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsBudgetAccount",
            "metadata":{"name":"account","namespace":"kars-system","uid":"account-uid"},
            "spec":{"scope":BudgetScope::GovernedInference,"root":root,"limits":Limits::default()}
        }))
        .unwrap();
        task.status = Some(serde_json::from_value(json!({
            "phase":"Ready", "observedGeneration":1, "envelopeDigest":task.envelope_digest(),
            "conditions":[{"type":"Ready","status":"True","reason":"Fixture","message":"Fixture","lastTransitionTime":"2026-09-08T00:00:00Z"}],
            "inferenceBudget":{"scope":BudgetScope::GovernedInference,"account":account_ref,"root":root,
                "taskUid":"task-uid","parentTaskUid":null,"rootTaskUid":"task-uid","authorizationDigest":authority.authorization_digest}
        })).unwrap());
        let mut team: crate::kars_team::KarsTeam = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTeam",
            "metadata":{"name":"team","namespace":"workspace","uid":"team-uid","resourceVersion":"1"},
            "spec":{"charter":"Finish authorized work","envelope":task.spec.envelope,"paused":scenario != "active"},
            "status":{"inferenceBudgetAccount":account_ref}
        })).unwrap();
        match scenario {
            "stopped" => task.spec.execution.as_mut().unwrap().launch = false,
            "replaced-team" => team.metadata.uid = Some("replacement-uid".into()),
            "wrong-account" => {
                team.status
                    .as_mut()
                    .unwrap()
                    .inference_budget_account
                    .as_mut()
                    .unwrap()
                    .uid = "other-account".into()
            }
            "narrowed-budget" => {
                team.spec.envelope.budget = Some(
                    serde_json::from_value(json!({"scope":"GovernedInference","tokens":1}))
                        .unwrap(),
                )
            }
            _ => {}
        }
        let mut sandbox = KarsSandbox::new("runtime", crate::crd::KarsSandboxSpec::default());
        sandbox.metadata.owner_references = Some(vec![serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"task","uid":"task-uid","controller":true
        })).unwrap()]);
        let identity: ExecutionIdentity = serde_json::from_value(json!({
            "taskUid":"task-uid","authorizationDigest":authority.authorization_digest,
            "sandbox":{"namespace":"workspace","name":"runtime","uid":"sandbox-uid"},
            "runtimeNamespaceUid":"runtime-namespace-uid","podName":"pod","podUid":"pod-uid"
        }))
        .unwrap();
        for (route, object) in [
            (
                "/api/v1/namespaces/workspace",
                json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"workspace","uid":"workspace-uid"}}),
            ),
            (
                "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks/task",
                json!(task),
            ),
            (
                "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsteams/team",
                json!(team),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(object))
                .mount(&server)
                .await;
        }
        let result =
            verify_task_binding(&client, &account, &ledger, &identity, &sandbox, true).await;
        assert_eq!(
            result.is_ok(),
            matches!(scenario, "active" | "paused"),
            "{scenario}: {result:?}"
        );
    }
}

fn pod() -> Value {
    json!({
        "apiVersion":"v1", "kind":"Pod",
        "metadata":{"name":"task-pod","namespace":"kars-task","uid":"pod-uid"},
        "spec":{
            "serviceAccountName":"sandbox",
            "volumes":[{"name":TOKEN_VOLUME,"projected":{"sources":[{"serviceAccountToken":{
                "audience":AUDIENCE,"path":"token","expirationSeconds":600
            }}]}}],
            "containers":[
                {"name":"agent","image":"agent:latest"},
                {"name":"inference-router","image":"router:latest",
                    "securityContext":{"runAsUser":1001,"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true},
                    "volumeMounts":[{"name":TOKEN_VOLUME,"mountPath":PRIVATE_MOUNT,"readOnly":true}]}
            ]
        }
    })
}

fn valid(value: Value) -> bool {
    has_router_only_projection(&serde_json::from_value::<Pod>(value).unwrap())
}

#[test]
fn privacy_pending_is_distinct_from_revoked_or_changed_authority() {
    use crate::sre_authority::PrivacyReadiness;

    let epoch = Some("old-epoch".to_string());
    assert!(matches!(
        check_privacy_readiness(Ok(PrivacyReadiness::Pending), &epoch, true),
        Err(StoreError::PrivacyPending)
    ));
    for readiness in [
        Err("live privacy validation failed".into()),
        Ok(PrivacyReadiness::Qualified(Some("new-epoch".into()))),
    ] {
        assert!(matches!(
            check_privacy_readiness(readiness, &epoch, true),
            Err(StoreError::Ledger(BudgetError::Authorization))
        ));
    }
}

#[test]
fn privacy_rotation_allows_only_settlement_after_live_privacy_checks_pass() {
    use crate::sre_authority::PrivacyReadiness;

    for projected in [None, Some("old-epoch".to_string())] {
        for current in [
            None,
            Some("old-epoch".to_string()),
            Some("new-epoch".to_string()),
        ] {
            for dispatch in [false, true] {
                assert_eq!(
                    check_privacy_readiness(
                        Ok(PrivacyReadiness::Qualified(current.clone())),
                        &projected,
                        dispatch,
                    )
                    .is_ok(),
                    !dispatch || current == projected,
                );
                assert_eq!(
                    check_privacy_readiness(Ok(PrivacyReadiness::Pending), &projected, dispatch)
                        .is_ok(),
                    !dispatch,
                );
                assert!(
                    check_privacy_readiness(
                        Err("live privacy validation failed".into()),
                        &projected,
                        dispatch,
                    )
                    .is_err()
                );
            }
        }
    }
}

#[tokio::test]
async fn settlement_still_performs_live_secret_denial_checks() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/apis/authorization.k8s.io/v1/subjectaccessreviews"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "apiVersion":"authorization.k8s.io/v1", "kind":"SubjectAccessReview",
            "spec":{}, "status":{"allowed":true}
        })))
        .expect(2)
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    for dispatch in [false, true] {
        assert!(
            verify_privacy_epoch(&client, "kars-task", &Some("old-epoch".into()), dispatch)
                .await
                .is_err()
        );
    }
    server.verify().await;
}

#[test]
fn only_the_secure_router_may_mount_the_budget_audience_token() {
    assert!(valid(pod()));
    let mut leaked = pod();
    leaked["spec"]["containers"][0]["volumeMounts"] =
        leaked["spec"]["containers"][1]["volumeMounts"].clone();
    assert!(!valid(leaked));
    let mut wrong_user = pod();
    wrong_user["spec"]["containers"][1]["securityContext"]["runAsUser"] = json!(1000);
    assert!(!valid(wrong_user));
}

#[test]
fn alternate_named_budget_projection_and_shared_process_namespaces_fail_closed() {
    let mut alternate = pod();
    let mut volume = alternate["spec"]["volumes"][0].clone();
    volume["name"] = json!("disguised");
    alternate["spec"]["volumes"]
        .as_array_mut()
        .unwrap()
        .push(volume);
    alternate["spec"]["containers"][0]["volumeMounts"] =
        json!([{"name":"disguised","mountPath":"/agent-token"}]);
    assert!(!valid(alternate));
    for field in ["hostPID", "hostNetwork", "hostIPC", "shareProcessNamespace"] {
        let mut shared = pod();
        shared["spec"][field] = json!(true);
        assert!(!valid(shared));
    }
}

#[test]
fn standard_admission_injections_are_not_mistaken_for_replaced_router_authority() {
    use k8s_openapi::api::core::v1::Container;
    let template: Container =
        serde_json::from_value(pod()["spec"]["containers"][1].clone()).unwrap();
    let mut actual = template.clone();
    actual.env = Some(serde_json::from_value(json!([
            {"name":"AZURE_CLIENT_ID","value":"operator-client"},
            {"name":"AZURE_TENANT_ID","value":"operator-tenant"},
            {"name":"AZURE_FEDERATED_TOKEN_FILE","value":"/var/run/secrets/azure/tokens/azure-identity-token"}
        ])).unwrap());
    actual.volume_mounts.as_mut().unwrap().extend(
            serde_json::from_value::<Vec<k8s_openapi::api::core::v1::VolumeMount>>(json!([
                {"name":"kube-api-access-abcde","mountPath":"/var/run/secrets/kubernetes.io/serviceaccount","readOnly":true},
                {"name":"azure-identity-token","mountPath":"/var/run/secrets/azure/tokens","readOnly":true}
            ])).unwrap()
        );
    assert!(router_env_matches(&actual, &template));
    assert!(router_mounts_match(&actual, &template));
    actual.env.as_mut().unwrap().push(
        serde_json::from_value(json!({
            "name":"KARS_INFERENCE_BUDGET_REQUIRED","value":"false"
        }))
        .unwrap(),
    );
    assert!(!router_env_matches(&actual, &template));
    actual.volume_mounts.as_mut().unwrap()[1].mount_path = PRIVATE_MOUNT.into();
    assert!(!router_mounts_match(&actual, &template));
}

#[test]
fn audience_or_expiry_substitution_does_not_authorize_a_router() {
    let mut wrong = pod();
    wrong["spec"]["volumes"][0]["projected"]["sources"][0]["serviceAccountToken"]["audience"] =
        json!("api");
    assert!(!valid(wrong));
    let mut persistent = pod();
    persistent["spec"]["volumes"][0]["projected"]["sources"][0]["serviceAccountToken"]["expirationSeconds"] =
        json!(86400);
    assert!(!valid(persistent));
}

#[test]
fn rejection_diagnostics_keep_error_contract_and_log_only_static_stage() {
    use std::sync::{Arc, Mutex};

    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_writer(move || Capture(writer.clone()))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let error = rejection(
            "router_token",
            StoreError::Api {
                stage: "error-details-must-not-be-logged",
                code: Some(403),
            },
        );
        assert!(matches!(
            error,
            StoreError::Api {
                stage: "error-details-must-not-be-logged",
                code: Some(403)
            }
        ));
        assert!(matches!(
            denied_at("team_authority"),
            StoreError::Ledger(BudgetError::Authorization)
        ));
    });
    let bytes = output.lock().unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("error-details-must-not-be-logged"));
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    for (event, stage) in events.iter().zip(["router_token", "team_authority"]) {
        assert_eq!(
            event["fields"],
            json!({
                "message": "Governed inference authority rejected",
                "authority_stage": stage
            })
        );
    }
}

#[test]
fn lineage_diagnostics_preserve_authority_contention_and_api_errors() {
    use crate::task_identity::Error;
    for error in [
        Error::Identity,
        Error::NotReady,
        Error::Attenuation,
        Error::Depth,
    ] {
        assert!(matches!(
            lineage_rejection(error),
            StoreError::Ledger(BudgetError::Authorization)
        ));
    }
    assert!(matches!(
        lineage_rejection(Error::Changed),
        StoreError::Contention
    ));
    assert!(matches!(
        lineage_rejection(Error::Api {
            stage: "read lineage",
            code: Some(503)
        }),
        StoreError::Api {
            stage: "read lineage",
            code: Some(503)
        }
    ));
}
