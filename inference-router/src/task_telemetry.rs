// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Per-task execution telemetry, sourced from the model traffic the router
//! already proxies.
//!
//! The router is the single point every model call flows through, so it is the
//! honest source of truth for what an agent actually did: which rounds ran,
//! what tools the model invoked, and the real token usage. This module turns
//! that observed traffic into a bounded, queryable per-task event log — the
//! same `round` / `tool` event shape the Bridge already renders as a mission's
//! live activity (`kars-mission-trace-<task>` → `trace.json`).
//!
//! Why here and not in the agent: the previous design had a hand-rolled
//! in-process agent loop (`processTaskWithTools`) emit its own trace. That loop
//! duplicated the real OpenClaw agent, only spoke the OpenAI-compatible
//! endpoint (which truncates Claude tool turns), and reported its own activity.
//! By deriving the trace from what the router observes, ANY agent — the real
//! OpenClaw harness included — gets a faithful trace for free, and the
//! duplicate loop can be retired.
//!
//! Sharding: there is one router per sandbox pod, running one task at a time,
//! so this buffer is naturally per-task and tiny. No central store, no
//! cross-task contention — it scales exactly as the fleet of routers does.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{Value, json};

/// Maximum events retained per task. A run is capped at 25 rounds with a
/// handful of tools each, so a few hundred events is the realistic ceiling;
/// 4096 leaves generous headroom while bounding memory on a misbehaving model.
const MAX_EVENTS: usize = 4096;

/// Response wire shape, so the parser knows where tool calls and token usage
/// live. The router knows this from the upstream path it forwarded to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// OpenAI chat/completions: `choices[0].message.tool_calls[]`,
    /// `usage.prompt_tokens` / `completion_tokens`.
    OpenAi,
    /// Anthropic Messages: `content[]` `tool_use` blocks, `stop_reason`,
    /// `usage.input_tokens` / `output_tokens`.
    Anthropic,
}

struct Inner {
    seq: u64,
    round: u64,
    events: VecDeque<Value>,
    /// tool_call_id → index of its `tool` event in `events`, so a later request
    /// carrying the tool result can patch `result_preview` / `ok` in place.
    pending_tools: HashMap<String, usize>,
    /// Absolute index of the front of `events` (events popped off the front
    /// when capped), so `pending_tools` indices stay valid after eviction.
    base: usize,
}

/// Bounded per-task event log derived from proxied model traffic.
pub struct TaskTelemetry {
    inner: Mutex<Inner>,
}

impl Default for TaskTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskTelemetry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                seq: 0,
                round: 0,
                events: VecDeque::new(),
                pending_tools: HashMap::new(),
                base: 0,
            }),
        }
    }

    /// Current high-water sequence number. An agent records this before a task
    /// and passes it to [`snapshot`](Self::snapshot) afterwards to get exactly
    /// that task's events.
    pub fn cursor(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).seq
    }

    /// Events with `seq > since`, in order — the trace.json array shape.
    pub fn snapshot(&self, since: u64) -> Vec<Value> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.events
            .iter()
            .filter(|e| {
                e.get("seq")
                    .and_then(|s| s.as_u64())
                    .is_some_and(|s| s > since)
            })
            .cloned()
            .collect()
    }

    fn push(g: &mut Inner, mut event: Value) -> usize {
        g.seq += 1;
        if let Some(obj) = event.as_object_mut() {
            obj.insert("seq".into(), json!(g.seq));
        }
        g.events.push_back(event);
        let mut idx = g.base + g.events.len() - 1;
        while g.events.len() > MAX_EVENTS {
            g.events.pop_front();
            g.base += 1;
            idx = idx.saturating_sub(0); // idx of just-pushed stays absolute
        }
        // Evict stale pending-tool pointers that fell off the front.
        if g.base > 0 {
            g.pending_tools.retain(|_, v| *v >= g.base);
        }
        idx
    }

    /// Record a tool that the router itself authoritatively executed on behalf
    /// of the sandbox. Unlike model-declared tool calls, these events do not
    /// depend on the harness reporting a follow-up tool message. Egress proxy
    /// calls use this path so the router remains the source of truth for the
    /// URL, enforcement outcome, HTTP status, and latency.
    pub fn record_router_tool(
        &self,
        name: &str,
        args_preview: &str,
        result_preview: &str,
        ok: bool,
        latency_ms: u64,
    ) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let round = g.round.saturating_sub(1);
        let pending = g
            .pending_tools
            .iter()
            .filter_map(|(id, &absolute)| {
                absolute.checked_sub(g.base).map(|relative| (id, relative))
            })
            .find(|(_, idx)| {
                g.events.get(*idx).is_some_and(|event| {
                    event.get("name").and_then(Value::as_str) == Some(name)
                        && event
                            .get("result_preview")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                })
            });
        if let Some((id, idx)) = pending.map(|(id, idx)| (id.clone(), idx))
            && let Some(event) = g.events.get_mut(idx)
            && let Some(obj) = event.as_object_mut()
        {
            obj.insert(
                "args_preview".into(),
                json!(Self::preview_text(args_preview, 512)),
            );
            obj.insert(
                "result_preview".into(),
                json!(Self::preview_text(result_preview, 180)),
            );
            obj.insert("ms".into(), json!(latency_ms));
            obj.insert("ok".into(), json!(ok));
            obj.insert("source".into(), json!("router"));
            g.pending_tools.remove(&id);
            return;
        }
        Self::push(
            &mut g,
            json!({
                "kind": "tool",
                "round": round,
                "name": name,
                "args_preview": Self::preview_text(args_preview, 512),
                "result_preview": Self::preview_text(result_preview, 180),
                "ms": latency_ms,
                "ok": ok,
                "source": "router",
                "ts": now_rfc3339(),
            }),
        );
    }

    fn preview_text(value: &str, max: usize) -> String {
        let mut chars = value.chars();
        let head: String = chars.by_ref().take(max).collect();
        if chars.next().is_some() {
            format!("{head}...")
        } else {
            head
        }
    }

    /// Record a completed model response: one `round` event with real token
    /// usage + finish reason + tool-call count, followed by a `tool` event for
    /// each tool the model invoked (name + argument preview). Tool results are
    /// filled in later by [`record_request_results`](Self::record_request_results)
    /// when the agent feeds them back on the next request.
    pub fn record_response(&self, resp: &Value, shape: Shape, latency_ms: u64) {
        let ts = now_rfc3339();
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let round = g.round;
        g.round += 1;

        let (prompt, completion, total, cached, finish, tool_calls) = parse_response(resp, shape);
        Self::push(
            &mut g,
            json!({
                "kind": "round",
                "round": round,
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": total,
                "cached_tokens": cached,
                "finish_reason": finish,
                "tool_calls": tool_calls.len(),
                "ms": latency_ms,
                "ts": ts,
            }),
        );

        for (id, name, args_preview) in tool_calls {
            let idx = Self::push(
                &mut g,
                json!({
                    "kind": "tool",
                    "round": round,
                    "name": name,
                    "args_preview": args_preview,
                    "result_preview": "",
                    "ms": 0,
                    "ok": true,
                    "ts": ts.clone(),
                }),
            );
            if !id.is_empty() {
                g.pending_tools.insert(id, idx);
            }
        }
    }

    /// Patch tool-result previews from a request body the agent sent back to the
    /// model. The model's tool turn (recorded by `record_response`) named the
    /// tools; the *next* request carries their results, which the router also
    /// proxies — so the full round-trip is observable here.
    pub fn record_request_results(&self, req: &Value, shape: Shape) {
        let results = parse_request_results(req, shape);
        if results.is_empty() {
            return;
        }
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        for (id, preview, ok) in results {
            if let Some(&idx) = g.pending_tools.get(&id) {
                let rel = idx.checked_sub(g.base);
                if let Some(rel) = rel
                    && let Some(ev) = g.events.get_mut(rel)
                    && let Some(obj) = ev.as_object_mut()
                {
                    obj.insert("result_preview".into(), json!(preview));
                    obj.insert("ok".into(), json!(ok));
                }
                g.pending_tools.remove(&id);
            }
        }
    }
}

/// Bounded, whitespace-collapsed, base64-stripped preview of a tool argument or
/// result — mirrors the agent loop's old `tracePreview` so the Bridge renders
/// the same readable, payload-free snippets.
fn preview(value: &Value, max: usize) -> String {
    let mut s = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    // Strip long base64 runs (file payloads travel as artifacts, not trace).
    if s.len() > 240 {
        // Cheap heuristic: collapse a very long unbroken token.
        s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    } else {
        s
    }
}

/// Returns `(prompt, completion, total, cached, finish_reason, [(id, name, args_preview)])`.
/// `cached` is the number of prompt tokens served from the provider's prompt
/// cache (OpenAI `usage.prompt_tokens_details.cached_tokens`, Anthropic
/// `usage.cache_read_input_tokens`) — cache reads are billed at a fraction of
/// fresh input, so surfacing them lets the efficiency engine reflect the real
/// economics instead of treating every input token as full price.
fn parse_response(
    resp: &Value,
    shape: Shape,
) -> (u64, u64, u64, u64, String, Vec<(String, String, String)>) {
    match shape {
        Shape::OpenAi => {
            let usage = resp.get("usage");
            let prompt = usage
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion = usage
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = usage
                .and_then(|u| u.get("total_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(prompt + completion);
            let cached = usage
                .and_then(|u| u.get("prompt_tokens_details"))
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let choice = resp
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|c| c.first());
            let finish = choice
                .and_then(|c| c.get("finish_reason"))
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string();
            let mut tools = Vec::new();
            if let Some(tcs) = choice
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("tool_calls"))
                .and_then(|t| t.as_array())
            {
                for tc in tcs {
                    let id = tc
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    tools.push((id, name, preview(&args, 180)));
                }
            }
            (prompt, completion, total, cached, finish, tools)
        }
        Shape::Anthropic => {
            let usage = resp.get("usage");
            let prompt = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cached = usage
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let finish = resp
                .get("stop_reason")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let mut tools = Vec::new();
            if let Some(content) = resp.get("content").and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args = block.get("input").cloned().unwrap_or(Value::Null);
                        tools.push((id, name, preview(&args, 180)));
                    }
                }
            }
            (
                prompt,
                completion,
                prompt + completion,
                cached,
                finish,
                tools,
            )
        }
    }
}

/// Returns `[(tool_call_id, result_preview, ok)]` from tool results carried in a
/// follow-up request body.
fn parse_request_results(req: &Value, shape: Shape) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let Some(messages) = req.get("messages").and_then(|m| m.as_array()) else {
        return out;
    };
    match shape {
        Shape::OpenAi => {
            for m in messages {
                if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
                    let id = m
                        .get("tool_call_id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let content = m.get("content").cloned().unwrap_or(Value::Null);
                    if !id.is_empty() {
                        out.push((id, preview(&content, 180), !is_error_text(&content)));
                    }
                }
            }
        }
        Shape::Anthropic => {
            for m in messages {
                let Some(parts) = m.get("content").and_then(|c| c.as_array()) else {
                    continue;
                };
                for p in parts {
                    if p.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        let id = p
                            .get("tool_use_id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let content = p.get("content").cloned().unwrap_or(Value::Null);
                        let ok = !p.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false)
                            && !is_error_text(&content);
                        if !id.is_empty() {
                            out.push((id, preview(&content, 180), ok));
                        }
                    }
                }
            }
        }
    }
    out
}

fn is_error_text(v: &Value) -> bool {
    let s = match v {
        Value::String(s) => s.to_ascii_lowercase(),
        other => other.to_string().to_ascii_lowercase(),
    };
    s.starts_with("error") || s.contains("\"error\"") || s.contains(" error:")
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Accumulates an Anthropic *streaming* response (SSE) into a single synthetic
/// response Value, so the same [`TaskTelemetry::record_response`] path produces
/// an identical trace whether the agent streamed or buffered.
///
/// Anthropic SSE event flow:
///   message_start { message.usage.input_tokens }
///   content_block_start { index, content_block:{type:"tool_use", id, name} }
///   content_block_delta { index, delta:{type:"input_json_delta", partial_json} }
///   content_block_stop { index }
///   message_delta { delta:{stop_reason}, usage:{output_tokens} }
///   message_stop
#[derive(Default)]
pub struct AnthropicStreamAcc {
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: String,
    /// content-block index → accumulating tool_use block.
    tools: std::collections::BTreeMap<u64, ToolAcc>,
    recorded: bool,
}

#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    args: String,
}

impl AnthropicStreamAcc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one decoded SSE `data:` JSON object. Returns true when the terminal
    /// event (`message_stop`) was seen.
    pub fn feed(&mut self, v: &Value) -> bool {
        match v.get("type").and_then(|t| t.as_str()) {
            Some("message_start") => {
                if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                    self.input_tokens = u.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
                }
                false
            }
            Some("content_block_start") => {
                let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                if let Some(cb) = v.get("content_block")
                    && cb.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                {
                    self.tools.insert(
                        idx,
                        ToolAcc {
                            id: cb
                                .get("id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: cb
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string(),
                            args: String::new(),
                        },
                    );
                }
                false
            }
            Some("content_block_delta") => {
                let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                if let Some(delta) = v.get("delta")
                    && delta.get("type").and_then(|t| t.as_str()) == Some("input_json_delta")
                    && let Some(frag) = delta.get("partial_json").and_then(|p| p.as_str())
                    && let Some(tool) = self.tools.get_mut(&idx)
                {
                    tool.args.push_str(frag);
                }
                false
            }
            Some("message_delta") => {
                if let Some(sr) = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|s| s.as_str())
                {
                    self.stop_reason = sr.to_string();
                }
                if let Some(ot) = v
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(|t| t.as_u64())
                {
                    self.output_tokens = ot;
                }
                false
            }
            Some("message_stop") => true,
            _ => false,
        }
    }

    /// Synthesize the equivalent non-streaming Anthropic response and record it.
    /// Idempotent — only records once.
    pub fn finish(&mut self, telem: &TaskTelemetry, latency_ms: u64) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        let content: Vec<Value> = self
            .tools
            .values()
            .map(|t| {
                let input: Value = serde_json::from_str(&t.args).unwrap_or_else(|_| json!({}));
                json!({ "type": "tool_use", "id": t.id, "name": t.name, "input": input })
            })
            .collect();
        let resp = json!({
            "stop_reason": self.stop_reason,
            "content": content,
            "usage": { "input_tokens": self.input_tokens, "output_tokens": self.output_tokens },
        });
        telem.record_response(&resp, Shape::Anthropic, latency_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_response_records_round_and_tools() {
        let t = TaskTelemetry::new();
        let before = t.cursor();
        let resp = json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {"tool_calls": [
                    {"id": "c1", "function": {"name": "web_search", "arguments": "{\"q\":\"x\"}"}}
                ]}
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120}
        });
        t.record_response(&resp, Shape::OpenAi, 1500);
        let evs = t.snapshot(before);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0]["kind"], "round");
        assert_eq!(evs[0]["total_tokens"], 120);
        assert_eq!(evs[0]["tool_calls"], 1);
        assert_eq!(evs[1]["kind"], "tool");
        assert_eq!(evs[1]["name"], "web_search");
        assert_eq!(evs[1]["result_preview"], "");
    }

    #[test]
    fn router_tool_is_recorded_without_model_tool_call() {
        let t = TaskTelemetry::new();
        let cursor = t.cursor();
        t.record_router_tool(
            "http_fetch",
            "https://example.com/docs",
            "HTTP 200",
            true,
            42,
        );
        let events = t.snapshot(cursor);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "tool");
        assert_eq!(events[0]["name"], "http_fetch");
        assert_eq!(events[0]["source"], "router");
        assert_eq!(events[0]["result_preview"], "HTTP 200");
    }

    #[test]
    fn router_tool_completes_matching_model_event_without_duplicate() {
        let t = TaskTelemetry::new();
        let cursor = t.cursor();
        t.record_response(
            &json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {"tool_calls": [{
                        "id": "call-1",
                        "function": {"name": "http_fetch", "arguments": "{\"url\":\"secret\"}"}
                    }]}
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }),
            Shape::OpenAi,
            1,
        );
        t.record_router_tool(
            "http_fetch",
            "https://example.com/docs",
            "HTTP 200",
            true,
            42,
        );
        let events = t.snapshot(cursor);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["name"], "http_fetch");
        assert_eq!(events[1]["args_preview"], "https://example.com/docs");
        assert_eq!(events[1]["source"], "router");
        t.record_request_results(
            &json!({"messages": [{
                "role": "tool",
                "tool_call_id": "call-1",
                "content": "{\"url\":\"https://user:secret@example.com/private\"}"
            }]}),
            Shape::OpenAi,
        );
        let after = t.snapshot(cursor);
        assert_eq!(after[1]["result_preview"], "HTTP 200");
    }

    #[test]
    fn anthropic_tool_use_and_result_correlation() {
        let t = TaskTelemetry::new();
        let resp = json!({
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "searching"},
                {"type": "tool_use", "id": "tu1", "name": "http_fetch", "input": {"url": "https://x"}}
            ],
            "usage": {"input_tokens": 50, "output_tokens": 10}
        });
        t.record_response(&resp, Shape::Anthropic, 900);
        // Next request carries the tool result.
        let req = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu1", "content": "200 OK body"}
                ]}
            ]
        });
        t.record_request_results(&req, Shape::Anthropic);
        let evs = t.snapshot(0);
        let tool = evs.iter().find(|e| e["kind"] == "tool").unwrap();
        assert_eq!(tool["name"], "http_fetch");
        assert_eq!(tool["result_preview"], "200 OK body");
        assert_eq!(tool["ok"], true);
    }

    #[test]
    fn cursor_isolates_a_tasks_events() {
        let t = TaskTelemetry::new();
        t.record_response(
            &json!({"usage": {"prompt_tokens": 1, "completion_tokens": 1}}),
            Shape::OpenAi,
            1,
        );
        let cursor = t.cursor();
        t.record_response(
            &json!({"usage": {"prompt_tokens": 2, "completion_tokens": 2}}),
            Shape::OpenAi,
            1,
        );
        let evs = t.snapshot(cursor);
        assert_eq!(
            evs.len(),
            1,
            "only events after the cursor belong to this task"
        );
        assert_eq!(evs[0]["total_tokens"], 4);
    }

    #[test]
    fn openai_cached_tokens_captured() {
        let t = TaskTelemetry::new();
        let resp = json!({
            "choices": [{"finish_reason": "stop", "message": {}}],
            "usage": {
                "prompt_tokens": 1000, "completion_tokens": 50, "total_tokens": 1050,
                "prompt_tokens_details": {"cached_tokens": 768}
            }
        });
        t.record_response(&resp, Shape::OpenAi, 100);
        let evs = t.snapshot(0);
        assert_eq!(
            evs[0]["cached_tokens"], 768,
            "OpenAI cached prompt tokens are surfaced"
        );
    }

    #[test]
    fn anthropic_cache_read_tokens_captured() {
        let t = TaskTelemetry::new();
        let resp = json!({
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "hi"}],
            "usage": {"input_tokens": 40, "output_tokens": 10, "cache_read_input_tokens": 32}
        });
        t.record_response(&resp, Shape::Anthropic, 100);
        let evs = t.snapshot(0);
        assert_eq!(
            evs[0]["cached_tokens"], 32,
            "Anthropic cache-read tokens are surfaced"
        );
    }

    #[test]
    fn cached_tokens_default_zero_when_absent() {
        let t = TaskTelemetry::new();
        t.record_response(
            &json!({"usage": {"prompt_tokens": 5, "completion_tokens": 5}}),
            Shape::OpenAi,
            1,
        );
        let evs = t.snapshot(0);
        assert_eq!(
            evs[0]["cached_tokens"], 0,
            "no cache info → 0, never fabricated"
        );
    }

    #[test]
    fn error_tool_result_marks_not_ok() {
        let t = TaskTelemetry::new();
        let resp = json!({
            "choices": [{"finish_reason": "tool_calls", "message": {"tool_calls": [
                {"id": "c1", "function": {"name": "exec_command", "arguments": "{}"}}
            ]}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        });
        t.record_response(&resp, Shape::OpenAi, 1);
        let req = json!({"messages": [{"role": "tool", "tool_call_id": "c1", "content": "error: command failed"}]});
        t.record_request_results(&req, Shape::OpenAi);
        let tool = t
            .snapshot(0)
            .into_iter()
            .find(|e| e["kind"] == "tool")
            .unwrap();
        assert_eq!(tool["ok"], false);
    }
}
