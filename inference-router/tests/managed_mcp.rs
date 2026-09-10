// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::Request,
};
use kars_inference_router::{
    access_request::Identity,
    governance::Governance,
    governed_services::GovernedServices,
    mcp::tools::{
        AsyncToolDispatcher, DispatchError, ToolCallOutput, ToolCatalog, ToolContent,
        ToolDefinition,
    },
    routes::{self, McpRouteState},
};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

struct Dispatcher {
    catalog: ToolCatalog,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl AsyncToolDispatcher for Dispatcher {
    fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }
    async fn invoke(
        &self,
        _name: &str,
        _arguments: &Value,
    ) -> Result<ToolCallOutput, DispatchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolCallOutput {
            content: vec![ToolContent::text("ok")],
            is_error: false,
            ..Default::default()
        })
    }
}
fn dispatcher() -> (Arc<dyn AsyncToolDispatcher>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let catalog = ToolCatalog::new(
        ["one.echo", "two.echo"]
            .into_iter()
            .map(|name| ToolDefinition {
                name: name.into(),
                description: "echo".into(),
                input_schema: json!({"type":"object"}),
            })
            .collect(),
    )
    .unwrap();
    (
        Arc::new(Dispatcher {
            catalog,
            calls: calls.clone(),
        }),
        calls,
    )
}
async fn request(state: McpRouteState, server: &str, method: &str, params: Value) -> (u16, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("x-kars-mcp-server", server)
        .body(Body::from(
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));
    let response = routes::mcp_route()
        .with_state(state)
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn scoped_catalog_and_invocation_never_cross_server_boundaries() {
    let (dispatcher, calls) = dispatcher();
    let state = McpRouteState::standard().with_tools(dispatcher);
    let (status, body) = request(state.clone(), "one", "tools/list", json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["result"]["tools"][0]["name"], "one.echo");
    let (_, body) = request(
        state.clone(),
        "one",
        "tools/call",
        json!({"name":"two.echo","arguments":{}}),
    )
    .await;
    assert!(body.get("error").is_some() || body["result"]["isError"] == true);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (_, body) = request(
        state,
        "one",
        "tools/call",
        json!({"name":"one.echo","arguments":{}}),
    )
    .await;
    assert_eq!(body["result"]["content"][0]["text"], "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unknown_server_scope_never_dispatches() {
    let (dispatcher, calls) = dispatcher();
    let (status, _) = request(
        McpRouteState::standard().with_tools(dispatcher),
        "unmounted",
        "tools/call",
        json!({"name":"one.echo"}),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn direct_mcp_http_calls_obey_live_agt_denials_and_record_no_argument_bodies() {
    let (inner, calls) = dispatcher();
    let governance = Arc::new(Governance::new("test"));
    let policy = tempfile::tempdir_in(".").unwrap();
    let path = policy.path().join("deny.yaml");
    std::fs::write(&path, "version: '1.0'\nagent: test\npolicies:\n  - name: deny-mcp\n    type: capability\n    denied_actions: ['tool:*']\n    priority: 100\n").unwrap();
    governance
        .policy
        .load_from_file(path.to_str().unwrap())
        .unwrap();
    let services = Arc::new(GovernedServices::new(Identity::standalone("test"), None));
    let (_, body) = request(
        McpRouteState::standard().with_tools(inner).with_governance(
            governance,
            services,
            "test".into(),
        ),
        "one",
        "tools/call",
        json!({"name":"one.echo","arguments":{"secret":"PRIVATE_SENTINEL"}}),
    )
    .await;
    assert!(body.get("error").is_some() || body["result"]["isError"] == true);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!body.to_string().contains("PRIVATE_SENTINEL"));
}

#[tokio::test]
async fn missing_peer_network_peer_and_forged_local_headers_never_evaluate_or_invoke() {
    for peer in [
        None,
        Some(SocketAddr::from(([10, 2, 3, 4], 32100))),
        Some("[fd00::1]:32100".parse().unwrap()),
    ] {
        let (inner, calls) = dispatcher();
        let governance = Arc::new(Governance::new("victim"));
        let services = Arc::new(GovernedServices::new(Identity::standalone("victim"), None));
        let state = McpRouteState::standard().with_tools(inner).with_governance(
            governance.clone(),
            services,
            "victim".into(),
        );
        let mut request = Request::post("/mcp")
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .header("forwarded", "for=127.0.0.1")
            .header("x-forwarded-for", "127.0.0.1")
            .header("x-real-ip", "127.0.0.1")
            .header("x-kars-sandbox", "victim")
            .header("mcp-session-id", "victim-session")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"one.echo","arguments":{}}})
                .to_string(),
            ))
            .unwrap();
        if let Some(peer) = peer {
            request.extensions_mut().insert(ConnectInfo(peer));
        }
        let response = routes::mcp_route()
            .with_state(state)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 403);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(governance.metrics.evaluations.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn actual_kind_rpc_client_negotiates_the_scoped_router_without_weakening_accept_checks() {
    let (inner, calls) = dispatcher();
    let state = McpRouteState::standard().with_tools(inner);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            routes::mcp_route()
                .with_state(state)
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let rejected = http
        .post(format!("http://{address}/mcp"))
        .header("accept", "application/json")
        .header("x-kars-mcp-server", "one")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status().as_u16(), 406);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Execute the actual native fixture helper, not a second client with
    // independently correct headers that could leave the Kind fixture broken.
    let script = r#"
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("managed_mcp_fixture", sys.argv[1])
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
port = int(sys.argv[2])
status, body = fixture.rpc(port, "tools/list", {}, "one")
assert status == 200, "actual Kind catalog client was rejected"
assert [tool["name"] for tool in body["result"]["tools"]] == ["one.echo"]
status, body = fixture.rpc(port, "tools/call", {"name":"one.echo","arguments":{}}, "one")
assert status == 200 and body["result"]["content"][0]["text"] == "ok"
status, body = fixture.rpc(port, "tools/call", {"name":"two.echo","arguments":{}}, "one")
assert status == 200 and ("error" in body or body["result"].get("isError"))
status, _ = fixture.rpc(port, "tools/list", {}, "unmounted")
assert status == 404, "unknown scope must remain rejected"
"#;
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/e2e/managed-mcp.py");
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::process::Command::new("python3")
            .args(["-B", "-c", script])
            .arg(fixture)
            .arg(address.port().to_string())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    server.abort();
    let _ = server.await;
    let output = output
        .expect("actual Kind RPC client exceeded its bounded local test deadline")
        .expect("python3 must be available for the native fixture regression");
    assert!(
        output.status.success(),
        "actual Kind RPC client failed; status {}",
        output.status
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn real_http_loopback_can_initialize_scope_catalog_and_invoke_only_its_server() {
    let (inner, calls) = dispatcher();
    let state = McpRouteState::standard().with_tools(inner);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            routes::mcp_route()
                .with_state(state)
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("http://{address}/mcp");
    let response = http.post(&url).header("accept","application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"loopback","version":"1"}}}))
        .send().await.unwrap();
    assert!(response.status().is_success());
    let session = response
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let catalog: Value = http
        .post(&url)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session)
        .header("x-kars-mcp-server", "one")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(catalog["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["result"]["tools"][0]["name"], "one.echo");
    let forbidden: Value = http.post(&url).header("accept","application/json, text/event-stream")
        .header("mcp-session-id",&session).header("x-kars-mcp-server","one")
        .json(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"two.echo","arguments":{}}}))
        .send().await.unwrap().json().await.unwrap();
    assert!(forbidden.get("error").is_some() || forbidden["result"]["isError"] == true);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result: Value = http.post(&url).header("accept","application/json, text/event-stream")
        .header("mcp-session-id",session).header("x-kars-mcp-server","one")
        .json(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"one.echo","arguments":{}}}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(result["result"]["content"][0]["text"], "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}
