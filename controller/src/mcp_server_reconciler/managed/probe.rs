// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

const MAX_RESPONSE: usize = 256 * 1024;
const MAX_TOOLS: usize = 256;
const PROTOCOL: &str = "2025-06-18";

#[derive(Clone, Debug)]
pub(super) struct Probe {
    pub names: Vec<String>,
    pub digest: String,
}

async fn payload(mut response: reqwest::Response, id: u64) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(format!(
            "Managed MCP probe HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE as u64)
    {
        return Err("Managed MCP probe response exceeds its byte limit".into());
    }
    let sse = response
        .headers()
        .get("content-type")
        .and_then(|header| header.to_str().ok())
        .is_some_and(|header| header.starts_with("text/event-stream"));
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Managed MCP probe response transport failure")?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err("Managed MCP probe response exceeds its byte limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = if sse {
        let text = std::str::from_utf8(&bytes).map_err(|_| "Managed MCP probe SSE is not UTF-8")?;
        let mut result = None;
        let mut data = String::new();
        for line in text.lines().chain(std::iter::once("")) {
            if line.is_empty() {
                if !data.is_empty() {
                    let event: Value = serde_json::from_str(&data)
                        .map_err(|_| "Managed MCP probe SSE is invalid JSON")?;
                    if event["jsonrpc"] == "2.0"
                        && event["id"].as_u64() == Some(id)
                        && result.replace(event).is_some()
                    {
                        return Err("Managed MCP probe returned duplicate RPC responses".into());
                    }
                    data.clear();
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.trim_start());
            }
        }
        result.ok_or("Managed MCP probe SSE omitted the matching RPC response")?
    } else {
        serde_json::from_slice(&bytes).map_err(|_| "Managed MCP probe is not valid JSON")?
    };
    if value["jsonrpc"] != "2.0"
        || value["id"].as_u64() != Some(id)
        || value.get("error").is_some()
        || !value["result"].is_object()
    {
        return Err("Managed MCP probe returned an invalid or failed RPC response".into());
    }
    Ok(value["result"].clone())
}

fn headers(session: Option<&str>, protocol: &str) -> Result<reqwest::header::HeaderMap, String> {
    use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "mcp-protocol-version",
        HeaderValue::from_str(protocol).map_err(|_| "Invalid MCP protocol header")?,
    );
    if let Some(session) = session {
        if session.is_empty()
            || session.len() > 1024
            || !session.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("Managed MCP probe received an invalid session identifier".into());
        }
        headers.insert(
            "mcp-session-id",
            HeaderValue::from_str(session).map_err(|_| "Invalid MCP session header")?,
        );
    }
    Ok(headers)
}

async fn list(
    http: &reqwest::Client,
    endpoint: &str,
    session: Option<&str>,
    protocol: &str,
    allowed: &[String],
) -> Result<Probe, String> {
    let headers = headers(session, protocol)?;
    let response = http
        .post(endpoint)
        .headers(headers.clone())
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await
        .map_err(|_| "Managed MCP initialization notification failed")?;
    if !response.status().is_success() {
        return Err(format!(
            "Managed MCP initialization notification HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut cursor = None;
    let mut cursors = BTreeSet::new();
    let mut tools = BTreeMap::new();
    for page in 0..8 {
        let id = page + 2;
        let params = cursor
            .as_ref()
            .map(|cursor: &String| json!({"cursor":cursor}))
            .unwrap_or_else(|| json!({}));
        let response = http
            .post(endpoint)
            .headers(headers.clone())
            .json(&json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":params}))
            .send()
            .await
            .map_err(|_| "Managed MCP tools/list transport failure")?;
        let result = payload(response, id).await?;
        let definitions = result["tools"]
            .as_array()
            .ok_or("Managed MCP tools/list omitted its tool array")?;
        for tool in definitions {
            let name = tool["name"]
                .as_str()
                .filter(|name| {
                    !name.is_empty()
                        && name.len() <= 128
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
                })
                .ok_or("Managed MCP tool name is invalid")?;
            if !tool["inputSchema"].is_object()
                || tool["inputSchema"]["type"] != "object"
                || tools.insert(name.to_string(), tool.clone()).is_some()
                || tools.len() > MAX_TOOLS
            {
                return Err(
                    "Managed MCP catalog is invalid, duplicated or exceeds its tool limit".into(),
                );
            }
        }
        match result.get("nextCursor").filter(|value| !value.is_null()) {
            None => {
                let selected: BTreeMap<_, _> = tools
                    .into_iter()
                    .filter(|(name, _)| {
                        allowed
                            .iter()
                            .any(|allowed| allowed == "*" || allowed == name)
                    })
                    .collect();
                let canonical = serde_json::to_vec(&selected)
                    .map_err(|_| "MCP catalog serialization failed")?;
                return Ok(Probe {
                    names: selected.keys().cloned().collect(),
                    digest: crate::providers::signing::content_digest(&canonical),
                });
            }
            Some(value) => {
                let next = value
                    .as_str()
                    .filter(|cursor| !cursor.is_empty() && cursor.len() <= 1024)
                    .ok_or("Managed MCP catalog cursor is invalid")?;
                if !cursors.insert(next.to_string()) {
                    return Err("Managed MCP catalog pagination repeated a cursor".into());
                }
                cursor = Some(next.to_string());
            }
        }
    }
    Err("Managed MCP catalog exceeds its page limit".into())
}

pub(super) async fn probe(
    http: &reqwest::Client,
    endpoint: &str,
    allowed: &[String],
) -> Result<Probe, String> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let response = http
            .post(endpoint)
            .headers(headers(None, PROTOCOL)?)
            .json(
                &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":PROTOCOL,"capabilities":{},
                "clientInfo":{"name":"kars-controller","version":env!("CARGO_PKG_VERSION")}}}),
            )
            .send()
            .await
            .map_err(|_| "Managed MCP initialize transport failure")?;
        let session = response
            .headers()
            .get("mcp-session-id")
            .map(|header| {
                header
                    .to_str()
                    .map(str::to_string)
                    .map_err(|_| "Invalid MCP session header")
            })
            .transpose()?;
        let initialized = payload(response, 1).await;
        let negotiated = initialized
            .as_ref()
            .ok()
            .and_then(|result| result["protocolVersion"].as_str())
            .filter(|version| {
                ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"].contains(version)
            });
        let protocol = negotiated.unwrap_or(PROTOCOL);
        let outcome = match initialized.as_ref() {
            Ok(_) if negotiated.is_some() => {
                list(http, endpoint, session.as_deref(), protocol, allowed).await
            }
            Ok(_) => Err("Managed MCP initialize negotiated an unsupported protocol".into()),
            Err(error) => Err(error.clone()),
        };
        if session.is_some() {
            let closed = tokio::time::timeout(
                Duration::from_secs(2),
                http.delete(endpoint)
                    .headers(headers(session.as_deref(), protocol)?)
                    .send(),
            )
            .await
            .map_err(|_| "Managed MCP probe session cleanup timed out")?
            .map_err(|_| "Managed MCP probe session cleanup transport failure")?;
            if !closed.status().is_success() && closed.status().as_u16() != 404 {
                return Err("Managed MCP probe session could not be closed".into());
            }
        }
        outcome
    })
    .await
    .map_err(|_| "Managed MCP probe exceeded its total deadline")?
}
