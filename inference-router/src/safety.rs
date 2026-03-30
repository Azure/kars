//! Azure AI Content Safety + Prompt Shields integration.
//!
//! On by default — filters inference requests through Content Safety before
//! forwarding to Azure OpenAI. Uses Managed Identity for auth (same as
//! inference routing — no API keys).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::auth::WorkloadIdentityAuth;

// Re-use a module-level auth instance for safety calls
static AUTH: std::sync::LazyLock<WorkloadIdentityAuth> =
    std::sync::LazyLock::new(WorkloadIdentityAuth::new);

/// Shared HTTP client for safety checks — reuses TCP connections and TLS sessions
/// across calls instead of creating a new client per request.
static SAFETY_CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .pool_max_idle_per_host(4)
        .timeout(std::time::Duration::from_millis(
            std::env::var("CONTENT_SAFETY_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1500),
        ))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// Determine audience for safety endpoints: Foundry project endpoints use ai.azure.com,
/// standalone Content Safety resources use cognitiveservices.azure.com.
fn safety_audience(endpoint: &str) -> &'static str {
    if endpoint.contains("services.ai.azure.com") {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    }
}

// ─── Circuit Breaker (#5) ────────────────────────────────────────────────────
// After CIRCUIT_BREAKER_THRESHOLD consecutive Content Safety API failures,
// switch to fail-closed (block all requests) until a probe succeeds.
// This prevents a downed safety service from silently allowing all content.

use std::sync::atomic::{AtomicU32, Ordering};

/// Consecutive failure count for Content Safety API.
static SAFETY_FAILURES: AtomicU32 = AtomicU32::new(0);
/// Circuit breaker state: 0 = closed (normal), 1 = open (fail-closed).
static SAFETY_CIRCUIT_OPEN: AtomicU32 = AtomicU32::new(0);
/// Number of consecutive failures before the circuit opens.
const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;

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
    // Circuit breaker: if open, fail-closed until service recovers
    if SAFETY_CIRCUIT_OPEN.load(Ordering::Relaxed) == 1 {
        bail!("Content Safety circuit breaker OPEN — blocking request until service recovers");
    }

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

    let resp = match SAFETY_CLIENT
        .post(&url)
        .bearer_auth(&token)
        .json(&request_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let n = SAFETY_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= CIRCUIT_BREAKER_THRESHOLD {
                SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
                tracing::error!(
                    failures = n,
                    "Content Safety circuit breaker OPENED — failing closed"
                );
                bail!("Content Safety unreachable ({n} consecutive failures) — circuit breaker open");
            }
            tracing::warn!(error = %e, failures = n, "Content Safety API unreachable, failing open ({n}/{CIRCUIT_BREAKER_THRESHOLD})");
            return Ok(());
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let n = SAFETY_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= CIRCUIT_BREAKER_THRESHOLD {
            SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
            tracing::error!(
                failures = n,
                "Content Safety circuit breaker OPENED — failing closed"
            );
            bail!("Content Safety error {status} ({n} consecutive failures) — circuit breaker open");
        }
        tracing::warn!(%status, failures = n, "Content Safety API error ({n}/{CIRCUIT_BREAKER_THRESHOLD}): {body}");
        return Ok(());
    }

    // Success — reset circuit breaker
    SAFETY_FAILURES.store(0, Ordering::Relaxed);
    if SAFETY_CIRCUIT_OPEN.swap(0, Ordering::Relaxed) == 1 {
        tracing::info!("Content Safety circuit breaker CLOSED — service recovered");
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

    let resp = match SAFETY_CLIENT
        .post(&url)
        .bearer_auth(&token)
        .json(&request_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let n = SAFETY_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= CIRCUIT_BREAKER_THRESHOLD {
                SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
                tracing::error!(failures = n, "Content Safety circuit breaker OPENED (via Prompt Shields)");
                bail!("Prompt Shields unreachable ({n} consecutive failures) — circuit breaker open");
            }
            tracing::warn!(error = %e, failures = n, "Prompt Shields API unreachable, failing open ({n}/{CIRCUIT_BREAKER_THRESHOLD})");
            return Ok(());
        }
    };

    if !resp.status().is_success() {
        let n = SAFETY_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= CIRCUIT_BREAKER_THRESHOLD {
            SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
            tracing::error!(failures = n, "Content Safety circuit breaker OPENED (via Prompt Shields)");
            bail!("Prompt Shields error ({n} consecutive failures) — circuit breaker open");
        }
        return Ok(());
    }

    // Success — reset circuit breaker
    SAFETY_FAILURES.store(0, Ordering::Relaxed);
    if SAFETY_CIRCUIT_OPEN.swap(0, Ordering::Relaxed) == 1 {
        tracing::info!("Content Safety circuit breaker CLOSED — service recovered");
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
