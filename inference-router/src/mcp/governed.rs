// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::tools::{AsyncToolDispatcher, DispatchError, ToolCallOutput, ToolCatalog};
use crate::{access_request::Request, governance::Governance, governed_services::GovernedServices};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Constructed per request after the route authenticates its socket/OAuth
/// caller. Configuration identity alone is not caller authentication.
/// A capability request records denial, never a grant.
pub struct GovernedDispatcher {
    inner: Arc<dyn AsyncToolDispatcher>,
    governance: Arc<Governance>,
    services: Arc<GovernedServices>,
    principal: String,
}

impl GovernedDispatcher {
    pub fn new(
        inner: Arc<dyn AsyncToolDispatcher>,
        governance: Arc<Governance>,
        services: Arc<GovernedServices>,
        principal: String,
    ) -> Self {
        Self {
            inner,
            governance,
            services,
            principal,
        }
    }
}

#[async_trait]
impl AsyncToolDispatcher for GovernedDispatcher {
    fn catalog(&self) -> &ToolCatalog {
        self.inner.catalog()
    }

    async fn invoke(&self, name: &str, arguments: &Value) -> Result<ToolCallOutput, DispatchError> {
        if !self.services.identity_valid {
            return Err(DispatchError::ExecutionFailed {
                tool: name.into(),
                reason: "MCP caller identity is unavailable".into(),
            });
        }
        let scope = self.services.telemetry.cursor().0;
        let action = format!("tool:{name}");
        let allowed = self.governance.check_tool_rate(name).0
            && self.governance.evaluate(
                &self.principal,
                &action,
                Some(&json!({"mcp":true,"tool_name":name,"arguments":arguments})),
            )["allowed"]
                .as_bool()
                == Some(true);
        self.services
            .telemetry
            .record_policy(&scope, &action, allowed);
        if !allowed {
            let _ = self.services.requests.record(Request {
                scope_id: scope,
                kind: "tool".into(),
                target: name.into(),
                reason: "Governance denied the requested MCP tool".into(),
                tier: None,
                port: None,
            });
            return Err(DispatchError::ExecutionFailed {
                tool: name.into(),
                reason: "MCP tool is not allowed by the current governance policy".into(),
            });
        }
        self.inner.invoke(name, arguments).await
    }
}
