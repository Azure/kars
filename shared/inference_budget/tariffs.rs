// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Operator-declared maximum bounds, never guessed token counts or prices.

use super::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum Operation {
    ChatCompletions,
    AnthropicMessages,
    Responses,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum OutputField {
    MaxTokens,
    MaxCompletionTokens,
    MaxOutputTokens,
}

impl OutputField {
    pub fn key(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::MaxCompletionTokens => "max_completion_tokens",
            Self::MaxOutputTokens => "max_output_tokens",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MaximumPrice {
    TokenRates {
        /// Maximum rate across fresh/cache-read/cache-creation input categories.
        input_micros_per_million: u64,
        /// Maximum rate across visible and reasoning output categories.
        output_micros_per_million: u64,
        fixed_micros: u64,
    },
    PerRequest {
        maximum_micros: u64,
    },
}

impl MaximumPrice {
    pub fn price(&self, input: u64, output: u64) -> Result<u64, BudgetError> {
        let amount = match self {
            Self::PerRequest { maximum_micros } => return Ok(*maximum_micros),
            Self::TokenRates {
                input_micros_per_million,
                output_micros_per_million,
                fixed_micros,
            } => {
                let input = u128::from(input)
                    .checked_mul(u128::from(*input_micros_per_million))
                    .ok_or(BudgetError::Overflow)?;
                let output = u128::from(output)
                    .checked_mul(u128::from(*output_micros_per_million))
                    .ok_or(BudgetError::Overflow)?;
                let input = input.checked_add(999_999).ok_or(BudgetError::Overflow)? / 1_000_000;
                let output = output.checked_add(999_999).ok_or(BudgetError::Overflow)? / 1_000_000;
                input
                    .checked_add(output)
                    .ok_or(BudgetError::Overflow)?
                    .checked_add(u128::from(*fixed_micros))
                    .ok_or(BudgetError::Overflow)?
            }
        };
        u64::try_from(amount).map_err(|_| BudgetError::Overflow)
    }

    fn valid(&self) -> bool {
        match self {
            Self::PerRequest { maximum_micros } => {
                *maximum_micros > 0 && *maximum_micros <= MAX_LEDGER_INTEGER
            }
            Self::TokenRates {
                input_micros_per_million,
                output_micros_per_million,
                fixed_micros,
            } => {
                (*input_micros_per_million > 0
                    || *output_micros_per_million > 0
                    || *fixed_micros > 0)
                    && [
                        *input_micros_per_million,
                        *output_micros_per_million,
                        *fixed_micros,
                    ]
                    .iter()
                    .all(|value| *value <= MAX_LEDGER_INTEGER)
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelContract {
    pub id: String,
    pub version: String,
    pub valid_until: String,
    /// Exact final named provider/authentication identity, never request headers.
    pub provider_id: String,
    pub endpoint: String,
    pub model: String,
    pub operation: Operation,
    pub output_field: OutputField,
    /// The provider-enforced maximum complete input/context bound, including
    /// hidden framing, cache input, and function schemas. Not bytes/4.
    pub maximum_input_tokens: u64,
    pub maximum_output_tokens: u64,
    pub maximum_wire_bytes: u64,
    /// Operator attestation that the output field bounds ALL output, including
    /// reasoning/thinking. Contracts without this guarantee cannot dispatch.
    pub output_bound_includes_reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_price: Option<MaximumPrice>,
}

impl ModelContract {
    pub fn validate(&self, now: i64) -> Result<(), BudgetError> {
        let endpoint = reqwest::Url::parse(&self.endpoint).map_err(|_| BudgetError::Contract)?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(BudgetError::Contract);
        }
        let expiry = chrono::DateTime::parse_from_rfc3339(&self.valid_until)
            .map_err(|_| BudgetError::Contract)?
            .timestamp();
        let field_matches = matches!(
            (self.operation, self.output_field),
            (
                Operation::ChatCompletions,
                OutputField::MaxTokens | OutputField::MaxCompletionTokens
            ) | (Operation::AnthropicMessages, OutputField::MaxTokens)
                | (Operation::Responses, OutputField::MaxOutputTokens)
        );
        if !valid_name(&self.id)
            || !valid_uid(&self.version)
            || self.provider_id.is_empty()
            || self.provider_id.len() > 253
            || self.model.is_empty()
            || self.model.len() > 253
            || !(self.endpoint.starts_with("https://") || self.endpoint.starts_with("http://"))
            || self.endpoint.contains(['@', '?', '#'])
            || self.endpoint.bytes().any(|b| b.is_ascii_control())
            || expiry <= now
            || !field_matches
            || !self.output_bound_includes_reasoning
            || self.maximum_input_tokens == 0
            || self.maximum_output_tokens == 0
            || self.maximum_wire_bytes == 0
            || self.maximum_wire_bytes > 1_048_576
            || self
                .maximum_price
                .as_ref()
                .is_some_and(|price| !price.valid())
        {
            return Err(BudgetError::Contract);
        }
        let tokens = self
            .maximum_input_tokens
            .checked_add(self.maximum_output_tokens)
            .ok_or(BudgetError::Overflow)?;
        if tokens > MAX_LEDGER_INTEGER {
            return Err(BudgetError::Overflow);
        }
        if let Some(price) = &self.maximum_price {
            if price.price(self.maximum_input_tokens, self.maximum_output_tokens)?
                > MAX_LEDGER_INTEGER
            {
                return Err(BudgetError::Overflow);
            }
        }
        Ok(())
    }

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

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Quote {
    pub contract: ModelContract,
    pub output_tokens: u64,
    pub maximum: Amounts,
    pub price_covered: bool,
}

impl Quote {
    pub fn validate(&self, now: i64, money_required: bool) -> Result<(), BudgetError> {
        self.contract.validate(now)?;
        if self.output_tokens == 0
            || self.output_tokens > self.contract.maximum_output_tokens
            || self.price_covered != self.contract.maximum_price.is_some()
            || (money_required && !self.price_covered)
        {
            return Err(BudgetError::Contract);
        }
        let expected = Amounts {
            tokens: self
                .contract
                .maximum_input_tokens
                .checked_add(self.output_tokens)
                .ok_or(BudgetError::Overflow)?,
            usd_micros: self
                .contract
                .maximum_price
                .as_ref()
                .map(|price| price.price(self.contract.maximum_input_tokens, self.output_tokens))
                .transpose()?
                .unwrap_or(0),
        };
        if expected != self.maximum {
            return Err(BudgetError::Contract);
        }
        Ok(())
    }

    pub fn usage(&self, usage: &Usage) -> Result<Amounts, BudgetError> {
        if usage.input_tokens > self.contract.maximum_input_tokens
            || usage.output_tokens > self.output_tokens
            || usage
                .cached_input_tokens
                .checked_add(usage.cache_creation_input_tokens)
                .is_none_or(|cached| cached > usage.input_tokens)
            || usage.reasoning_output_tokens > usage.output_tokens
        {
            return Err(BudgetError::Breach);
        }
        Ok(Amounts {
            tokens: usage
                .input_tokens
                .checked_add(usage.output_tokens)
                .ok_or(BudgetError::Overflow)?,
            usd_micros: self
                .contract
                .maximum_price
                .as_ref()
                .map(|price| price.price(usage.input_tokens, usage.output_tokens))
                .transpose()?
                .unwrap_or(0),
        })
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

#[cfg(test)]
#[path = "tariff_tests.rs"]
mod tests;
