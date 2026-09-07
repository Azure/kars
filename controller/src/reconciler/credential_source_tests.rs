// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[path = "credential_source_test_server.rs"]
mod server;
use server::*;

#[test]
fn provider_control_plane_and_process_environment_keys_are_not_credential_sources() {
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "AZURE_CLIENT_SECRET",
        "AGT_SIGNING_KEY",
        "KARS_ADMIN_TOKEN",
        "NODE_OPTIONS",
        "PATH",
        "token",
        "CUSTOM_TOKEN",
    ] {
        let mut secret = source();
        secret.data = Some(BTreeMap::from([(
            key.into(),
            ByteString(b"sensitive".to_vec()),
        )]));
        assert!(validate_values(&secret).is_err(), "{key}");
    }
    for bytes in [vec![0], vec![255]] {
        let mut secret = source();
        secret.data = Some(BTreeMap::from([(
            "TELEGRAM_BOT_TOKEN".into(),
            ByteString(bytes),
        )]));
        assert!(validate_values(&secret).is_err());
    }
}

#[test]
fn mode_preserves_legacy_shape_and_keeps_projection_values_out_of_pod_specs() {
    assert_eq!(
        Mode::Legacy.env_from("demo"),
        json!([
            {"secretRef": {"name": "demo-credentials", "optional": true}}
        ])
    );
    let mut deployment = deployment();
    let original = deployment.clone();
    Mode::Legacy.decorate(&mut deployment, &sandbox(), &namespace());
    assert_eq!(deployment, original);
    let mode = Mode::Source {
        uid: "projection-uid".into(),
        version: "42".into(),
        source_uid: "source-a".into(),
        source_version: "30".into(),
    };
    mode.decorate(&mut deployment, &sandbox(), &namespace());
    assert_eq!(
        mode.env_from("demo"),
        json!([
            {"secretRef": {"name": "demo-credential-projection", "optional": false}}
        ])
    );
    assert_eq!(
        deployment.spec.unwrap().strategy.unwrap().type_.as_deref(),
        Some("Recreate")
    );
}

#[test]
fn errors_never_echo_api_bodies_or_credential_values() {
    let error = api_error(
        "write",
        kube::Error::Api(Box::new(
            serde_json::from_value(json!({
                "status": "Failure", "reason": "Invalid", "code": 422,
                "message": "stringData: secret-never-log-this"
            }))
            .unwrap(),
        )),
    );
    assert!(!format!("{error:?} {error}").contains("secret-never-log-this"));
}

#[test]
fn metadata_watch_owner_is_the_explicitly_bound_sandbox_uid() {
    let reference = source_owner(&sandbox()).unwrap();
    assert_eq!(reference.name, "demo");
    assert_eq!(reference.uid, "sandbox-a");
    assert_eq!(reference.controller, Some(true));
}

#[tokio::test]
async fn absent_reference_leaves_legacy_credentials_and_workloads_unchanged() {
    let mut state = State::new();
    state.sandbox.spec.credentials_ref = None;
    let original = state.deployment.clone();
    let sandbox = state.sandbox.clone();
    let (server, client, state) = setup(state).await;
    assert!(matches!(
        reconcile(&client, &sandbox, Some(&namespace()), "agent:latest")
            .await
            .unwrap(),
        Mode::Legacy
    ));
    assert_eq!(state.lock().unwrap().deployment, original);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path() == SOURCE_PATH)
    );
}

#[tokio::test]
async fn first_source_creation_binds_uid_then_fences_values_behind_an_empty_anchor() {
    let (server, client, state) = setup(State::new()).await;
    let mode = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    assert!(matches!(mode, Mode::Source { .. }));
    let requests = server.received_requests().await.unwrap();
    let anchor = requests
        .iter()
        .position(|r| r.method == "POST" && r.url.path() == TARGETS_PATH)
        .unwrap();
    let anchor_body: Value = serde_json::from_slice(&requests[anchor].body).unwrap();
    assert!(anchor_body.get("data").is_none() && anchor_body.get("stringData").is_none());
    let value_write = requests.iter().position(nonempty_value_write).unwrap();
    assert!(anchor < value_write);
    assert!(
        requests[anchor + 1..value_write]
            .iter()
            .any(|r| r.url.path() == NS_PATH)
    );
    assert!(
        requests[anchor + 1..value_write]
            .iter()
            .any(|r| r.url.path() == SOURCE_PATH)
    );
    let body: Value = serde_json::from_slice(&requests[value_write].body).unwrap();
    assert_eq!(body["metadata"]["uid"], "projection-uid");
    assert!(body["metadata"]["resourceVersion"].is_string());
    let state = state.lock().unwrap();
    assert_eq!(
        state.projection.as_ref().unwrap().data,
        state.source.as_ref().unwrap().data
    );
    assert_eq!(
        state
            .deployment
            .as_ref()
            .unwrap()
            .spec
            .as_ref()
            .unwrap()
            .replicas,
        Some(0)
    );
    assert_eq!(
        annotation(&state.source.as_ref().unwrap().metadata, SANDBOX_UID),
        Some("sandbox-a")
    );
}

#[tokio::test]
async fn source_key_updates_and_removal_refresh_without_resurrecting_legacy_values() {
    let (_server, client, state) = setup(State::new()).await;
    let first = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    resume(&state, &first);
    {
        let mut state = state.lock().unwrap();
        let source = state.source.as_mut().unwrap();
        source.data = Some(BTreeMap::from([(
            "SLACK_BOT_TOKEN".into(),
            ByteString(b"rotated".to_vec()),
        )]));
        source.metadata.resource_version = Some("source-rotated".into());
    }
    let next = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    assert_ne!(format!("{first:?}"), format!("{next:?}"));
    let state = state.lock().unwrap();
    let data = state.projection.as_ref().unwrap().data.as_ref().unwrap();
    assert!(!data.contains_key("TELEGRAM_BOT_TOKEN"));
    assert_eq!(data["SLACK_BOT_TOKEN"].0, b"rotated");
    assert_eq!(
        state
            .deployment
            .as_ref()
            .unwrap()
            .spec
            .as_ref()
            .unwrap()
            .replicas,
        Some(0)
    );
}

#[tokio::test]
async fn metadata_only_source_changes_do_not_roll_pods_or_rewrite_projection_values() {
    let (server, client, state) = setup(State::new()).await;
    let first = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    resume(&state, &first);
    state
        .lock()
        .unwrap()
        .source
        .as_mut()
        .unwrap()
        .metadata
        .resource_version = Some("metadata-change".into());
    let before = server.received_requests().await.unwrap().len();
    let next = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    let mut before_deployment = deployment();
    let mut after_deployment = deployment();
    first.decorate(&mut before_deployment, &sandbox(), &namespace());
    next.decorate(&mut after_deployment, &sandbox(), &namespace());
    assert_eq!(before_deployment, after_deployment);
    assert!(
        server.received_requests().await.unwrap()[before..]
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn missing_replaced_or_invalid_sources_stop_runtime_and_revoke_only_owned_projection() {
    for variant in ["missing", "replaced", "purpose", "owner", "provider-key"] {
        let (_server, client, state) = setup(State::new()).await;
        let mode = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
            .await
            .unwrap();
        resume(&state, &mode);
        {
            let mut state = state.lock().unwrap();
            match variant {
                "missing" => state.source = None,
                "replaced" => {
                    state.source.as_mut().unwrap().metadata.uid = Some("new-source".into())
                }
                "purpose" => {
                    state
                        .source
                        .as_mut()
                        .unwrap()
                        .annotations_mut()
                        .insert(PURPOSE.into(), "other".into());
                }
                "owner" => {
                    state
                        .source
                        .as_mut()
                        .unwrap()
                        .annotations_mut()
                        .insert(SANDBOX_UID.into(), "other".into());
                }
                _ => {
                    state.source.as_mut().unwrap().data = Some(BTreeMap::from([(
                        "OPENAI_API_KEY".into(),
                        ByteString(b"provider-secret".to_vec()),
                    )]))
                }
            }
        }
        assert!(
            reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
                .await
                .is_err(),
            "{variant}"
        );
        let state = state.lock().unwrap();
        assert!(
            state
                .projection
                .as_ref()
                .unwrap()
                .data
                .as_ref()
                .is_none_or(BTreeMap::is_empty),
            "{variant}"
        );
        assert_eq!(
            state
                .deployment
                .as_ref()
                .unwrap()
                .spec
                .as_ref()
                .unwrap()
                .replicas,
            Some(0)
        );
        assert_eq!(
            state.sandbox.status.as_ref().unwrap().phase.as_deref(),
            Some("Degraded")
        );
    }
}

#[tokio::test]
async fn explicit_reference_removal_revokes_projection_and_preserves_legacy_secret() {
    let (_server, client, state) = setup(State::new()).await;
    let mode = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    resume(&state, &mode);
    let current = {
        let mut state = state.lock().unwrap();
        state.sandbox.spec.credentials_ref = None;
        state.sandbox.clone()
    };
    assert!(matches!(
        reconcile(&client, &current, Some(&namespace()), "agent:latest")
            .await
            .unwrap(),
        Mode::Legacy
    ));
    assert!(state.lock().unwrap().projection.is_none());
    assert_eq!(
        state
            .lock()
            .unwrap()
            .deployment
            .as_ref()
            .unwrap()
            .spec
            .as_ref()
            .unwrap()
            .replicas,
        Some(0)
    );
}

#[tokio::test]
async fn foreign_projection_and_namespace_never_receive_values_or_get_taken_over() {
    for variant in [
        "projection",
        "namespace",
        "namespace-replaced",
        "source-cross-workspace",
    ] {
        let mut state = State::new();
        match variant {
            "projection" => {
                let mut foreign = source();
                foreign.metadata.name = Some(projection_name("demo"));
                foreign.metadata.namespace = Some("kars-demo".into());
                foreign.metadata.uid = Some("foreign-projection".into());
                state.projection = Some(foreign);
            }
            "namespace" => {
                state.namespace.annotations_mut().insert(
                    super::super::namespace_ownership::SOURCE_NAMESPACE.into(),
                    "other-workspace".into(),
                );
            }
            "namespace-replaced" => state.namespace.metadata.uid = Some("replacement".into()),
            _ => state.source.as_mut().unwrap().metadata.namespace = Some("other-workspace".into()),
        }
        let original = state.projection.clone();
        let (server, client, state) = setup(state).await;
        assert!(
            reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
                .await
                .is_err()
        );
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(nonempty_value_write)
        );
        if variant == "projection" {
            assert_eq!(state.lock().unwrap().projection, original);
        }
    }
}

#[tokio::test]
async fn namespace_source_and_destination_races_never_write_values_after_failed_fences() {
    for fault in [
        Fault::ReplaceNamespaceAfterAnchor,
        Fault::ReplaceProjectionOnWrite,
        Fault::ChangeSourceAfterAnchor,
        Fault::FailValueWrite,
        Fault::CreateConflict,
        Fault::SourceReadForbidden,
    ] {
        let mut state = State::new();
        state.fault = fault;
        let (_server, client, state) = setup(state).await;
        assert!(
            reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
                .await
                .is_err()
        );
        assert_eq!(state.lock().unwrap().successful_value_writes, 0);
    }
}

#[test]
fn chart_schema_and_generated_reference_contract_agree() {
    use kube::CustomResourceExt;
    use serde::Deserialize;
    let document = serde_yaml::Deserializer::from_str(include_str!(
        "../../../deploy/helm/kars/templates/crd.yaml"
    ))
    .next()
    .unwrap();
    let chart = serde_yaml::Value::deserialize(document).unwrap();
    let chart = serde_json::to_value(chart).unwrap();
    let generated = serde_json::to_value(KarsSandbox::crd()).unwrap();
    let path = "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/credentialsRef";
    for key in ["name", "uid"] {
        for attribute in ["type", "minLength", "maxLength", "pattern"] {
            assert_eq!(
                chart.pointer(path).unwrap()["properties"][key][attribute],
                generated.pointer(path).unwrap()["properties"][key][attribute],
                "{key}/{attribute}"
            );
        }
    }
}

#[tokio::test]
async fn every_wired_managed_runtime_accepts_the_same_agent_projection() {
    for (kind, variant, config) in [
        ("OpenClaw", "openclaw", json!({})),
        ("OpenAIAgents", "openaiAgents", json!({})),
        (
            "MicrosoftAgentFramework",
            "microsoftAgentFramework",
            json!({"language": "python"}),
        ),
        ("LangGraph", "langGraph", json!({"language": "python"})),
        ("LangGraph", "langGraph", json!({"language": "typescript"})),
        ("Anthropic", "anthropic", json!({})),
        ("PydanticAi", "pydanticAi", json!({})),
        ("Hermes", "hermes", json!({})),
        (
            "BYO",
            "byo",
            json!({"image": "example.test/agent:latest", "contractVersion": "v1"}),
        ),
    ] {
        let mut state = State::new();
        let mut runtime = json!({"kind": kind});
        runtime[variant] = config;
        state.sandbox.spec.runtime = serde_json::from_value(runtime).unwrap();
        let sandbox = state.sandbox.clone();
        let (_server, client, _) = setup(state).await;
        let mode = reconcile(&client, &sandbox, Some(&namespace()), "agent:latest")
            .await
            .unwrap();
        assert_eq!(
            mode.env_from("demo"),
            json!([
                {"secretRef": {"name": "demo-credential-projection", "optional": false}}
            ]),
            "{kind}"
        );
    }
}

#[tokio::test]
async fn empty_source_removes_all_owned_keys_and_does_not_cause_repeated_rollouts() {
    let (server, client, state) = setup(State::new()).await;
    let first = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    resume(&state, &first);
    {
        let mut state = state.lock().unwrap();
        state.source.as_mut().unwrap().data = None;
        state.source.as_mut().unwrap().metadata.resource_version = Some("empty-source".into());
    }
    let empty = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    assert!(
        state
            .lock()
            .unwrap()
            .projection
            .as_ref()
            .unwrap()
            .data
            .as_ref()
            .is_none_or(BTreeMap::is_empty)
    );
    resume(&state, &empty);
    let before = server.received_requests().await.unwrap().len();
    reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    assert!(
        server.received_requests().await.unwrap()[before..]
            .iter()
            .all(|request| request.method == "GET")
    );
}

#[tokio::test]
async fn unsafe_reference_names_are_rejected_without_reading_the_named_secret() {
    for name in [
        "controller-receipt-identity",
        "../other",
        "kars-credential-source-other",
    ] {
        let mut state = State::new();
        state.sandbox.spec.credentials_ref.as_mut().unwrap().name = name.into();
        let sandbox = state.sandbox.clone();
        let (server, client, _) = setup(state).await;
        assert!(
            reconcile(&client, &sandbox, Some(&namespace()), "agent:latest")
                .await
                .is_err()
        );
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path() == SOURCE_PATH)
        );
    }
}

#[tokio::test]
async fn opaque_type_and_explicit_runtime_env_conflicts_fail_closed_before_projection() {
    for variant in ["token-type", "runtime-override", "overlay"] {
        let mut state = State::new();
        match variant {
            "token-type" => {
                state.source.as_mut().unwrap().type_ =
                    Some("kubernetes.io/service-account-token".into())
            }
            "runtime-override" => {
                state
                    .sandbox
                    .spec
                    .runtime
                    .openclaw
                    .as_mut()
                    .unwrap()
                    .extra_env = Some(BTreeMap::from([(
                    "TELEGRAM_BOT_TOKEN".into(),
                    "old-value".into(),
                )]));
            }
            _ => {
                state.sandbox.spec.upstream_compatibility = Some(
                    serde_json::from_value(json!({
                        "sigsAgentSandbox": "overlay", "upstreamSandboxName": "external"
                    }))
                    .unwrap(),
                )
            }
        }
        let sandbox = state.sandbox.clone();
        let (server, client, state) = setup(state).await;
        assert!(
            reconcile(&client, &sandbox, Some(&namespace()), "agent:latest")
                .await
                .is_err()
        );
        assert!(state.lock().unwrap().projection.is_none());
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(nonempty_value_write)
        );
    }
}

#[test]
fn extracted_environment_merge_retains_existing_reserved_prefix_and_precedence_rules() {
    let runtime = serde_json::from_value(json!({
        "kind": "OpenClaw", "openclaw": {"extraEnv": {
            "AGT_KEY": "blocked", "AZURE_CLIENT_SECRET": "blocked", "KARS_TOKEN": "blocked",
            "EXISTING": "ignored", "SAFE": "allowed", "NUL": "bad\u{0000}value", "1BAD": "invalid"
        }}
    }))
    .unwrap();
    let mut plan = super::super::runtime::build_runtime_plan(&runtime, "agent:latest").unwrap();
    plan.raw_env = vec![
        json!({"name": "AGT_OTHER", "value": "blocked"}),
        json!({"name": "SAFE", "value": "ignored"}),
        json!({"name": "FROM_SECRET", "valueFrom": {"secretKeyRef": {"name": "owned", "key": "key"}}}),
    ];
    let mut env = vec![json!({"name": "EXISTING", "value": "controller"})];
    super::super::agent_env::merge(&mut env, &plan);
    assert_eq!(
        env,
        vec![
            json!({"name": "EXISTING", "value": "controller"}),
            json!({"name": "SAFE", "value": "allowed"}),
            json!({"name": "FROM_SECRET", "valueFrom": {"secretKeyRef": {"name": "owned", "key": "key"}}}),
        ]
    );
}

#[test]
fn credential_status_acknowledges_metadata_versions_without_containing_values() {
    let mode = Mode::Source {
        uid: "projection".into(),
        version: "100".into(),
        source_uid: "source-a".into(),
        source_version: "101".into(),
    };
    let mut sandbox = sandbox();
    assert!(mode.needs_status_update(&sandbox));
    let condition = mode.condition(&sandbox).unwrap();
    assert_eq!(condition.type_, "CredentialsReady");
    assert!(condition.message.contains("sourceVersion"));
    assert!(!condition.message.contains("initial"));
    sandbox.status = Some(crate::crd::KarsSandboxStatus {
        conditions: vec![condition],
        ..Default::default()
    });
    assert!(!mode.needs_status_update(&sandbox));
    assert!(Mode::Legacy.needs_status_update(&sandbox));
}

#[tokio::test]
async fn a_recreated_projection_cannot_inherit_the_previous_uid_seal() {
    let (_server, client, state) = setup(State::new()).await;
    let mode = reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    resume(&state, &mode);
    let foreign = {
        let mut state = state.lock().unwrap();
        state.projection.as_mut().unwrap().metadata.uid = Some("replacement-uid".into());
        state.projection.clone()
    };
    assert!(
        reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().projection, foreign);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .deployment
            .as_ref()
            .unwrap()
            .spec
            .as_ref()
            .unwrap()
            .replicas,
        Some(0)
    );
}

#[tokio::test]
async fn an_empty_controller_anchor_recovers_after_a_failed_uid_seal() {
    let mut state = State::new();
    state.fault = Fault::FailSealOnce;
    let (_server, client, state) = setup(state).await;
    assert!(
        reconcile(&client, &sandbox(), Some(&namespace()), "agent:latest")
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().successful_value_writes, 0);
    let current = state.lock().unwrap().sandbox.clone();
    reconcile(&client, &current, Some(&namespace()), "agent:latest")
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().successful_value_writes, 1);
}
