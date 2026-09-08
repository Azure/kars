// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded router observations, not a durable execution or budget ledger.
//! No prompts, tool arguments/results, URLs, headers or credentials are retained.

use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub mod mcp;
pub mod observe;
pub mod parse;

const MAX_EVENTS: usize = 1024;
const MAX_PENDING: usize = 256;
const TOOL_TTL: Duration = Duration::from_secs(20 * 60);

struct Pending {
    name: String,
    at: Instant,
    round: u64,
}
struct Inner {
    scope: String,
    seq: u64,
    round: u64,
    dropped: u64,
    events: VecDeque<Value>,
    model_tools: HashMap<String, Pending>,
    seen_model: HashSet<String>,
    harness_tools: HashMap<String, Pending>,
    seen_harness: HashSet<String>,
}

pub struct TaskTelemetry {
    inner: Mutex<Inner>,
}

impl TaskTelemetry {
    pub fn new(scope: String) -> Self {
        Self {
            inner: Mutex::new(Inner {
                scope,
                seq: 0,
                round: 0,
                dropped: 0,
                events: VecDeque::new(),
                model_tools: HashMap::new(),
                seen_model: HashSet::new(),
                harness_tools: HashMap::new(),
                seen_harness: HashSet::new(),
            }),
        }
    }
    pub fn reset(&self, scope: String) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.scope = scope;
        inner.events.clear();
        inner.model_tools.clear();
        inner.seen_model.clear();
        inner.harness_tools.clear();
        inner.seen_harness.clear();
        inner.round = 0;
        inner.dropped = 0;
    }
    fn push(inner: &mut Inner, mut event: Value) {
        inner.seq = inner.seq.saturating_add(1);
        event["seq"] = json!(inner.seq);
        event["scope_id"] = json!(inner.scope);
        inner.events.push_back(event);
        if inner.events.len() > MAX_EVENTS {
            inner.events.pop_front();
            inner.dropped = inner.dropped.saturating_add(1);
        }
    }
    pub fn cursor(&self) -> (String, u64) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.scope.clone(), inner.seq)
    }
    pub fn snapshot(&self, scope: &str, since: u64) -> Option<Value> {
        let inner = self.inner.lock().ok()?;
        if inner.scope != scope {
            return None;
        }
        let events = inner
            .events
            .iter()
            .filter(|event| event["seq"].as_u64().is_some_and(|seq| seq > since))
            .take(256)
            .cloned()
            .collect::<Vec<_>>();
        let cursor = events
            .last()
            .and_then(|event| event["seq"].as_u64())
            .unwrap_or(inner.seq);
        Some(json!({
            "scope_id": scope, "cursor": cursor, "high_water": inner.seq,
            "has_more": cursor<inner.seq, "dropped_events": inner.dropped,
            "coverage": "router-observed-only", "durable": false,
            "events": events,
        }))
    }
    pub fn begin(
        self: &Arc<Self>,
        path: &str,
        provider: &str,
        model: &str,
        body: &[u8],
    ) -> Option<observe::Observation> {
        let shape = parse::Shape::for_path(path)?;
        let mut inner = self.inner.lock().ok()?;
        inner
            .model_tools
            .retain(|_, pending| pending.at.elapsed() < TOOL_TTL);
        for (id, ok) in parse::request_results(body, shape) {
            if let Some(pending) = inner.model_tools.remove(&id) {
                Self::push(
                    &mut inner,
                    json!({"kind":"tool_result", "call_id":id, "name":pending.name,
                    "round":pending.round, "ok":ok, "source":"harness-reported"}),
                );
            }
        }
        inner.round = inner.round.saturating_add(1);
        Some(observe::Observation::new(
            self.clone(),
            inner.scope.clone(),
            inner.round,
            shape,
            parse::identifier(provider, 64),
            parse::identifier(model, 253),
        ))
    }
    pub(crate) fn finish(
        &self,
        scope: &str,
        round: u64,
        event: Value,
        tools: Vec<(String, String)>,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.scope != scope {
            return;
        }
        Self::push(&mut inner, event);
        for (id, name) in tools {
            Self::push(
                &mut inner,
                json!({"kind":"tool_proposed", "round":round, "call_id":id,
                "name":name, "ok":null, "source":"model-proposed"}),
            );
            if !id.is_empty()
                && inner.seen_model.len() < MAX_PENDING
                && inner.seen_model.insert(id.clone())
            {
                inner.model_tools.insert(
                    id,
                    Pending {
                        name,
                        at: Instant::now(),
                        round,
                    },
                );
            } else {
                inner.model_tools.remove(&id);
                inner.dropped = inner.dropped.saturating_add(1);
            }
        }
    }
    pub fn record_policy(&self, scope: &str, capability: &str, allowed: bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.scope != scope {
            return;
        }
        Self::push(
            &mut inner,
            json!({"kind":"policy", "capability":parse::identifier(capability,128),
            "allowed":allowed, "source":"router-policy"}),
        );
    }
    pub fn record_gap(&self, scope: &str, source: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if inner.scope != scope {
            return;
        }
        inner.dropped = inner.dropped.saturating_add(1);
        Self::push(
            &mut inner,
            json!({"kind":"observation_gap","source":parse::identifier(source,128)}),
        );
    }
    pub fn record_router_tool(
        &self,
        scope: &str,
        name: &str,
        ok: Option<bool>,
        status: Option<u16>,
        latency_ms: u64,
        complete: bool,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.scope != scope {
            return;
        }
        Self::push(
            &mut inner,
            json!({"kind":"tool_result", "name":parse::identifier(name,128),
            "ok":ok, "http_status":status, "ms":latency_ms, "complete":complete, "source":"router"}),
        );
    }
    pub fn authorize_harness_tool(&self, scope: &str, id: &str, name: &str) -> bool {
        if !crate::access_request::scope::identifier(id, 128)
            || parse::identifier(name, 128).is_none()
        {
            return false;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.scope != scope {
            return false;
        }
        if inner.seen_harness.contains(id) || inner.seen_harness.len() >= MAX_PENDING {
            inner.dropped = inner.dropped.saturating_add(1);
            return false;
        }
        inner.seen_harness.insert(id.to_string());
        let round = inner.round;
        inner.harness_tools.insert(
            id.to_string(),
            Pending {
                name: name.to_string(),
                at: Instant::now(),
                round,
            },
        );
        true
    }
    pub fn complete_harness_tool(&self, scope: &str, id: &str, ok: bool, latency_ms: u64) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.scope != scope {
            return false;
        }
        let Some(pending) = inner.harness_tools.remove(id) else {
            return false;
        };
        if pending.at.elapsed() >= TOOL_TTL {
            return false;
        }
        Self::push(
            &mut inner,
            json!({"kind":"tool_result", "call_id":id, "name":pending.name,
            "round":pending.round, "ok":ok, "ms":latency_ms.min(3_600_000), "source":"harness-reported"}),
        );
        true
    }
}
