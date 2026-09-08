// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Usage is settlement evidence, never pre-dispatch authority. Missing,
//! incomplete, malformed, or interrupted evidence commits the full reservation.

use crate::inference_budget_contract::{Usage, tariffs::Operation};
use serde_json::Value;

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}

fn optional(value: &Value, key: &str) -> Option<u64> {
    match value.get(key) {
        None => Some(0),
        Some(value) => value.as_u64(),
    }
}

fn detail(value: &Value, object: &str, key: &str) -> Option<u64> {
    match value.get(object) {
        None => Some(0),
        Some(value) if value.is_object() => optional(value, key),
        _ => None,
    }
}

pub fn buffered(body: &[u8], operation: Operation) -> Option<Usage> {
    let response: Value = serde_json::from_slice(body).ok()?;
    parse(response.get("usage")?, operation)
}

fn parse(usage: &Value, operation: Operation) -> Option<Usage> {
    match operation {
        Operation::ChatCompletions => {
            let input = number(usage, "prompt_tokens")?;
            let output = number(usage, "completion_tokens")?;
            if let Some(total) = usage.get("total_tokens") {
                if total.as_u64()? != input.checked_add(output)? {
                    return None;
                }
            }
            Some(Usage {
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: detail(usage, "prompt_tokens_details", "cached_tokens")?,
                cache_creation_input_tokens: 0,
                reasoning_output_tokens: detail(
                    usage,
                    "completion_tokens_details",
                    "reasoning_tokens",
                )?,
            })
        }
        Operation::Responses => {
            let input = number(usage, "input_tokens")?;
            let output = number(usage, "output_tokens")?;
            if let Some(total) = usage.get("total_tokens") {
                if total.as_u64()? != input.checked_add(output)? {
                    return None;
                }
            }
            Some(Usage {
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: detail(usage, "input_tokens_details", "cached_tokens")?,
                cache_creation_input_tokens: 0,
                reasoning_output_tokens: detail(
                    usage,
                    "output_tokens_details",
                    "reasoning_tokens",
                )?,
            })
        }
        Operation::AnthropicMessages => {
            let fresh = number(usage, "input_tokens")?;
            let cached = optional(usage, "cache_read_input_tokens")?;
            let creation = optional(usage, "cache_creation_input_tokens")?;
            Some(Usage {
                input_tokens: fresh.checked_add(cached)?.checked_add(creation)?,
                output_tokens: number(usage, "output_tokens")?,
                cached_input_tokens: cached,
                cache_creation_input_tokens: creation,
                // Anthropic output_tokens already includes thinking output.
                reasoning_output_tokens: 0,
            })
        }
    }
}

pub struct StreamUsage {
    operation: Operation,
    buffer: Vec<u8>,
    usage: Option<Usage>,
    terminal: bool,
    invalid: bool,
}

impl StreamUsage {
    pub fn new(operation: Operation) -> Self {
        Self {
            operation,
            buffer: Vec::new(),
            usage: None,
            terminal: false,
            invalid: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.invalid {
            return;
        }
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > 262_144 {
            self.invalid = true;
            self.buffer.clear();
            return;
        }
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
            while line
                .last()
                .is_some_and(|byte| matches!(*byte, b'\n' | b'\r'))
            {
                line.pop();
            }
            let Some(data) = line.strip_prefix(b"data:") else {
                continue;
            };
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if data == b"[DONE]" {
                if self.operation == Operation::ChatCompletions {
                    self.terminal = true;
                }
                continue;
            }
            if data.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_slice::<Value>(data) else {
                self.invalid = true;
                continue;
            };
            self.event(&event);
        }
    }

    fn event(&mut self, event: &Value) {
        if self.terminal {
            self.invalid = true;
            return;
        }
        match self.operation {
            Operation::ChatCompletions => {
                if let Some(usage) = event.get("usage").filter(|value| !value.is_null()) {
                    let parsed = parse(usage, self.operation);
                    if parsed.is_none()
                        || self
                            .usage
                            .as_ref()
                            .is_some_and(|old| Some(old) != parsed.as_ref())
                    {
                        self.invalid = true;
                    }
                    self.usage = parsed;
                }
                if event.get("error").is_some() {
                    self.invalid = true;
                }
            }
            Operation::Responses => match event.get("type").and_then(Value::as_str) {
                Some("response.completed") => {
                    self.usage = event
                        .pointer("/response/usage")
                        .and_then(|usage| parse(usage, self.operation));
                    self.terminal = true;
                }
                Some("response.failed" | "error") => self.invalid = true,
                _ => {}
            },
            Operation::AnthropicMessages => match event.get("type").and_then(Value::as_str) {
                Some("message_start") => {
                    self.usage = event
                        .pointer("/message/usage")
                        .and_then(|usage| parse(usage, self.operation));
                    if self.usage.is_none() {
                        self.invalid = true;
                    }
                }
                Some("message_delta") => {
                    match (
                        self.usage.as_mut(),
                        event
                            .get("usage")
                            .and_then(|usage| number(usage, "output_tokens")),
                    ) {
                        (Some(usage), Some(output)) if output >= usage.output_tokens => {
                            usage.output_tokens = output
                        }
                        _ => self.invalid = true,
                    }
                }
                Some("message_stop") => self.terminal = true,
                Some("error") => self.invalid = true,
                _ => {}
            },
        }
    }

    /// Only a cleanly completed transport with a terminal provider event is
    /// eligible for a refund. Client disconnect/drop uses None instead.
    pub fn finish(self) -> Option<Usage> {
        if self.invalid || !self.terminal || !self.buffer.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_cache_tokens_are_included_once_in_the_input_total() {
        let usage = buffered(br#"{"usage":{"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":7,"output_tokens":11}}"#,
            Operation::AnthropicMessages).unwrap();
        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.cached_input_tokens, 5);
        assert_eq!(usage.cache_creation_input_tokens, 7);
    }

    #[test]
    fn chat_stream_usage_survives_every_byte_boundary_without_counting_chunks_as_tokens() {
        let wire = b"data: {\"choices\":[]}\n\ndata: {\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"total_tokens\":8}}\n\ndata: [DONE]\n\n";
        for size in 1..wire.len() {
            let mut stream = StreamUsage::new(Operation::ChatCompletions);
            for chunk in wire.chunks(size) {
                stream.push(chunk);
            }
            assert_eq!(stream.finish().unwrap().output_tokens, 3, "{size}");
        }
    }

    #[test]
    fn partial_missing_or_malformed_usage_does_not_prove_a_refund() {
        for wire in [
            &b"data: {\"choices\":[]}\n\ndata: [DONE]\n\n"[..],
            &b"data: {\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n"[..],
            &b"data: {\"usage\":{\"prompt_tokens\":-1,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n"[..],
        ] {
            let mut stream = StreamUsage::new(Operation::ChatCompletions);
            stream.push(wire);
            assert!(stream.finish().is_none());
        }
    }

    #[test]
    fn native_stream_requires_start_usage_and_terminal_stop() {
        let mut stream = StreamUsage::new(Operation::AnthropicMessages);
        stream.push(b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n");
        stream.push(b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":7}}\n\n");
        stream.push(b"data: {\"type\":\"message_stop\"}\n\n");
        assert_eq!(stream.finish().unwrap().output_tokens, 7);
    }

    #[test]
    fn responses_only_settles_complete_final_usage() {
        let mut stream = StreamUsage::new(Operation::Responses);
        stream.push(b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n");
        assert_eq!(stream.finish().unwrap().input_tokens, 2);
        let mut incomplete = StreamUsage::new(Operation::Responses);
        incomplete.push(b"data: {\"type\":\"response.incomplete\",\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n");
        assert!(incomplete.finish().is_none());
    }
}
