// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::mcp::{
    forwarder::server_name_to_prefix,
    tools::{AsyncToolDispatcher, DispatchError, ToolCallOutput, ToolCatalog},
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

pub(super) struct ScopedDispatcher {
    inner: Arc<dyn AsyncToolDispatcher>,
    catalog: ToolCatalog,
    prefix: String,
}

impl ScopedDispatcher {
    pub(super) fn exclude(
        inner: Arc<dyn AsyncToolDispatcher>,
        excluded: &std::collections::BTreeSet<String>,
    ) -> Result<Self, &'static str> {
        let tools = inner
            .catalog()
            .tools()
            .iter()
            .filter(|tool| !excluded.iter().any(|prefix| tool.name.starts_with(prefix)))
            .cloned()
            .collect();
        let catalog = ToolCatalog::new(tools).map_err(|_| "Invalid remote MCP catalog")?;
        Ok(Self {
            inner,
            catalog,
            prefix: String::new(),
        })
    }
    pub(super) fn new(
        inner: Arc<dyn AsyncToolDispatcher>,
        server: &str,
    ) -> Result<Self, &'static str> {
        if server.is_empty()
            || server.len() > 253
            || !server.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-.".contains(&byte)
            })
        {
            return Err("Invalid MCP server scope");
        }
        let prefix = format!("{}.", server_name_to_prefix(server));
        let tools = inner
            .catalog()
            .tools()
            .iter()
            .filter(|tool| tool.name.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        if tools.is_empty() {
            return Err("Unknown or unqualified MCP server scope");
        }
        let catalog = ToolCatalog::new(tools).map_err(|_| "Invalid scoped MCP catalog")?;
        Ok(Self {
            inner,
            catalog,
            prefix,
        })
    }
}

#[async_trait]
impl AsyncToolDispatcher for ScopedDispatcher {
    fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    async fn invoke(&self, name: &str, arguments: &Value) -> Result<ToolCallOutput, DispatchError> {
        if !name.starts_with(&self.prefix)
            || !self.catalog.tools().iter().any(|tool| tool.name == name)
        {
            return Err(DispatchError::UnknownTool(name.into()));
        }
        self.inner.invoke(name, arguments).await
    }
}
