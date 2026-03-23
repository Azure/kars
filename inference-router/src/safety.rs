//! Azure AI Content Safety + Prompt Shields integration.
//!
//! On by default — filters inference requests through Content Safety before
//! forwarding to Azure OpenAI. Uses Managed Identity for auth (same as
//! inference routing — no API keys).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::auth::WorkloadIdentityAuth;

// Re-use a module-level auth instance for safety calls
static AUTH: std::sync::LazyLock<WorkloadIdentityAuth> =
    std::sync::LazyLock::new(WorkloadIdentityAuth::new);

/// Determine audience for safety endpoints: Foundry project endpoints use ai.azure.com,
/// standalone Content Safety resources use cognitiveservices.azure.com.
fn safety_audience(endpoint: &str) -> &'static str {
    if endpoint.contains("services.ai.azure.com") {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    }
}

// ─── Content Safety ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct AnalyzeTextRequest {
    text: String,
    categories: Vec<String>,
    #[serde(rename = "outputType")]
    output_type: String,
}

#[derive(Debug, Deserialize)]
struct AnalyzeTextResponse {
    #[serde(rename = "categoriesAnalysis")]
    categories_analysis: Vec<CategoryAnalysis>,
}

#[derive(Debug, Deserialize)]
struct CategoryAnalysis {
    category: String,
    severity: u8,
}

/// Check input text against Azure AI Content Safety.
/// Returns Ok(()) if safe, Err if content is blocked.
pub async fn check_content_safety(endpoint: &str, text: &str) -> Result<()> {
    let token = AUTH
        .get_token(safety_audience(endpoint))
        .await
        .context("Failed to get token for Content Safety")?;

    let api_version = if endpoint.contains("services.ai.azure.com") {
        "2025-04-01-preview"
    } else {
        "2024-09-01"
    };
    let url = format!(
        "{}/contentsafety/text:analyze?api-version={api_version}",
        endpoint.trim_end_matches('/')
    );

    let request_body = AnalyzeTextRequest {
        text: text.to_string(),
        categories: vec![
            "Hate".into(),
            "SelfHarm".into(),
            "Sexual".into(),
            "Violence".into(),
        ],
        output_type: "FourSeverityLevels".into(),
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&request_body)
        .send()
        .await
        .context("Content Safety API request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!("Content Safety API error: {status} — {body}");
        // Fail open on API errors (don't block inference if safety service is down)
        return Ok(());
    }

    let result: AnalyzeTextResponse = resp
        .json()
        .await
        .context("Failed to parse Content Safety response")?;

    // Block if any category severity >= 4 (configurable threshold)
    let block_threshold = 4;
    for category in &result.categories_analysis {
        if category.severity >= block_threshold {
            bail!(
                "Content blocked: category={}, severity={}",
                category.category,
                category.severity
            );
        }
    }

    Ok(())
}

// ─── Prompt Shields ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct PromptShieldRequest {
    #[serde(rename = "userPrompt")]
    user_prompt: String,
    documents: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PromptShieldResponse {
    #[serde(rename = "userPromptAnalysis")]
    user_prompt_analysis: Option<PromptAnalysis>,
}

#[derive(Debug, Deserialize)]
struct PromptAnalysis {
    #[serde(rename = "attackDetected")]
    attack_detected: bool,
}

/// Check for prompt injection / jailbreak attempts via Prompt Shields.
pub async fn check_prompt_shields(endpoint: &str, prompt: &str) -> Result<()> {
    let token = AUTH
        .get_token(safety_audience(endpoint))
        .await
        .context("Failed to get token for Prompt Shields")?;

    let api_version = if endpoint.contains("services.ai.azure.com") {
        "2025-04-01-preview"
    } else {
        "2024-09-01"
    };
    let url = format!(
        "{}/contentsafety/text:shieldPrompt?api-version={api_version}",
        endpoint.trim_end_matches('/')
    );

    let request_body = PromptShieldRequest {
        user_prompt: prompt.to_string(),
        documents: vec![],
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&request_body)
        .send()
        .await
        .context("Prompt Shields API request failed")?;

    if !resp.status().is_success() {
        // Fail open on API errors
        return Ok(());
    }

    let result: PromptShieldResponse = resp
        .json()
        .await
        .context("Failed to parse Prompt Shields response")?;

    if let Some(analysis) = result.user_prompt_analysis
        && analysis.attack_detected
    {
        bail!("Prompt injection attack detected");
    }

    Ok(())
}
