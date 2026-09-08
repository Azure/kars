// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TaskTelemetry, parse};
use crate::mcp::pipeline::ProcessOutcome;
use serde_json::Value;

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
            if body.len() > parse::MAX_BODY {
                telemetry.record_gap(scope, "mcp_response_metadata");
                (200, None)
            } else {
                (200, serde_json::from_slice::<Value>(body).ok())
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
            let replies = match reply {
                Value::Array(values) => values.as_slice(),
                value => std::slice::from_ref(value),
            };
            id.filter(|id| id.is_number() || id.as_str().is_some_and(|id| id.len() <= 128))
                .and_then(|id| replies.iter().find(|reply| reply.get("id") == Some(id)))
        });
        let ok = response.and_then(|reply| {
            if reply.get("error").is_some() {
                return Some(false);
            }
            let result = reply.get("result")?;
            result
                .get("isError")
                .and_then(Value::as_bool)
                .map(|error| !error)
                .or_else(|| {
                    result
                        .get("content")
                        .and_then(Value::as_array)
                        .map(|_| true)
                })
        });
        telemetry.record_router_tool(scope, &name, ok, Some(status), latency_ms, ok.is_some());
    }
}
