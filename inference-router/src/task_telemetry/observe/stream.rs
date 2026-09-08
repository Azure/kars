// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::task_telemetry::parse::{self, Parsed, Shape, add_tool, merge_usage};
use serde_json::Value;
use std::collections::BTreeMap;

const FRAME_LIMIT: usize = 64 * 1024;
pub(super) struct Accumulator {
    shape: Shape,
    sse: bool,
    buffer: Vec<u8>,
    discarding: bool,
    tail: [u8; 4],
    parsed: Parsed,
    terminal: bool,
    pub failed: bool,
    incomplete: bool,
    tools: BTreeMap<(u64, u64), (String, String)>,
}
impl Accumulator {
    pub(super) fn new(shape: Shape, sse: bool) -> Self {
        Self {
            shape,
            sse,
            buffer: Vec::new(),
            discarding: false,
            tail: [0; 4],
            parsed: Parsed::default(),
            terminal: false,
            failed: false,
            incomplete: false,
            tools: BTreeMap::new(),
        }
    }
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        if !self.sse {
            if self.buffer.len().saturating_add(bytes.len()) <= FRAME_LIMIT && !self.discarding {
                self.buffer.extend_from_slice(bytes);
            } else {
                self.buffer.clear();
                self.discarding = true;
                self.parsed.partial = true;
            }
            return;
        }
        for byte in bytes {
            self.tail.rotate_left(1);
            self.tail[3] = *byte;
            if !self.discarding {
                if self.buffer.len() < FRAME_LIMIT {
                    self.buffer.push(*byte);
                } else {
                    self.buffer.clear();
                    self.discarding = true;
                    self.parsed.partial = true;
                }
            }
            if self.tail[2..] == *b"\n\n" || self.tail == *b"\r\n\r\n" {
                if !self.discarding {
                    let frame = std::mem::take(&mut self.buffer);
                    self.frame(&frame);
                }
                self.discarding = false;
            }
        }
    }
    fn frame(&mut self, bytes: &[u8]) {
        let Ok(frame) = std::str::from_utf8(bytes) else {
            self.parsed.partial = true;
            return;
        };
        if frame.lines().any(|line| {
            line.strip_prefix("event:")
                .is_some_and(|name| name.trim() == "error")
        }) {
            self.failed = true;
            self.terminal = true;
            self.parsed.semantic = Some(parse::SemanticOutcome::Failed);
        }
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.terminal = true;
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            self.parsed.partial = true;
            return;
        };
        if let Some(usage) = value.get("usage") {
            merge_usage(&mut self.parsed.usage, usage);
        }
        match self.shape {
            Shape::OpenAi => {
                if parse::semantic_outcome(&value, self.shape).is_some() {
                    self.failed = true;
                    self.terminal = true;
                    self.parsed.semantic = Some(parse::SemanticOutcome::Failed);
                    return;
                }
                if let Some(choices) = value["choices"].as_array() {
                    for choice in choices.iter().take(parse::MAX_TOOLS) {
                        if let Some(reason) = choice["finish_reason"]
                            .as_str()
                            .and_then(|value| parse::identifier(value, 64))
                        {
                            self.parsed.finish = Some(reason);
                        }
                        if let Some(calls) = choice["delta"]["tool_calls"].as_array() {
                            for call in calls.iter().take(parse::MAX_TOOLS) {
                                let key = (
                                    choice["index"].as_u64().unwrap_or(0),
                                    call["index"].as_u64().unwrap_or(0),
                                );
                                if !self.tools.contains_key(&key)
                                    && self.tools.len() >= parse::MAX_TOOLS
                                {
                                    self.parsed.partial = true;
                                    continue;
                                }
                                let tool = self.tools.entry(key).or_default();
                                for (slot, value) in [
                                    (&mut tool.0, &call["id"]),
                                    (&mut tool.1, &call["function"]["name"]),
                                ] {
                                    if let Some(value) = value.as_str() {
                                        if slot.as_str() == value {
                                            continue;
                                        }
                                        if slot.len().saturating_add(value.len()) <= 128 {
                                            slot.push_str(value);
                                        } else {
                                            self.parsed.partial = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Shape::Anthropic => match value["type"].as_str() {
                Some("message_start") => {
                    if let Some(usage) = value["message"].get("usage") {
                        merge_usage(&mut self.parsed.usage, usage);
                    }
                }
                Some("content_block_start") if value["content_block"]["type"] == "tool_use" => {
                    add_tool(
                        &mut self.parsed,
                        &value["content_block"]["id"],
                        &value["content_block"]["name"],
                    )
                }
                Some("message_delta") => {
                    self.parsed.finish = value["delta"]["stop_reason"]
                        .as_str()
                        .and_then(|value| parse::identifier(value, 64));
                }
                Some("message_stop") => self.terminal = true,
                Some("error") => {
                    self.terminal = true;
                    self.failed = true;
                }
                _ => {}
            },
            Shape::Responses => match value["type"].as_str() {
                Some("response.completed" | "response.failed" | "response.incomplete") => {
                    self.terminal = true;
                    self.failed |= value["type"] == "response.failed";
                    self.incomplete |= value["type"] == "response.incomplete";
                    let parsed = parse::response(&value["response"], Shape::Responses);
                    self.failed |= parsed.semantic == Some(parse::SemanticOutcome::Failed);
                    self.incomplete |= parsed.semantic == Some(parse::SemanticOutcome::Incomplete);
                    self.parsed.usage = parsed.usage;
                    self.parsed.finish = parsed.finish;
                    self.parsed.tools = parsed.tools;
                    self.parsed.semantic = parsed.semantic;
                    self.parsed.partial |= parsed.partial;
                }
                Some("error") => {
                    self.terminal = true;
                    self.failed = true;
                }
                _ => {}
            },
        }
    }
    pub(super) fn complete(&self) -> bool {
        self.terminal && !self.failed && !self.incomplete
    }
    pub(super) fn finish(&mut self) -> Parsed {
        if !self.sse && !self.discarding {
            match serde_json::from_slice::<Value>(&self.buffer) {
                Ok(value) => {
                    self.parsed = parse::response(&value, self.shape);
                    self.terminal = true;
                }
                Err(_) => self.parsed.partial = true,
            }
        }
        if !self.terminal {
            self.parsed.partial = true;
        }
        if self.failed {
            self.parsed.semantic = Some(parse::SemanticOutcome::Failed);
        } else if self.incomplete {
            self.parsed.semantic = Some(parse::SemanticOutcome::Incomplete);
        }
        for (_, (id, name)) in std::mem::take(&mut self.tools) {
            add_tool(&mut self.parsed, &Value::String(id), &Value::String(name));
        }
        std::mem::take(&mut self.parsed)
    }
}
