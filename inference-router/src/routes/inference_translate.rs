// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Chat ↔ Responses API body translation helpers.
//!
//! Extracted from `routes/inference.rs` in `phase1/hotspot-pass2-inference-split`
//! to bring that file under the §4.2 Phase 1 LOC cap (1500). No behaviour
//! change — this module is a literal lift of the three translation helpers
//! plus their unit tests.
//!
//! ## Public surface
//! - `uuid_v4()` — best-effort UUIDv4-like string for synthetic message IDs.
//! - `chat_to_responses_body(&Bytes) -> Bytes` — converts a `/v1/chat/completions`
//!   request body to `/v1/responses` shape.
//! - `responses_to_chat_body(&Bytes) -> Bytes` — converts a `/v1/responses`
//!   response body back to chat-completions shape.
//!
//! Both translators are tolerant of malformed input (return the original
//! bytes on parse failure) — they're not validators, they're shape-mappers.

use bytes::Bytes;

#[allow(dead_code)]
pub(super) fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}-{:x}", d.as_secs(), d.subsec_nanos())
}

// ── Chat ↔ Responses format translation ────────────────────────────────────

/// Convert a chat/completions request body to Responses API format.
/// messages[] → input, max_completion_tokens → max_output_tokens
pub(super) fn chat_to_responses_body(chat_body: &Bytes) -> Bytes {
    let Ok(mut body) = serde_json::from_slice::<serde_json::Value>(chat_body) else {
        return chat_body.clone();
    };
    let obj = match body.as_object_mut() {
        Some(o) => o,
        None => return chat_body.clone(),
    };

    // Convert chat messages to Responses API input format.
    //
    // Chat format:
    //   {"role":"user","content":"text"}
    //   {"role":"assistant","content":null,"tool_calls":[{id,type,function:{name,arguments}}]}
    //   {"role":"tool","tool_call_id":"...","content":"result"}
    //
    // Responses API format:
    //   {"type":"message","role":"user","content":[{"type":"input_text","text":"..."}]}
    //   {"type":"function_call","name":"...","arguments":"...","call_id":"..."}
    //   {"type":"function_call_output","call_id":"...","output":"..."}
    if let Some(messages) = obj.remove("messages") {
        if let Some(msgs) = messages.as_array() {
            let mut converted: Vec<serde_json::Value> = Vec::new();
            for msg in msgs {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");

                match role {
                    "tool" => {
                        // Tool result → function_call_output
                        let call_id = msg
                            .get("tool_call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let output = msg
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        converted.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": output
                        }));
                    }
                    "assistant"
                        if msg
                            .get("tool_calls")
                            .and_then(|v| v.as_array())
                            .map(|a| !a.is_empty())
                            .unwrap_or(false) =>
                    {
                        // Assistant with tool_calls → function_call items
                        // First emit any text content as a message
                        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                            if !content.is_empty() {
                                converted.push(serde_json::json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{"type": "output_text", "text": content}]
                                }));
                            }
                        }
                        // Then emit each tool call as a function_call item
                        for tc in msg["tool_calls"].as_array().unwrap() {
                            let call_id = tc
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}")
                                .to_string();
                            converted.push(serde_json::json!({
                                "type": "function_call",
                                "name": name,
                                "arguments": arguments,
                                "call_id": call_id
                            }));
                        }
                    }
                    _ => {
                        // Regular message (user/assistant/system/developer)
                        let resp_role = if role == "system" { "developer" } else { role };
                        let content = if let Some(arr) =
                            msg.get("content").and_then(|c| c.as_array())
                        {
                            // Array content — convert type names
                            let items: Vec<serde_json::Value> = arr
                                .iter()
                                .map(|item| {
                                    let mut it = item.clone();
                                    if let Some(t) =
                                        it.get("type").and_then(|t| t.as_str()).map(String::from)
                                    {
                                        let new_type = match (t.as_str(), role) {
                                            ("text", "assistant") => "output_text",
                                            ("text", _) => "input_text",
                                            ("image_url", _) => "input_image",
                                            ("refusal", _) => "refusal",
                                            _ => t.as_str(),
                                        };
                                        it.as_object_mut()
                                            .unwrap()
                                            .insert("type".into(), serde_json::json!(new_type));
                                        if new_type == "input_image" {
                                            if let Some(url_obj) = it.get("image_url").cloned() {
                                                let url =
                                                    url_obj.get("url").cloned().unwrap_or(url_obj);
                                                let obj = it.as_object_mut().unwrap();
                                                obj.remove("image_url");
                                                obj.insert("image_url".into(), url);
                                            }
                                        }
                                    }
                                    it
                                })
                                .collect();
                            serde_json::json!(items)
                        } else if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                            // String content — wrap in typed content block
                            let ct = if role == "assistant" {
                                "output_text"
                            } else {
                                "input_text"
                            };
                            serde_json::json!([{"type": ct, "text": text}])
                        } else {
                            serde_json::json!([])
                        };
                        converted.push(serde_json::json!({
                            "type": "message",
                            "role": resp_role,
                            "content": content
                        }));
                    }
                }
            }
            obj.insert("input".into(), serde_json::json!(converted));
        } else {
            obj.insert("input".into(), messages);
        }
    }

    // max_completion_tokens → max_output_tokens
    if let Some(max) = obj.remove("max_completion_tokens") {
        obj.insert("max_output_tokens".into(), max);
    }
    if let Some(max) = obj.remove("max_tokens") {
        obj.entry("max_output_tokens").or_insert(max);
    }

    // Convert tools format: chat uses {type, function:{name, parameters, ...}}
    // Responses API uses flattened {type, name, parameters, ...}
    if let Some(tools) = obj.remove("tools") {
        if let Some(tools_arr) = tools.as_array() {
            let converted_tools: Vec<serde_json::Value> = tools_arr
                .iter()
                .map(|tool| {
                    if let Some(func) = tool.get("function") {
                        let mut t = serde_json::json!({"type": "function"});
                        let t_obj = t.as_object_mut().unwrap();
                        if let Some(f_obj) = func.as_object() {
                            for (k, v) in f_obj {
                                t_obj.insert(k.clone(), v.clone());
                            }
                        }
                        t
                    } else {
                        tool.clone()
                    }
                })
                .collect();
            obj.insert("tools".into(), serde_json::json!(converted_tools));
        } else {
            obj.insert("tools".into(), tools);
        }
    }

    // Convert tool_choice format if present
    if let Some(tc) = obj.remove("tool_choice") {
        // Chat: {"type":"function","function":{"name":"foo"}}
        // Responses: {"type":"function","name":"foo"}
        if let Some(func) = tc.get("function") {
            if let Some(name) = func.get("name") {
                obj.insert(
                    "tool_choice".into(),
                    serde_json::json!({
                        "type": "function",
                        "name": name
                    }),
                );
            }
        } else {
            // "auto", "none", "required" pass through unchanged
            obj.insert("tool_choice".into(), tc);
        }
    }

    // Remove chat-specific fields that Responses API doesn't accept
    obj.remove("stream");
    obj.remove("stop");
    obj.remove("frequency_penalty");
    obj.remove("presence_penalty");
    obj.remove("logprobs");
    obj.remove("top_logprobs");
    obj.remove("n");

    serde_json::to_vec(&body)
        .map(Bytes::from)
        .unwrap_or_else(|_| chat_body.clone())
}

/// Convert a Responses API response back to chat/completions format.
/// output[].content[].text → choices[].message.content
/// output[] function_call items → choices[].message.tool_calls
pub(super) fn responses_to_chat_body(resp_body: &Bytes) -> Bytes {
    let Ok(resp) = serde_json::from_slice::<serde_json::Value>(resp_body) else {
        return resp_body.clone();
    };

    // If it's an error response, pass through
    if resp
        .get("error")
        .and_then(|e| if e.is_null() { None } else { Some(e) })
        .is_some()
    {
        return resp_body.clone();
    }

    // Extract text content and tool_calls from output
    let mut content = String::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();

    if let Some(items) = resp.get("output").and_then(|o| o.as_array()) {
        for item in items {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    if let Some(texts) = item.get("content").and_then(|c| c.as_array()) {
                        for c in texts {
                            if let Some(text) = c.get("text").and_then(|t| t.as_str()) {
                                content.push_str(text);
                            }
                        }
                    }
                }
                "function_call" => {
                    let call_id = item
                        .get("call_id")
                        .or(item.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}")
                        .to_string();
                    tool_calls.push(serde_json::json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    // Build chat/completions-shaped response
    let usage = resp.get("usage").cloned().unwrap_or(serde_json::json!({}));
    let mut message = serde_json::json!({
        "role": "assistant",
    });
    let finish_reason;
    if !tool_calls.is_empty() {
        message["tool_calls"] = serde_json::json!(tool_calls);
        message["content"] = serde_json::Value::Null;
        if !content.is_empty() {
            message["content"] = serde_json::json!(content);
        }
        finish_reason = "tool_calls";
    } else {
        message["content"] = serde_json::json!(content);
        finish_reason = "stop";
    }

    let chat_resp = serde_json::json!({
        "id": resp.get("id").cloned().unwrap_or(serde_json::json!("")),
        "object": "chat.completion",
        "created": resp.get("created_at").cloned().unwrap_or(serde_json::json!(0)),
        "model": resp.get("model").cloned().unwrap_or(serde_json::json!("")),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }],
        "usage": usage
    });

    serde_json::to_vec(&chat_resp)
        .map(Bytes::from)
        .unwrap_or_else(|_| resp_body.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_chat_to_responses_simple_message() {
        let chat = serde_json::json!({
            "model": "gpt-5.4-pro",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_completion_tokens": 100,
            "stream": true
        });
        let body = Bytes::from(serde_json::to_vec(&chat).unwrap());
        let result = chat_to_responses_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        assert!(v.get("messages").is_none(), "messages should be removed");
        assert!(v.get("stream").is_none(), "stream should be removed");
        assert_eq!(v["max_output_tokens"], 100);

        let input = v["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn test_chat_to_responses_tool_calls() {
        let chat = serde_json::json!({
            "model": "gpt-5.4-pro",
            "messages": [
                {"role": "user", "content": "Search for cats"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {"name": "web_search", "arguments": "{\"q\":\"cats\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_123", "content": "Cats are great"},
                {"role": "assistant", "content": "Here's what I found about cats."}
            ],
            "tools": [{"type": "function", "function": {"name": "web_search", "parameters": {}}}]
        });
        let body = Bytes::from(serde_json::to_vec(&chat).unwrap());
        let result = chat_to_responses_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let input = v["input"].as_array().unwrap();
        assert_eq!(input.len(), 4);
        // User message
        assert_eq!(input[0]["type"], "message");
        // Function call
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["name"], "web_search");
        assert_eq!(input[1]["call_id"], "call_123");
        // Function call output
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_123");
        assert_eq!(input[2]["output"], "Cats are great");
        // Assistant response
        assert_eq!(input[3]["type"], "message");
        assert_eq!(input[3]["role"], "assistant");

        // Tools should be flattened
        let tools = v["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "web_search");
        assert!(tools[0].get("function").is_none());
    }

    #[test]
    fn test_responses_to_chat_with_tool_calls() {
        let resp = serde_json::json!({
            "id": "resp_123",
            "model": "gpt-5.4-pro",
            "created_at": 1234567890,
            "output": [
                {"type": "function_call", "call_id": "call_456", "name": "search", "arguments": "{\"q\":\"dogs\"}"}
            ],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let body = Bytes::from(serde_json::to_vec(&resp).unwrap());
        let result = responses_to_chat_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        let tc = &v["choices"][0]["message"]["tool_calls"];
        assert_eq!(tc[0]["id"], "call_456");
        assert_eq!(tc[0]["function"]["name"], "search");
    }

    #[test]
    fn test_chat_to_responses_system_to_developer() {
        let chat = serde_json::json!({
            "model": "gpt-5.4-pro",
            "messages": [
                {"role": "system", "content": "You are helpful"}
            ]
        });
        let body = Bytes::from(serde_json::to_vec(&chat).unwrap());
        let result = chat_to_responses_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let input = v["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "developer");
    }

    #[test]
    fn test_responses_to_chat_with_null_error() {
        // Real Responses API includes "error": null — must NOT short-circuit
        let resp = serde_json::json!({
            "id": "resp_456",
            "object": "response",
            "model": "gpt-5.4-pro",
            "created_at": 1234567890,
            "error": null,
            "output": [
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "Hello!"}
                ]}
            ],
            "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}
        });
        let body = Bytes::from(serde_json::to_vec(&resp).unwrap());
        let result = responses_to_chat_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        // Should be converted to chat format, NOT raw passthrough
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
    }
}
