// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{OrchestratorPrompt, call_llm, orchestrator_complete, orchestrator_via_router};
use crate::kars::cluster::Cluster;
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type Requests = Arc<Mutex<Vec<(Method, String, Value)>>>;

async fn fixture(response: Value) -> (String, Cluster, Requests, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    async fn handle(
        State((requests, response)): State<(Requests, Value)>,
        method: Method,
        uri: Uri,
        body: Bytes,
    ) -> (StatusCode, Json<Value>) {
        let body: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        };
        requests
            .lock()
            .unwrap()
            .push((method, uri.path().to_string(), body.clone()));
        if uri.path().ends_with("/chat/completions") && body.get("max_tokens").is_some() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {
                    "code": "unsupported_parameter", "param": "max_tokens",
                    "message": "Unsupported parameter: 'max_tokens'; use 'max_completion_tokens' instead."
                }})),
            );
        }
        (StatusCode::OK, Json(response))
    }
    let requests = Requests::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let config = kube::Config::new(endpoint.parse().unwrap());
    let config = Cluster::transport_config(config);
    let cluster = Cluster::for_test_client(kube::Client::try_from(config).unwrap());
    let app = Router::new()
        .fallback(handle)
        .with_state((requests.clone(), response));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (endpoint, cluster, requests, server)
}

#[tokio::test]
async fn mission_package_allowance_reaches_the_governed_router() {
    let maximum = super::super::mission::MISSION_COMPOSE_MAX_TOKENS;
    assert_eq!(maximum, 8192);
    let (_, cluster, requests, server) = fixture(json!({
        "choices": [{"finish_reason":"stop", "message":{"content":"{\"complete\":true}"}}]
    }))
    .await;
    let content = orchestrator_via_router(
        &cluster,
        "kars-orchestrator",
        "orchestrator-pod",
        "gpt-5.4-mini",
        OrchestratorPrompt {
            system: "system prompt",
            user: "repair every invalid role budget",
        },
        false,
        maximum,
    )
    .await
    .unwrap();
    assert_eq!(content, "{\"complete\":true}");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].2["max_completion_tokens"], maximum);
    assert!(requests[0].2.get("max_tokens").is_none());
    server.abort();
}

#[test]
fn repair_failure_preserves_the_call_error_and_original_diagnostics() {
    let message = super::repair_failure(
        "role budget insufficient; synthesis needs 8 tool calls",
        &anyhow::anyhow!("HTTP 429: reservation denied"),
    );
    assert!(message.contains("repair call failed (HTTP 429: reservation denied)"));
    assert!(message.contains("no corrected package was received"));
    assert!(message.contains("role budget insufficient; synthesis needs 8 tool calls"));
    assert!(!message.contains("after retry"));
}

#[tokio::test]
async fn incomplete_provider_responses_are_not_complete_packages() {
    for (response, expected) in [
        (
            json!({"choices":[{"finish_reason":"length","message":{"content":"{}"}}]}),
            "truncated at the requested 4096-token",
        ),
        (
            json!({"choices":[{"finish_reason":"content_filter","message":{"content":"{}"}}]}),
            "content filter",
        ),
        (
            json!({"choices":[{"finish_reason":"stop","message":{"content":" "}}]}),
            "no completion content",
        ),
    ] {
        let (endpoint, cluster, requests, server) = fixture(response).await;
        let direct = call_llm(
            &endpoint,
            "fixture-token",
            "gpt-5.4-mini",
            "system prompt",
            "user prompt",
            4096,
        )
        .await
        .unwrap_err();
        let routed = orchestrator_via_router(
            &cluster,
            "work",
            "agent-pod",
            "gpt-5.4-mini",
            OrchestratorPrompt {
                system: "system prompt",
                user: "user prompt",
            },
            false,
            4096,
        )
        .await
        .unwrap_err();
        assert!(direct.to_string().contains(expected), "{direct}");
        assert!(routed.to_string().contains(expected), "{routed}");
        assert_eq!(requests.lock().unwrap().len(), 2);
        server.abort();
    }
    let (_, cluster, requests, server) =
        fixture(json!({"stop_reason":"max_tokens","content":[{"type":"text","text":"{}"}]})).await;
    let error = orchestrator_via_router(
        &cluster,
        "work",
        "agent-pod",
        "claude-sonnet-5",
        OrchestratorPrompt {
            system: "system prompt",
            user: "user prompt",
        },
        true,
        4096,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("truncated at the requested 4096-token")
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
    server.abort();
}

fn chat_response() -> Value {
    json!({"choices": [{"message": {"content": "a real response body"}}]})
}

fn expected_chat(model: &str, limit: u32) -> Value {
    json!({"model": model, "messages": [
        {"role": "system", "content": "system prompt"},
        {"role": "user", "content": "user prompt"}
    ], "max_completion_tokens": limit, "response_format": {"type": "json_object"}})
}

#[tokio::test]
async fn direct_chat_bounds_completion_tokens_for_legacy_reasoning_and_deployment_aliases() {
    let (endpoint, _, requests, server) = fixture(chat_response()).await;
    for model in ["gpt-4.1", "gpt-5.4-mini", "o3", "operator-deployment"] {
        let result = call_llm(
            &format!("{endpoint}/v1/"),
            "fixture-token",
            model,
            "system prompt",
            "user prompt",
            1200,
        )
        .await
        .unwrap();
        assert_eq!(result, "a real response body");
        assert_eq!(
            requests.lock().unwrap().last().unwrap(),
            &(
                Method::POST,
                "/v1/chat/completions".into(),
                expected_chat(model, 1200)
            )
        );
    }
    assert_eq!(requests.lock().unwrap().len(), 4);
    server.abort();
}

#[tokio::test]
async fn native_chat_bounds_reasoning_output_through_the_exact_governed_proxy_path() {
    let (_, cluster, requests, server) = fixture(chat_response()).await;
    let result = orchestrator_via_router(
        &cluster,
        "work",
        "agent-pod",
        "operator-deployment",
        OrchestratorPrompt {
            system: "system prompt",
            user: "user prompt",
        },
        false,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(result, "a real response body");
    assert_eq!(
        *requests.lock().unwrap(),
        vec![(
            Method::POST,
            "/api/v1/namespaces/work/pods/agent-pod:8443/proxy/v1/chat/completions".into(),
            expected_chat("operator-deployment", 4096)
        )]
    );
    server.abort();
}

#[tokio::test]
async fn anthropic_messages_preserve_the_native_token_limit_and_text_blocks() {
    let (_, cluster, requests, server) = fixture(json!({"content": [
        {"type": "text", "text": "first"}, {"type": "tool_use", "name": "ignored"}, {"type": "text", "text": "second"}
    ]})).await;
    let result = orchestrator_via_router(
        &cluster,
        "work",
        "agent-pod",
        "claude-sonnet-5",
        OrchestratorPrompt {
            system: "system prompt",
            user: "user prompt",
        },
        true,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(result, "firstsecond");
    assert_eq!(
        *requests.lock().unwrap(),
        vec![(
            Method::POST,
            "/api/v1/namespaces/work/pods/agent-pod:8443/proxy/v1/messages".into(),
            json!({
                "model": "claude-sonnet-5", "system": "system prompt", "messages": [{"role": "user", "content": "user prompt"}], "max_tokens": 4096
            })
        )]
    );
    server.abort();
}

fn native_source() -> Value {
    json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
        "metadata":{"name":"bridge-orchestrator", "namespace":"kars-system",
            "uid":"standing-source", "resourceVersion":"1"},
        "spec":{"inferenceRef":{"name":"bridge-orchestrator-inference"}},
        "status":{"namespace":"work"}
    })
}

async fn native_fixture(
    source: Option<Value>,
    pods: Vec<Value>,
) -> (Cluster, Requests, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let requests = Requests::default();
    let recorded = requests.clone();
    let app = Router::new().fallback(move |method: Method, uri: Uri| {
        let requests = recorded.clone();
        let source = source.clone();
        let pods = pods.clone();
        async move {
            requests.lock().unwrap().push((method.clone(), uri.path().into(), Value::Null));
            let path = uri.path();
            if path.ends_with("/deployments/kars-controller") {
                return (StatusCode::OK, Json(json!({
                    "apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"kars-controller"},
                    "spec":{"selector":{"matchLabels":{"app":"controller"}},
                        "template":{"spec":{"containers":[{"name":"controller","env":[]}]}}}
                })));
            }
            if path.ends_with("/karssandboxes") {
                return (StatusCode::OK, Json(json!({
                    "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandboxList","items":[]
                })));
            }
            if path.ends_with("/karssandboxes/bridge-orchestrator") {
                return match source {
                    Some(source) => (StatusCode::OK, Json(source)),
                    None => (StatusCode::NOT_FOUND, Json(json!({
                        "apiVersion":"v1", "kind":"Status", "status":"Failure",
                        "reason":"NotFound", "message":"standing source absent", "code":404
                    }))),
                };
            }
            if path == "/api/v1/namespaces/work/pods" {
                assert_eq!(uri.query().map(|query| query.trim_start_matches('&')), Some("labelSelector=kars.azure.com%2Fsandbox%3Dbridge-orchestrator"));
                return (StatusCode::OK, Json(json!({"apiVersion":"v1","kind":"PodList","items":pods})));
            }
            if path.ends_with("/inferencepolicies/bridge-orchestrator-inference") {
                return (StatusCode::NOT_FOUND, Json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404})));
            }
            assert_eq!(method, Method::POST);
            let message = if path.contains("/first:") {
                "original accounting settlement unavailable"
            } else {
                assert!(path.contains("/second:"), "unexpected proxy path: {path}");
                "subsequent provider unavailable"
            };
            (StatusCode::SERVICE_UNAVAILABLE, Json(json!({
                "apiVersion":"v1","kind":"Status","status":"Failure","reason":"ServiceUnavailable",
                "message":message,"code":503
            })))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = kube::Config::new(
        format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let config = Cluster::transport_config(config);
    let cluster = Cluster::for_test_client(kube::Client::try_from(config).unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (cluster, requests, server)
}

#[tokio::test]
async fn router_candidates_exclude_terminating_and_private_pods_even_when_ready() {
    let pod = |name: &str, deleting: bool, ready: bool, phase: &str| {
        let mut value = json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
            "name": name, "namespace": "work", "labels": {"kars.azure.com/sandbox": "bridge-orchestrator"}
        }, "status": {"phase": phase, "containerStatuses": [{"name": "inference-router", "image": "router:latest", "imageID": "fixture", "ready": ready, "restartCount": 0}]}});
        if deleting {
            value["metadata"]["deletionTimestamp"] = json!("2026-10-02T14:25:00Z");
        }
        value
    };
    let mut registered = pod("private-router", false, true, "Running");
    registered["metadata"]["annotations"] =
        json!({"kars.azure.com/sre-registration-uid":"registration"});
    let mut credentialed = pod("credentialed-router", false, true, "Running");
    credentialed["metadata"]["annotations"] =
        json!({"kars.azure.com/sre-private-credential-uid":"credential"});
    let mut customer = pod("customer-mission", false, true, "Running");
    customer["metadata"]["labels"]["kars.azure.com/sandbox"] = json!("customer-mission");
    customer["metadata"]["creationTimestamp"] = json!("2026-10-25T19:00:00Z");
    let mut unrelated_namespace = pod("unrelated-router", false, true, "Running");
    unrelated_namespace["metadata"]["namespace"] = json!("other-workspace");
    let (cluster, requests, server) = native_fixture(
        Some(native_source()),
        vec![
            pod("old-router", true, true, "Running"),
            pod("new-router", false, true, "Running"),
            pod("warming-router", false, false, "Running"),
            pod("pending-router", false, true, "Pending"),
            registered,
            credentialed,
            customer,
            unrelated_namespace,
        ],
    )
    .await;
    assert_eq!(
        cluster.running_sandbox_candidates().await.unwrap(),
        vec![("work".into(), "new-router".into())]
    );
    assert_eq!(
        *requests.lock().unwrap(),
        vec![
            (Method::GET, "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/bridge-orchestrator".into(), Value::Null),
            (Method::GET, "/api/v1/namespaces/work/pods".into(), Value::Null),
        ]
    );
    server.abort();
}

#[tokio::test]
async fn native_fallback_preserves_the_original_router_failure() {
    let pods = ["first", "second"]
        .into_iter()
        .map(|name| {
            json!({
                "apiVersion":"v1","kind":"Pod", "metadata":{"name":name,"namespace":"work",
                    "labels":{"kars.azure.com/sandbox":"bridge-orchestrator"}},
                "status":{"phase":"Running","containerStatuses":[{"name":"inference-router",
                    "image":"router:latest","imageID":"fixture","ready":true,"restartCount":0}]}
            })
        })
        .collect();
    let (cluster, requests, server) = native_fixture(Some(native_source()), pods).await;
    let error = orchestrator_complete(&cluster, "system", "user", "model", 1024)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("work/first: ApiError: original accounting settlement unavailable"),
        "{error}"
    );
    assert!(
        error.contains("work/second: ApiError: subsequent provider unavailable"),
        "{error}"
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| entry.0 == Method::POST)
            .count(),
        2
    );
    server.abort();
}

#[tokio::test]
async fn native_composition_never_borrows_a_customer_router_when_the_standing_actor_is_unavailable()
{
    for condition in [
        "absent",
        "unreconciled",
        "invalid",
        "suspended",
        "warming",
        "missing-pod",
    ] {
        let mut source = native_source();
        if condition == "unreconciled" {
            source["status"] = json!({});
        } else if condition == "invalid" {
            source["spec"]["inferenceRef"]["name"] = json!("customer-inference");
        } else if condition == "suspended" {
            source["spec"]["suspended"] = json!(true);
        }
        let mut pods = vec![json!({
            "apiVersion":"v1", "kind":"Pod",
            "metadata":{"name":"customer-mission", "namespace":"work",
                "labels":{"kars.azure.com/sandbox":"customer-mission"}},
            "status":{"phase":"Running", "containerStatuses":[{"name":"inference-router",
                "image":"router:latest", "imageID":"fixture", "ready":true,"restartCount":0}]}
        })];
        if condition == "warming" {
            let mut warming = pods[0].clone();
            warming["metadata"]["name"] = json!("standing-warming");
            warming["metadata"]["labels"]["kars.azure.com/sandbox"] = json!("bridge-orchestrator");
            warming["status"]["containerStatuses"][0]["ready"] = json!(false);
            pods.push(warming);
        }
        let (cluster, requests, server) =
            native_fixture((condition != "absent").then_some(source), pods).await;
        let error = orchestrator_complete(&cluster, "system", "user", "model", 1024)
            .await
            .unwrap_err()
            .to_string();
        if condition == "invalid" {
            assert!(
                error.contains("source identity or inference reference is invalid"),
                "{error}"
            );
        } else {
            assert!(
                error.contains("no inference path yet"),
                "{condition}: {error}"
            );
        }
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .all(|entry| entry.0 == Method::GET)
        );
        server.abort();
    }
}

async fn funding_fixture(
    env: Value,
    sandboxes: Vec<Value>,
    failure: Option<&'static str>,
) -> (Cluster, Requests, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let requests = Requests::default();
    let recorded = requests.clone();
    let app = Router::new().fallback(move |method: Method, uri: Uri| {
        let requests = recorded.clone();
        let env = env.clone();
        let sandboxes = sandboxes.clone();
        async move {
            requests.lock().unwrap().push((method.clone(), uri.path().into(), Value::Null));
            assert_eq!(method, Method::GET, "funding refusal must precede model/proxy sends");
            if uri.path().ends_with("/deployments/kars-controller") {
                if failure == Some("controller") {
                    return (StatusCode::FORBIDDEN, Json(json!({
                        "apiVersion":"v1","kind":"Status","status":"Failure",
                        "reason":"Forbidden","message":"denied","code":403
                    })));
                }
                return (StatusCode::OK, Json(json!({
                    "apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"kars-controller"},
                    "spec":{"selector":{"matchLabels":{"app":"controller"}},
                        "template":{"spec":{"containers":[{"name":"controller","env":env}]}}}
                })));
            }
            assert_eq!(uri.path(), "/apis/kars.azure.com/v1alpha1/karssandboxes");
            if failure == Some("sources") {
                return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({
                    "apiVersion":"v1","kind":"Status","status":"Failure",
                    "reason":"ServiceUnavailable","message":"unavailable","code":503
                })));
            }
            (StatusCode::OK, Json(json!({
                "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandboxList",
                "metadata":{},"items":sandboxes
            })))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = kube::Config::new(
        format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let config = Cluster::transport_config(config);
    let cluster = Cluster::for_test_client(kube::Client::try_from(config).unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (cluster, requests, server)
}

#[tokio::test]
async fn funded_orchestrator_refuses_before_any_direct_or_fallback_model_send() {
    let grants = json!([{
        "root":{"kind":"KarsSandbox","resource":{"namespace":"kars-system",
            "name":"bridge-orchestrator","uid":"standing-uid"},
            "workspaceUid":"workspace-uid","clusterUid":"cluster-uid"},
        "limits":{"tokens":202402},"expiresAt":"2026-10-13T12:19:35Z"
    }]);
    for env in [
        json!([{"name":"KARS_BUILTIN_INFERENCE_FUNDING","value":grants.to_string()}]),
        json!([{"name":"KARS_BUILTIN_INFERENCE_FUNDING","value":"invalid"}]),
        json!([{"name":"KARS_BUILTIN_INFERENCE_FUNDING","valueFrom":{"configMapKeyRef":{"name":"funding","key":"intent"}}}]),
    ] {
        let (cluster, requests, server) = funding_fixture(env, vec![], None).await;
        let result = orchestrator_complete(&cluster, "system", "user", "gpt-5.4-mini", 4096).await;
        assert!(result.is_err());
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "no candidate lookup or model send"
        );
        server.abort();
    }
}

#[tokio::test]
async fn funded_orchestrator_pin_prevents_downgrade_after_declaration_removal() {
    let source = json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
        "metadata":{"name":"bridge-orchestrator","namespace":"custom-workspace","uid":"standing-uid"},
        "spec":{"inferenceBudgetRef":{"root":{"kind":"KarsSandbox"},"account":{"uid":"account-uid"}}}
    });
    let (cluster, requests, server) = funding_fixture(json!([]), vec![source], None).await;
    let error = orchestrator_complete(&cluster, "system", "user", "model", 1024)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("same account"));
    assert_eq!(requests.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn funded_orchestrator_unknown_selection_fails_closed_but_legacy_unconfigured_is_allowed() {
    for failure in [Some("controller"), Some("sources"), None] {
        let (cluster, requests, server) = funding_fixture(json!([]), vec![], failure).await;
        assert_eq!(
            cluster.funded_orchestrator_route().await.is_ok(),
            failure.is_none()
        );
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.0 == Method::GET)
        );
        server.abort();
    }

    let sre_only = json!([{"name":"KARS_BUILTIN_INFERENCE_FUNDING",
        "value":json!([{"root":{"resource":{"name":"sre"}}}]).to_string()}]);
    let (cluster, _, server) = funding_fixture(sre_only, vec![], None).await;
    assert!(
        cluster.funded_orchestrator_route().await.is_err(),
        "retired environment configuration cannot authorize legacy fallback"
    );
    server.abort();
}

async fn governed_route_fixture(fault: &str) -> (Cluster, Requests, tokio::task::JoinHandle<()>) {
    use axum::response::IntoResponse;
    use std::collections::BTreeMap;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let core = std::env::var("BRIDGE_CORE_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let runtime = "kars-bridge-orchestrator";
    let digest = format!("sha256:{}", "7".repeat(64));
    let root = json!({"kind":"KarsSandbox","resource":{"namespace":core,"name":"bridge-orchestrator","uid":"source-uid"},
            "workspaceUid":"workspace-uid","clusterUid":"cluster-uid"});
    let expiry = if fault.starts_with("renewed-") || fault == "expired-unchanged" {
        "2000-01-01T00:00:00Z"
    } else {
        "2035-01-01T00:00:00Z"
    };
    let grant = json!({"root":root,"limits":{"tokens":202402},"expiresAt":expiry});
    let mut canonical = grant.clone();
    canonical.sort_all_objects();
    let authorization = format!(
        "sha256:{}",
        crate::providers::signing::sha256_hex(serde_json::to_vec(&canonical).unwrap())
    );
    let bound = json!({"scope":"GovernedInference",
            "account":{"namespace":core,"name":"inference-budget-source-uid","uid":"account-uid"},
            "root":root,"taskUid":"source-uid","parentTaskUid":null,"rootTaskUid":"source-uid","authorizationDigest":authorization});
    let router_binding = json!({"task":bound,"sandbox":root["resource"],
            "runtimeNamespace":runtime,"runtimeNamespaceUid":"runtime-uid","privacyEpoch":"current-epoch"});
    let meta = |name: &str, namespace: &str, uid: &str| {
        json!({
            "name":name,"namespace":namespace,"uid":uid,"resourceVersion":"1","generation":1
        })
    };
    let controller = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":meta("kars-controller",&core,"controller-uid"),
    "spec":{"selector":{"matchLabels":{"app":"controller"}},"template":{"spec":{"containers":[{
        "name":"controller","env":[
            {"name":"KARS_INFERENCE_BUDGET_ENABLED","value":"true"},
            {"name":"KARS_INFERENCE_BUDGET_ROUTER_DIGEST","value":digest},
            {"name":"KARS_INFERENCE_BUDGET_CATALOG","value":"kars-inference-budget-contracts"}
        ]
    }]}}}});
    let mut catalog = json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":meta("kars-inference-budget-contracts",&core,"catalog-uid"),
        "data":{"built-in-funding.json":json!([grant]).to_string()}});
    catalog["metadata"]["annotations"] = json!({"kars.azure.com/inference-budget-contracts":"v1"});
    let source = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":meta("bridge-orchestrator",&core,"source-uid"),"spec":{"inferenceBudgetRef":bound}});
    let zero = json!({"tokens":0,"usdMicros":0});
    let meters = json!({"reserved":zero,"settled":zero,"uncertain":zero,"unpricedAttempts":0});
    let mut account = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsBudgetAccount",
    "metadata":meta("inference-budget-source-uid",&core,"account-uid"),
    "spec":{"scope":"GovernedInference","root":root,"limits":grant["limits"]},
    "status":{"phase":"Active","observedGeneration":1,"conditions":[
        {"type":"Ready","status":"True","observedGeneration":1},
        {"type":"LedgerValid","status":"True","observedGeneration":1}],
        "ledger":{"version":"governed-inference/v1","scope":"GovernedInference","accountUid":"account-uid",
            "root":root,"limits":grant["limits"],"phase":"Active","meters":meters,"sessions":{},"attempts":{},
            "nodes":{"source-uid":{"active":true,"meters":meters,"authority":{
                "task":root["resource"],"parentUid":null,"rootTaskUid":"source-uid",
                "authorizationDigest":authorization,"effectiveAuthorization":grant,"limits":grant["limits"]
            }}}
        }
    }});
    if fault.starts_with("renewed-") {
        use crate::inference_budget_contract::ledger::{Ledger, TokenTopUp};
        let original: Ledger = serde_json::from_value(account["status"]["ledger"].clone()).unwrap();
        let closed = original.close_account().unwrap().next;
        let expiry = if fault == "renewed-expired" {
            "2001-01-01T00:00:00Z"
        } else {
            "2035-01-01T00:00:00Z"
        };
        let renewal: TokenTopUp = serde_json::from_value(json!({
            "id":"renewal-1", "configUid":"catalog-uid", "accountUid":"account-uid", "root":root,
            "originalLimits":grant["limits"], "tokens":100000, "expiresAt":expiry,
            "nodes":{"source-uid":authorization}, "fundingRenewal":{
                "originalExpiresAt":grant["expiresAt"], "previousTopUpIds":[],
                "bridgeReactivation":{"closedLedgerDigest":closed.funding_renewal_state_digest().unwrap()}
            }
        })).unwrap();
        let applied = closed
            .apply_token_top_up(&renewal, 946_684_800)
            .unwrap()
            .next;
        account["status"]["ledger"] = serde_json::to_value(&applied).unwrap();
        catalog["data"]["token-top-ups.json"] =
            json!(serde_json::to_string(&vec![renewal]).unwrap());
        if fault == "renewed-missing-receipt" {
            account["status"]["ledger"]["tokenTopUps"] = json!({});
        }
        if fault == "renewed-reclosed" {
            account["status"]["ledger"] =
                serde_json::to_value(applied.close_account().unwrap().next).unwrap();
        }
    }
    account["metadata"]["annotations"] =
        json!({"kars.azure.com/inference-budget-bootstrap":"sealed"});
    account["metadata"]["labels"] = json!({"app.kubernetes.io/managed-by":"kars-inference-budget"});
    let router = json!({"name":"inference-router","image":format!("router:latest@{digest}"),
            "env":[{"name":"KARS_INFERENCE_BUDGET_REQUIRED","value":"true"},
                {"name":"KARS_INFERENCE_BUDGET_BINDING","value":router_binding.to_string()}]});
    let pod_spec = json!({"serviceAccountName":"sandbox","containers":[router]});
    let owner = |kind: &str, name: &str, uid: &str| {
        json!([{
            "apiVersion":"apps/v1","kind":kind,"name":name,"uid":uid,"controller":true
        }])
    };
    let mut pod = json!({"apiVersion":"v1","kind":"Pod","metadata":meta("standing-pod",runtime,"pod-uid"),
            "spec":pod_spec,"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],
                "containerStatuses":[{"name":"inference-router","image":"router:latest","imageID":"image","restartCount":0,"ready":true}]}});
    pod["metadata"]["ownerReferences"] = owner("ReplicaSet", "standing-rs", "rs-uid");
    pod["metadata"]["labels"] = json!({"kars.azure.com/sandbox":"bridge-orchestrator"});
    let mut rs = json!({"apiVersion":"apps/v1","kind":"ReplicaSet","metadata":meta("standing-rs",runtime,"rs-uid"),
            "spec":{"selector":{"matchLabels":{"app":"standing"}},"template":{"spec":pod_spec}}});
    rs["metadata"]["ownerReferences"] =
        owner("Deployment", "bridge-orchestrator", "deployment-uid");
    let deployment = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":meta("bridge-orchestrator",runtime,"deployment-uid"),
            "spec":{"replicas":1,"selector":{"matchLabels":{"app":"standing"}},"template":{"spec":pod_spec}}});
    let mut namespace =
        json!({"apiVersion":"v1","kind":"Namespace","metadata":meta(runtime,"","runtime-uid")});
    namespace["metadata"]["labels"] = json!({"kars.azure.com/inference-budget":"v1"});
    namespace["metadata"]["annotations"] = json!({
            "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":core,
            "kars.azure.com/sandbox-name":"bridge-orchestrator","kars.azure.com/sandbox-uid":"source-uid"});
    let controller_path = format!("/apis/apps/v1/namespaces/{core}/deployments/kars-controller");
    let source_path = format!(
        "/apis/kars.azure.com/v1alpha1/namespaces/{core}/karssandboxes/bridge-orchestrator"
    );
    let account_path = format!(
        "/apis/kars.azure.com/v1alpha1/namespaces/{core}/karsbudgetaccounts/inference-budget-source-uid"
    );
    let pod_path = format!("/api/v1/namespaces/{runtime}/pods/standing-pod");
    let list_path = format!("/api/v1/namespaces/{runtime}/pods");
    let deployment_path =
        format!("/apis/apps/v1/namespaces/{runtime}/deployments/bridge-orchestrator");
    let catalog_path =
        format!("/api/v1/namespaces/{core}/configmaps/kars-inference-budget-contracts");
    let mut objects = BTreeMap::from([
        (catalog_path.clone(), catalog),
        (controller_path.clone(), controller),
        (source_path.clone(), source),
        (account_path.clone(), account),
        (pod_path.clone(), pod),
        (deployment_path.clone(), deployment),
        (
            format!("/apis/apps/v1/namespaces/{runtime}/replicasets/standing-rs"),
            rs,
        ),
        (format!("/api/v1/namespaces/{runtime}"), namespace),
        (
            format!("/api/v1/namespaces/{core}"),
            json!({"apiVersion":"v1","kind":"Namespace","metadata":meta(&core,"","workspace-uid")}),
        ),
        (
            "/api/v1/namespaces/kube-system".into(),
            json!({"apiVersion":"v1","kind":"Namespace","metadata":meta("kube-system","","cluster-uid")}),
        ),
    ]);
    match fault {
        "source-parent-pruned" => {
            objects.get_mut(&source_path).unwrap()["spec"]["inferenceBudgetRef"]
                .as_object_mut()
                .unwrap()
                .remove("parentTaskUid");
        }
        "source-binding-unknown" => {
            objects.get_mut(&source_path).unwrap()["spec"]["inferenceBudgetRef"]["unknownAuthority"] =
                json!(true);
        }
        "catalog-default" => {
            objects.get_mut(&controller_path).unwrap()["spec"]["template"]["spec"]["containers"][0]
                ["env"]
                .as_array_mut()
                .unwrap()
                .pop();
        }
        "declaration-removed" => {
            objects.get_mut(&catalog_path).unwrap()["data"]["built-in-funding.json"] = json!("[]");
        }
        "catalog-setting-duplicated" => {
            objects.get_mut(&controller_path).unwrap()["spec"]["template"]["spec"]["containers"][0]
                ["env"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"KARS_INFERENCE_BUDGET_CATALOG","value":"another-catalog"}));
        }
        "catalog-setting-indirect" => {
            objects.get_mut(&controller_path).unwrap()["spec"]["template"]["spec"]["containers"]
                [0]["env"][2] = json!({"name":"KARS_INFERENCE_BUDGET_CATALOG","valueFrom":{"configMapKeyRef":{"name":"other","key":"name"}}});
        }
        "catalog-setting-invalid" => {
            objects.get_mut(&controller_path).unwrap()["spec"]["template"]["spec"]["containers"]
                [0]["env"][2]["value"] = json!("../other");
        }
        "source-uid" => {
            objects.get_mut(&source_path).unwrap()["metadata"]["uid"] = json!("replacement")
        }
        "account-uid" => {
            objects.get_mut(&account_path).unwrap()["metadata"]["uid"] = json!("replacement")
        }
        "account-unready" => {
            objects.get_mut(&account_path).unwrap()["status"]["conditions"][0]["status"] =
                json!("False")
        }
        "account-stale" => {
            objects.get_mut(&account_path).unwrap()["status"]["observedGeneration"] = json!(2)
        }
        "account-inactive" => {
            objects.get_mut(&account_path).unwrap()["status"]["ledger"]["phase"] = json!("Closed")
        }
        "authority-changed" => {
            objects.get_mut(&account_path).unwrap()["status"]["ledger"]["nodes"]["source-uid"]["authority"]
                ["effectiveAuthorization"]["limits"]["tokens"] = json!(1)
        }
        "node-inactive" => {
            objects.get_mut(&account_path).unwrap()["status"]["ledger"]["nodes"]["source-uid"]["active"] =
                json!(false);
        }
        "ledger-malformed" => {
            objects.get_mut(&account_path).unwrap()["status"]["ledger"]["meters"] = Value::Null;
        }
        "duplicate-selection" => {
            objects.get_mut(&catalog_path).unwrap()["data"]["built-in-funding.json"] =
                json!(json!([grant, grant]).to_string());
        }
        "catalog-malformed" => {
            objects.get_mut(&catalog_path).unwrap()["data"]["built-in-funding.json"] =
                json!("invalid");
        }
        "catalog-oversized" => {
            objects.get_mut(&catalog_path).unwrap()["data"]["built-in-funding.json"] =
                json!(" ".repeat(32_769));
        }
        "catalog-unowned" => {
            objects.get_mut(&catalog_path).unwrap()["metadata"]["annotations"] = json!({});
        }
        "catalog-missing-uid" => {
            objects.get_mut(&catalog_path).unwrap()["metadata"]["uid"] = Value::Null;
        }
        "catalog-missing-version" => {
            objects.get_mut(&catalog_path).unwrap()["metadata"]["resourceVersion"] = Value::Null;
        }
        "catalog-deleting" => {
            objects.get_mut(&catalog_path).unwrap()["metadata"]["deletionTimestamp"] =
                json!("2026-10-14T00:00:00Z");
        }
        "optional-null" => {
            let mut configured = grant.clone();
            configured["limits"]["usdMicros"] = Value::Null;
            objects.get_mut(&catalog_path).unwrap()["data"]["built-in-funding.json"] =
                json!(json!([configured]).to_string());
        }
        "expired" => {
            let mut expired = grant.clone();
            expired["expiresAt"] = json!("2000-01-01T00:00:00Z");
            objects.get_mut(&catalog_path).unwrap()["data"]["built-in-funding.json"] =
                json!(json!([expired]).to_string());
        }
        "unrelated-expired" => {
            let mut expired = grant.clone();
            expired["root"]["resource"]["name"] = json!("sre");
            expired["root"]["resource"]["uid"] = json!("sre-uid");
            expired["expiresAt"] = json!("2000-01-01T00:00:00Z");
            objects.get_mut(&catalog_path).unwrap()["data"]["built-in-funding.json"] =
                json!(json!([expired, grant]).to_string());
        }
        "binding-epoch" => {
            let mut stale = router_binding.clone();
            stale["privacyEpoch"] = json!("stale-epoch");
            objects.get_mut(&pod_path).unwrap()["spec"]["containers"][0]["env"][1]["value"] =
                json!(stale.to_string());
        }
        "router-unfunded" => {
            objects.get_mut(&pod_path).unwrap()["spec"]["containers"][0]["env"][0]["value"] =
                json!("false")
        }
        "router-image" => {
            objects.get_mut(&pod_path).unwrap()["spec"]["containers"][0]["image"] =
                json!("router:latest")
        }
        "owner-uid" => {
            objects.get_mut(&pod_path).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
                json!("replacement")
        }
        "pod-unready" => {
            objects.get_mut(&pod_path).unwrap()["status"]["conditions"][0]["status"] =
                json!("False")
        }
        "namespace-uid" => {
            objects
                .get_mut(&format!("/api/v1/namespaces/{runtime}"))
                .unwrap()["metadata"]["uid"] = json!("replacement")
        }
        _ => {}
    }
    objects.insert(
        "/apis/kars.azure.com/v1alpha1/karssandboxes".into(),
        json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandboxList", "metadata":{},
            "items":[objects[&source_path].clone()]
        }),
    );
    let mut items = vec![objects[&pod_path].clone()];
    if fault == "ambiguous-pods" {
        items.push(objects[&pod_path].clone());
    }
    objects.insert(
        list_path,
        json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":items}),
    );
    let requests = Requests::default();
    let state = Arc::new(Mutex::new(objects));
    let recorded = requests.clone();
    let fault = fault.to_owned();
    let app = Router::new().fallback(move |method: Method, uri: Uri, body: Bytes| {
            let state = state.clone();
            let recorded = recorded.clone();
            let fault = fault.clone();
            let account_path = account_path.clone();
            let pod_path = pod_path.clone();
            let source_path = source_path.clone();
            let catalog_path = catalog_path.clone();
            let controller_path = controller_path.clone();
            async move {
                recorded.lock().unwrap().push((method.clone(),uri.path().into(),
                    if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() }));
                let refusal = || (StatusCode::FORBIDDEN,Json(json!({"apiVersion":"v1","kind":"Status",
                    "status":"Failure","code":403,"reason":"Forbidden","message":"refused"}))).into_response();
                if uri.path().ends_with("/proxy/readyz") {
                    if fault == "ready-unavailable" || fault == "renewed-ready-unavailable" { return refusal(); }
                    if fault == "pod-replaced" { state.lock().unwrap().get_mut(&pod_path).unwrap()["metadata"]["uid"] = json!("replacement"); }
                    if fault == "pin-replaced" { state.lock().unwrap().get_mut(&source_path).unwrap()["spec"]["inferenceBudgetRef"]["account"]["uid"] = json!("replacement"); }
                    if fault == "account-changed" { state.lock().unwrap().get_mut(&account_path).unwrap()["status"]["conditions"][0]["status"] = json!("False"); }
                    if fault == "catalog-replaced" { state.lock().unwrap().get_mut(&catalog_path).unwrap()["metadata"]["uid"] = json!("replacement"); }
                    if fault == "catalog-changed" { state.lock().unwrap().get_mut(&catalog_path).unwrap()["data"]["built-in-funding.json"] = json!("[]"); }
                    if fault == "controller-changed" { state.lock().unwrap().get_mut(&controller_path).unwrap()["spec"]["template"]["spec"]["containers"][0]["env"][0]["value"] = json!("false"); }
                    return (StatusCode::OK, if fault == "legacy-ready" { "ok" } else {
                        "governed inference authority and model contracts available"
                    }).into_response();
                }
                if method == Method::POST {
                    assert!(uri.path().starts_with("/api/v1/namespaces/kars-bridge-orchestrator/pods/standing-pod:8443/proxy/"));
                    if fault == "model-refused" { return refusal(); }
                    if fault == "model-quota" { return (StatusCode::TOO_MANY_REQUESTS,Json(json!({
                        "apiVersion":"v1","kind":"Status","status":"Failure","code":429,
                        "reason":"TooManyRequests","message":"reservation exceeds remaining funding"
                    }))).into_response(); }
                    if fault == "model-empty" { return Json(json!({"choices":[]})).into_response(); }
                    return Json(if uri.path().ends_with("/messages") {
                        json!({"content":[{"type":"text","text":"funded reply"}]})
                    } else { json!({"choices":[{"message":{"content":"funded reply"}}]}) }).into_response();
                }
                if uri.path().ends_with("/inferencepolicies/bridge-orchestrator-inference") {
                    if fault.starts_with("selected-") {
                        return Json(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy",
                            "metadata":{"name":"bridge-orchestrator-inference","namespace":"kars-system","uid":"policy","resourceVersion":"1","generation":1},
                            "spec":{"modelPreference":{"primary":{"provider":"azure-openai","deployment":fault},"fallback":[{"provider":"other","deployment":"retained"}]}},
                            "status":{"observedGeneration":1,"compiledDigest":"sha256:selected","loadedDigest":"sha256:selected","conditions":[{"type":"Ready","status":"True"}]}})).into_response();
                    }
                    return (StatusCode::NOT_FOUND, Json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404}))).into_response();
                }
                if fault == "account-unreadable" && uri.path() == account_path { return refusal(); }
                if fault == "catalog-unreadable" && uri.path() == catalog_path { return refusal(); }
                let objects = state.lock().unwrap();
                Json(objects.get(uri.path()).unwrap_or_else(|| panic!("unexpected route: {uri}")).clone()).into_response()
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = kube::Config::new(
        format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let config = Cluster::transport_config(config);
    let cluster = Cluster::for_test_client(kube::Client::try_from(config).unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (cluster, requests, server)
}

#[tokio::test]
async fn funded_orchestrator_composes_only_through_the_bound_router_for_both_wire_apis() {
    for (fault, model, suffix) in [
        ("none", "bounded-test-model", "/chat/completions"),
        ("none", "claude-test-model", "/messages"),
        ("renewed-valid", "bounded-test-model", "/chat/completions"),
        ("renewed-valid", "claude-test-model", "/messages"),
        ("optional-null", "bounded-test-model", "/chat/completions"),
        (
            "source-parent-pruned",
            "bounded-test-model",
            "/chat/completions",
        ),
        ("catalog-default", "bounded-test-model", "/chat/completions"),
        (
            "unrelated-expired",
            "bounded-test-model",
            "/chat/completions",
        ),
    ] {
        let (cluster, requests, server) = governed_route_fixture(fault).await;
        let (reply, source) = orchestrator_complete(&cluster, "system", "user", model, 1024)
            .await
            .unwrap();
        assert_eq!(reply, "funded reply");
        assert!(source.contains("funded orchestrator router"));
        let calls = requests.lock().unwrap();
        let posts: Vec<_> = calls.iter().filter(|call| call.0 == Method::POST).collect();
        assert_eq!(posts.len(), 1);
        assert!(posts[0].1.ends_with(suffix));
        assert!(calls.iter().any(|call| call.1.ends_with("/proxy/readyz")));
        assert!(!calls.iter().any(|call| call.1 == "/api/v1/pods"));
        drop(calls);
        server.abort();
    }
}

#[tokio::test]
async fn funded_orchestrator_uses_selected_policy_without_mutating_it() {
    for (model, suffix) in [
        ("selected-chat", "/chat/completions"),
        ("selected-claude", "/messages"),
    ] {
        let (cluster, requests, server) = governed_route_fixture(model).await;
        let (reply, _) =
            orchestrator_complete(&cluster, "system", "user", "ignored-recommendation", 1024)
                .await
                .unwrap();
        assert_eq!(reply, "funded reply");
        let calls = requests.lock().unwrap();
        let posts: Vec<_> = calls.iter().filter(|call| call.0 == Method::POST).collect();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].2["model"], model);
        assert!(posts[0].1.ends_with(suffix));
        assert!(
            calls
                .iter()
                .filter(|call| call.1.contains("/inferencepolicies/"))
                .all(|call| call.0 == Method::GET)
        );
        drop(calls);
        server.abort();
    }
}

#[tokio::test]
async fn funded_orchestrator_refuses_unverified_account_pod_epoch_and_readiness_without_a_send() {
    for fault in [
        "source-binding-unknown",
        "source-uid",
        "account-uid",
        "account-unready",
        "account-stale",
        "account-inactive",
        "authority-changed",
        "node-inactive",
        "ledger-malformed",
        "duplicate-selection",
        "declaration-removed",
        "catalog-setting-duplicated",
        "catalog-setting-indirect",
        "catalog-setting-invalid",
        "catalog-malformed",
        "catalog-oversized",
        "catalog-unowned",
        "catalog-missing-uid",
        "catalog-missing-version",
        "catalog-deleting",
        "catalog-unreadable",
        "catalog-replaced",
        "catalog-changed",
        "controller-changed",
        "expired",
        "expired-unchanged",
        "renewed-expired",
        "renewed-missing-receipt",
        "renewed-reclosed",
        "renewed-ready-unavailable",
        "binding-epoch",
        "router-unfunded",
        "router-image",
        "owner-uid",
        "pod-unready",
        "namespace-uid",
        "ambiguous-pods",
        "legacy-ready",
        "ready-unavailable",
        "pod-replaced",
        "pin-replaced",
        "account-unreadable",
        "account-changed",
    ] {
        let (cluster, requests, server) = governed_route_fixture(fault).await;
        assert!(
            orchestrator_complete(&cluster, "system", "user", "model", 1024)
                .await
                .is_err(),
            "{fault}"
        );
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .all(|call| call.0 == Method::GET),
            "{fault}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn funded_orchestrator_router_denial_is_terminal_not_a_direct_or_sandbox_fallback() {
    for fault in ["model-refused", "model-quota", "model-empty"] {
        let (cluster, requests, server) = governed_route_fixture(fault).await;
        assert!(
            orchestrator_complete(&cluster, "system", "user", "model", 1024)
                .await
                .is_err()
        );
        let calls = requests.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|call| call.0 == Method::POST).count(),
            1
        );
        assert!(!calls.iter().any(|call| call.1 == "/api/v1/pods"));
        drop(calls);
        server.abort();
    }
}
