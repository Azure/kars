use super::*;
use crate::providers::receipt::ReceiptTestSigner as SigningKey;
use crate::providers::signing::sha256_hex;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Extension, Path, State},
    http::{Method, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const PRIVATE: &str = "PRIVATE_RECEIPT_OR_API_BODY_MUST_NOT_ESCAPE";

fn entries(count: usize) -> Vec<Value> {
    let mut result = Vec::new();
    let mut previous = "genesis".to_string();
    for index in 0..count {
        let payload = format!("older-opaque-payload-{index}");
        let receipt = format!("work/task-{index}");
        let hash = chain_entry_hash(index as i64, &receipt, &payload, &previous);
        result.push(
            json!({"seq":index,"receipt":receipt,"payloadSha256":payload,
                           "prevHash":previous,"entryHash":hash}),
        );
        previous = hash;
    }
    result
}

fn map(namespace: &str, name: &str, data: Value) -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap",
           "metadata":{"name":name,"namespace":namespace,"uid":format!("uid-{name}"),"resourceVersion":"1"},
           "data":data})
}

fn log_maps(namespace: &str, chain: &[Value], split: Option<usize>) -> Vec<Value> {
    let cut = split.unwrap_or(chain.len());
    let mut maps = vec![map(
        namespace,
        HEAD,
        json!({"chain.json":serde_json::to_string(&chain[..cut]).unwrap()}),
    )];
    if let Some(split) = split {
        maps[0]["immutable"] = true.into();
        let mut segment = map(
            namespace,
            "kars-receipt-log-000001",
            json!({
            "chain.json":serde_json::to_string(&chain[split..]).unwrap(),
            "segmentIndex":"1","previousRootHash":chain[split-1]["entryHash"]}),
        );
        segment["metadata"]["labels"] = json!({COMPONENT:SEGMENT_COMPONENT});
        maps.push(segment);
    }
    maps
}

fn wire_snapshot(maps: Vec<Value>) -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMapList","metadata":{"resourceVersion":"snapshot-1"},"items":maps})
}

fn parsed(value: Value, namespace: &str) -> Result<ReceiptLog, ReceiptLogError> {
    let snapshot = serde_json::from_value(value).map_err(|_| invalid("malformed API snapshot"))?;
    parse_snapshot(snapshot, namespace)
}

#[test]
fn receipt_log_distinguishes_absent_legacy_empty_and_rotated_history() {
    assert!(!parsed(wire_snapshot(vec![]), "work").unwrap().present);
    let empty = parsed(wire_snapshot(log_maps("work", &[], None)), "work").unwrap();
    assert!(empty.present);
    assert!(empty.entries.is_empty());
    for split in [None, Some(2)] {
        let log = parsed(wire_snapshot(log_maps("work", &entries(5), split)), "work").unwrap();
        assert!(log.present);
        assert_eq!(log.entries.len(), 5);
        assert_eq!(log.entries[4].payload_sha256, "older-opaque-payload-4");
    }
}

fn invalid_snapshots(namespace: &str) -> Vec<Value> {
    let valid = wire_snapshot(log_maps(namespace, &entries(3), Some(1)));
    let mut cases = Vec::new();
    let mut push = |value: Value| cases.push(value);
    let mut value = valid.clone();
    value["items"][0]["data"]["chain.json"] = PRIVATE.into();
    push(value);
    let mut value = valid.clone();
    value["items"][0].as_object_mut().unwrap().remove("data");
    push(value);
    let mut value = valid.clone();
    value["items"][0]["data"] = json!({});
    push(value);
    let mut value = valid.clone();
    value["items"].as_array_mut().unwrap().remove(0);
    push(value);
    let mut value = valid.clone();
    value["items"][1]["metadata"]["name"] = "kars-receipt-log-000002".into();
    value["items"][1]["data"]["segmentIndex"] = "2".into();
    push(value);
    let mut value = valid.clone();
    value["items"][1]["data"]["previousRootHash"] = PRIVATE.into();
    push(value);
    let mut value = valid.clone();
    value["items"][1]["data"]
        .as_object_mut()
        .unwrap()
        .remove("segmentIndex");
    push(value);
    let mut value = valid.clone();
    value["items"][1]["data"]["segmentIndex"] = "2".into();
    push(value);
    let mut value = valid.clone();
    value["items"][1]["data"]["chain.json"] = "[]".into();
    push(value);
    let mut value = valid.clone();
    value["items"][1]["metadata"]["namespace"] = "foreign".into();
    push(value);
    let mut value = valid.clone();
    value["items"][1]["metadata"]["name"] = "foreign-name".into();
    push(value);
    let mut value = valid.clone();
    value["items"][0]["metadata"]["labels"] = json!({COMPONENT:"foreign"});
    push(value);
    let mut value = valid.clone();
    value["items"][0]["metadata"]["ownerReferences"] = json!([
        {"apiVersion":"v1","kind":"Pod","name":"foreign","uid":"foreign"}]);
    push(value);
    for key in ["uid", "resourceVersion"] {
        let mut value = valid.clone();
        value["items"][0]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        push(value);
    }
    let mut value = valid.clone();
    value["items"][0]["metadata"]["deletionTimestamp"] = "2026-09-11T00:00:00Z".into();
    push(value);
    let mut value = valid.clone();
    value["metadata"]["continue"] = "another-page".into();
    push(value);
    let mut value = valid.clone();
    value["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("resourceVersion");
    push(value);
    let mut value = valid.clone();
    value["kind"] = "SecretList".into();
    push(value);
    let mut value = valid.clone();
    value["apiVersion"] = "foreign/v1".into();
    push(value);
    let mut value = valid.clone();
    value["items"] = Value::Null;
    push(value);
    let mut value = valid.clone();
    value.as_object_mut().unwrap().remove("items");
    push(value);
    let mut value = valid.clone();
    let duplicate = value["items"][1].clone();
    value["items"].as_array_mut().unwrap().push(duplicate);
    push(value);
    let mut broken = entries(3);
    broken[2]["payloadSha256"] = PRIVATE.into();
    push(wire_snapshot(log_maps(namespace, &broken, Some(1))));
    let mut broken = entries(3);
    broken[2]["seq"] = 7.into();
    push(wire_snapshot(log_maps(namespace, &broken, Some(1))));
    push(wire_snapshot(vec![map("foreign", "unrelated", json!({}))]));
    cases
}

#[test]
fn receipt_log_rejects_invalid_history_and_foreign_or_incomplete_snapshots_without_payload_errors()
{
    for value in invalid_snapshots("work") {
        let error = parsed(value, "work").unwrap_err();
        assert!(!error.to_string().contains(PRIVATE));
    }
}

struct ApiState {
    snapshot: Value,
    subsequent_snapshot: Option<Value>,
    snapshot_reads: usize,
    status: u16,
    core_namespace: String,
    calls: Vec<(String, String)>,
    tasks: Vec<Value>,
    receipts: Vec<Value>,
    receipt_details: BTreeMap<String, Value>,
}

async fn api(State(state): State<Arc<Mutex<ApiState>>>, method: Method, uri: Uri) -> Response {
    let mut state = state.lock().unwrap();
    state.calls.push((method.to_string(), uri.to_string()));
    assert_eq!(
        method,
        Method::GET,
        "receipt readers must not mutate Kubernetes"
    );
    if let Some(value) = state.receipt_details.get(uri.path()) {
        return Json(value.clone()).into_response();
    }
    if uri.path() == format!("/api/v1/namespaces/{}/configmaps", state.core_namespace)
        && uri.query().is_none_or(str::is_empty)
    {
        if state.status != 200 {
            return (
                StatusCode::from_u16(state.status).unwrap(),
                Json(json!({
                    "apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden",
                    "code":state.status,"message":PRIVATE
                })),
            )
                .into_response();
        }
        state.snapshot_reads += 1;
        let snapshot = if state.snapshot_reads > 1 {
            state
                .subsequent_snapshot
                .as_ref()
                .unwrap_or(&state.snapshot)
        } else {
            &state.snapshot
        };
        return Json(snapshot.clone()).into_response();
    }
    if uri.path().ends_with("/karstasks") {
        return Json(
            json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTaskList",
                           "metadata":{"resourceVersion":"1"},"items":state.tasks}),
        )
        .into_response();
    }
    if uri.path().ends_with("/karsreceipts") {
        return Json(
            json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsReceiptList",
                           "metadata":{"resourceVersion":"1"},"items":state.receipts}),
        )
        .into_response();
    }
    if uri.path().ends_with("/configmaps") {
        return Json(wire_snapshot(vec![])).into_response();
    }
    if uri.path().ends_with("/karssandboxes") || uri.path().ends_with("/karsapprovals") {
        return Json(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"List",
                           "metadata":{"resourceVersion":"1"},"items":[]}))
        .into_response();
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({"apiVersion":"v1","kind":"Status",
        "status":"Failure","reason":"NotFound","code":404,"message":"missing"})),
    )
        .into_response()
}

async fn fixture(
    namespace: &str,
) -> (
    Cluster,
    crate::state::AppState,
    Arc<Mutex<ApiState>>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(Mutex::new(ApiState {
        snapshot: wire_snapshot(vec![]),
        subsequent_snapshot: None,
        snapshot_reads: 0,
        status: 200,
        core_namespace: namespace.into(),
        calls: vec![],
        tasks: vec![],
        receipts: vec![],
        receipt_details: BTreeMap::new(),
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let application = Router::new().fallback(api).with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, application).await.unwrap() });
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = kube::Client::try_from(kube::Config::new(
        format!("http://{address}").parse().unwrap(),
    ))
    .unwrap();
    (
        Cluster::for_test_client(client.clone()),
        crate::state::AppState::for_test_client(client, "work"),
        state,
        server,
    )
}

async fn request(state: crate::state::AppState, path: &str, owner: bool) -> (StatusCode, Value) {
    request_with_pins(state, path, owner, None).await
}

async fn request_with_pins(
    state: crate::state::AppState,
    path: &str,
    owner: bool,
    pins: Option<crate::routes::receipts::AnchorPins>,
) -> (StatusCode, Value) {
    let verify = move |state: State<crate::state::AppState>,
                       principal: Extension<crate::auth::Principal>,
                       path: Path<(String, String)>| {
        let pins = pins.clone();
        async move {
            match pins {
                Some(pins) => {
                    crate::routes::receipts::verify_receipt_with_pins(
                        state,
                        principal,
                        path,
                        Ok(pins),
                    )
                    .await
                }
                None => crate::routes::receipts::verify_receipt(state, principal, path).await,
            }
        }
    };
    let app = Router::new()
        .route("/api/insights", get(crate::routes::insights::get_insights))
        .route("/api/system", get(crate::routes::system::get_system))
        .route(
            "/api/operator/audit",
            get(crate::routes::operator::get_audit),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/receipt/verify",
            get(verify),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/receipt",
            get(crate::routes::receipts::get_receipt),
        )
        .with_state(state);
    let mut request = Request::get(path).body(Body::empty()).unwrap();
    request.extensions_mut().insert(crate::auth::Principal {
        sub: "owner".into(),
        name: "owner".into(),
        roles: vec![if owner { "user" } else { "operator" }.into()],
    });
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains(PRIVATE));
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn receipt_summary_handlers_count_legacy_and_overflow_from_one_complete_snapshot() {
    let namespace = std::env::var("BRIDGE_CORE_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let (_, state, api, server) = fixture(&namespace).await;
    for split in [None, Some(1)] {
        for path in ["/api/insights", "/api/system", "/api/operator/audit"] {
            let separate_trace_read = path == "/api/system" && namespace == "kars-system";
            {
                let mut api = api.lock().unwrap();
                api.snapshot = wire_snapshot(log_maps(&namespace, &entries(4), split));
                api.snapshot_reads = 0;
                // System also lists legacy trace ConfigMaps. Its separate read
                // must not provide or overwrite the receipt snapshot.
                api.subsequent_snapshot = separate_trace_read.then(|| {
                    wire_snapshot(vec![map(
                        &namespace,
                        "kars-mission-trace-other",
                        json!({"trace.json":"{\"frames\":[]}","assignmentNonce":"other"}),
                    )])
                });
                api.calls.clear();
            }
            let (status, body) = request(state.clone(), path, false).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            let size = if path == "/api/system" {
                &body["counts"]["inclusion_log_size"]
            } else {
                &body["inclusion_log_size"]
            };
            assert_eq!(size, &json!(4), "{path}");
            if separate_trace_read {
                assert_eq!(body["counts"]["trace_records"], 1);
            }
            if path == "/api/operator/audit" {
                assert_eq!(body["integrity"]["tree_size"], 4);
                assert_eq!(body["integrity"]["chain_consistent"], true);
                assert_eq!(body["integrity"]["checkpoint_verified"], false);
            }
            let api = api.lock().unwrap();
            assert_eq!(
                api.calls
                    .iter()
                    .filter(|(_, path)| path.trim_end_matches('?')
                        == format!("/api/v1/namespaces/{namespace}/configmaps"))
                    .count(),
                if separate_trace_read { 2 } else { 1 },
                "{path}: receipt snapshot plus the existing independent trace read"
            );
        }
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn receipt_summary_handlers_never_turn_malformed_history_or_api_failure_into_zero() {
    let namespace = std::env::var("BRIDGE_CORE_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let (_, state, api, server) = fixture(&namespace).await;
    for snapshot in invalid_snapshots(&namespace) {
        api.lock().unwrap().snapshot = snapshot;
        for path in ["/api/insights", "/api/system", "/api/operator/audit"] {
            let (status, body) = request(state.clone(), path, false).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
            assert_eq!(body["error"]["code"], "upstream_error");
            assert!(body.get("inclusion_log_size").is_none());
            assert!(body.get("integrity").is_none());
        }
    }
    for status in [206, 403, 503] {
        api.lock().unwrap().status = status;
        for path in ["/api/insights", "/api/system", "/api/operator/audit"] {
            assert_eq!(
                request(state.clone(), path, false).await.0,
                StatusCode::BAD_GATEWAY
            );
        }
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn receipt_summary_absence_is_zero_and_configured_namespace_is_not_replaced() {
    let (cluster, _, api, server) = fixture("custom-core").await;
    let absent = cluster.receipt_log_in("custom-core").await.unwrap();
    assert!(!absent.present);
    api.lock().unwrap().snapshot = wire_snapshot(log_maps("custom-core", &entries(3), Some(1)));
    assert_eq!(
        cluster
            .receipt_log_in("custom-core")
            .await
            .unwrap()
            .entries
            .len(),
        3
    );
    assert!(
        api.lock()
            .unwrap()
            .calls
            .iter()
            .all(|(_, path)| path.starts_with("/api/v1/namespaces/custom-core/"))
    );
    server.abort();
    let _ = server.await;
    let namespace = std::env::var("BRIDGE_CORE_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let (_, state, _, server) = fixture(&namespace).await;
    for path in ["/api/insights", "/api/system", "/api/operator/audit"] {
        let (status, body) = request(state.clone(), path, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            if path == "/api/system" {
                &body["counts"]["inclusion_log_size"]
            } else {
                &body["inclusion_log_size"]
            },
            &json!(0)
        );
        if path == "/api/operator/audit" {
            assert_eq!(body["integrity"]["chain_consistent"], false);
            assert_eq!(body["integrity"]["checkpoint_verified"], false);
        }
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn receipt_summary_owner_count_is_filtered_to_actual_owned_inclusions() {
    let namespace = std::env::var("BRIDGE_CORE_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let (_, state, api, server) = fixture(&namespace).await;
    {
        let mut api = api.lock().unwrap();
        api.snapshot = wire_snapshot(log_maps(&namespace, &entries(4), Some(1)));
        api.tasks = vec![
            json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
            "metadata":{"name":"task-3","namespace":"work","annotations":{"kars.azure.com/owner-sub":"owner"}},
            "spec":{},"status":{}}),
        ];
        api.receipts = vec![
            json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsReceipt",
            "metadata":{"name":"task-3","namespace":"work"},"spec":{"taskRef":{"name":"task-3"}}}),
        ];
    }
    let (status, body) = request(state, "/api/insights", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["inclusion_log_size"], 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn receipt_detail_handler_reads_legacy_overflow_and_absence_without_hiding_log_errors() {
    let namespace = std::env::var("BRIDGE_CORE_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let (_, state, api, server) = fixture(&namespace).await;
    let key = SigningKey::from_bytes(&[42; 32]);
    let payload_type = "application/vnd.in-toto+json";
    let predicate_type = "https://kars.azure.com/attestations/GovernanceReceipt/v0";
    let digest = "0123456789abcdef0123456789abcdef";
    let payload = serde_json::to_vec(&json!({
        "_type":"https://in-toto.io/Statement/v1","predicateType":predicate_type,
        "subject":[{"name":"work/task-3","digest":{"sha256":digest}}],
        "predicate":{"claims":[{"class":"integrity","status":"PASS","detail":"signed detail"}]}
    }))
    .unwrap();
    let mut pae = format!(
        "DSSEv1 {} {payload_type} {} ",
        payload_type.len(),
        payload.len()
    )
    .into_bytes();
    pae.extend_from_slice(&payload);
    let receipt = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsReceipt",
        "metadata":{"name":"task-3","namespace":"work","uid":"receipt","resourceVersion":"1"},
        "spec":{"taskRef":{"name":"task-3"},"envelopeDigest":format!("sha256:{digest}"),
            "predicateType":predicate_type,"scheme":"DSSEv1+ed25519","keyId":"test",
            "dsse":{"payloadType":payload_type,"payload":STANDARD.encode(&payload),
                    "signatures":[{"keyid":"test","sig":STANDARD.encode(key.sign(&pae))}]},
            "claims":[]},"status":{"inclusionSeq":3}});
    let mut chain = entries(4);
    chain[3]["payloadSha256"] = sha256_hex(&payload).into();
    chain[3]["entryHash"] = chain_entry_hash(
        3,
        "work/task-3",
        chain[3]["payloadSha256"].as_str().unwrap(),
        chain[3]["prevHash"].as_str().unwrap(),
    )
    .into();
    let receipt_path = "/apis/kars.azure.com/v1alpha1/namespaces/work/karsreceipts/task-3";
    api.lock()
        .unwrap()
        .receipt_details
        .insert(receipt_path.into(), receipt);
    let path = "/api/namespaces/work/tasks/task-3/receipt";
    for split in [None, Some(1)] {
        let mut maps = log_maps(&namespace, &chain, split);
        maps.push(map(&namespace, "kars-receipt-checkpoint", json!({
            "treeSize":"4","rootHash":chain[3]["entryHash"],"keyId":"test","publishedAt":"2026-09-11T00:00:00Z"})));
        api.lock().unwrap().snapshot = wire_snapshot(maps);
        let (status, body) = request(state.clone(), path, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "task-3");
        assert_eq!(body["namespace"], "work");
        assert_eq!(body["task"], "task-3");
        assert_eq!(body["inclusion_seq"], 3);
        assert_eq!(body["claims"][0]["detail"], "signed detail");
        assert_eq!(body["checkpoint"]["tree_size"], 4);
        assert_eq!(body["checkpoint"]["root_hash"], chain[3]["entryHash"]);
    }
    api.lock().unwrap().snapshot = wire_snapshot(vec![]);
    let (status, body) = request(state.clone(), path, false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["checkpoint"].is_null());
    for snapshot in invalid_snapshots(&namespace) {
        api.lock().unwrap().snapshot = snapshot;
        let (status, body) = request(state.clone(), path, false).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["code"], "upstream_error");
        assert!(body.get("checkpoint").is_none());
        assert!(body.get("statement").is_none());
    }
    for code in [403, 503] {
        api.lock().unwrap().status = code;
        let (status, body) = request(state.clone(), path, false).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["code"], "upstream_error");
    }
    api.lock().unwrap().receipt_details.clear();
    assert_eq!(request(state, path, false).await.0, StatusCode::NOT_FOUND);
    server.abort();
    let _ = server.await;
}

#[test]
fn receipt_log_integrity_still_verifies_real_signed_checkpoints_and_exact_tree_size() {
    let chain = entries(4);
    let key = SigningKey::from_bytes(&[42; 32]);
    for tree_size in [4, 3] {
        let root = chain.last().unwrap()["entryHash"].as_str().unwrap();
        let signature = key.sign(format!("kars-receipt-log\n{tree_size}\n{root}\n").as_bytes());
        let mut maps = log_maps("work", &chain, Some(1));
        maps.push(map("work", "kars-receipt-pubkey", json!({
            "keyId":"test","publicKey":STANDARD.encode(key.public_key()),"scheme":"DSSEv1+ed25519"})));
        maps.push(map(
            "work",
            "kars-receipt-checkpoint",
            json!({
            "treeSize":tree_size.to_string(),"rootHash":root,"keyId":"test",
            "signature":STANDARD.encode(signature)}),
        ));
        let log = parsed(wire_snapshot(maps), "work").unwrap();
        let integrity = crate::routes::receipts::verify_log_integrity(&log);
        assert!(integrity.chain_consistent);
        assert_eq!(integrity.tree_size, 4);
        assert_eq!(integrity.checkpoint_verified, tree_size == 4);
    }
}

#[tokio::test]
async fn receipt_endpoint_still_requires_signed_payload_binding_and_full_overflow_inclusion() {
    use crate::routes::receipts::{AnchorPins, verify_log_integrity_with_pins};
    let namespace = std::env::var("BRIDGE_CORE_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let (_, state, api, server) = fixture(&namespace).await;
    let key = SigningKey::from_bytes(&[42; 32]);
    let public_key = STANDARD.encode(key.public_key());
    let key_id = sha256_hex(key.public_key());
    let payload_type = "application/vnd.in-toto+json";
    let predicate_type = "https://kars.azure.com/attestations/GovernanceReceipt/v0";
    let digest = "0123456789abcdef0123456789abcdef";
    let payload = serde_json::to_vec(&json!({
        "_type":"https://in-toto.io/Statement/v1","predicateType":predicate_type,
        "subject":[{"name":"work/task-3","digest":{"sha256":digest}}],
        "predicate":{"claims":[{"class":"integrity","status":"PASS","detail":"signed"}]}
    }))
    .unwrap();
    let mut pae = format!(
        "DSSEv1 {} {payload_type} {} ",
        payload_type.len(),
        payload.len()
    )
    .into_bytes();
    pae.extend_from_slice(&payload);
    let receipt = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsReceipt",
            "metadata":{"name":"task-3","namespace":"work","uid":"receipt","resourceVersion":"1"},
            "spec":{"taskRef":{"name":"task-3"},"envelopeDigest":format!("sha256:{digest}"),
                "predicateType":predicate_type,"scheme":"DSSEv1+ed25519","keyId":key_id,
                "dsse":{"payloadType":payload_type,"payload":STANDARD.encode(&payload),
                        "signatures":[{"keyid":key_id,"sig":STANDARD.encode(key.sign(&pae))}]},
                "claims":[]},"status":{"inclusionSeq":3}});
    let mut chain = entries(4);
    chain[3]["payloadSha256"] = sha256_hex(&payload).into();
    chain[3]["entryHash"] = chain_entry_hash(
        3,
        "work/task-3",
        chain[3]["payloadSha256"].as_str().unwrap(),
        chain[3]["prevHash"].as_str().unwrap(),
    )
    .into();
    let root = chain[3]["entryHash"].as_str().unwrap();
    let mut maps = log_maps(&namespace, &chain, Some(1));
    maps.push(map(
        &namespace,
        "kars-receipt-pubkey",
        json!({"keyId":key_id,"publicKey":public_key,"scheme":"DSSEv1+ed25519"}),
    ));
    maps.push(map(&namespace, "kars-receipt-checkpoint", json!({"treeSize":"4","rootHash":root,
            "keyId":key_id,"signature":STANDARD.encode(key.sign(format!("kars-receipt-log\n4\n{root}\n").as_bytes()))})));
    maps.push(map(
        &namespace,
        "kars-receipt-witness",
        json!({"witnessKeyId":"advisory-only",
            "witnessSignature":"present-not-independently-verified"}),
    ));
    let receipt_path = "/apis/kars.azure.com/v1alpha1/namespaces/work/karsreceipts/task-3";
    {
        let mut api = api.lock().unwrap();
        api.snapshot = wire_snapshot(maps);
        api.receipt_details.insert(receipt_path.into(), receipt);
    }
    let path = "/api/namespaces/work/tasks/task-3/receipt/verify";
    let (status, body) = request(state.clone(), path, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["verified"], true);
    assert_eq!(body["evidence"]["inclusion"]["tree_size"], 4);
    assert_eq!(body["evidence"]["inclusion"]["seq"], 3);
    api.lock().unwrap().snapshot["items"]
        .as_array_mut()
        .unwrap()
        .retain(|item| item["metadata"]["name"] != "kars-receipt-witness");
    let (_, without_witness) = request(state.clone(), path, false).await;
    assert_eq!(without_witness["verified"], true);
    let checks = without_witness["checks"].as_array().unwrap();
    assert_eq!(
        checks
            .iter()
            .filter(|c| c["name"] == "Signed checkpoint")
            .count(),
        1
    );
    let witness = checks
        .iter()
        .find(|c| c["name"] == "Independent witness")
        .unwrap();
    assert_eq!(witness["advisory"], true);
    assert_eq!(witness["passed"], false);

    for forged in [false, true] {
        if forged {
            let replacement = SigningKey::from_bytes(&[17; 32]);
            let mut api = api.lock().unwrap();
            for (name, field, value) in [
                (
                    "kars-receipt-pubkey",
                    "publicKey",
                    STANDARD.encode(replacement.public_key()),
                ),
                (
                    "kars-receipt-checkpoint",
                    "signature",
                    STANDARD.encode(
                        replacement.sign(format!("kars-receipt-log\n4\n{root}\n").as_bytes()),
                    ),
                ),
            ] {
                let map = api.snapshot["items"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|item| item["metadata"]["name"] == name)
                    .unwrap();
                map["data"][field] = value.into();
            }
            api.receipt_details.get_mut(receipt_path).unwrap()["spec"]["dsse"]["signatures"][0]["sig"] =
                STANDARD.encode(replacement.sign(&pae)).into();
        }
        for (pin_id, pin_key, matches_original) in [
            (None, None, true),
            (Some(key_id.clone()), None, true),
            (None, Some(public_key.clone()), true),
            (Some(key_id.clone()), Some(public_key.clone()), true),
            (Some("wrong".into()), None, false),
            (None, Some(STANDARD.encode([0_u8; 32])), false),
            (
                Some(key_id.clone()),
                Some(STANDARD.encode([0_u8; 32])),
                false,
            ),
            (Some(String::new()), None, false),
            (None, Some(String::new()), false),
        ] {
            let configured = pin_id.is_some() || pin_key.is_some();
            let expected = matches_original && (!forged || !configured);
            let pins = AnchorPins {
                key_id: pin_id,
                public_key: pin_key,
            };
            let snapshot = api.lock().unwrap().snapshot.clone();
            let log = parsed(snapshot, &namespace).unwrap();
            let integrity = verify_log_integrity_with_pins(&log, Ok(pins.clone()));
            assert!(integrity.chain_consistent);
            assert_eq!(
                integrity.checkpoint_verified, expected,
                "{pins:?}, forged={forged}"
            );
            assert_eq!(integrity.anchor_pinned, expected && configured);
            let (status, result) =
                request_with_pins(state.clone(), path, false, Some(pins.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(result["verified"], expected, "{pins:?}, forged={forged}");
            if !expected {
                assert!(
                    result["checks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|check| check["name"] == "Trust anchor" && check["passed"] == false)
                );
            }
        }
    }
    api.lock()
        .unwrap()
        .receipt_details
        .get_mut(receipt_path)
        .unwrap()["spec"]["dsse"]["signatures"][0]["sig"] = STANDARD.encode([0; 64]).into();
    assert_eq!(
        request(state.clone(), path, false).await.1["verified"],
        false
    );
    api.lock().unwrap().status = 403;
    assert_eq!(request(state, path, false).await.1["verified"], false);
    server.abort();
    let _ = server.await;
}
