//! Azure AI Content Safety + Prompt Shields integration.
//! On by default — filters inference requests through Content Safety before forwarding.

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ContentSafetyRequest {
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct ContentSafetyResult {
    pub categories: Vec<CategoryResult>,
}

#[derive(Debug, Deserialize)]
pub struct CategoryResult {
    pub category: String,
    pub severity: u8,
}

/// Check input text against Azure AI Content Safety.
/// Returns Ok(()) if safe, Err with details if blocked.
pub async fn check_content_safety(
    _endpoint: &str,
    _text: &str,
) -> Result<()> {
    // TODO: Call Azure AI Content Safety REST API with Managed Identity token
    // POST {endpoint}/contentsafety/text:analyze?api-version=2024-09-01
    Ok(())
}

/// Check for prompt injection / jailbreak attempts via Prompt Shields.
pub async fn check_prompt_shields(
    _endpoint: &str,
    _prompt: &str,
) -> Result<()> {
    // TODO: Call Azure AI Prompt Shields REST API
    Ok(())
}
