// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::mission_records::{
    mission_evidence_key, mission_output_candidate, project_mission_output_record,
    select_mission_evidence_records, select_mission_output_records, trace_record_identity,
};
use super::providers::{
    classify_provider, image_registry_host, normalize_registry_host, public_registry,
};
use super::sandboxes::descendant_sandbox_objects;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::DynamicObject;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn controller_model_catalog_accepts_json_and_legacy_csv_without_inventing_models() {
    use super::providers::parse_model_catalog;
    for raw in [
        r#"["gpt-4.1", " gpt-5.4-mini ", ""]"#,
        "gpt-4.1, gpt-5.4-mini,",
    ] {
        assert_eq!(parse_model_catalog(raw), vec!["gpt-4.1", "gpt-5.4-mini"]);
    }
    for raw in ["", "[]", "[broken", r#"["valid", 42]"#] {
        assert!(parse_model_catalog(raw).is_empty(), "{raw}");
    }
}

#[test]
fn bootstrap_model_preserves_the_explicit_core_provider_and_default() {
    use super::providers::configured_bootstrap_model;
    use k8s_openapi::api::core::v1::EnvVar;
    let mut env = vec![
        json!({"name":"AZURE_OPENAI_DEPLOYMENT","value":"legacy-model"}),
        json!({"name":"FOUNDRY_ENDPOINT","value":"https://bootstrap.example"}),
        json!({"name":"KARS_TASK_DEFAULT_PROVIDER","value":"github-copilot"}),
        json!({"name":"KARS_TASK_DEFAULT_MODEL","value":"selected-model"}),
    ];
    for _ in 0..env.len() {
        env.rotate_left(1);
        let model = configured_bootstrap_model(
            &serde_json::from_value::<Vec<EnvVar>>(json!(env)).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(model.provider, "github-copilot");
        assert_eq!(model.deployment, "selected-model");
    }
    for env in [
        json!([{"name":"KARS_TASK_DEFAULT_PROVIDER","value":"github-copilot"}]),
        json!([
            {"name":"KARS_TASK_DEFAULT_PROVIDER","value":"github-copilot"},
            {"name":"AZURE_OPENAI_DEPLOYMENT","value":"legacy-model"},
        ]),
        json!([
            {"name":"KARS_TASK_DEFAULT_PROVIDER","value":"github-copilot"},
            {"name":"AZURE_OPENAI_DEPLOYMENT","value":"legacy-model"},
            {"name":"KARS_TASK_DEFAULT_MODEL","value":" "},
        ]),
        json!([{"name":"KARS_TASK_DEFAULT_PROVIDER","valueFrom":{"configMapKeyRef":{"name":"settings","key":"provider"}}}]),
        json!([
            {"name":"KARS_TASK_DEFAULT_PROVIDER","value":"github-copilot"},
            {"name":"AZURE_OPENAI_DEPLOYMENT","value":"legacy-model"},
            {"name":"KARS_TASK_DEFAULT_MODEL","valueFrom":{"configMapKeyRef":{"name":"settings","key":"model"}}},
        ]),
    ] {
        assert!(configured_bootstrap_model(&serde_json::from_value::<Vec<EnvVar>>(env).unwrap()).is_err());
    }
}

#[test]
fn provider_switch_requires_a_retained_route_for_the_previous_default() {
    use super::providers::inherited_provider_registered;
    use k8s_openapi::api::core::v1::EnvVar;
    let azure: Vec<EnvVar> = serde_json::from_value(json!([
        {"name":"FOUNDRY_ENDPOINT","value":"https://example.cognitiveservices.azure.com/"},
        {"name":"FOUNDRY_DEPLOYMENTS","value":"[\"azure-only\"]"},
        {"name":"AZURE_OPENAI_API_KEY","valueFrom":{"secretKeyRef":{"name":"bootstrap","key":"api-key"}}}
    ])).unwrap();
    let mut keys = BTreeMap::from([
        ("COPILOT_GITHUB_TOKEN".into(), "test-only-token".into()),
        (
            "KARS_PROVIDER_OTHER_ENDPOINT".into(),
            "https://unrelated.example".into(),
        ),
    ]);
    assert!(!inherited_provider_registered(&azure, &keys));
    keys.insert(
        "KARS_PROVIDER_AZURE_FOUNDRY_ENDPOINT".into(),
        "https://example.cognitiveservices.azure.com".into(),
    );
    assert!(inherited_provider_registered(&azure, &keys));
    let copilot: Vec<EnvVar> = serde_json::from_value(json!([
        {"name":"KARS_PROVIDER","value":"github-copilot"},
        {"name":"AZURE_OPENAI_ENDPOINT","value":"https://api.githubcopilot.com"},
        {"name":"FOUNDRY_DEPLOYMENTS","value":"[\"azure-only\"]"},
        {"name":"KARS_MODEL_CATALOG","value":"copilot-model"}
    ]))
    .unwrap();
    assert!(inherited_provider_registered(&copilot, &keys));
    keys.remove("COPILOT_GITHUB_TOKEN");
    assert!(!inherited_provider_registered(&copilot, &keys));
    assert!(inherited_provider_registered(&[], &BTreeMap::new()));
}

#[tokio::test]
async fn unregistered_bootstrap_default_cannot_be_discarded_by_a_provider_switch() {
    use axum::{
        Json, Router,
        http::{Method, StatusCode, Uri},
        response::IntoResponse,
    };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    async fn handle(method: Method, uri: Uri) -> axum::response::Response {
        assert_eq!(
            method,
            Method::GET,
            "Must not mutate the current provider or read its bootstrap credential"
        );
        if uri.path().ends_with("/deployments/kars-controller") {
            return Json(json!({
                "apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"kars-controller"},
                "spec":{"selector":{"matchLabels":{"app":"controller"}},"template":{"spec":{"containers":[
                    {"name":"controller","env":[{"name":"FOUNDRY_ENDPOINT","value":"https://bootstrap.example"}]}
                ]}}}
            })).into_response();
        }
        assert!(
            uri.path().contains("/karscredentialgrants/"),
            "No arbitrary bootstrap Secret read"
        );
        (StatusCode::NOT_FOUND, Json(json!({
            "apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"absent","code":404
        }))).into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = kube::Config::new(
        format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(handle))
            .await
            .unwrap()
    });
    let cluster = super::Cluster {
        client: kube::Client::try_from(config).unwrap(),
    };
    for error in [
        cluster
            .set_controller_catalog("new-model", Some("https://new.example"), None)
            .await
            .unwrap_err(),
    ] {
        assert!(
            matches!(error, kube::Error::Api(status) if status.code == 409 && status.reason == "ProviderRegistrationRequired")
        );
    }
    server.abort();
}

#[tokio::test]
async fn orchestrator_bootstrap_uses_the_controller_route_instead_of_a_fixed_provider() {
    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::{Method, StatusCode, Uri},
        response::{IntoResponse, Response},
    };
    use std::sync::{Arc, Mutex};

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    type Fixture = Arc<(serde_json::Value, Mutex<Vec<serde_json::Value>>)>;
    async fn handle(
        State(state): State<Fixture>,
        method: Method,
        uri: Uri,
        body: Bytes,
    ) -> Response {
        if method == Method::PATCH || method == Method::POST {
            let mut document: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(method, Method::POST);
            state.1.lock().unwrap().push(document.clone());
            document["metadata"]["uid"] = json!("orchestrator-uid");
            document["metadata"]["resourceVersion"] = json!("1");
            Json(document).into_response()
        } else {
            assert_eq!(method, Method::GET);
            if uri.path().ends_with("/inferencepolicies/bridge-orchestrator-inference")
                || uri.path().ends_with("/karssandboxes/bridge-orchestrator")
            {
                return (StatusCode::NOT_FOUND, Json(json!({"kind":"Status", "apiVersion":"v1", "status":"Failure", "reason":"NotFound", "message":"not found", "code":404}))).into_response();
            }
            Json(state.0.clone()).into_response()
        }
    }

    for (default, catalog, expected, provider) in [
        (
            Some("gpt-5.4-mini"),
            r#"["gpt-4.1", "gpt-5.4-mini"]"#,
            Some("gpt-5.4-mini"),
            None,
        ),
        (None, r#"["gpt-4.1", "gpt-5.4-mini"]"#, Some("gpt-4.1"), None),
        (None, "", None, None),
        (
            Some("selected-model"),
            "legacy-model",
            Some("selected-model"),
            Some("github-copilot"),
        ),
    ] {
        let mut env = vec![
            json!({"name":"FOUNDRY_ENDPOINT","value":"https://example.cognitiveservices.azure.com/"}),
            json!({"name":"FOUNDRY_DEPLOYMENTS","value":catalog}),
        ];
        if let Some(provider) = provider {
            env.push(json!({"name":"AZURE_OPENAI_DEPLOYMENT","value":"legacy-model"}));
            env.push(json!({"name":"KARS_TASK_DEFAULT_PROVIDER","value":provider}));
        }
        if let Some(default) = default {
            env.push(json!({"name":"KARS_TASK_DEFAULT_MODEL","value":default}));
        }
        let state: Fixture = Arc::new((
            json!({
                "apiVersion":"apps/v1","kind":"Deployment",
                "metadata":{"name":"kars-controller"},
                "spec":{"selector":{"matchLabels":{"app":"controller"}},
                    "template":{"spec":{"containers":[{"name":"controller","env":env}]}}}
            }),
            Mutex::new(Vec::new()),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = kube::Config::new(
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        );
        let app = Router::new().fallback(handle).with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cluster = super::Cluster {
            client: kube::Client::try_from(config).unwrap(),
        };
        cluster.ensure_orchestrator_sandbox().await.unwrap();
        let writes = state.1.lock().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0]["kind"], "InferencePolicy");
        if let Some(expected) = expected {
            assert_eq!(
                writes[0]["spec"]["modelPreference"]["primary"],
                json!({"provider":provider.unwrap_or("azure-foundry"),"deployment":expected})
            );
        } else {
            assert!(writes[0]["spec"].get("modelPreference").is_none());
        }
        assert_eq!(writes[1]["kind"], "KarsSandbox");
        server.abort();
    }
}

#[tokio::test]
async fn orchestrator_bootstrap_preserves_existing_or_concurrently_created_policy() {
    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::{Method, StatusCode, Uri},
        response::{IntoResponse, Response},
    };
    use std::sync::{Arc, Mutex};
    type Fixture = Arc<(StatusCode, Mutex<Vec<(Method, String)>>)>;
    async fn handle(
        State(state): State<Fixture>,
        method: Method,
        uri: Uri,
        body: Bytes,
    ) -> Response {
        state
            .1
            .lock()
            .unwrap()
            .push((method.clone(), uri.path().into()));
        if uri.path().contains("/inferencepolicies") {
            let status = if method == Method::POST {
                StatusCode::CONFLICT
            } else {
                state.0
            };
            if status == StatusCode::OK {
                return Json(json!({
                    "apiVersion":"kars.azure.com/v1alpha1", "kind":"InferencePolicy",
                    "metadata":{"name":"bridge-orchestrator-inference", "namespace":"kars-system"},
                    "spec":{"modelPreference":{"primary":{"provider":"azure-openai", "deployment":"selected"}}}
                })).into_response();
            }
            return (status, Json(json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":match status { StatusCode::NOT_FOUND => "NotFound", StatusCode::CONFLICT => "AlreadyExists", _ => "Forbidden" }, "message":"fixture", "code":status.as_u16()}))).into_response();
        }
        if uri.path().contains("/karssandboxes") {
            if method == Method::GET {
                return (StatusCode::NOT_FOUND, Json(json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"NotFound", "message":"fixture", "code":404}))).into_response();
            }
            assert_eq!(method, Method::POST);
            let mut document: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(document["kind"], "KarsSandbox");
            document["metadata"]["uid"] = json!("orchestrator-uid");
            document["metadata"]["resourceVersion"] = json!("1");
            return Json(document).into_response();
        }
        Json(json!({"apiVersion":"apps/v1", "kind":"Deployment", "metadata":{"name":"kars-controller"},
            "spec":{"selector":{"matchLabels":{"app":"controller"}},"template":{"spec":{"containers":[{"name":"controller", "env":[]}]}}}}
        )).into_response()
    }
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for status in [StatusCode::OK, StatusCode::NOT_FOUND, StatusCode::FORBIDDEN] {
        let state: Fixture = Arc::new((status, Mutex::new(Vec::new())));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = kube::Config::new(
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        );
        let app = Router::new().fallback(handle).with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cluster = super::Cluster {
            client: kube::Client::try_from(config).unwrap(),
        };
        let result = cluster.ensure_orchestrator_sandbox().await;
        assert_eq!(result.is_ok(), status != StatusCode::FORBIDDEN);
        let requests = state.1.lock().unwrap();
        assert!(
            !requests
                .iter()
                .any(|(method, path)| *method == Method::PATCH
                    && path.contains("/inferencepolicies"))
        );
        if status == StatusCode::OK {
            assert_eq!(
                requests.len(),
                3,
                "existing policy must not trigger controller default reads or policy writes"
            );
        } else if status == StatusCode::FORBIDDEN {
            assert_eq!(
                requests.len(),
                2,
                "unreadable policy must not be overwritten"
            );
        } else {
            assert_eq!(
                requests
                    .iter()
                    .filter(|(method, _)| *method == Method::POST)
                    .count(),
                2
            );
        }
        server.abort();
    }
}

fn id(r: Option<(String, String, String)>) -> Option<String> {
    r.map(|(i, _, _)| i)
}

#[test]
fn descendant_sandboxes_include_nested_agents_once() {
    let sandbox = |name: &str, parent: Option<&str>| -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsSandbox",
            "metadata": {
                "name": name,
                "labels": parent.map(|parent| json!({"kars.azure.com/parent": parent}))
            }
        }))
        .expect("sandbox")
    };
    let items = vec![
        sandbox("child", Some("root")),
        sandbox("grandchild", Some("child")),
        sandbox("unrelated", Some("other")),
    ];

    let names = descendant_sandbox_objects(&items, "root")
        .into_iter()
        .filter_map(|sandbox| sandbox.metadata.name)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        names,
        std::collections::HashSet::from(["child".to_string(), "grandchild".to_string(),])
    );
}

#[test]
fn registry_matching_covers_private_runtime_images() {
    assert_eq!(
        image_registry_host("example.azurecr.io/kars-runtime-hermes:latest"),
        "example.azurecr.io"
    );
    assert_eq!(
        normalize_registry_host("https://example.azurecr.io/v1/"),
        "example.azurecr.io"
    );
    assert!(!public_registry("example.azurecr.io"));
    assert!(public_registry("mcr.microsoft.com"));
}

#[test]
fn mission_evidence_annotation_restores_long_nonce_identity() {
    let full = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let mut config_map = ConfigMap::default();
    config_map.metadata.annotations = Some(BTreeMap::from([(
        "kars.azure.com/mission-evidence-key".to_string(),
        full.to_string(),
    )]));
    config_map.metadata.labels = Some(BTreeMap::from([(
        "kars.azure.com/mission-output".to_string(),
        "stock-monitor-persistent-qual-principal-assig-0123456789ab".to_string(),
    )]));

    assert_eq!(
        mission_evidence_key(&config_map, "kars.azure.com/mission-output").as_deref(),
        Some(full)
    );
}

#[test]
fn mission_evidence_label_remains_legacy_fallback() {
    let mut config_map = ConfigMap::default();
    config_map.metadata.labels = Some(BTreeMap::from([(
        "kars.azure.com/mission-output".to_string(),
        "team-run-100".to_string(),
    )]));

    assert_eq!(
        mission_evidence_key(&config_map, "kars.azure.com/mission-output").as_deref(),
        Some("team-run-100")
    );
}

#[test]
fn legacy_principal_label_restores_stable_task_name() {
    let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let mut config_map = ConfigMap::default();
    config_map.metadata.labels = Some(BTreeMap::from([
        (
            "kars.azure.com/mission-output".to_string(),
            nonce.to_string(),
        ),
        (
            "kars.azure.com/mission-principal".to_string(),
            "stock-monitor-persistent-qual-principal".to_string(),
        ),
    ]));
    config_map.data = Some(BTreeMap::from([(
        "assignmentNonce".to_string(),
        nonce.to_string(),
    )]));

    let (_, _, data) = mission_output_candidate(config_map).expect("candidate");
    assert_eq!(
        data.get("taskName").map(String::as_str),
        Some("stock-monitor-persistent-qual-principal")
    );
}

#[test]
fn ordinary_mission_enumeration_keeps_the_task_pointer() {
    let first_nonce = "run-1784912062312097896";
    let latest_nonce = "run-1784915840189332732";
    let first = BTreeMap::from([
        ("assignmentNonce".to_string(), first_nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let latest = BTreeMap::from([
        ("assignmentNonce".to_string(), latest_nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let selected = select_mission_output_records(vec![
        (first_nonce.to_string(), Some("archive".to_string()), first),
        (
            latest_nonce.to_string(),
            Some("archive".to_string()),
            latest.clone(),
        ),
        (
            "kompli-research".to_string(),
            Some("current".to_string()),
            latest,
        ),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "kompli-research");
}

#[test]
fn explicit_current_pointer_beats_legacy_archive_for_same_task() {
    let old_nonce = "rev-1";
    let latest_nonce = "rev-2";
    let legacy = BTreeMap::from([
        ("assignmentNonce".to_string(), old_nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let current = BTreeMap::from([
        ("assignmentNonce".to_string(), latest_nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let selected = select_mission_output_records(vec![
        (old_nonce.to_string(), None, legacy),
        (
            "kompli-research".to_string(),
            Some("current".to_string()),
            current,
        ),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "kompli-research");
}

#[test]
fn legacy_ordinary_run_archives_do_not_become_phantom_tasks() {
    let old_nonce = "run-1784912062312097896";
    let latest_nonce = "run-1784915840189332732";
    let mut archive = ConfigMap::default();
    archive.metadata.labels = Some(BTreeMap::from([
        (
            "kars.azure.com/mission-output".to_string(),
            old_nonce.to_string(),
        ),
        (
            "kars.azure.com/mission-principal".to_string(),
            "kompli-research".to_string(),
        ),
    ]));
    archive.data = Some(BTreeMap::from([(
        "assignmentNonce".to_string(),
        old_nonce.to_string(),
    )]));
    let mut current = ConfigMap::default();
    current.metadata.labels = Some(BTreeMap::from([(
        "kars.azure.com/mission-output".to_string(),
        "kompli-research".to_string(),
    )]));
    current.data = Some(BTreeMap::from([(
        "assignmentNonce".to_string(),
        latest_nonce.to_string(),
    )]));
    let selected = select_mission_output_records(vec![
        mission_output_candidate(archive).expect("archive"),
        mission_output_candidate(current).expect("current"),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "kompli-research");
}

#[test]
fn persistent_team_latest_enumeration_keeps_the_current_pointer() {
    let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let data = BTreeMap::from([
        ("assignmentNonce".to_string(), nonce.to_string()),
        (
            "taskName".to_string(),
            "stock-monitor-persistent-qual-principal".to_string(),
        ),
        (
            "team".to_string(),
            "stock-monitor-persistent-qual".to_string(),
        ),
    ]);
    let selected = select_mission_output_records(vec![
        (nonce.to_string(), Some("archive".to_string()), data.clone()),
        (
            "stock-monitor-persistent-qual-principal".to_string(),
            Some("current".to_string()),
            data,
        ),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "stock-monitor-persistent-qual-principal");
}

#[test]
fn accounting_enumeration_keeps_archives_and_drops_current_pointers() {
    let nonce = "run-1784915840189332732";
    let data = BTreeMap::from([
        ("assignmentNonce".to_string(), nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let selected = select_mission_evidence_records(vec![
        (nonce.to_string(), Some("archive".to_string()), data.clone()),
        (
            "kompli-research".to_string(),
            Some("current".to_string()),
            data,
        ),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, nonce);
}

#[test]
fn accounting_enumeration_keeps_each_rerun_archive() {
    let first_nonce = "rev-1";
    let latest_nonce = "rev-2";
    let selected = select_mission_evidence_records(vec![
        (
            first_nonce.to_string(),
            Some("archive".to_string()),
            BTreeMap::from([("assignmentNonce".to_string(), first_nonce.to_string())]),
        ),
        (
            latest_nonce.to_string(),
            Some("archive".to_string()),
            BTreeMap::from([("assignmentNonce".to_string(), latest_nonce.to_string())]),
        ),
        (
            "kompli-research".to_string(),
            Some("current".to_string()),
            BTreeMap::from([("assignmentNonce".to_string(), latest_nonce.to_string())]),
        ),
    ]);

    assert_eq!(selected.len(), 2);
    assert!(selected.iter().any(|(key, _)| key == first_nonce));
    assert!(selected.iter().any(|(key, _)| key == latest_nonce));
}

#[test]
fn persistent_archive_projects_stable_task_and_separate_evidence_key() {
    let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let data = BTreeMap::from([
        ("assignmentNonce".to_string(), nonce.to_string()),
        (
            "taskName".to_string(),
            "stock-monitor-persistent-qual-principal".to_string(),
        ),
        (
            "team".to_string(),
            "stock-monitor-persistent-qual".to_string(),
        ),
    ]);
    let projected = project_mission_output_record(nonce.to_string(), data);

    assert_eq!(
        projected.task_name,
        "stock-monitor-persistent-qual-principal"
    );
    assert_eq!(projected.evidence_key, nonce);
}

#[test]
fn mirrored_trace_records_share_one_counting_identity() {
    let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let data = BTreeMap::from([
        ("assignmentNonce".to_string(), nonce.to_string()),
        (
            "trace.json".to_string(),
            r#"[{"kind":"round"}]"#.to_string(),
        ),
        ("capturedAt".to_string(), "2026-07-24T19:00:00Z".to_string()),
    ]);
    let mut archive = ConfigMap::default();
    archive.metadata.name = Some(format!("kars-mission-trace-{nonce}"));
    archive.metadata.annotations = Some(BTreeMap::from([(
        "kars.azure.com/mission-evidence-role".to_string(),
        "archive".to_string(),
    )]));
    archive.data = Some(data.clone());
    let mut current = ConfigMap::default();
    current.metadata.name =
        Some("kars-mission-trace-stock-monitor-persistent-qual-principal".to_string());
    current.metadata.annotations = Some(BTreeMap::from([(
        "kars.azure.com/mission-evidence-role".to_string(),
        "current".to_string(),
    )]));
    current.data = Some(data);

    assert!(trace_record_identity(&archive).is_some());
    assert!(trace_record_identity(&current).is_none());
}

#[test]
fn explicit_override_wins() {
    let eps = vec!["https://models.github.ai/inference".to_string()];
    assert_eq!(
        id(classify_provider(Some("github-copilot"), &eps, None)).as_deref(),
        Some("github-copilot")
    );
    assert_eq!(
        id(classify_provider(
            Some("github-models"),
            &eps,
            Some("gho_x")
        ))
        .as_deref(),
        Some("github-models")
    );
    assert_eq!(
        id(classify_provider(Some("foundry"), &[], None)).as_deref(),
        Some("azure-foundry")
    );
}

#[test]
fn github_endpoint_with_oauth_token_is_copilot() {
    // The real localkarstest shape: models.github.ai + a gho_ OAuth token.
    let eps = vec!["https://models.github.ai/inference".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, Some("gho_"))).as_deref(),
        Some("github-copilot")
    );
    assert_eq!(
        id(classify_provider(None, &eps, Some("ghu_"))).as_deref(),
        Some("github-copilot")
    );
}

#[test]
fn github_endpoint_with_pat_is_models() {
    let eps = vec!["https://models.github.ai/inference".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, Some("ghp_"))).as_deref(),
        Some("github-models")
    );
    assert_eq!(
        id(classify_provider(None, &eps, None)).as_deref(),
        Some("github-models")
    );
}

#[test]
fn copilot_endpoint_is_copilot() {
    let eps = vec!["https://api.githubcopilot.com".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, None)).as_deref(),
        Some("github-copilot")
    );
}

#[tokio::test]
async fn controller_settings_refuse_sealed_or_replaced_workspaces_before_secret_access() {
    use axum::{
        Json, Router,
        http::{Method, Uri},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for (profile, phase, replaced) in [
        ("kcm-certificate", "Qualified", false),
        ("service-accounts", "Qualified", false),
        ("service-accounts", "Pending", false),
        ("service-accounts", "Qualified", true),
    ] {
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        let app = Router::new().fallback(move |method: Method, uri: Uri| {
            let reads = reads.clone();
            async move {
                assert_eq!(method, Method::GET, "Refused settings must never mutate objects");
                if uri.path().contains("/karscredentialgrants/") {
                    return Json(json!({
                        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsCredentialGrant",
                        "metadata":{"name":"workspace", "uid":"grant", "generation":1},
                        "spec":{"enabled":true, "workspaceUid":"workspace",
                            "controller":{"name":"kars-controller", "uid":"controller"},
                            "integrationStores":[], "agentKeys":[]},
                        "status":{"phase":"Ready", "observedGeneration":1,
                            "sources":[], "legacySources":[]}
                    }));
                }
                assert!(uri.path().starts_with("/api/v1/namespaces/")
                    && !uri.path().contains("/secrets/"), "No Secret reads or other resource access: {uri}");
                let read = reads.fetch_add(1, Ordering::SeqCst);
                Json(json!({"apiVersion":"v1", "kind":"Namespace", "metadata":{
                    "name":"kars-system", "uid":if replaced && read > 0 { "replacement" } else { "workspace" },
                    "annotations":{"kars.azure.com/private-enabled":"true",
                        "kars.azure.com/private-state":phase,
                        "kars.azure.com/private-controller-profile":profile}
                }}))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = kube::Config::new(
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cluster = super::Cluster {
            client: kube::Client::try_from(config).unwrap(),
        };
        let error = cluster
            .write_controller_environment(vec![
                json!({"name":"KARS_TASK_DEFAULT_MODEL", "value":"new-model"}),
            ])
            .await
            .unwrap_err();
        server.abort();
        assert_eq!(observed.load(Ordering::SeqCst), 2);
        match error {
            kube::Error::Api(status) => {
                assert_eq!(status.code, 409);
                assert!(
                    status.message.contains(if replaced {
                        "identity changed"
                    } else {
                        "No controller settings were saved"
                    }),
                    "{}",
                    status.message
                );
            }
            error => panic!("Unexpected refusal: {error}"),
        }
    }
}

#[test]
fn foundry_endpoint_and_empty() {
    let eps = vec!["https://my-proj.openai.azure.com".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, None)).as_deref(),
        Some("azure-foundry")
    );
    assert_eq!(id(classify_provider(None, &[], None)), None);
}

#[test]
fn local_inference_endpoint_is_not_mislabeled_as_foundry() {
    // A promoted local model's endpoint is always a Service DNS name in
    // the Bridge-owned kars-local-inference namespace — must be labeled
    // distinctly, not fall into the generic Foundry bucket every other
    // unrecognized endpoint gets.
    let eps = vec!["http://my-model.kars-local-inference.svc.cluster.local:80".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, None)).as_deref(),
        Some("local-inference")
    );
}
