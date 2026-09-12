// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "../../../shared/mcp_content.rs"]
mod wire;
pub(super) use wire::present;
pub use wire::*;

impl ToolContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            attributes: ContentAttributes::default(),
        }
    }
}

#[cfg(test)]
#[path = "content/tests.rs"]
mod tests;
