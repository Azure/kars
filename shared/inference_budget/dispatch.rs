// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Router-only request normalization and final-dispatch classification.
//! The controller compiles this module only for shared contract tests.

use crate::inference_budget_contract::{
    catalog::{Catalog, endpoint_host},
    tariffs::{ModelContract, Operation, OutputField, Quote},
    types::*,
};
use serde_json::Value;

impl OutputField {
    fn key(self) -> &'static str {
        match self {
            Self::Tokens => "max_tokens",
            Self::Completion => "max_completion_tokens",
            Self::Output => "max_output_tokens",
        }
    }
}

impl ModelContract {
    pub fn normalize(
        &self,
        body: &[u8],
        now: i64,
        money_required: bool,
    ) -> Result<(Vec<u8>, Quote), BudgetError> {
        self.validate(now)?;
        if body.len() as u64 > self.maximum_wire_bytes
            || (money_required && self.maximum_price.is_none())
        {
            return Err(BudgetError::Contract);
        }
        let mut value: Value = serde_json::from_slice(body).map_err(|_| BudgetError::Contract)?;
        validate_shape(&value, self.operation)?;
        let map = value.as_object_mut().ok_or(BudgetError::Contract)?;
        if map
            .get("model")
            .is_some_and(|model| model.as_str() != Some(self.model.as_str()))
        {
            return Err(BudgetError::Contract);
        }
        map.insert("model".into(), self.model.clone().into());
        let key = self.output_field.key();
        for other in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
            if other != key && map.contains_key(other) {
                return Err(BudgetError::Contract);
            }
        }
        let output = match map.get(key) {
            None => self.maximum_output_tokens,
            Some(value) => value
                .as_u64()
                .filter(|value| *value > 0 && *value <= self.maximum_output_tokens)
                .ok_or(BudgetError::Contract)?,
        };
        map.insert(key.into(), output.into());
        let bytes = serde_json::to_vec(&value).map_err(|_| BudgetError::Contract)?;
        if bytes.len() as u64 > self.maximum_wire_bytes {
            return Err(BudgetError::Contract);
        }
        let maximum = Amounts {
            tokens: self
                .maximum_input_tokens
                .checked_add(output)
                .ok_or(BudgetError::Overflow)?,
            usd_micros: self
                .maximum_price
                .as_ref()
                .map(|price| price.price(self.maximum_input_tokens, output))
                .transpose()?
                .unwrap_or(0),
        };
        Ok((
            bytes,
            Quote {
                contract: self.clone(),
                output_tokens: output,
                maximum,
                price_covered: self.maximum_price.is_some(),
            },
        ))
    }
}

fn only_keys(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| keys.contains(&key.as_str())))
}

fn cache_control(value: &Value) -> bool {
    value.get("cache_control").is_none_or(|cache| {
        only_keys(cache, &["type", "ttl"])
            && cache.get("type").and_then(Value::as_str) == Some("ephemeral")
            && cache
                .get("ttl")
                .is_none_or(|ttl| matches!(ttl.as_str(), Some("5m" | "1h")))
    })
}

fn text_content(value: &Value, anthropic: bool) -> bool {
    if value.is_null() || value.is_string() {
        return true;
    }
    value.as_array().is_some_and(|blocks| {
        blocks.len() <= 256
            && blocks
                .iter()
                .all(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text" | "input_text" | "output_text") => {
                        block.get("text").is_some_and(Value::is_string)
                            && only_keys(block, &["type", "text", "cache_control"])
                            && cache_control(block)
                    }
                    Some("tool_use") if anthropic => {
                        only_keys(block, &["type", "id", "name", "input", "cache_control"])
                            && cache_control(block)
                            && block.get("name").is_some_and(Value::is_string)
                            && block.get("id").is_some_and(Value::is_string)
                            && block.get("input").is_some_and(Value::is_object)
                    }
                    Some("tool_result") if anthropic => {
                        only_keys(
                            block,
                            &[
                                "type",
                                "tool_use_id",
                                "content",
                                "is_error",
                                "cache_control",
                            ],
                        ) && cache_control(block)
                            && block.get("tool_use_id").is_some_and(Value::is_string)
                            && block
                                .get("content")
                                .is_some_and(|value| text_content(value, false))
                    }
                    _ => false,
                })
    })
}

fn messages(value: &Value, anthropic: bool) -> bool {
    value.as_array().is_some_and(|items| {
        !items.is_empty()
            && items.len() <= 256
            && items.iter().all(|item| {
                let keys: &[&str] = if anthropic {
                    &["role", "content"]
                } else {
                    &[
                        "role",
                        "content",
                        "name",
                        "tool_calls",
                        "tool_call_id",
                        "refusal",
                    ]
                };
                let role = item.get("role").and_then(Value::as_str);
                only_keys(item, keys)
                    && matches!(
                        role,
                        Some("system" | "developer" | "user" | "assistant" | "tool")
                    )
                    && item
                        .get("content")
                        .is_some_and(|content| text_content(content, anthropic))
                    && item.get("tool_calls").is_none_or(|tools| {
                        tools.as_array().is_some_and(|items| {
                            items.iter().all(|tool| {
                                tool.get("type").and_then(Value::as_str) == Some("function")
                            })
                        })
                    })
            })
    })
}

fn validate_shape(value: &Value, operation: Operation) -> Result<(), BudgetError> {
    let map = value.as_object().ok_or(BudgetError::Contract)?;
    let allowed: &[&str] = match operation {
        Operation::ChatCompletions => &[
            "model",
            "messages",
            "max_tokens",
            "max_completion_tokens",
            "stream",
            "stream_options",
            "temperature",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
            "stop",
            "seed",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "response_format",
            "user",
            "metadata",
            "n",
            "reasoning_effort",
            "store",
        ],
        Operation::AnthropicMessages => &[
            "model",
            "messages",
            "system",
            "max_tokens",
            "stream",
            "temperature",
            "top_p",
            "top_k",
            "stop_sequences",
            "tools",
            "tool_choice",
            "thinking",
            "metadata",
        ],
        Operation::Responses => &[
            "model",
            "input",
            "instructions",
            "max_output_tokens",
            "stream",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "text",
            "reasoning",
            "store",
            "background",
            "metadata",
            "user",
        ],
    };
    if map.keys().any(|key| !allowed.contains(&key.as_str()))
        || map.get("n").is_some_and(|value| value.as_u64() != Some(1))
        || map
            .get("background")
            .is_some_and(|value| value.as_bool() != Some(false))
        || map
            .get("store")
            .is_some_and(|value| value.as_bool() != Some(false))
        || map.get("stream").is_some_and(|value| !value.is_boolean())
    {
        return Err(BudgetError::Contract);
    }
    let valid_input = match operation {
        Operation::ChatCompletions => map
            .get("messages")
            .is_some_and(|value| messages(value, false)),
        Operation::AnthropicMessages => {
            map.get("messages")
                .is_some_and(|value| messages(value, true))
                && map
                    .get("system")
                    .is_none_or(|value| text_content(value, false))
        }
        Operation::Responses => map.get("input").is_some_and(|input| {
            input.is_string()
                || input.as_array().is_some_and(|items| {
                    items.len() <= 256
                        && items
                            .iter()
                            .all(|item| match item.get("type").and_then(Value::as_str) {
                                Some("function_call") => {
                                    only_keys(
                                        item,
                                        &["type", "id", "call_id", "name", "arguments", "status"],
                                    ) && item.get("name").is_some_and(Value::is_string)
                                        && item.get("arguments").is_some_and(Value::is_string)
                                }
                                Some("function_call_output") => {
                                    only_keys(item, &["type", "id", "call_id", "output", "status"])
                                        && item.get("call_id").is_some_and(Value::is_string)
                                        && item.get("output").is_some_and(Value::is_string)
                                }
                                None | Some("message") => {
                                    only_keys(item, &["type", "role", "content", "id", "status"])
                                        && item
                                            .get("content")
                                            .is_some_and(|content| text_content(content, false))
                                }
                                _ => false,
                            })
                })
        }),
    };
    if !valid_input {
        return Err(BudgetError::Contract);
    }
    if let Some(tools) = map.get("tools") {
        let tools = tools.as_array().ok_or(BudgetError::Contract)?;
        if tools.len() > 128
            || tools.iter().any(|tool| {
                if operation == Operation::AnthropicMessages {
                    tool.get("type").is_some()
                        || !tool.get("name").is_some_and(Value::is_string)
                        || !tool.get("input_schema").is_some_and(Value::is_object)
                } else {
                    tool.get("type").and_then(Value::as_str) != Some("function")
                }
            })
        {
            return Err(BudgetError::Contract);
        }
    }
    Ok(())
}

impl Catalog {
    pub fn allows_mediated_non_inference(
        &self,
        host: &str,
        configured_model_hosts: &[String],
    ) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.non_inference_egress_hosts
            .iter()
            .any(|allowed| allowed == &host)
            && !configured_model_hosts
                .iter()
                .any(|model| model.eq_ignore_ascii_case(&host))
            && !self
                .contracts
                .iter()
                .any(|contract| endpoint_host(&contract.endpoint) == Some(host.as_str()))
    }
}

/// Exact, closed final-dispatch path classification. No substring such as
/// "completion" grants access to an unimplemented provider operation.
pub(crate) fn operation(path: &str) -> Option<Operation> {
    if path.contains(['?', '#']) {
        return None;
    }
    let path = path.trim_matches('/');
    match path {
        "chat/completions" | "v1/chat/completions" | "openai/v1/chat/completions" => {
            Some(Operation::ChatCompletions)
        }
        "messages" | "v1/messages" | "anthropic/v1/messages" => Some(Operation::AnthropicMessages),
        "responses" | "v1/responses" | "openai/v1/responses" => Some(Operation::Responses),
        _ => None,
    }
}
