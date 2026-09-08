// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::Serialize;
use serde_json::Value;

pub const MAX_BODY: usize = 64 * 1024;
pub const MAX_TOOLS: usize = 32;

#[derive(Clone, Copy, Debug)]
pub enum Shape {
    OpenAi,
    Responses,
    Anthropic,
}
impl Shape {
    pub fn for_path(path: &str) -> Option<Self> {
        match path.trim_matches('/').trim_start_matches("v1/") {
            "chat/completions" => Some(Self::OpenAi),
            "responses" => Some(Self::Responses),
            "messages" => Some(Self::Anthropic),
            _ => None,
        }
    }
}

pub fn identifier(value: &str, max: usize) -> Option<String> {
    crate::access_request::scope::identifier(value, max).then(|| value.to_string())
}
#[derive(Default, Clone, Serialize)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
}
#[derive(Default)]
pub struct Parsed {
    pub usage: Usage,
    pub finish: Option<String>,
    pub tools: Vec<(String, String)>,
    pub partial: bool,
    pub semantic: Option<SemanticOutcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticOutcome {
    Failed,
    Incomplete,
}

impl SemanticOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Failed => "upstream_error",
            Self::Incomplete => "incomplete",
        }
    }
}

pub fn semantic_outcome(value: &Value, shape: Shape) -> Option<SemanticOutcome> {
    if matches!(shape, Shape::Responses) {
        match value.get("status").and_then(Value::as_str) {
            Some("failed") => return Some(SemanticOutcome::Failed),
            Some("incomplete") => return Some(SemanticOutcome::Incomplete),
            _ => {}
        }
    }
    (value.get("error").is_some_and(|error| !error.is_null())
        || value.get("type").and_then(Value::as_str) == Some("error"))
    .then_some(SemanticOutcome::Failed)
}

pub fn merge_usage(target: &mut Usage, usage: &Value) {
    let get = |key: &str| usage.get(key).and_then(Value::as_u64);
    if let Some(value) = get("prompt_tokens").or_else(|| get("input_tokens")) {
        target.prompt_tokens = Some(value);
    }
    if let Some(value) = get("completion_tokens").or_else(|| get("output_tokens")) {
        target.completion_tokens = Some(value);
    }
    if let Some(value) = get("total_tokens") {
        target.total_tokens = Some(value);
    }
    if let Some(value) = get("cache_read_input_tokens").or_else(|| {
        usage
            .get("prompt_tokens_details")
            .or_else(|| usage.get("input_tokens_details"))
            .and_then(|value| value.get("cached_tokens"))
            .and_then(Value::as_u64)
    }) {
        target.cached_tokens = Some(value);
    }
}

pub fn add_tool(parsed: &mut Parsed, id: &Value, name: &Value) {
    let Some(name) = name.as_str().and_then(|value| identifier(value, 128)) else {
        parsed.partial = true;
        return;
    };
    let id = id
        .as_str()
        .and_then(|value| identifier(value, 128))
        .unwrap_or_default();
    if parsed.tools.len() >= MAX_TOOLS {
        parsed.partial = true;
        return;
    }
    parsed.tools.push((id, name));
}

pub fn response(value: &Value, shape: Shape) -> Parsed {
    let mut parsed = Parsed {
        semantic: semantic_outcome(value, shape),
        ..Default::default()
    };
    if let Some(usage) = value.get("usage") {
        merge_usage(&mut parsed.usage, usage);
    }
    match shape {
        Shape::OpenAi => {
            if let Some(choices) = value.get("choices").and_then(Value::as_array) {
                parsed.partial |= choices.len() > MAX_TOOLS;
                for choice in choices.iter().take(MAX_TOOLS) {
                    parsed.finish = choice
                        .get("finish_reason")
                        .and_then(Value::as_str)
                        .and_then(|value| identifier(value, 64));
                    if let Some(tools) = choice
                        .pointer("/message/tool_calls")
                        .and_then(Value::as_array)
                    {
                        for tool in tools.iter().take(MAX_TOOLS + 1) {
                            add_tool(&mut parsed, &tool["id"], &tool["function"]["name"]);
                        }
                    }
                }
            }
        }
        Shape::Responses => {
            parsed.finish = value
                .get("status")
                .and_then(Value::as_str)
                .and_then(|value| identifier(value, 64));
            if let Some(items) = value.get("output").and_then(Value::as_array) {
                parsed.partial |= items.len() > MAX_TOOLS;
                for item in items.iter().take(MAX_TOOLS + 1) {
                    if item["type"] == "function_call" {
                        add_tool(&mut parsed, &item["call_id"], &item["name"]);
                    }
                }
            }
        }
        Shape::Anthropic => {
            parsed.finish = value
                .get("stop_reason")
                .and_then(Value::as_str)
                .and_then(|value| identifier(value, 64));
            if let Some(items) = value.get("content").and_then(Value::as_array) {
                parsed.partial |= items.len() > MAX_TOOLS;
                for item in items.iter().take(MAX_TOOLS + 1) {
                    if item["type"] == "tool_use" {
                        add_tool(&mut parsed, &item["id"], &item["name"]);
                    }
                }
            }
        }
    }
    parsed
}

pub fn request_results(body: &[u8], shape: Shape) -> Vec<(String, Option<bool>)> {
    if body.len() > MAX_BODY {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let mut results = Vec::new();
    let mut add = |id: &Value, ok: Option<bool>| {
        if results.len() < MAX_TOOLS
            && let Some(id) = id.as_str().and_then(|value| identifier(value, 128))
        {
            results.push((id, ok));
        }
    };
    if matches!(shape, Shape::Responses) {
        if let Some(items) = value["input"].as_array() {
            for item in items.iter().rev().take(128) {
                if item["type"] == "function_call_output" {
                    add(
                        &item["call_id"],
                        item["is_error"].as_bool().map(|error| !error),
                    );
                }
            }
        }
    } else if let Some(messages) = value["messages"].as_array() {
        for message in messages.iter().rev().take(128) {
            if message["role"] == "tool" {
                add(
                    &message["tool_call_id"],
                    message["is_error"].as_bool().map(|error| !error),
                );
            }
            if let Some(content) = message["content"].as_array() {
                for item in content.iter().take(MAX_TOOLS) {
                    if item["type"] == "tool_result" {
                        add(
                            &item["tool_use_id"],
                            item["is_error"].as_bool().map(|error| !error),
                        );
                    }
                }
            }
        }
    }
    results
}
