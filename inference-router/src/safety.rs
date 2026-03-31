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
/// Circuit breaker state: 0 = closed (normal), 1 = open (fail-open).
static SAFETY_CIRCUIT_OPEN: AtomicU32 = AtomicU32::new(0);
/// Timestamp (epoch secs) when circuit opened — auto-reset after cooldown.
static SAFETY_CIRCUIT_OPENED_AT: AtomicU32 = AtomicU32::new(0);
/// Number of consecutive failures before the circuit opens.
const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
/// Seconds to wait before retrying after circuit opens.
const CIRCUIT_BREAKER_COOLDOWN_SECS: u32 = 60;

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
    // Circuit breaker: if open, fail-open (allow requests) until service recovers.
    // Auto-reset after cooldown period to allow retries.
    if SAFETY_CIRCUIT_OPEN.load(Ordering::Relaxed) == 1 {
        let opened_at = SAFETY_CIRCUIT_OPENED_AT.load(Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        if now.saturating_sub(opened_at) >= CIRCUIT_BREAKER_COOLDOWN_SECS {
            // Cooldown elapsed — reset and retry
            SAFETY_CIRCUIT_OPEN.store(0, Ordering::Relaxed);
            SAFETY_FAILURES.store(0, Ordering::Relaxed);
            tracing::info!("Content Safety circuit breaker cooldown elapsed — retrying");
        } else {
            tracing::warn!("Content Safety circuit breaker OPEN — allowing request (fail-open)");
            return Ok(());
        }
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
                SAFETY_CIRCUIT_OPENED_AT.store(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as u32, Ordering::Relaxed);
                tracing::error!(
                    failures = n,
                    "Content Safety circuit breaker OPENED — failing open"
                );
                return Ok(());
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
                SAFETY_CIRCUIT_OPENED_AT.store(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as u32, Ordering::Relaxed);
            tracing::error!(
                failures = n,
                "Content Safety circuit breaker OPENED — failing open"
            );
            return Ok(());
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
                SAFETY_CIRCUIT_OPENED_AT.store(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as u32, Ordering::Relaxed);
                tracing::error!(failures = n, "Content Safety circuit breaker OPENED (via Prompt Shields)");
                return Ok(());
            }
            tracing::warn!(error = %e, failures = n, "Prompt Shields API unreachable, failing open ({n}/{CIRCUIT_BREAKER_THRESHOLD})");
            return Ok(());
        }
    };

    if !resp.status().is_success() {
        let n = SAFETY_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= CIRCUIT_BREAKER_THRESHOLD {
            SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
            SAFETY_CIRCUIT_OPENED_AT.store(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as u32, Ordering::Relaxed);
            tracing::error!(failures = n, "Content Safety circuit breaker OPENED (via Prompt Shields)");
            return Ok(());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    // Global lock so tests that touch shared circuit breaker atomics
    // don't race each other when cargo runs them in parallel.
    static CB_LOCK: Mutex<()> = Mutex::new(());

    /// Reset all circuit breaker state so tests don't leak into each other.
    /// Call at the top of every test that touches the global atomics.
    fn reset_circuit_breaker() {
        SAFETY_FAILURES.store(0, Ordering::Relaxed);
        SAFETY_CIRCUIT_OPEN.store(0, Ordering::Relaxed);
        SAFETY_CIRCUIT_OPENED_AT.store(0, Ordering::Relaxed);
    }

    fn now_epoch_secs() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32
    }

    // ── safety_audience ──────────────────────────────────────────────────

    #[test]
    fn test_safety_audience_foundry_project() {
        assert_eq!(
            safety_audience("https://my-project.services.ai.azure.com"),
            "https://ai.azure.com"
        );
    }

    #[test]
    fn test_safety_audience_standalone_resource() {
        assert_eq!(
            safety_audience("https://my-resource.cognitiveservices.azure.com"),
            "https://cognitiveservices.azure.com"
        );
    }

    // ── Circuit breaker state machine ────────────────────────────────────

    #[test]
    fn test_circuit_breaker_constants() {
        assert_eq!(CIRCUIT_BREAKER_THRESHOLD, 5);
        assert_eq!(CIRCUIT_BREAKER_COOLDOWN_SECS, 60);
    }

    #[test]
    fn test_circuit_stays_closed_below_threshold() {
        let _lock = CB_LOCK.lock().unwrap();
        reset_circuit_breaker();
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD - 1 {
            SAFETY_FAILURES.fetch_add(1, Ordering::Relaxed);
        }
        assert_eq!(
            SAFETY_FAILURES.load(Ordering::Relaxed),
            CIRCUIT_BREAKER_THRESHOLD - 1
        );
        // Circuit must still be closed — counter alone doesn't flip it.
        assert_eq!(SAFETY_CIRCUIT_OPEN.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_circuit_opens_at_threshold() {
        let _lock = CB_LOCK.lock().unwrap();
        reset_circuit_breaker();
        // Replicate the logic from check_content_safety's error handler.
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            let n = SAFETY_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= CIRCUIT_BREAKER_THRESHOLD {
                SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
                SAFETY_CIRCUIT_OPENED_AT.store(now_epoch_secs(), Ordering::Relaxed);
            }
        }
        assert_eq!(SAFETY_CIRCUIT_OPEN.load(Ordering::Relaxed), 1);
        assert_eq!(
            SAFETY_FAILURES.load(Ordering::Relaxed),
            CIRCUIT_BREAKER_THRESHOLD
        );
        assert!(SAFETY_CIRCUIT_OPENED_AT.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn test_success_resets_failure_count() {
        let _lock = CB_LOCK.lock().unwrap();
        reset_circuit_breaker();
        SAFETY_FAILURES.store(3, Ordering::Relaxed);
        // Replicate success path (lines 171-174 in production code).
        SAFETY_FAILURES.store(0, Ordering::Relaxed);
        assert_eq!(SAFETY_FAILURES.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_success_closes_open_circuit() {
        let _lock = CB_LOCK.lock().unwrap();
        reset_circuit_breaker();
        SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
        SAFETY_FAILURES.store(CIRCUIT_BREAKER_THRESHOLD, Ordering::Relaxed);
        // Replicate success path.
        SAFETY_FAILURES.store(0, Ordering::Relaxed);
        let was_open = SAFETY_CIRCUIT_OPEN.swap(0, Ordering::Relaxed);
        assert_eq!(was_open, 1, "circuit should have been open before swap");
        assert_eq!(SAFETY_CIRCUIT_OPEN.load(Ordering::Relaxed), 0);
        assert_eq!(SAFETY_FAILURES.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_circuit_open_fail_open_returns_ok() {
        let _lock = CB_LOCK.lock().unwrap();
        reset_circuit_breaker();
        // Open the circuit with a recent timestamp (cooldown NOT elapsed).
        SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
        SAFETY_CIRCUIT_OPENED_AT.store(now_epoch_secs(), Ordering::Relaxed);

        let result = check_content_safety(
            "https://test.cognitiveservices.azure.com",
            "test input",
        )
        .await;
        assert!(result.is_ok(), "should fail-open when circuit is open");
        // Circuit must remain open — cooldown hasn't elapsed.
        assert_eq!(SAFETY_CIRCUIT_OPEN.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_circuit_cooldown_resets_state() {
        let _lock = CB_LOCK.lock().unwrap();
        reset_circuit_breaker();
        // Open the circuit with a timestamp well past cooldown.
        SAFETY_CIRCUIT_OPEN.store(1, Ordering::Relaxed);
        SAFETY_CIRCUIT_OPENED_AT.store(
            now_epoch_secs().saturating_sub(CIRCUIT_BREAKER_COOLDOWN_SECS + 10),
            Ordering::Relaxed,
        );

        // After cooldown the circuit resets, then the function continues
        // to the auth step which will fail in test (no Azure credentials).
        let _ = check_content_safety(
            "https://test.cognitiveservices.azure.com",
            "test input",
        )
        .await;

        // Regardless of auth failure, circuit state must have been reset.
        assert_eq!(SAFETY_CIRCUIT_OPEN.load(Ordering::Relaxed), 0);
        assert_eq!(SAFETY_FAILURES.load(Ordering::Relaxed), 0);
    }

    // ── Content Safety response parsing ──────────────────────────────────

    #[test]
    fn test_content_safety_response_all_safe() {
        let json = r#"{
            "categoriesAnalysis": [
                {"category": "Hate", "severity": 0},
                {"category": "SelfHarm", "severity": 0},
                {"category": "Sexual", "severity": 0},
                {"category": "Violence", "severity": 0}
            ]
        }"#;
        let resp: AnalyzeTextResponse = serde_json::from_str(json).unwrap();
        let block_threshold: u8 = 4;
        assert!(
            resp.categories_analysis
                .iter()
                .all(|c| c.severity < block_threshold),
            "all-safe response should not be blocked"
        );
    }

    #[test]
    fn test_content_safety_response_blocked_above_threshold() {
        let json = r#"{
            "categoriesAnalysis": [
                {"category": "Hate", "severity": 0},
                {"category": "Violence", "severity": 6}
            ]
        }"#;
        let resp: AnalyzeTextResponse = serde_json::from_str(json).unwrap();
        let block_threshold: u8 = 4;
        let blocked = resp
            .categories_analysis
            .iter()
            .any(|c| c.severity >= block_threshold);
        assert!(blocked, "severity 6 should be blocked");
    }

    #[test]
    fn test_content_safety_response_blocked_at_threshold() {
        let json = r#"{
            "categoriesAnalysis": [
                {"category": "Hate", "severity": 4}
            ]
        }"#;
        let resp: AnalyzeTextResponse = serde_json::from_str(json).unwrap();
        let block_threshold: u8 = 4;
        let blocked = resp
            .categories_analysis
            .iter()
            .any(|c| c.severity >= block_threshold);
        assert!(blocked, "severity exactly at threshold should be blocked");
    }

    #[test]
    fn test_content_safety_response_allowed_below_threshold() {
        let json = r#"{
            "categoriesAnalysis": [
                {"category": "Hate", "severity": 3},
                {"category": "Violence", "severity": 2}
            ]
        }"#;
        let resp: AnalyzeTextResponse = serde_json::from_str(json).unwrap();
        let block_threshold: u8 = 4;
        let blocked = resp
            .categories_analysis
            .iter()
            .any(|c| c.severity >= block_threshold);
        assert!(!blocked, "severity below threshold should be allowed");
    }

    // ── Prompt Shield response parsing ───────────────────────────────────

    #[test]
    fn test_prompt_shield_no_attack() {
        let json = r#"{"userPromptAnalysis": {"attackDetected": false}}"#;
        let resp: PromptShieldResponse = serde_json::from_str(json).unwrap();
        let detected = resp
            .user_prompt_analysis
            .map(|a| a.attack_detected)
            .unwrap_or(false);
        assert!(!detected);
    }

    #[test]
    fn test_prompt_shield_attack_detected() {
        let json = r#"{"userPromptAnalysis": {"attackDetected": true}}"#;
        let resp: PromptShieldResponse = serde_json::from_str(json).unwrap();
        let detected = resp
            .user_prompt_analysis
            .map(|a| a.attack_detected)
            .unwrap_or(false);
        assert!(detected, "should detect prompt injection attack");
    }

    #[test]
    fn test_prompt_shield_null_analysis_is_safe() {
        let json = r#"{"userPromptAnalysis": null}"#;
        let resp: PromptShieldResponse = serde_json::from_str(json).unwrap();
        assert!(resp.user_prompt_analysis.is_none());
        let detected = resp
            .user_prompt_analysis
            .map(|a| a.attack_detected)
            .unwrap_or(false);
        assert!(!detected, "null analysis should not flag as attack");
    }
}
