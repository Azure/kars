// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TaskTelemetry, parse};
use crate::mcp::pipeline::ProcessOutcome;
use serde::Deserialize;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::fmt;

// The pipeline already owns the response body. Inspect only outcome metadata;
// a large tool payload is not a missing telemetry event.
#[derive(Deserialize)]
struct Reply {
    id: Value,
    #[serde(default, deserialize_with = "present")]
    error: bool,
    result: Option<ResultMetadata>,
}

fn present<'de, D: de::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    IgnoredAny::deserialize(deserializer).map(|_| true)
}

#[derive(Deserialize)]
struct ResultMetadata {
    #[serde(rename = "isError")]
    is_error: Option<bool>,
    content: Option<ContentArray>,
}

struct ContentArray;

impl<'de> Deserialize<'de> for ContentArray {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ContentVisitor;
        impl<'de> Visitor<'de> for ContentVisitor {
            type Value = ContentArray;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an MCP content array")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ContentArray)
            }
        }
        deserializer.deserialize_seq(ContentVisitor)
    }
}

struct Replies(Vec<Reply>);

impl<'de> Deserialize<'de> for Replies {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RepliesVisitor;
        impl<'de> Visitor<'de> for RepliesVisitor {
            type Value = Replies;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an MCP response or bounded response batch")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                Reply::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(|reply| Replies(vec![reply]))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut replies = Vec::new();
                while let Some(reply) = sequence.next_element::<Reply>()? {
                    if replies.len() == parse::MAX_TOOLS {
                        return Err(de::Error::custom(
                            "MCP response batch exceeds metadata limit",
                        ));
                    }
                    replies.push(reply);
                }
                Ok(Replies(replies))
            }
        }
        deserializer.deserialize_any(RepliesVisitor)
    }
}

pub fn record(
    telemetry: &TaskTelemetry,
    scope: &str,
    request: &[u8],
    outcome: &ProcessOutcome,
    latency_ms: u64,
) {
    if request.len() > parse::MAX_BODY {
        telemetry.record_gap(scope, "mcp_request_metadata");
        return;
    }
    let Ok(request) = serde_json::from_slice::<Value>(request) else {
        return;
    };
    let (status, reply) = match outcome {
        ProcessOutcome::JsonRpcResponse { body, .. } => {
            match serde_json::from_slice::<Replies>(body) {
                Ok(reply) => (200, Some(reply)),
                Err(_) => {
                    telemetry.record_gap(scope, "mcp_response_metadata");
                    (200, None)
                }
            }
        }
        ProcessOutcome::Accepted => (202, None),
        ProcessOutcome::PayloadTooLarge => (413, None),
        ProcessOutcome::NotAcceptable(_) => (406, None),
    };
    let requests = match &request {
        Value::Array(values) => values.as_slice(),
        value => std::slice::from_ref(value),
    };
    if requests.len() > parse::MAX_TOOLS {
        telemetry.record_gap(scope, "mcp_batch_metadata");
    }
    for request in requests.iter().take(parse::MAX_TOOLS) {
        if request["method"] != "tools/call" {
            continue;
        }
        let Some(name) = request["params"]["name"]
            .as_str()
            .and_then(|name| parse::identifier(name, 128))
        else {
            continue;
        };
        let id = request.get("id");
        let response = reply.as_ref().and_then(|reply| {
            id.filter(|id| id.is_number() || id.as_str().is_some_and(|id| id.len() <= 128))
                .and_then(|id| reply.0.iter().find(|reply| &reply.id == id))
        });
        let ok = response.and_then(|reply| {
            if reply.error {
                return Some(false);
            }
            let result = reply.result.as_ref()?;
            result
                .is_error
                .map(|error| !error)
                .or_else(|| result.content.as_ref().map(|_| true))
        });
        telemetry.record_router_tool(scope, &name, ok, Some(status), latency_ms, ok.is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observe(request: Value, body: Vec<u8>) -> Value {
        let telemetry = TaskTelemetry::new("mcp-test".into());
        record(
            &telemetry,
            "mcp-test",
            &serde_json::to_vec(&request).unwrap(),
            &ProcessOutcome::JsonRpcResponse {
                body,
                session_id: None,
            },
            12,
        );
        telemetry.snapshot("mcp-test", 0).unwrap()
    }

    fn call(id: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"deepwiki.read_wiki_contents","arguments":{"repoName":"kube-rs/kube"}}})
    }

    #[test]
    fn large_native_mcp_content_keeps_complete_outcome_without_retaining_payload() {
        let payload = "private tool payload ".repeat(20_000);
        for is_error in [false, true] {
            let body = serde_json::to_vec(&json!({"id":7,"result":{
                "content":[{"type":"text","text":payload}],"isError":is_error}}))
            .unwrap();
            assert!(body.len() > parse::MAX_BODY);
            let trace = observe(call(json!(7)), body);
            assert_eq!(trace["dropped_events"], 0);
            assert_eq!(trace["events"].as_array().unwrap().len(), 1);
            let event = &trace["events"][0];
            assert_eq!(event["ok"], !is_error);
            assert_eq!(event["complete"], true);
            assert_eq!(event["http_status"], 200);
            assert!(!trace.to_string().contains("private tool payload"));
        }
    }

    #[test]
    fn large_content_without_explicit_error_flag_is_successful() {
        let body = serde_json::to_vec(&json!({"id":"call-1","result":{
            "content":[{"type":"text","text":"x".repeat(parse::MAX_BODY + 1)}]}}))
        .unwrap();
        let trace = observe(call(json!("call-1")), body);
        assert_eq!(trace["dropped_events"], 0);
        assert_eq!(trace["events"][0]["ok"], true);
    }

    #[test]
    fn large_rpc_error_is_failed_not_missing_or_successful() {
        let body = serde_json::to_vec(&json!({"id":7,"error":{
            "code":-32603,"message":"x".repeat(parse::MAX_BODY + 1)}}))
        .unwrap();
        let trace = observe(call(json!(7)), body);
        assert_eq!(trace["dropped_events"], 0);
        assert_eq!(trace["events"][0]["ok"], false);
        assert_eq!(trace["events"][0]["complete"], true);
    }

    #[test]
    fn batch_outcomes_correlate_by_id_not_position() {
        let trace = observe(
            json!([call(json!(1)), call(json!(2)), call(json!(3))]),
            serde_json::to_vec(&json!([
                {"id":2,"result":{"isError":true,"content":[]}},
                {"id":1,"result":{"content":[]}}
            ]))
            .unwrap(),
        );
        assert_eq!(trace["events"][0]["ok"], true);
        assert_eq!(trace["events"][1]["ok"], false);
        assert_eq!(trace["events"][2]["ok"], Value::Null);
        assert_eq!(trace["events"][2]["complete"], false);
    }

    #[test]
    fn malformed_or_excessive_metadata_remains_an_explicit_gap() {
        let bodies = [
            br#"{"id":7,"result":{"content":["truncated"#.to_vec(),
            serde_json::to_vec(&json!({"id":7,"result":{"content":"not an array"}})).unwrap(),
            serde_json::to_vec(&vec![
                json!({"id":7,"result":{"content":[]}});
                parse::MAX_TOOLS + 1
            ])
            .unwrap(),
        ];
        for body in bodies {
            let trace = observe(call(json!(7)), body);
            assert_eq!(trace["dropped_events"], 1);
            assert_eq!(trace["events"][0]["kind"], "observation_gap");
            assert_eq!(trace["events"][1]["ok"], Value::Null);
            assert_eq!(trace["events"][1]["complete"], false);
        }
    }
}
