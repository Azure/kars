// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::mcp::tools::ToolCallOutput;
use serde_json::json;

#[test]
fn protocol_content_real_json_roundtrips_losslessly() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/mcp-tool-content.json"
    ))
    .unwrap();
    for fixture in fixtures {
        let output: ToolCallOutput = serde_json::from_str(&fixture.to_string()).unwrap();
        let encoded = serde_json::to_value(&output).unwrap();
        assert_eq!(encoded, fixture);
        assert_eq!(
            serde_json::from_value::<ToolCallOutput>(encoded).unwrap(),
            output
        );
    }
}

#[test]
fn protocol_content_preserves_existing_text_wire_bytes_and_error_default() {
    let output = ToolCallOutput {
        content: vec![ToolContent::text("hello")],
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&output).unwrap(),
        r#"{"content":[{"type":"text","text":"hello"}],"isError":false}"#
    );
    let decoded: ToolCallOutput =
        serde_json::from_str(r#"{"content":[{"type":"text","text":"hello"}]}"#).unwrap();
    assert_eq!(decoded, output);
}

#[test]
fn protocol_content_rejects_invalid_known_fields_and_unknown_kinds() {
    let invalid = [
        json!({"type":"video","data":"AA==","mimeType":"video/mp4"}),
        json!({"type":"text"}),
        json!({"type":"text","text":42}),
        json!({"text":"missing discriminator"}),
        json!({"type":"image","data":"AA=="}),
        json!({"type":"image","data":"not base64!","mimeType":"image/png"}),
        json!({"type":"audio","data":null,"mimeType":"audio/wav"}),
        json!({"type":"audio","data":"AA==","mimeType":5}),
        json!({"type":"resource","resource":{"text":"missing URI"}}),
        json!({"type":"resource","resource":{"uri":"file:///example"}}),
        json!({"type":"resource","resource":{"uri":"relative/path","text":"invalid URI"}}),
        json!({"type":"resource","resource":{"uri":"file:///example","text":null}}),
        json!({"type":"resource","resource":{"uri":"file:///example","blob":"?"}}),
        json!({"type":"resource","resource":{"uri":"file:///example","text":"valid","blob":12}}),
        json!({"type":"resource","resource":{"uri":"file:///example","text":12,"blob":"AA=="}}),
        json!({"type":"resource","resource":{"uri":"file:///example","text":"valid","blob":"?"}}),
        json!({"type":"resource","resource":{"uri":"file:///example","text":"ok","mimeType":12}}),
        json!({"type":"resource_link","uri":"https://example.invalid"}),
        json!({"type":"resource_link","name":"link","uri":" https://example.invalid"}),
        json!({"type":"resource_link","name":"link","uri":"urn:test","size":0.5}),
        json!({"type":"resource_link","name":"link","uri":"urn:test","icons":[{"src":"urn:icon","theme":"blue"}]}),
        json!({"type":"text","text":"ok","annotations":{"audience":["system"]}}),
        json!({"type":"text","text":"ok","annotations":{"priority":1.1}}),
        json!({"type":"text","text":"ok","annotations":{"priority":-0.1}}),
        json!({"type":"text","text":"ok","annotations":{"priority":"1"}}),
        json!({"type":"text","text":"ok","annotations":null}),
        json!({"type":"text","text":"ok","_meta":[]}),
    ];
    for content in invalid {
        assert!(
            serde_json::from_value::<ToolContent>(content.clone()).is_err(),
            "accepted invalid content: {content}"
        );
    }
    for result in [
        json!({}),
        json!({"content":null}),
        json!({"content":[],"isError":null}),
        json!({"content":[],"isError":"false"}),
        json!({"content":[],"structuredContent":[]}),
        json!({"content":[],"structuredContent":null}),
        json!({"content":[],"_meta":42}),
    ] {
        assert!(
            serde_json::from_value::<ToolCallOutput>(result.clone()).is_err(),
            "accepted invalid result: {result}"
        );
    }
}

#[test]
fn protocol_content_remains_visible_to_existing_structured_response_scanner() {
    use agentmesh_mcp::{
        InMemoryAuditSink, McpMetricsCollector, McpResponseScanner, SystemClock,
        redactor::CredentialRedactor,
    };
    use std::sync::Arc;

    let scanner = McpResponseScanner::new(
        CredentialRedactor::new(),
        Arc::new(InMemoryAuditSink::new(CredentialRedactor::new())),
        McpMetricsCollector::default(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let marker = "<system>inspection regression</system>";
    let fixture = json!({
        "content": [
            {"type":"text","text":marker},
            {"type":"resource","resource":{"uri":"urn:example:text","text":marker}},
            {"type":"resource_link","uri":"urn:example:link","name":marker,"title":marker,"description":marker},
            {"type":"image","data":"AA==","mimeType":"image/png","_meta":{"alt":marker}},
            {"type":"audio","data":"AA==","mimeType":"audio/wav","_meta":{"transcript":marker}}
        ],
        "structuredContent":{"nested":[{"text":marker}]},
        "_meta":{"note":marker}
    });
    let output: ToolCallOutput = serde_json::from_value(fixture).unwrap();
    let serialized = serde_json::to_value(&output).unwrap();
    let inspected = scanner.scan_value(&serialized).unwrap();
    assert_eq!(inspected.findings.len(), 9);
    assert!(!inspected.sanitized.to_string().contains(marker));
    assert_eq!(inspected.sanitized["content"][3]["data"], "AA==");
    assert_eq!(serialized["content"][1]["resource"]["text"], marker);
}
