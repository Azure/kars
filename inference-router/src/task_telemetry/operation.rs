// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TOOL_TTL, TaskTelemetry};
use serde_json::{Value, json};

/// Router-issued proposal identity, not an attestation of runtime execution.
pub struct CorrelatedTool {
    scope: String,
    round: u64,
    call_id: String,
    name: String,
}

pub enum RouterOperation {
    ToolPolicy {
        allowed: bool,
        rate_limited: bool,
    },
    EgressPolicy {
        allowed: bool,
        flag: &'static str,
    },
    Fetch {
        status: Option<u16>,
        complete: bool,
        flag: &'static str,
    },
}

impl TaskTelemetry {
    pub fn correlate_operation(
        &self,
        scope: &str,
        id: &str,
        name: Option<&str>,
    ) -> Option<CorrelatedTool> {
        let inner = self.inner.lock().ok()?;
        if inner.scope != scope {
            return None;
        }
        let pending = inner.model_tools.get(id)?;
        if pending.at.elapsed() >= TOOL_TTL || name.is_some_and(|name| name != pending.name) {
            return None;
        }
        Some(CorrelatedTool {
            scope: scope.into(),
            round: pending.round,
            call_id: id.into(),
            name: pending.name.clone(),
        })
    }

    pub fn correlate_fetch(&self, request: &Value) -> Option<CorrelatedTool> {
        let correlation = request.get("telemetry")?;
        let tool = self.correlate_operation(
            correlation.get("scope_id")?.as_str()?,
            correlation.get("call_id")?.as_str()?,
            None,
        )?;
        if !matches!(
            tool.name.as_str(),
            "web_fetch" | "http_fetch" | "kars_mission_http_fetch"
        ) {
            return None;
        }
        let inner = self.inner.lock().ok()?;
        let authorized = inner.harness_tools.get(&tool.call_id)?;
        (inner.scope == tool.scope
            && authorized.round == tool.round
            && authorized.name == tool.name
            && authorized.at.elapsed() < TOOL_TTL)
            .then_some(tool)
    }

    pub fn record_operation(
        &self,
        tool: Option<&CorrelatedTool>,
        operation: RouterOperation,
        url: Option<&str>,
        ms: u64,
    ) {
        let Some(tool) = tool else {
            return;
        };
        let (operation, source, flag, allowed, status, complete) = match operation {
            RouterOperation::ToolPolicy {
                allowed,
                rate_limited,
            } => (
                "tool_policy",
                "router-policy",
                if rate_limited {
                    "rate_limit"
                } else {
                    "governance"
                },
                Some(allowed),
                None,
                None,
            ),
            RouterOperation::EgressPolicy { allowed, flag } => (
                "egress_policy",
                "router-policy",
                flag,
                Some(allowed),
                None,
                None,
            ),
            RouterOperation::Fetch {
                status,
                complete,
                flag,
            } => ("egress_fetch", "router", flag, None, status, Some(complete)),
        };
        let destination = url
            .and_then(|url| reqwest::Url::parse(url).ok())
            .and_then(|url| {
                let host = url.host_str()?;
                let port = url.port_or_known_default()?;
                (host.len() <= 253
                    && host
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b".-:[]".contains(&c)))
                .then(|| (host.to_ascii_lowercase(), port))
            });
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.scope != tool.scope {
            return;
        }
        Self::push(
            &mut inner,
            json!({"kind":"router_operation", "scope_id":tool.scope,
            "round":tool.round, "call_id":tool.call_id, "name":tool.name, "source":source,
            "operation":operation, "flag":flag, "allowed":allowed, "http_status":status, "complete":complete,
            "destination_host":destination.as_ref().map(|(host, _)| host),
            "destination_port":destination.map(|(_, port)| port), "ms":ms.min(3_600_000)}),
        );
    }
}
