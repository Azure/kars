// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! OpenAI Moderation backend, the ordered guardrail pipeline, and the
//! request/response text-extraction helpers.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;

use crate::config::Config;
use crate::metrics;

use super::{
    ApplyTo, Direction, Guardrail, GuardrailError, GuardrailStageCfg, GuardrailVerdict,
    GuardrailViolation, MAX_SCAN_CHARS,
};

// ─── OpenAI Moderation backend ───────────────────────────────────────────────

/// OpenAI Moderation API backend (`POST {endpoint}/v1/moderations`).
pub struct OpenAiModeration {
    client: Client,
    endpoint: String,
    api_key: String,
    model: String,
}

impl OpenAiModeration {
    #[must_use]
    pub fn new(client: Client, endpoint: String, api_key: String, model: String) -> Self {
        Self {
            client,
            endpoint,
            api_key,
            model,
        }
    }
}

/// Parse a Moderation API response body into a verdict. Pure — unit
/// tested without I/O. Missing/malformed `results` is an error, not a
/// pass: an unparseable verdict must fail closed.
pub fn parse_moderation_response(body: &serde_json::Value) -> Result<GuardrailVerdict, String> {
    let result = body
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|r| r.first())
        .ok_or_else(|| "moderation response missing results[0]".to_string())?;
    let flagged = result
        .get("flagged")
        .and_then(|f| f.as_bool())
        .ok_or_else(|| "moderation response missing results[0].flagged".to_string())?;
    let categories = result
        .get("categories")
        .and_then(|c| c.as_object())
        .map(|c| {
            c.iter()
                .filter(|(_, v)| v.as_bool() == Some(true))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    Ok(GuardrailVerdict {
        flagged,
        categories,
    })
}

#[async_trait]
impl Guardrail for OpenAiModeration {
    fn name(&self) -> &'static str {
        "openai-moderation"
    }

    async fn scan(&self, text: &str) -> Result<GuardrailVerdict, GuardrailError> {
        let url = format!(
            "{}/v1/moderations",
            self.endpoint.trim_end_matches('/').trim_end_matches("/v1")
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({ "model": self.model, "input": text }))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| GuardrailError::Unavailable {
                provider: "openai-moderation",
                reason: format!("transport error: {e}"),
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let preview: String = body.chars().take(512).collect();
            return Err(GuardrailError::Unavailable {
                provider: "openai-moderation",
                reason: format!("upstream status {status}: {preview}"),
            });
        }
        let body: serde_json::Value =
            response
                .json()
                .await
                .map_err(|e| GuardrailError::Unavailable {
                    provider: "openai-moderation",
                    reason: format!("non-JSON response: {e}"),
                })?;
        parse_moderation_response(&body).map_err(|reason| GuardrailError::Unavailable {
            provider: "openai-moderation",
            reason,
        })
    }
}

// ─── Pipeline ────────────────────────────────────────────────────────────────

pub(crate) struct BuiltStage {
    pub(crate) apply_to: ApplyTo,
    pub(crate) guard: Arc<dyn Guardrail>,
}

/// Ordered guardrail stages materialised from a policy snapshot.
/// Cheap to build per request (clones a shared `reqwest::Client`).
pub struct GuardrailPipeline {
    pub(crate) stages: Vec<BuiltStage>,
}

impl GuardrailPipeline {
    /// Build from the compiled-policy stage list. A stage naming an
    /// unknown backend, or one whose router-side credential/endpoint
    /// is absent, is a construction error — see the module-level
    /// fail-closed contract.
    pub fn from_stages(
        stages: &[GuardrailStageCfg],
        config: &Config,
        client: &Client,
    ) -> Result<Self, GuardrailError> {
        let mut built = Vec::with_capacity(stages.len());
        for stage in stages {
            match stage.provider.trim().to_ascii_lowercase().as_str() {
                "openai-moderation" => {
                    let api_key = config.openai_moderation_api_key.clone().ok_or_else(|| {
                        GuardrailError::Config {
                            provider: stage.provider.clone(),
                            reason: "no API key configured (OPENAI_MODERATION_API_KEY, \
                                     OPENAI_API_KEY, or secret mount)"
                                .into(),
                        }
                    })?;
                    built.push(BuiltStage {
                        apply_to: stage.apply_to,
                        guard: Arc::new(OpenAiModeration::new(
                            client.clone(),
                            config.openai_moderation_endpoint.clone(),
                            api_key,
                            config.openai_moderation_model.clone(),
                        )),
                    });
                }
                other => {
                    return Err(GuardrailError::Config {
                        provider: other.to_string(),
                        reason: "unknown guardrail backend".into(),
                    });
                }
            }
        }
        Ok(Self { stages: built })
    }

    /// True when at least one stage covers `direction` — callers use
    /// this to skip text extraction entirely on the hot path.
    #[must_use]
    pub fn covers(&self, direction: Direction) -> bool {
        self.stages.iter().any(|s| s.apply_to.covers(direction))
    }

    /// Run every stage covering `direction` over `text`, first flag
    /// wins. Text over [`MAX_SCAN_CHARS`] is scanned in successive
    /// windows, not truncated, so content can't be hidden past the
    /// cap.
    pub async fn scan(
        &self,
        text: &str,
        direction: Direction,
    ) -> Result<Option<GuardrailViolation>, GuardrailError> {
        if text.is_empty() {
            return Ok(None);
        }
        let windows = scan_windows(text);
        for stage in self.stages.iter().filter(|s| s.apply_to.covers(direction)) {
            for window in &windows {
                match stage.guard.scan(window).await {
                    Ok(verdict) if verdict.flagged => {
                        metrics::GUARDRAIL_SCANS
                            .with_label_values(&[stage.guard.name(), direction.as_str(), "flagged"])
                            .inc();
                        return Ok(Some(GuardrailViolation {
                            provider: stage.guard.name(),
                            direction,
                            categories: verdict.categories,
                        }));
                    }
                    Ok(_) => {
                        metrics::GUARDRAIL_SCANS
                            .with_label_values(&[stage.guard.name(), direction.as_str(), "pass"])
                            .inc();
                    }
                    Err(e) => {
                        metrics::GUARDRAIL_SCANS
                            .with_label_values(&[stage.guard.name(), direction.as_str(), "error"])
                            .inc();
                        return Err(e);
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Split `text` into consecutive windows of at most [`MAX_SCAN_CHARS`]
/// chars (never mid-char) so scanning all windows covers everything.
pub(crate) fn scan_windows(text: &str) -> Vec<&str> {
    if text.len() <= MAX_SCAN_CHARS {
        return vec![text];
    }
    let mut windows = Vec::new();
    let mut start = 0;
    let mut count = 0;
    for (i, _) in text.char_indices() {
        if count == MAX_SCAN_CHARS {
            windows.push(&text[start..i]);
            start = i;
            count = 0;
        }
        count += 1;
    }
    windows.push(&text[start..]);
    windows
}

// ─── Request / response text extraction ──────────────────────────────────────

/// Extract the human-visible text of an OpenAI chat-completions
/// request body: every `messages[].content` string, plus `text`
/// fields of array-shaped content parts.
#[must_use]
pub fn extract_openai_input_text(body: &serde_json::Value) -> String {
    let mut out: Vec<String> = Vec::new();
    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        for m in messages {
            match m.get("content") {
                Some(serde_json::Value::String(s)) if !s.is_empty() => out.push(s.clone()),
                Some(serde_json::Value::Array(parts)) => {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(|t| t.as_str())
                            && !t.is_empty()
                        {
                            out.push(t.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out.join("\n")
}

/// Extract the human-visible text of an Anthropic Messages request
/// body: `system` (string or parts) plus `messages[].content` text /
/// `tool_result` strings.
#[must_use]
pub fn extract_anthropic_input_text(body: &serde_json::Value) -> String {
    let mut out: Vec<String> = Vec::new();
    match body.get("system") {
        Some(serde_json::Value::String(s)) if !s.is_empty() => out.push(s.clone()),
        Some(serde_json::Value::Array(parts)) => {
            for p in parts {
                if let Some(t) = p.get("text").and_then(|t| t.as_str())
                    && !t.is_empty()
                {
                    out.push(t.to_string());
                }
            }
        }
        _ => {}
    }
    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        for m in messages {
            match m.get("content") {
                Some(serde_json::Value::String(s)) if !s.is_empty() => out.push(s.clone()),
                Some(serde_json::Value::Array(parts)) => {
                    for p in parts {
                        match p.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(t) = p.get("text").and_then(|t| t.as_str())
                                    && !t.is_empty()
                                {
                                    out.push(t.to_string());
                                }
                            }
                            Some("tool_result") => {
                                if let Some(t) = p.get("content").and_then(|c| c.as_str())
                                    && !t.is_empty()
                                {
                                    out.push(t.to_string());
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out.join("\n")
}

/// Extract the assistant text of a buffered OpenAI chat-completions
/// response (`choices[*].message.content`).
#[must_use]
pub fn extract_openai_output_text(body: &serde_json::Value) -> String {
    let mut out: Vec<String> = Vec::new();
    if let Some(choices) = body.get("choices").and_then(|c| c.as_array()) {
        for c in choices {
            if let Some(t) = c
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|t| t.as_str())
                && !t.is_empty()
            {
                out.push(t.to_string());
            }
        }
    }
    out.join("\n")
}

/// Extract the assistant text of a buffered Anthropic Messages
/// response (`content[*].text`).
#[must_use]
pub fn extract_anthropic_output_text(body: &serde_json::Value) -> String {
    let mut out: Vec<String> = Vec::new();
    if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if let Some(t) = block.get("text").and_then(|t| t.as_str())
                && !t.is_empty()
            {
                out.push(t.to_string());
            }
        }
    }
    out.join("\n")
}

/// Extract scan text via `extract`, falling back to raw lossy-UTF-8
/// bytes when the body isn't JSON, so a declared guardrail is never
/// skipped on a parse failure. (Parsed-but-empty extraction — e.g. a
/// tool-call-only response — is a deliberate pass.)
#[must_use]
pub fn scan_text_or_raw(body: &[u8], extract: impl FnOnce(&serde_json::Value) -> String) -> String {
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(v) => extract(&v),
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    }
}
