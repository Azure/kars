// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "common/governed_services.rs"]
mod support;
use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use kars_inference_router::{
    auth::WorkloadIdentityAuth,
    config::ProviderEndpoint,
    proxy::{self, AuthenticationProvenance, UpstreamConfig},
    routes::{self, McpRouteState},
    task_telemetry::{TaskTelemetry, observe::wrap_stream},
};
use serde_json::{Value, json};
use std::sync::Arc;
use support::*;
use tower::ServiceExt;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

fn telemetry() -> Arc<TaskTelemetry> {
    Arc::new(TaskTelemetry::new("scope-a".into()))
}
fn upstream(endpoint: String, telemetry: Arc<TaskTelemetry>) -> UpstreamConfig {
    let mut target = UpstreamConfig::azure(endpoint, "model".into(), "test".into());
    target.authentication = AuthenticationProvenance::Named {
        provider_id: "qualified-provider".into(),
    };
    target.telemetry = Some(telemetry);
    target
}
fn events(telemetry: &TaskTelemetry) -> Vec<Value> {
    telemetry.snapshot("scope-a", 0).unwrap()["events"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn correlated_egress_records_actual_policy_denial_without_inventing_fetch_or_harness_success()
{
    let state = state("workspace", "uid");
    let scope = state.services.telemetry.cursor().0;
    state.services.telemetry.begin("chat/completions", "provider", "model", b"{}").unwrap().buffered(
        200, br#"{"choices":[{"message":{"tool_calls":[{"id":"web-1","function":{"name":"web_fetch","arguments":"{}"}}]}}]}"#,
    );
    let app = app(state.clone());
    let (status, result) = send(&app, request("POST", "/agt/evaluate", json!({
        "agent_id":"test", "action":"tool:web_fetch", "context":{"scope_id":scope,"tool_call_id":"web-1"}
    }), None, None, true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["allowed"], true);
    let (status, _) = send(&app, request("POST", "/egress/fetch", json!({
        "url":"https://localhost/secret?token=private", "telemetry":{"scope_id":scope,"call_id":"web-1"}
    }), None, None, true)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let trace = state
        .services
        .telemetry
        .selected_snapshot(&scope, &[1])
        .unwrap();
    let operations: Vec<_> = trace["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "router_operation")
        .collect();
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[0]["flag"], "governance");
    assert_eq!(operations[0]["allowed"], true);
    assert_eq!(operations[1]["flag"], "private_target");
    assert_eq!(operations[1]["allowed"], false);
    assert_eq!(operations[1]["call_id"], "web-1");
    assert_eq!(operations[1]["destination_host"], "localhost");
    assert_eq!(operations[1]["destination_port"], 443);
    assert_eq!(operations[1]["http_status"], Value::Null);
    assert!(
        !trace["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "tool_result")
    );
    assert!(!trace.to_string().contains("secret"));
    assert!(!trace.to_string().contains("token="));
}

#[tokio::test]
async fn correlated_egress_retains_upstream_status_and_truncation_separately_from_policy() {
    for (status, size, complete) in [
        (200, 12, true),
        (403, 12, true),
        (200, 2 * 1024 * 1024 + 1, false),
    ] {
        let upstream = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_string("x".repeat(size)))
            .expect(1)
            .mount(&upstream)
            .await;
        let address = *upstream.address();
        let mut state = state("workspace", "uid");
        state.blocklist = kars_inference_router::blocklist::Blocklist::new(None).await;
        state
            .blocklist
            .replace_allowlist(vec!["approved.example".into()])
            .await;
        state.client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("approved.example", address)
            .build()
            .unwrap();
        let scope = state.services.telemetry.cursor().0;
        state.services.telemetry.begin("chat/completions", "provider", "model", b"{}").unwrap().buffered(
            200, br#"{"choices":[{"message":{"tool_calls":[{"id":"web-1","function":{"name":"web_fetch","arguments":"{}"}}]}}]}"#,
        );
        assert!(
            state
                .services
                .telemetry
                .authorize_harness_tool(&scope, "web-1", "web_fetch")
        );
        let app = app(state.clone());
        let response = app.oneshot(request("POST", "/egress/fetch", json!({
            "url":format!("http://approved.example:{}/private?token=secret-marker", address.port()),
            "telemetry":{"scope_id":scope,"call_id":"web-1"}
        }), None, None, true)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The JSON envelope adds bytes beyond the router's 2 MiB fetched-body cap.
        let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024 + 16384)
            .await
            .unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["status"], status);
        assert_eq!(response["truncated"], !complete);
        let body = response["body"].as_str().unwrap();
        let fetched = if complete {
            body
        } else {
            body.strip_suffix("... [truncated at 2097152 bytes]")
                .unwrap()
        };
        assert_eq!(fetched.len(), size.min(2 * 1024 * 1024));
        let trace = state
            .services
            .telemetry
            .selected_snapshot(&scope, &[1])
            .unwrap();
        let operations: Vec<_> = trace["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "router_operation")
            .collect();
        assert_eq!(operations.len(), 2);
        assert_eq!(operations[0]["operation"], "egress_policy");
        assert_eq!(operations[0]["allowed"], true);
        assert_eq!(operations[1]["operation"], "egress_fetch");
        assert_eq!(operations[1]["http_status"], status);
        assert_eq!(operations[1]["complete"], complete);
        assert_eq!(operations[1]["allowed"], Value::Null);
        assert_eq!(operations[1]["destination_host"], "approved.example");
        assert!(!trace.to_string().contains("secret-marker"));
        assert!(
            !trace["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["kind"] == "tool_result")
        );
    }
}

#[test]
fn argument_previews_cover_all_buffered_api_shapes_without_retaining_credentials() {
    let arguments = json!({"path":"/sandbox/handoff.md", "content":"Useful recommendations", "headers":{"Authorization":"private-value"}, "url":"https://docs.example/page?token=private-value"});
    for (path, response) in [
        (
            "chat/completions",
            json!({"choices":[{"message":{"tool_calls":[{"id":"call-1","function":{"name":"write_file","arguments":arguments.to_string()}}]}}]}),
        ),
        (
            "responses",
            json!({"status":"completed","output":[{"type":"function_call","call_id":"call-1","name":"write_file","arguments":arguments.to_string()}]}),
        ),
        (
            "messages",
            json!({"content":[{"type":"tool_use","id":"call-1","name":"write_file","input":arguments}]}),
        ),
    ] {
        let telemetry = telemetry();
        telemetry
            .begin(path, "provider", "model", b"{}")
            .unwrap()
            .buffered(200, &serde_json::to_vec(&response).unwrap());
        let events = events(&telemetry);
        let preview = &events[1]["argument_preview"];
        assert_eq!(preview["redacted"], true, "{path}");
        assert_eq!(preview["truncated"], false, "{path}");
        assert!(
            preview["text"]
                .as_str()
                .unwrap()
                .contains("/sandbox/handoff.md")
        );
        assert!(
            preview["text"]
                .as_str()
                .unwrap()
                .contains("Useful recommendations")
        );
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("private-value")
        );
    }
}

#[tokio::test]
async fn argument_previews_reassemble_streamed_arguments_before_redaction() {
    let arguments = json!({"path":"/sandbox/report.md", "content":"Body with useful evidence", "api_key":"private-value"}).to_string();
    let middle = arguments.len() / 2;
    let openai = vec![
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"write_file","arguments":&arguments[..middle]}}]}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":&arguments[middle..]}}]},"finish_reason":"tool_calls"}]}),
    ];
    let anthropic = vec![
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-1","name":"write_file","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":&arguments[..middle]}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":&arguments[middle..]}}),
        json!({"type":"message_stop"}),
    ];
    let responses = vec![
        json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","call_id":"call-1","name":"write_file","arguments":arguments}]}}),
    ];
    for (path, frames) in [
        ("chat/completions", openai),
        ("messages", anthropic),
        ("responses", responses),
    ] {
        let telemetry = telemetry();
        let mut observation = telemetry.begin(path, "provider", "model", b"{}").unwrap();
        observation.headers(200);
        let stream = frames
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>()
            + "data: [DONE]\n\n";
        let chunks = stream
            .as_bytes()
            .chunks(7)
            .map(|b| Ok::<_, std::io::Error>(Bytes::copy_from_slice(b)))
            .collect::<Vec<_>>();
        wrap_stream(
            futures::stream::iter(chunks).boxed(),
            Some(observation),
            true,
        )
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        let records = events(&telemetry);
        assert_eq!(records[1]["argument_preview"]["redacted"], true, "{path}");
        assert!(
            records[1]["argument_preview"]["text"]
                .as_str()
                .unwrap()
                .contains("Body with useful evidence")
        );
        assert!(
            !serde_json::to_string(&records)
                .unwrap()
                .contains("private-value")
        );
    }
}

#[test]
fn argument_preview_budget_is_bounded_without_dropping_execution_records() {
    let telemetry = telemetry();
    for number in 0..100 {
        let arguments = json!({"content":"useful body ".repeat(160), "context":"more useful detail ".repeat(100)});
        telemetry.begin("chat/completions", "provider", "model", b"{}").unwrap().buffered(200, &serde_json::to_vec(&json!({"choices":[{"message":{"tool_calls":[{"id":format!("call-{number}"),"function":{"name":"write_file","arguments":arguments.to_string()}}]}}]})).unwrap());
    }
    let records = events(&telemetry);
    assert_eq!(records.len(), 200);
    assert!(records.iter().any(|v| {
        v["argument_preview"]["text"]
            .as_str()
            .is_some_and(|s| s.contains("budget exhausted"))
    }));
    let bytes: usize = records
        .iter()
        .filter_map(|v| v.get("argument_preview"))
        .map(|v| v.to_string().len())
        .sum();
    assert!(bytes < 80 * 1024);
    assert_eq!(
        telemetry.snapshot("scope-a", 0).unwrap()["dropped_events"],
        0
    );
}

#[test]
fn fetch_correlation_requires_current_authorized_proposal_and_allowed_tool_name() {
    let telemetry = telemetry();
    telemetry.begin("chat/completions", "provider", "model", b"{}").unwrap().buffered(
        200, br#"{"choices":[{"message":{"tool_calls":[{"id":"web-1","function":{"name":"web_fetch","arguments":"{}"}},{"id":"write-1","function":{"name":"file_write","arguments":"{}"}}]}}]}"#,
    );
    let request = json!({"telemetry":{"scope_id":"scope-a","call_id":"web-1"}});
    assert!(telemetry.correlate_fetch(&request).is_none());
    assert!(telemetry.authorize_harness_tool("scope-a", "web-1", "web_fetch"));
    assert!(telemetry.correlate_fetch(&request).is_some());
    for request in [
        json!({}),
        json!({"telemetry":{"scope_id":"other","call_id":"web-1"}}),
        json!({"telemetry":{"scope_id":"scope-a","call_id":"unknown"}}),
    ] {
        assert!(telemetry.correlate_fetch(&request).is_none());
    }
    assert!(telemetry.authorize_harness_tool("scope-a", "write-1", "file_write"));
    assert!(
        telemetry
            .correlate_fetch(&json!({"telemetry":{"scope_id":"scope-a","call_id":"write-1"}}))
            .is_none()
    );
    telemetry.reset("scope-b".into());
    assert!(telemetry.correlate_fetch(&request).is_none());
}

#[test]
fn authorized_completion_survives_conversation_projection_without_raw_content() {
    use kars_inference_router::task_telemetry::ToolSummary;
    for (ok, contradictory) in [(true, false), (false, false), (true, true)] {
        let telemetry = telemetry();
        telemetry.begin("chat/completions", "provider", "model", b"{}").unwrap().buffered(
            200, br#"{"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"file-1","function":{"name":"file_write","arguments":"private-content"}}]}}]}"#,
        );
        let summary = if ok {
            ToolSummary::FileWrite {
                bytes: 12,
                artifact_name: Some("briefing.md".into()),
            }
        } else {
            serde_json::from_value(json!({"kind":"failure","reason":"filesystem_operation_failed"}))
                .unwrap()
        };
        assert!(!telemetry.complete_harness_tool_with_summary(
            "scope-a",
            "file-1",
            ok,
            12,
            Some(summary.clone())
        ));
        // A different model request must not change the proposed call's round.
        telemetry
            .begin("chat/completions", "provider", "model", b"{}")
            .unwrap()
            .buffered(200, b"{}");
        assert!(telemetry.authorize_harness_tool("scope-a", "file-1", "file_write"));
        assert!(!telemetry.complete_harness_tool_with_summary(
            "wrong",
            "file-1",
            ok,
            12,
            Some(summary.clone())
        ));
        assert!(!telemetry.complete_harness_tool_with_summary(
            "scope-a",
            "file-1",
            ok,
            12,
            Some(ToolSummary::FileRead {
                bytes: 12,
                returned_bytes: 12,
                truncated: false
            })
        ));
        assert!(telemetry.complete_harness_tool_with_summary(
            "scope-a",
            "file-1",
            ok,
            12,
            Some(summary)
        ));
        assert!(!telemetry.complete_harness_tool("scope-a", "file-1", ok, 12));
        let mut result = json!({"role":"tool","tool_call_id":"file-1","content":"private-content"});
        if contradictory {
            result["is_error"] = json!(true);
        }
        telemetry
            .begin(
                "chat/completions",
                "provider",
                "model",
                &serde_json::to_vec(&json!({"messages":[result]})).unwrap(),
            )
            .unwrap()
            .buffered(200, b"{}");
        let snapshot = telemetry.selected_snapshot("scope-a", &[1, 3]).unwrap();
        let results: Vec<_> = snapshot["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "tool_result")
            .collect();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]["ok"],
            if contradictory {
                Value::Null
            } else {
                json!(ok)
            }
        );
        assert_eq!(results[0]["ms"], 12);
        assert_eq!(results[0]["reported_in_round"], 3);
        assert_eq!(results[0].get("result_summary").is_some(), !contradictory);
        for event in snapshot["events"].as_array().unwrap() {
            assert!(
                chrono::DateTime::parse_from_rfc3339(event["observed_at"].as_str().unwrap())
                    .is_ok()
            );
        }
        assert!(!snapshot.to_string().contains("private-content"));
        telemetry.reset("scope-b".into());
        assert!(!telemetry.complete_harness_tool("scope-a", "file-1", ok, 12));
    }
}

#[test]
fn native_file_summaries_require_matching_authorized_calls() {
    use kars_inference_router::task_telemetry::ToolSummary;
    for (name, summary) in [
        (
            "read_file",
            ToolSummary::FileRead {
                bytes: 12,
                returned_bytes: 12,
                truncated: false,
            },
        ),
        (
            "write_file",
            ToolSummary::FileWrite {
                bytes: 12,
                artifact_name: Some("briefing.md".into()),
            },
        ),
        (
            "kars_mission_file_read",
            ToolSummary::FileRead {
                bytes: 12,
                returned_bytes: 12,
                truncated: false,
            },
        ),
        (
            "kars_mission_file_write",
            ToolSummary::FileWrite {
                bytes: 12,
                artifact_name: Some("briefing.md".into()),
            },
        ),
    ] {
        let telemetry = telemetry();
        let response = json!({"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"native-1","function":{"name":name,"arguments":"private-arguments"}}]}}]});
        telemetry
            .begin("chat/completions", "provider", "model", b"{}")
            .unwrap()
            .buffered(200, &serde_json::to_vec(&response).unwrap());
        assert!(!telemetry.complete_harness_tool_with_summary(
            "scope-a",
            "native-1",
            true,
            1,
            Some(summary.clone())
        ));
        assert!(telemetry.authorize_harness_tool("scope-a", "search-1", "search_files"));
        assert!(!telemetry.complete_harness_tool_with_summary(
            "scope-a",
            "search-1",
            true,
            1,
            Some(summary.clone())
        ));
        assert!(telemetry.authorize_harness_tool("scope-a", "native-1", name));
        assert!(!telemetry.complete_harness_tool_with_summary(
            "scope-a",
            "native-1",
            false,
            1,
            Some(summary.clone())
        ));
        assert!(!telemetry.complete_harness_tool_with_summary(
            "wrong-scope",
            "native-1",
            true,
            1,
            Some(summary.clone())
        ));
        assert!(telemetry.complete_harness_tool_with_summary(
            "scope-a",
            "native-1",
            true,
            1,
            Some(summary.clone())
        ));
        let snapshot = telemetry.snapshot("scope-a", 0).unwrap();
        assert_eq!(snapshot["has_more"], false);
        assert_eq!(snapshot["cursor"], snapshot["high_water"]);
        assert_eq!(snapshot["coverage"], "router-observed-only");
        let result = snapshot["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["kind"] == "tool_result")
            .unwrap();
        assert_eq!(
            result["result_summary"],
            serde_json::to_value(summary).unwrap()
        );
        assert!(!snapshot.to_string().contains("private-arguments"));
    }
}

#[test]
fn selected_rounds_keep_immediate_results_after_large_requests_and_final_tools() {
    let telemetry = telemetry();
    telemetry.begin("chat/completions", "provider", "model", b"{}").unwrap().buffered(200,
        br#"{"choices":[{"message":{"tool_calls":[{"id":"read-1","function":{"name":"read_file","arguments":"{}"}}]}}]}"#);
    assert!(telemetry.authorize_harness_tool("scope-a", "read-1", "read_file"));
    assert!(telemetry.complete_harness_tool("scope-a", "read-1", true, 7));
    assert!(telemetry.authorize_harness_tool("scope-a", "unmatched", "read_file"));
    assert!(telemetry.complete_harness_tool("scope-a", "unmatched", true, 7));
    let result = |rounds: &[u64]| {
        telemetry.selected_snapshot("scope-a", rounds).unwrap()["events"].as_array().unwrap()
            .iter().filter(|event| event["kind"] == "tool_result").cloned().collect::<Vec<_>>()
    };
    let immediate = result(&[1]);
    assert_eq!(immediate.len(), 1);
    assert_eq!(immediate[0]["call_id"], "read-1");
    assert_eq!(immediate[0]["ok"], true);
    assert!(immediate[0].get("reported_in_round").is_none());
    let large = serde_json::to_vec(&json!({"messages":[{"role":"tool", "tool_call_id":"read-1",
        "content":"x".repeat(65_536)}]})).unwrap();
    telemetry.begin("chat/completions", "provider", "model", &large).unwrap().buffered(200, b"{}");
    assert_eq!(result(&[1, 2]), immediate);
    assert!(result(&[2]).is_empty());
    telemetry.begin("chat/completions", "provider", "model",
        br#"{"messages":[{"role":"tool","tool_call_id":"read-1","is_error":true,"content":"contradiction"}]}"#)
        .unwrap().buffered(200, b"{}");
    let echoed = result(&[1, 2, 3]);
    assert_eq!(echoed.len(), 1);
    assert_eq!(echoed[0]["reported_in_round"], 3);
    assert!(echoed[0]["ok"].is_null());
    assert_eq!(result(&[1, 2]), immediate);
}

#[test]
fn selected_rounds_match_the_runtime_and_bridge_contract() {
    let telemetry = telemetry();
    let mut first = telemetry
        .begin("chat/completions", "qualified-provider", "model", b"{}")
        .unwrap();
    first.buffered(200, br#"{"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"call-1","function":{"name":"fetch","arguments":"private-arguments"}}]}}],"usage":{"prompt_tokens":0}}"#);
    let mut second = telemetry.begin("chat/completions", "qualified-provider", "model",
        br#"{"messages":[{"role":"tool","tool_call_id":"call-1","content":"private-result","is_error":true}]}"#).unwrap();
    second.buffered(
        200,
        br#"{"choices":[{"finish_reason":"stop","message":{"content":"private-output"}}]}"#,
    );
    let mut selected = telemetry.selected_snapshot("scope-a", &[1, 2]).unwrap();
    for event in selected["events"].as_array_mut().unwrap() {
        let stamp = event["observed_at"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(stamp).is_ok());
        event.as_object_mut().unwrap().remove("observed_at");
        if event["kind"] == "round" {
            event["ms"] = json!(0);
        }
    }
    let contract: Value =
        serde_json::from_str(include_str!("fixtures/router-observations-v1.json")).unwrap();
    let mut router_trace = contract["trace"].clone();
    // The shared fixture also exercises historical records without previews.
    router_trace["events"][1]["argument_preview"] = json!({
        "text":"[Argument preview unavailable: capture limit or incomplete JSON]",
        "truncated":true,"redacted":false
    });
    for event in router_trace["events"].as_array_mut().unwrap() {
        if event["kind"] == "round" {
            event["usage"]["cached_tokens"] = Value::Null;
        }
    }
    assert_eq!(selected, router_trace);
    for rounds in [&[1][..], &[2][..]] {
        let selected = telemetry.selected_snapshot("scope-a", rounds).unwrap();
        assert!(
            selected["events"]
                .as_array()
                .unwrap()
                .iter()
                .all(|event| event["kind"] != "tool_result"
                    && rounds.contains(&event["round"].as_u64().unwrap()))
        );
    }
    assert!(telemetry.selected_snapshot("wrong-scope", &[1]).is_none());
    for rounds in [vec![], vec![0], vec![2, 1], vec![1, 1], (1..=131).collect()] {
        assert!(telemetry.selected_snapshot("scope-a", &rounds).is_none());
    }
    assert_eq!(
        telemetry.selected_snapshot("scope-a", &[1, 2, 3]).unwrap()["missing_rounds"],
        json!([3])
    );
    telemetry.reset("scope-b".into());
    assert!(telemetry.selected_snapshot("scope-a", &[1, 2]).is_none());
    assert_eq!(
        telemetry.selected_snapshot("scope-b", &[1, 2]).unwrap()["missing_rounds"],
        json!([1, 2])
    );
}

#[test]
fn selected_rounds_report_truncation_eviction_and_reused_call_ids() {
    let telemetry = telemetry();
    let response = json!({"choices":[{"message":{"tool_calls":(0..16).map(|n|
        json!({"id":format!("call-{n}"),"function":{"name":"fetch","arguments":"private"}})).collect::<Vec<_>>()}}]});
    let body = serde_json::to_vec(&response).unwrap();
    for _ in 0..20 {
        telemetry
            .begin("chat/completions", "provider", "model", b"{}")
            .unwrap()
            .buffered(200, &body);
    }
    let selection = telemetry
        .selected_snapshot("scope-a", &(1..=20).collect::<Vec<_>>())
        .unwrap();
    assert_eq!(selection["events"].as_array().unwrap().len(), 340);
    assert_eq!(selection["truncated"], false);
    assert_eq!(selection["missing_rounds"], json!([]));
    assert!(selection["dropped_events"].as_u64().unwrap() > 0);
    let mut report = telemetry
        .begin(
            "chat/completions",
            "provider",
            "model",
            br#"{"messages":[{"role":"tool","tool_call_id":"call-0","content":"private"}]}"#,
        )
        .unwrap();
    report.buffered(200, b"{}");
    let selected = telemetry
        .selected_snapshot("scope-a", &[1, 20, 21])
        .unwrap();
    let results: Vec<_> = selected["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "tool_result")
        .collect();
    // Reused call IDs are ambiguous, so the recorder must not attribute a result.
    assert!(results.is_empty());
    for _ in 0..1100 {
        telemetry.record_policy("scope-a", "fetch", false);
    }
    let evicted = telemetry
        .selected_snapshot("scope-a", &[1, 20, 21])
        .unwrap();
    assert_eq!(evicted["events"], json!([]));
    assert_eq!(evicted["missing_rounds"], json!([1, 20, 21]));
    assert_eq!(evicted["truncated"], false);
    assert!(evicted["dropped_events"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn selected_rounds_http_pages_ordinary_128_tool_phase_without_losing_correlation() {
    use kars_inference_router::task_telemetry::operation::RouterOperation;
    let state = state("workspace", "uid");
    let telemetry = state.services.telemetry.clone();
    let scope = telemetry.cursor().0;
    for round in 1..=129 {
        let request_body = if round == 1 {
            json!({})
        } else {
            json!({"messages":[{"role":"tool","tool_call_id":format!("call-{}", round - 1),"content":"result"}]})
        };
        let response = if round == 129 {
            json!({})
        } else {
            json!({"choices":[{"message":{"tool_calls":[{"id":format!("call-{round}"),
                "function":{"name":"web_fetch","arguments":"{}"}}]}}]})
        };
        telemetry
            .begin(
                "chat/completions",
                "foundry",
                "reviewed",
                &serde_json::to_vec(&request_body).unwrap(),
            )
            .unwrap()
            .buffered(200, &serde_json::to_vec(&response).unwrap());
        if round == 129 {
            break;
        }
        let id = format!("call-{round}");
        assert!(telemetry.authorize_harness_tool(&scope, &id, "web_fetch"));
        let tool = telemetry
            .correlate_operation(&scope, &id, Some("web_fetch"))
            .unwrap();
        for operation in [
            RouterOperation::ToolPolicy {
                allowed: true,
                rate_limited: false,
            },
            RouterOperation::EgressPolicy {
                allowed: true,
                flag: "egress_check",
            },
            RouterOperation::Fetch {
                status: Some(200),
                complete: true,
                flag: "response",
            },
        ] {
            telemetry.record_operation(
                Some(&tool),
                operation,
                Some("https://source.example/public"),
                1,
            );
        }
        assert!(telemetry.complete_harness_tool(&scope, &id, true, 1));
    }
    let rounds: Vec<u64> = (1..=129).collect();
    let expected = telemetry.selected_snapshot(&scope, &rounds).unwrap();
    assert_eq!(expected["events"].as_array().unwrap().len(), 769);
    assert_eq!(expected["dropped_events"], 0);
    assert!(
        telemetry
            .selected_snapshot(&scope, &(1..=130).collect::<Vec<_>>())
            .is_some()
    );
    let app = app(state);
    let selection = rounds
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut cursor = 0;
    let mut collected = Vec::<Value>::new();
    for page in 0..4 {
        let (status, response) = send(
            &app,
            request(
                "GET",
                &format!("/telemetry/trace?rounds={selection}&after={cursor}"),
                json!({}),
                None,
                Some(&scope),
                true,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["has_more"], page < 3);
        assert_eq!(response["missing_rounds"], json!([]));
        let events = response["events"].as_array().unwrap();
        assert!(events.len() <= 256);
        assert!(
            events
                .iter()
                .all(|event| event["seq"].as_u64().unwrap() > cursor)
        );
        cursor = response["cursor"].as_u64().unwrap();
        collected.extend(events.iter().cloned());
    }
    assert_eq!(json!(collected), expected["events"]);
    telemetry.reset("new-scope".into());
    assert_eq!(
        send(
            &app,
            request(
                "GET",
                &format!("/telemetry/trace?rounds={selection}&after={cursor}"),
                json!({}),
                None,
                Some(&scope),
                true
            )
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn selected_rounds_http_validates_queries_and_preserves_cursor_mode() {
    let state = state("workspace", "uid");
    let scope = state.services.telemetry.cursor().0;
    state
        .services
        .telemetry
        .begin("responses", "provider", "model", b"{}")
        .unwrap()
        .buffered(200, b"{}");
    let app = app(state);
    let mut invalid = vec![
        "".to_owned(),
        "0".into(),
        "1,1".into(),
        "2,1".into(),
        "-1".into(),
        "1,".into(),
        "1,,2".into(),
        "a".into(),
        "18446744073709551616".into(),
        "1&since=1".into(),
    ];
    invalid.push(
        (1..=131)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(","),
    );
    for query in invalid {
        let path = format!("/telemetry/trace?rounds={query}");
        let response = app
            .clone()
            .oneshot(request("GET", &path, json!({}), None, Some(&scope), true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
    for suffix in ["rounds=1,2", "rounds=1,2&since=0"] {
        let (status, selected) = send(
            &app,
            request(
                "GET",
                &format!("/telemetry/trace?{suffix}"),
                json!({}),
                None,
                Some(&scope),
                true,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(selected["rounds"], json!([1, 2]));
        assert_eq!(selected["missing_rounds"], json!([2]));
        assert_eq!(selected["coverage"], "returned-responses-only");
    }
    for bad_scope in [None, Some("wrong-scope")] {
        assert_eq!(
            send(
                &app,
                request(
                    "GET",
                    "/telemetry/trace?rounds=1",
                    json!({}),
                    None,
                    bad_scope,
                    true
                )
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    let (status, cursor) = send(
        &app,
        request(
            "GET",
            "/telemetry/trace?since=0",
            json!({}),
            None,
            Some(&scope),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cursor["coverage"], "router-observed-only");
    assert!(cursor.get("rounds").is_none());
}

#[tokio::test]
async fn providers_cannot_advertise_router_ids_without_an_observer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-kars-telemetry-scope", "provider-spoof")
                .insert_header("x-kars-telemetry-round", "999")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    let mut target = upstream(server.uri(), telemetry());
    target.telemetry = None;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let (_, headers, _) = proxy::forward(
        &WorkloadIdentityAuth::new(),
        None,
        &client,
        &target,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await
    .unwrap();
    assert!(!headers.contains_key("x-kars-telemetry-scope"));
    assert!(!headers.contains_key("x-kars-telemetry-round"));
    let (_, headers, stream) = proxy::forward_stream(
        Arc::new(WorkloadIdentityAuth::new()),
        None,
        client,
        target,
        "chat/completions",
        HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await
    .unwrap();
    assert!(!headers.contains_key("x-kars-telemetry-scope"));
    assert!(!headers.contains_key("x-kars-telemetry-round"));
    let _: Vec<_> = stream.try_collect().await.unwrap();
}

#[tokio::test]
async fn buffered_http_producer_observes_usage_but_never_api_bodies_or_tool_arguments() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "choices":[{"finish_reason":"tool_calls","message":{"content":"output-secret-marker","tool_calls":[
            {"id":"call-1","function":{"name":"fetch","arguments":"argument-secret-marker"}}
        ]}}],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18}
    })).insert_header("x-kars-telemetry-scope", "provider-spoof")
        .insert_header("x-kars-telemetry-round", "999")).mount(&server).await;
    let telemetry = telemetry();
    let target = upstream(server.uri(), telemetry.clone());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut caller_headers = HeaderMap::new();
    caller_headers.insert("x-kars-service-scope", "caller-spoof".parse().unwrap());
    caller_headers.insert("x-kars-telemetry-scope", "caller-spoof".parse().unwrap());
    caller_headers.insert("x-kars-telemetry-round", "888".parse().unwrap());
    let (_, headers, body) = proxy::forward(
        &WorkloadIdentityAuth::new(),
        None,
        &client,
        &target,
        Method::POST,
        "chat/completions",
        &caller_headers,
        Bytes::from(r#"{"messages":[{"role":"user","content":"prompt-secret-marker"}]}"#),
    )
    .await
    .unwrap();
    assert_eq!(headers["x-kars-telemetry-scope"], "scope-a");
    assert_eq!(headers["x-kars-telemetry-round"], "1");
    let received = server.received_requests().await.unwrap();
    assert!(!received[0].headers.contains_key("x-kars-service-scope"));
    assert!(!received[0].headers.contains_key("x-kars-telemetry-scope"));
    assert!(!received[0].headers.contains_key("x-kars-telemetry-round"));
    assert!(String::from_utf8_lossy(&body).contains("output-secret-marker"));
    let trace = events(&telemetry);
    assert_eq!(trace[0]["usage"]["total_tokens"], 18);
    assert_eq!(trace[0]["provider"], "qualified-provider");
    assert_eq!(trace[1]["kind"], "tool_proposed");
    assert!(trace[1]["ok"].is_null());
    proxy::forward(&WorkloadIdentityAuth::new(),None,&client,&target,Method::POST,
        "chat/completions",&HeaderMap::new(),Bytes::from(r#"{"messages":[{"role":"tool","tool_call_id":"call-1","content":"result-secret-marker","is_error":true}]}"#)).await.unwrap();
    let trace = events(&telemetry);
    assert!(trace.iter().any(|event| event["kind"] == "tool_result"
        && event["ok"] == false
        && event["source"] == "harness-reported"));
    let serialized = serde_json::to_string(&trace).unwrap();
    for secret in [
        "prompt-secret-marker",
        "output-secret-marker",
        "argument-secret-marker",
        "result-secret-marker",
    ] {
        assert!(!serialized.contains(secret));
    }
}

#[tokio::test]
async fn buffered_http_200_responses_distinguish_failed_and_incomplete_model_outcomes() {
    for (status, outcome) in [("failed", "upstream_error"), ("incomplete", "incomplete")] {
        let server = MockServer::start().await;
        let body = json!({
            "status":status,"error":{"message":"credential-secret-marker"},
            "incomplete_details":{"reason":"private-details-marker"},
            "usage":if status=="failed"{json!({"input_tokens":7})}else{Value::Null},
            "output":[],
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
            .expect(1)
            .mount(&server)
            .await;
        let telemetry = telemetry();
        let target = upstream(server.uri(), telemetry.clone());
        let (http, _, bytes) = proxy::forward(
            &WorkloadIdentityAuth::new(),
            None,
            &reqwest::Client::builder().no_proxy().build().unwrap(),
            &target,
            Method::POST,
            "responses",
            &HeaderMap::new(),
            Bytes::from("{}"),
        )
        .await
        .unwrap();
        assert_eq!(http, StatusCode::OK);
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), body);
        let trace = events(&telemetry);
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0]["accepted"], true);
        assert_eq!(trace[0]["outcome"], outcome);
        assert_eq!(trace[0]["finish_reason"], status);
        assert_eq!(
            trace[0]["usage_state"],
            if status == "failed" {
                "partial"
            } else {
                "missing"
            }
        );
        assert!(trace[0]["usage"]["completion_tokens"].is_null());
        assert!(trace[0]["usage"]["total_tokens"].is_null());
        assert!(!trace[0].to_string().contains("secret-marker"));
        assert!(!trace[0].to_string().contains("private-details-marker"));
    }
}

#[tokio::test]
async fn fragmented_openai_error_frames_cannot_be_overwritten_by_done() {
    for error in [
        "data: {\"error\":{\"message\":\"credential-secret-marker🚫\"}}\n\n",
        "event: error\ndata: {\"message\":\"credential-secret-marker🚫\"}\n\n",
    ] {
        let body = format!(
            "data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":3}}}}\n\n{error}data: [DONE]\n\n"
        );
        let telemetry = telemetry();
        let mut observation = telemetry
            .begin("chat/completions", "provider", "model", b"{}")
            .unwrap();
        observation.headers(200);
        let chunks = body
            .as_bytes()
            .iter()
            .map(|byte| Ok::<_, reqwest::Error>(Bytes::from(vec![*byte])))
            .collect::<Vec<_>>();
        let returned = wrap_stream(
            futures::stream::iter(chunks).boxed(),
            Some(observation),
            true,
        )
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        assert_eq!(returned.concat(), body.as_bytes());
        let trace = events(&telemetry);
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0]["accepted"], true);
        assert_eq!(trace[0]["outcome"], "upstream_error");
        assert_eq!(trace[0]["usage"]["prompt_tokens"], 3);
        assert!(trace[0]["usage"]["completion_tokens"].is_null());
        assert!(trace[0]["usage"]["total_tokens"].is_null());
        assert!(!trace[0].to_string().contains("credential-secret-marker"));
    }
}

#[tokio::test]
async fn responses_incomplete_stream_is_not_reported_as_a_complete_model_response() {
    let telemetry = telemetry();
    let mut observation = telemetry
        .begin("responses", "provider", "model", b"{}")
        .unwrap();
    observation.headers(200);
    let body = "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2},\"output\":[]}}\n\n";
    let returned = wrap_stream(
        futures::stream::once(async move {
            Ok::<_, reqwest::Error>(Bytes::from_static(body.as_bytes()))
        })
        .boxed(),
        Some(observation),
        true,
    )
    .try_collect::<Vec<_>>()
    .await
    .unwrap();
    assert_eq!(returned.concat(), body.as_bytes());
    let trace = events(&telemetry);
    assert_eq!(trace[0]["accepted"], true);
    assert_eq!(trace[0]["outcome"], "incomplete");
    assert_eq!(trace[0]["usage_state"], "present");
    assert!(trace[0]["usage"]["total_tokens"].is_null());
}

#[tokio::test]
async fn fragmented_streams_preserve_bytes_and_report_each_supported_usage_shape_once() {
    let cases = [
        (
            "chat/completions",
            concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"🚀stream-secret-marker\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":5,\"total_tokens\":9}}\n\n",
                "data: [DONE]\n\n"
            ),
            4,
            5,
            Some(9),
        ),
        (
            "v1/messages",
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":3}}}\n\n",
                "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":8},\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            ),
            12,
            8,
            None,
        ),
        (
            "responses",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5},\"output\":[]}}\n\n",
            2,
            3,
            Some(5),
        ),
    ];
    for (path, body, input, output, total) in cases {
        let telemetry = telemetry();
        let mut observation = telemetry.begin(path, "provider", "model", b"{}").unwrap();
        observation.headers(200);
        let chunks = body
            .as_bytes()
            .iter()
            .map(|byte| Ok::<_, reqwest::Error>(Bytes::from(vec![*byte])))
            .collect::<Vec<_>>();
        let bytes = wrap_stream(
            futures::stream::iter(chunks).boxed(),
            Some(observation),
            true,
        )
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        assert_eq!(bytes.concat(), body.as_bytes());
        let events = events(&telemetry);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["usage"]["prompt_tokens"], input);
        assert_eq!(events[0]["usage"]["completion_tokens"], output);
        assert_eq!(events[0]["usage"]["total_tokens"], json!(total));
        assert_eq!(events[0]["outcome"], "complete");
        assert!(!events[0].to_string().contains("stream-secret-marker"));
    }
}

#[tokio::test]
async fn streaming_http_producer_reports_missing_usage_and_no_replay_after_incomplete_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"delta\":{\"content\":\"private\"}}]}\n\n",
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let telemetry = telemetry();
    let (_, headers, stream) = proxy::forward_stream(
        Arc::new(WorkloadIdentityAuth::new()),
        None,
        reqwest::Client::builder().no_proxy().build().unwrap(),
        upstream(server.uri(), telemetry.clone()),
        "chat/completions",
        HeaderMap::new(),
        Bytes::from(r#"{"stream":true}"#),
    )
    .await
    .unwrap();
    assert_eq!(headers["x-kars-telemetry-scope"], "scope-a");
    assert_eq!(headers["x-kars-telemetry-round"], "1");
    let _: Vec<_> = stream.try_collect().await.unwrap();
    let events = events(&telemetry);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["outcome"], "incomplete");
    assert_eq!(events[0]["accepted"], true);
    assert_eq!(events[0]["usage_state"], "missing");
    assert!(events[0]["usage"]["total_tokens"].is_null());
}

#[tokio::test]
async fn reset_discards_old_inflight_observations_and_ring_overflow_is_explicit() {
    let telemetry = telemetry();
    let mut old = telemetry
        .begin("responses", "provider", "model", b"{}")
        .unwrap();
    telemetry.reset("scope-b".into());
    old.buffered(
        200,
        br#"{"usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14}}"#,
    );
    assert!(telemetry.snapshot("scope-a", 0).is_none());
    assert_eq!(
        telemetry.snapshot("scope-b", 0).unwrap()["events"],
        json!([])
    );
    for _ in 0..1100 {
        telemetry.record_policy("scope-b", "tool:fetch", false);
    }
    let snapshot = telemetry.snapshot("scope-b", 0).unwrap();
    assert_eq!(snapshot["dropped_events"], 76);
    assert_eq!(snapshot["events"].as_array().unwrap().len(), 256);
    assert_eq!(snapshot["has_more"], true);
}

#[tokio::test]
async fn physical_failover_attempts_keep_actual_provider_identity_without_counting_false_successes()
{
    use kars_inference_router::{
        failover::forward_with_failover,
        inference_policy_loader::{InferencePolicySnapshot, ModelPreference, ModelRef},
    };
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(json!({"error":{"message":"secret-error-body"}})),
        )
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}))).expect(1).mount(&second).await;
    let state = state("workspace", "uid");
    let mut config = Arc::try_unwrap(state.config)
        .ok()
        .expect("fixture config is uniquely owned");
    for (id, endpoint) in [("first", first.uri()), ("second", second.uri())] {
        config.providers.insert(
            id.into(),
            ProviderEndpoint {
                tag: id.into(),
                endpoint,
                api_key: None,
            },
        );
    }
    let telemetry = telemetry();
    let base = upstream("http://127.0.0.1:1".into(), telemetry.clone());
    let policy = InferencePolicySnapshot {
        model_preference: Some(ModelPreference {
            primary: ModelRef {
                provider: "first".into(),
                deployment: "same-model".into(),
            },
            fallback: vec![ModelRef {
                provider: "second".into(),
                deployment: "same-model".into(),
            }],
        }),
        ..Default::default()
    };
    let result = forward_with_failover(
        &state.auth,
        None,
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &state.deployment_health,
        &base,
        &config,
        &policy,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await
    .unwrap();
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["x-kars-telemetry-scope"], "scope-a");
    assert_eq!(result.1["x-kars-telemetry-round"], "2");
    let selected = telemetry.selected_snapshot("scope-a", &[2]).unwrap();
    assert_eq!(selected["coverage"], "returned-responses-only");
    assert_eq!(selected["events"].as_array().unwrap().len(), 1);
    assert_eq!(selected["events"][0]["provider"], "second");
    let events = events(&telemetry);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["provider"], "first");
    assert_eq!(events[0]["outcome"], "http_error");
    assert_eq!(events[1]["provider"], "second");
    assert_eq!(events[1]["usage"]["total_tokens"], 3);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("secret-error-body")
    );
}

#[tokio::test]
async fn telemetry_http_requires_scope_and_only_correlated_native_outcomes_are_accepted() {
    let state = state("workspace", "uid");
    let scope = state.services.telemetry.cursor().0;
    let app = app(state.clone());
    assert_eq!(
        send(
            &app,
            request("GET", "/telemetry/trace", json!({}), None, None, true)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let body = json!({"scope_id":scope,"call_id":"call-a","ok":true,"latency_ms":5});
    assert_eq!(
        send(
            &app,
            request("POST", "/telemetry/tool", body.clone(), None, None, true)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status,evaluation)=send(&app,request("POST","/agt/evaluate",
        json!({"agent_id":"test","action":"tool:fetch","context":{
            "scope_id":scope,"tool_call_id":"call-a","tool_name":"spoofed-name","args_preview":"native-secret-marker"
        }}),None,None,true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(evaluation["allowed"], true);
    assert_eq!(
        send(
            &app,
            request("POST", "/telemetry/tool", body.clone(), None, None, true)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            request("POST", "/telemetry/tool", body, None, None, true)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, trace) = send(
        &app,
        request(
            "GET",
            "/telemetry/trace",
            json!({}),
            None,
            Some(&scope),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        trace["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["source"] == "harness-reported" && event["name"] == "fetch")
    );
    assert!(!trace.to_string().contains("native-secret-marker"));
    assert!(!trace.to_string().contains("spoofed-name"));
    assert_eq!(trace["durable"], false);
}

#[tokio::test]
async fn actual_mcp_http_errors_are_not_mistaken_for_success_and_bodies_are_not_retained() {
    let telemetry = telemetry();
    let mut state = McpRouteState::standard();
    state.task_telemetry = Some(telemetry.clone());
    let app = routes::mcp_route().with_state(state);
    for (id, name) in [(1, "echo"), (2, "missing-tool")] {
        let mut request = Request::post("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":name,"arguments":{"text":"mcp-secret-body"}}})
                .to_string(),
            ))
            .unwrap();
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                12345,
            ))));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
    }

    let events = events(&telemetry);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["ok"], true);
    assert_eq!(events[1]["ok"], false);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("mcp-secret-body")
    );
}

#[tokio::test]
async fn mcp_is_error_and_dispatch_transport_failure_are_both_failure_observations() {
    use kars_inference_router::mcp::tools::{
        DispatchError, SyncToAsync, ToolCallOutput, ToolCatalog, ToolDefinition, ToolDispatcher,
    };
    struct Outcomes(ToolCatalog);
    impl ToolDispatcher for Outcomes {
        fn catalog(&self) -> &ToolCatalog {
            &self.0
        }
        fn invoke(&self, name: &str, _args: &Value) -> Result<ToolCallOutput, DispatchError> {
            if name == "tool_error" {
                Ok(ToolCallOutput {
                    content: vec![],
                    is_error: true,
                    ..Default::default()
                })
            } else {
                Err(DispatchError::ExecutionFailed {
                    tool: name.into(),
                    reason: "transport-secret-marker".into(),
                })
            }
        }
    }
    let catalog = ToolCatalog::new(
        ["tool_error", "transport_error"]
            .map(|name| ToolDefinition {
                name: name.into(),
                description: "test".into(),
                input_schema: json!({"type":"object"}),
            })
            .to_vec(),
    )
    .unwrap();
    let telemetry = telemetry();
    let mut state =
        McpRouteState::standard().with_tools(Arc::new(SyncToAsync::new(Outcomes(catalog))));
    state.task_telemetry = Some(telemetry.clone());
    let app = routes::mcp_route().with_state(state);
    for (id, name) in [(1, "tool_error"), (2, "transport_error")] {
        let mut request=Request::post("/mcp").header("content-type","application/json").header("accept","application/json, text/event-stream")
                .body(Body::from(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}}).to_string())).unwrap();
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                12345,
            ))));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
    }
    let events = events(&telemetry);
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| event["ok"] == false && event["http_status"] == 200)
    );
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("transport-secret-marker")
    );
}

#[tokio::test]
async fn transport_failures_and_cancelled_partial_streams_never_become_success() {
    let telemetry = telemetry();
    let result = proxy::forward(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &upstream("http://127.0.0.1:1".into(), telemetry.clone()),
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(events(&telemetry)[0]["outcome"], "transport_error");
    let mut observation = telemetry
        .begin("v1/messages", "provider", "model", b"{}")
        .unwrap();
    observation.headers(200);
    let chunks = futures::stream::once(async {
        Ok::<_, reqwest::Error>(Bytes::from_static(
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
        ))
    })
    .chain(futures::stream::pending())
    .boxed();
    let mut wrapped = wrap_stream(chunks, Some(observation), true);
    wrapped.next().await.unwrap().unwrap();
    drop(wrapped);
    let trace = events(&telemetry);
    assert_eq!(trace[1]["outcome"], "cancelled");
    assert_eq!(trace[1]["usage"]["prompt_tokens"], 12);
    assert!(trace[1]["usage"]["completion_tokens"].is_null());
}
