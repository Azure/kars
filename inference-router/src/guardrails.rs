// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pluggable guardrail pipeline (multi-cloud guardrails slice).
//!
//! An `InferencePolicy` can declare an ordered list of guardrail
//! stages (`spec.guardrails[]`) that the router runs around every
//! inference call it governs — on the request text before the
//! upstream forward, and on the response text both buffered and
//! streaming. The first backend today is the OpenAI Moderation API;
//! Bedrock Guardrails and Model Armor extend the same [`Guardrail`]
//! trait in follow-up slices.
//!
//! ## Fail-closed contract
//!
//! A *declared* guardrail that cannot run must never become an open
//! gate:
//!
//! - A stage whose backend the router does not recognise, or whose
//!   credential/endpoint is missing, fails pipeline construction —
//!   the handler rejects the request with an operator-actionable
//!   error before any prompt bytes leave the pod.
//! - A scan that errors at runtime (transport failure, non-2xx,
//!   unparseable verdict) blocks the request with
//!   `guardrail_unavailable` rather than passing unscanned content.
//!
//! ## Streaming semantics (hold-and-release)
//!
//! SSE responses are guarded with a hold-and-release window: chunks
//! are buffered until the accumulated new text reaches
//! [`STREAM_SCAN_THRESHOLD_CHARS`] (or the stream ends), the
//! accumulated text is scanned, and only then is the held window
//! released to the client. No model output is ever delivered before
//! some scan has covered it. On a flagged scan, the client receives a
//! structured SSE error frame + `data: [DONE]` and the upstream
//! stream is dropped. The cost is scan-sized delivery granularity
//! (one moderation round-trip per window), which is the standard
//! trade-off for streaming guardrails.
//!
//! Scanned text is capped at [`MAX_SCAN_CHARS`] (most recent chars
//! for output, leading chars for input) to stay under moderation
//! input limits; truncation is logged at WARN — never silent.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use reqwest::Client;

use crate::config::Config;
use crate::metrics;

/// Upper bound on characters submitted to a backend in one scan call.
pub const MAX_SCAN_CHARS: usize = 16_000;

/// Default hold-and-release window for streaming output scans, in
/// characters of extracted delta text. Override with
/// `GUARDRAIL_STREAM_SCAN_CHARS`.
pub const STREAM_SCAN_THRESHOLD_CHARS: usize = 1_000;

/// Env override for [`STREAM_SCAN_THRESHOLD_CHARS`].
pub const STREAM_SCAN_THRESHOLD_ENV: &str = "GUARDRAIL_STREAM_SCAN_CHARS";

/// Scan direction, from the policy's `applyTo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApplyTo {
    Input,
    Output,
    #[default]
    Both,
}

impl ApplyTo {
    /// Liberal parse of the compiled-profile string. Unknown values
    /// widen to `Both` — scanning more than asked is safe; scanning
    /// less is not.
    #[must_use]
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("input") => Self::Input,
            Some(v) if v.eq_ignore_ascii_case("output") => Self::Output,
            Some(v) if v.eq_ignore_ascii_case("both") || v.is_empty() => Self::Both,
            None => Self::Both,
            Some(other) => {
                tracing::warn!(
                    apply_to = other,
                    "guardrail applyTo not recognised — widening to 'both'"
                );
                Self::Both
            }
        }
    }

    #[must_use]
    pub fn covers(&self, direction: Direction) -> bool {
        matches!(
            (self, direction),
            (Self::Both, _) | (Self::Input, Direction::Input) | (Self::Output, Direction::Output)
        )
    }
}

/// Which side of the inference call a scan covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

impl Direction {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// One stage as it travels through the compiled policy JSON
/// (`{"provider": "...", "applyTo": "..." | null}`). Parsed liberally
/// by the loader; strictness (unknown backend ⇒ fail closed) applies
/// at pipeline *construction*, where a request is available to
/// reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardrailStageCfg {
    pub provider: String,
    pub apply_to: ApplyTo,
}

impl GuardrailStageCfg {
    /// Parse the compiled `guardrails` block (array | null | absent).
    /// Entries without a string `provider` are dropped with a WARN —
    /// they cannot be built into anything enforceable and the
    /// controller schema rejects them at admission anyway.
    #[must_use]
    pub fn from_compiled_json(v: &serde_json::Value) -> Vec<Self> {
        let Some(arr) = v.as_array() else {
            return Vec::new();
        };
        arr.iter()
            .filter_map(|stage| {
                let Some(provider) = stage.get("provider").and_then(|p| p.as_str()) else {
                    tracing::warn!(
                        stage = %stage,
                        "guardrail stage missing string 'provider' — dropped"
                    );
                    return None;
                };
                Some(Self {
                    provider: provider.to_string(),
                    apply_to: ApplyTo::parse(stage.get("applyTo").and_then(|a| a.as_str())),
                })
            })
            .collect()
    }
}

/// A guardrail verdict for one scanned text.
#[derive(Debug, Clone, Default)]
pub struct GuardrailVerdict {
    pub flagged: bool,
    /// Backend-specific category names that flagged (e.g.
    /// `violence`, `hate/threatening`).
    pub categories: Vec<String>,
}

/// A confirmed violation, carrying enough context for the audit log
/// and the client-facing error body.
#[derive(Debug, Clone)]
pub struct GuardrailViolation {
    pub provider: &'static str,
    pub direction: Direction,
    pub categories: Vec<String>,
}

impl GuardrailViolation {
    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "Blocked by guardrail '{}' ({}): flagged categories [{}]",
            self.provider,
            self.direction.as_str(),
            self.categories.join(", ")
        )
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        "guardrail_blocked"
    }
}

/// Errors from the pipeline. Both variants block the request
/// (fail-closed) but carry distinct codes so operators can tell a
/// config gap from a backend outage.
#[derive(Debug, thiserror::Error)]
pub enum GuardrailError {
    #[error("guardrail stage '{provider}' cannot run: {reason}")]
    Config { provider: String, reason: String },
    #[error("guardrail '{provider}' scan failed: {reason}")]
    Unavailable {
        provider: &'static str,
        reason: String,
    },
}

impl GuardrailError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config { .. } => "guardrail_misconfigured",
            Self::Unavailable { .. } => "guardrail_unavailable",
        }
    }
}

/// One guardrail backend. `scan` returns the backend's verdict for a
/// single text; transport/parse failures are `Err` and block the
/// request at the pipeline layer.
#[async_trait]
pub trait Guardrail: Send + Sync {
    fn name(&self) -> &'static str;
    async fn scan(&self, text: &str) -> Result<GuardrailVerdict, GuardrailError>;
}

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

struct BuiltStage {
    apply_to: ApplyTo,
    guard: Arc<dyn Guardrail>,
}

/// Ordered guardrail stages materialised from a policy snapshot.
/// Cheap to build per request (clones a shared `reqwest::Client`).
pub struct GuardrailPipeline {
    stages: Vec<BuiltStage>,
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

    /// Run every stage covering `direction` over `text`, in order.
    /// First flagged verdict wins. Empty text short-circuits to pass.
    pub async fn scan(
        &self,
        text: &str,
        direction: Direction,
    ) -> Result<Option<GuardrailViolation>, GuardrailError> {
        if text.is_empty() {
            return Ok(None);
        }
        let capped = cap_for_scan(text, direction);
        for stage in self.stages.iter().filter(|s| s.apply_to.covers(direction)) {
            let outcome = stage.guard.scan(capped).await;
            match outcome {
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
        Ok(None)
    }
}

/// Cap text to [`MAX_SCAN_CHARS`]: leading chars for input (the
/// system prompt + earliest instructions), trailing chars for output
/// (the newest generated text — earlier output was already scanned by
/// previous windows in the streaming path). Logs at WARN on
/// truncation.
fn cap_for_scan(text: &str, direction: Direction) -> &str {
    if text.chars().count() <= MAX_SCAN_CHARS {
        return text;
    }
    tracing::warn!(
        direction = direction.as_str(),
        total_chars = text.chars().count(),
        scanned_chars = MAX_SCAN_CHARS,
        "guardrail scan text exceeds cap — scanning a truncated window"
    );
    match direction {
        Direction::Input => {
            let end = text
                .char_indices()
                .nth(MAX_SCAN_CHARS)
                .map_or(text.len(), |(i, _)| i);
            &text[..end]
        }
        Direction::Output => {
            let start = text
                .char_indices()
                .rev()
                .nth(MAX_SCAN_CHARS - 1)
                .map_or(0, |(i, _)| i);
            &text[start..]
        }
    }
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

/// Extract scan text from a body via `extract`, falling back to the
/// raw (lossy-UTF-8) bytes when the body is not JSON. A declared
/// guardrail must never be skipped because a body failed to parse —
/// the raw fallback keeps unknown/malformed shapes covered instead of
/// letting them through unscanned. (A body that parses but yields no
/// extractable text — e.g. tool-call-only responses — is a deliberate
/// pass: the extractors define the scannable surface.)
#[must_use]
pub fn scan_text_or_raw(body: &[u8], extract: impl FnOnce(&serde_json::Value) -> String) -> String {
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(v) => extract(&v),
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    }
}

// ─── Streaming (SSE) guard ───────────────────────────────────────────────────

/// SSE wire dialect of the guarded stream — decides how delta text is
/// extracted from `data:` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDialect {
    /// OpenAI chat-completions chunks: `choices[0].delta.content`.
    OpenAiChat,
    /// Anthropic Messages events: `content_block_delta` →
    /// `delta.text`.
    AnthropicMessages,
}

/// Extract delta text from one complete SSE `data:` JSON payload.
#[must_use]
fn delta_text_from_event(dialect: StreamDialect, event: &serde_json::Value) -> Option<String> {
    match dialect {
        StreamDialect::OpenAiChat => event
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
            .map(str::to_string),
        StreamDialect::AnthropicMessages => {
            if event.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                event
                    .get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(|t| t.as_str())
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
            } else {
                None
            }
        }
    }
}

/// The client-facing SSE error frame emitted when a stream is cut by
/// a guardrail. OpenAI-style error object works for both dialects'
/// SDK error paths and is what the existing content-safety stream cut
/// emits too. Public so buffered-to-SSE conversion paths (e.g. the
/// Responses-API recovery branch) can emit the same frame shape.
#[must_use]
pub fn violation_sse_frame(violation: &GuardrailViolation) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "error": {
                "message": violation.message(),
                "type": "content_policy_violation",
                "code": violation.code()
            }
        })
    ))
}

/// SSE frame for a guardrail that could not run (config gap or
/// backend outage) — carries the error's own `type`/`code` so a
/// fail-closed cut is never mislabelled as a content violation.
#[must_use]
pub fn error_sse_frame(err: &GuardrailError) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "error": {
                "message": err.to_string(),
                "type": "guardrail_error",
                "code": err.code()
            }
        })
    ))
}

/// Effective hold-and-release window size.
fn stream_scan_threshold() -> usize {
    std::env::var(STREAM_SCAN_THRESHOLD_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &usize| *v > 0)
        .unwrap_or(STREAM_SCAN_THRESHOLD_CHARS)
}

/// Hold-and-release state machine for one guarded SSE stream. Kept
/// separate from the stream adaptor so the release/hold/block logic
/// is unit-testable with a fake [`Guardrail`].
struct SseGuardState {
    pipeline: Arc<GuardrailPipeline>,
    dialect: StreamDialect,
    threshold: usize,
    /// Raw chunks held back until the text they carry has been
    /// covered by a scan.
    held: Vec<Bytes>,
    /// Carry buffer for `data:` lines split across chunk boundaries.
    line_carry: String,
    /// All delta text accumulated so far (scan context).
    accumulated: String,
    /// Chars of `accumulated` not yet covered by a scan.
    unscanned: usize,
}

/// What the state machine wants the adaptor to emit next.
enum SseGuardStep {
    /// Forward these bytes (possibly empty ⇒ nothing to emit yet).
    Release(Vec<Bytes>),
    /// Emit this terminal frame and drop the upstream stream.
    Cut(Bytes),
}

impl SseGuardState {
    fn new(pipeline: Arc<GuardrailPipeline>, dialect: StreamDialect, threshold: usize) -> Self {
        Self {
            pipeline,
            dialect,
            threshold,
            held: Vec::new(),
            line_carry: String::new(),
            accumulated: String::new(),
            unscanned: 0,
        }
    }

    /// Pull complete lines out of `chunk` (+ carry), extract delta
    /// text, and account it as unscanned.
    fn ingest_text(&mut self, chunk: &[u8]) {
        self.line_carry.push_str(&String::from_utf8_lossy(chunk));
        // Keep the trailing partial line (no '\n' yet) in the carry.
        let (complete, rest) = match self.line_carry.rfind('\n') {
            Some(idx) => {
                let (c, r) = self.line_carry.split_at(idx + 1);
                (c.to_string(), r.to_string())
            }
            None => (String::new(), std::mem::take(&mut self.line_carry)),
        };
        self.line_carry = rest;
        for line in complete.lines() {
            // SSE permits `data:` with or without a following space.
            let Some(payload) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim_start();
            if payload == "[DONE]" {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(payload)
                && let Some(text) = delta_text_from_event(self.dialect, &event)
            {
                self.unscanned += text.chars().count();
                self.accumulated.push_str(&text);
            }
        }
    }

    async fn on_chunk(&mut self, chunk: Bytes) -> SseGuardStep {
        self.ingest_text(&chunk);
        self.held.push(chunk);
        if self.unscanned < self.threshold {
            // Fast path: window not full. Chunks carrying no delta
            // text at all (keepalives, role/annotation frames) are
            // safe to release immediately when nothing text-bearing
            // is being held alongside them.
            if self.unscanned == 0 {
                return SseGuardStep::Release(std::mem::take(&mut self.held));
            }
            return SseGuardStep::Release(Vec::new());
        }
        self.scan_and_release().await
    }

    async fn on_end(&mut self) -> SseGuardStep {
        if self.unscanned == 0 {
            return SseGuardStep::Release(std::mem::take(&mut self.held));
        }
        self.scan_and_release().await
    }

    async fn scan_and_release(&mut self) -> SseGuardStep {
        match self
            .pipeline
            .scan(&self.accumulated, Direction::Output)
            .await
        {
            Ok(None) => {
                self.unscanned = 0;
                self.trim_scan_context();
                SseGuardStep::Release(std::mem::take(&mut self.held))
            }
            Ok(Some(violation)) => SseGuardStep::Cut(violation_sse_frame(&violation)),
            Err(e) => SseGuardStep::Cut(error_sse_frame(&e)),
        }
    }

    /// Bound the retained scan context after a clean scan. Output
    /// scans only ever submit the trailing [`MAX_SCAN_CHARS`] chars
    /// (see [`cap_for_scan`]), so anything older cannot influence a
    /// future scan's input — dropping it keeps per-connection memory
    /// bounded on long-lived streams without weakening the
    /// scanned-before-delivery contract.
    fn trim_scan_context(&mut self) {
        if self.accumulated.chars().count() <= MAX_SCAN_CHARS {
            return;
        }
        let start = self
            .accumulated
            .char_indices()
            .rev()
            .nth(MAX_SCAN_CHARS - 1)
            .map_or(0, |(i, _)| i);
        self.accumulated = self.accumulated.split_off(start);
    }
}

/// Wrap an SSE byte stream with the hold-and-release output guard.
/// `sandbox` and `policy_digest` feed the audit log line on a cut.
///
/// No-op-cheap when the pipeline has no output stages — callers
/// should check [`GuardrailPipeline::covers`] and skip the wrap.
pub fn guard_sse_stream<E>(
    stream: BoxStream<'static, Result<Bytes, E>>,
    pipeline: Arc<GuardrailPipeline>,
    dialect: StreamDialect,
    sandbox: String,
    policy_digest: String,
) -> BoxStream<'static, Result<Bytes, E>>
where
    E: Send + 'static,
{
    let state = SseGuardState::new(pipeline, dialect, stream_scan_threshold());

    struct Ctx<E> {
        inner: BoxStream<'static, Result<Bytes, E>>,
        state: SseGuardState,
        sandbox: String,
        policy_digest: String,
        /// Terminal frame queued for emission; stream ends after.
        pending_cut: Option<Bytes>,
        finished: bool,
    }

    let ctx = Ctx {
        inner: stream,
        state,
        sandbox,
        policy_digest,
        pending_cut: None,
        finished: false,
    };

    futures::stream::unfold(ctx, |mut ctx| async move {
        if let Some(frame) = ctx.pending_cut.take() {
            ctx.finished = true;
            return Some((Ok(frame), ctx));
        }
        if ctx.finished {
            return None;
        }
        loop {
            match ctx.inner.next().await {
                Some(Ok(chunk)) => match ctx.state.on_chunk(chunk).await {
                    SseGuardStep::Release(chunks) if chunks.is_empty() => continue,
                    SseGuardStep::Release(chunks) => {
                        let merged = merge_chunks(chunks);
                        return Some((Ok(merged), ctx));
                    }
                    SseGuardStep::Cut(frame) => {
                        tracing::warn!(
                            target: "inference.audit",
                            sandbox = %ctx.sandbox,
                            inference_policy_digest = %ctx.policy_digest,
                            decision = "deny",
                            gate = "guardrail_stream",
                            "guardrail pipeline cut SSE stream"
                        );
                        ctx.finished = true;
                        return Some((Ok(frame), ctx));
                    }
                },
                Some(Err(e)) => {
                    // Upstream transport error: surface it verbatim.
                    // Held chunks are dropped — their text was never
                    // scanned, so releasing them would violate the
                    // scanned-before-delivery contract.
                    ctx.finished = true;
                    return Some((Err(e), ctx));
                }
                None => match ctx.state.on_end().await {
                    SseGuardStep::Release(chunks) => {
                        ctx.finished = true;
                        if chunks.is_empty() {
                            return None;
                        }
                        return Some((Ok(merge_chunks(chunks)), ctx));
                    }
                    SseGuardStep::Cut(frame) => {
                        tracing::warn!(
                            target: "inference.audit",
                            sandbox = %ctx.sandbox,
                            inference_policy_digest = %ctx.policy_digest,
                            decision = "deny",
                            gate = "guardrail_stream",
                            "guardrail pipeline cut SSE stream at end-of-stream"
                        );
                        ctx.finished = true;
                        return Some((Ok(frame), ctx));
                    }
                },
            }
        }
    })
    .boxed()
}

fn merge_chunks(chunks: Vec<Bytes>) -> Bytes {
    if chunks.len() == 1 {
        return chunks.into_iter().next().expect("len checked");
    }
    let total: usize = chunks.iter().map(Bytes::len).sum();
    let mut merged = Vec::with_capacity(total);
    for c in chunks {
        merged.extend_from_slice(&c);
    }
    Bytes::from(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- config parsing ----

    #[test]
    fn apply_to_parses_liberally_and_widens_unknowns() {
        assert_eq!(ApplyTo::parse(Some("input")), ApplyTo::Input);
        assert_eq!(ApplyTo::parse(Some("OUTPUT")), ApplyTo::Output);
        assert_eq!(ApplyTo::parse(Some("both")), ApplyTo::Both);
        assert_eq!(ApplyTo::parse(None), ApplyTo::Both);
        assert_eq!(ApplyTo::parse(Some("sideways")), ApplyTo::Both);
    }

    #[test]
    fn apply_to_covers_directions() {
        assert!(ApplyTo::Both.covers(Direction::Input));
        assert!(ApplyTo::Both.covers(Direction::Output));
        assert!(ApplyTo::Input.covers(Direction::Input));
        assert!(!ApplyTo::Input.covers(Direction::Output));
        assert!(ApplyTo::Output.covers(Direction::Output));
        assert!(!ApplyTo::Output.covers(Direction::Input));
    }

    #[test]
    fn stage_cfg_parses_compiled_json() {
        let v = serde_json::json!([
            { "provider": "openai-moderation", "applyTo": "output" },
            { "provider": "openai-moderation", "applyTo": null },
            { "applyTo": "input" } // dropped: no provider
        ]);
        let stages = GuardrailStageCfg::from_compiled_json(&v);
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].provider, "openai-moderation");
        assert_eq!(stages[0].apply_to, ApplyTo::Output);
        assert_eq!(stages[1].apply_to, ApplyTo::Both);
    }

    #[test]
    fn stage_cfg_handles_null_and_absent() {
        assert!(GuardrailStageCfg::from_compiled_json(&serde_json::Value::Null).is_empty());
        assert!(GuardrailStageCfg::from_compiled_json(&serde_json::json!({})).is_empty());
    }

    // ---- moderation response parsing ----

    #[test]
    fn moderation_parse_flags_and_categories() {
        let body = serde_json::json!({
            "results": [{
                "flagged": true,
                "categories": { "violence": true, "hate": false, "self-harm": true }
            }]
        });
        let v = parse_moderation_response(&body).unwrap();
        assert!(v.flagged);
        let mut cats = v.categories.clone();
        cats.sort();
        assert_eq!(cats, vec!["self-harm", "violence"]);
    }

    #[test]
    fn moderation_parse_pass() {
        let body = serde_json::json!({ "results": [{ "flagged": false, "categories": {} }] });
        let v = parse_moderation_response(&body).unwrap();
        assert!(!v.flagged);
        assert!(v.categories.is_empty());
    }

    #[test]
    fn moderation_parse_fails_closed_on_malformed() {
        assert!(parse_moderation_response(&serde_json::json!({})).is_err());
        assert!(parse_moderation_response(&serde_json::json!({ "results": [] })).is_err());
        assert!(
            parse_moderation_response(&serde_json::json!({ "results": [{ "categories": {} }] }))
                .is_err()
        );
    }

    // ---- text extraction ----

    #[test]
    fn scan_text_or_raw_extracts_json_and_falls_back_to_raw() {
        let json = br#"{"choices":[{"message":{"content":"answer"}}]}"#;
        assert_eq!(scan_text_or_raw(json, extract_openai_output_text), "answer");
        let not_json = b"plain text that failed to parse";
        assert_eq!(
            scan_text_or_raw(not_json, extract_openai_output_text),
            "plain text that failed to parse"
        );
    }

    #[test]
    fn openai_input_text_handles_string_and_parts() {
        let body = serde_json::json!({
            "messages": [
                { "role": "system", "content": "be nice" },
                { "role": "user", "content": [ { "type": "text", "text": "hello" },
                                                { "type": "image_url", "image_url": {} } ] }
            ]
        });
        assert_eq!(extract_openai_input_text(&body), "be nice\nhello");
    }

    #[test]
    fn anthropic_input_text_handles_system_and_tool_results() {
        let body = serde_json::json!({
            "system": "be nice",
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "hello" },
                    { "type": "tool_result", "content": "result text" }
                ]},
                { "role": "assistant", "content": "earlier reply" }
            ]
        });
        assert_eq!(
            extract_anthropic_input_text(&body),
            "be nice\nhello\nresult text\nearlier reply"
        );
    }

    #[test]
    fn output_text_extractors() {
        let openai = serde_json::json!({
            "choices": [ { "message": { "content": "answer" } } ]
        });
        assert_eq!(extract_openai_output_text(&openai), "answer");
        let anthropic = serde_json::json!({
            "content": [ { "type": "text", "text": "answer" } ]
        });
        assert_eq!(extract_anthropic_output_text(&anthropic), "answer");
    }

    #[test]
    fn delta_extraction_per_dialect() {
        let openai = serde_json::json!({
            "choices": [ { "delta": { "content": "hi" } } ]
        });
        assert_eq!(
            delta_text_from_event(StreamDialect::OpenAiChat, &openai),
            Some("hi".to_string())
        );
        let anthropic = serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "hi" }
        });
        assert_eq!(
            delta_text_from_event(StreamDialect::AnthropicMessages, &anthropic),
            Some("hi".to_string())
        );
        let other = serde_json::json!({ "type": "message_start" });
        assert_eq!(
            delta_text_from_event(StreamDialect::AnthropicMessages, &other),
            None
        );
    }

    // ---- cap ----

    #[test]
    fn cap_keeps_short_text_intact() {
        assert_eq!(cap_for_scan("short", Direction::Input), "short");
    }

    #[test]
    fn cap_truncates_head_for_input_and_tail_for_output() {
        let long: String = "a".repeat(MAX_SCAN_CHARS) + "TAIL";
        let capped_in = cap_for_scan(&long, Direction::Input);
        assert_eq!(capped_in.len(), MAX_SCAN_CHARS);
        assert!(capped_in.starts_with('a') && !capped_in.contains("TAIL"));
        let long2 = "HEAD".to_string() + &"b".repeat(MAX_SCAN_CHARS);
        let capped_out = cap_for_scan(&long2, Direction::Output);
        assert_eq!(capped_out.len(), MAX_SCAN_CHARS);
        assert!(!capped_out.contains("HEAD"));
    }

    // ---- pipeline + streaming with a fake backend ----

    /// Test backend: flags any text containing the marker. Counts
    /// scans so tests can assert hold-and-release windowing.
    struct MarkerGuard {
        marker: &'static str,
        scans: Arc<AtomicUsize>,
        fail: bool,
    }

    #[async_trait]
    impl Guardrail for MarkerGuard {
        fn name(&self) -> &'static str {
            "marker-test"
        }
        async fn scan(&self, text: &str) -> Result<GuardrailVerdict, GuardrailError> {
            self.scans.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(GuardrailError::Unavailable {
                    provider: "marker-test",
                    reason: "boom".into(),
                });
            }
            Ok(GuardrailVerdict {
                flagged: text.contains(self.marker),
                categories: vec!["marker".into()],
            })
        }
    }

    fn pipeline_with(
        marker: &'static str,
        apply_to: ApplyTo,
        scans: Arc<AtomicUsize>,
        fail: bool,
    ) -> GuardrailPipeline {
        GuardrailPipeline {
            stages: vec![BuiltStage {
                apply_to,
                guard: Arc::new(MarkerGuard {
                    marker,
                    scans,
                    fail,
                }),
            }],
        }
    }

    #[tokio::test]
    async fn pipeline_scan_flags_and_passes() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = pipeline_with("BAD", ApplyTo::Both, scans.clone(), false);
        assert!(
            p.scan("all good", Direction::Input)
                .await
                .unwrap()
                .is_none()
        );
        let v = p
            .scan("some BAD text", Direction::Output)
            .await
            .unwrap()
            .expect("flagged");
        assert_eq!(v.provider, "marker-test");
        assert_eq!(v.direction, Direction::Output);
        assert_eq!(scans.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pipeline_skips_direction_not_covered() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = pipeline_with("BAD", ApplyTo::Output, scans.clone(), false);
        assert!(!p.covers(Direction::Input));
        assert!(
            p.scan("BAD input", Direction::Input)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(scans.load(Ordering::SeqCst), 0, "input stage must not run");
    }

    #[tokio::test]
    async fn pipeline_scan_error_fails_closed() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = pipeline_with("BAD", ApplyTo::Both, scans, true);
        let err = p.scan("anything", Direction::Output).await.unwrap_err();
        assert_eq!(err.code(), "guardrail_unavailable");
    }

    #[test]
    fn pipeline_from_stages_rejects_unknown_backend() {
        let cfg = crate::config::Config::from_env().expect("env config");
        let stages = vec![GuardrailStageCfg {
            provider: "not-a-backend".into(),
            apply_to: ApplyTo::Both,
        }];
        let err = GuardrailPipeline::from_stages(&stages, &cfg, &reqwest::Client::new())
            .err()
            .expect("must fail");
        assert_eq!(err.code(), "guardrail_misconfigured");
    }

    fn sse_chunk(text: &str) -> Bytes {
        Bytes::from(format!(
            "data: {}\n\n",
            serde_json::json!({ "choices": [ { "delta": { "content": text } } ] })
        ))
    }

    fn collect_stream(
        stream: BoxStream<'static, Result<Bytes, std::io::Error>>,
    ) -> impl std::future::Future<Output = String> {
        use futures::TryStreamExt;
        async move {
            let all: Vec<Bytes> = stream.try_collect().await.expect("stream ok");
            all.iter()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .collect()
        }
    }

    #[tokio::test]
    async fn sse_guard_releases_clean_stream_intact() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(pipeline_with("BAD", ApplyTo::Output, scans.clone(), false));
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(sse_chunk("hello ")),
            Ok(sse_chunk("world")),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];
        let guarded = guard_sse_stream(
            futures::stream::iter(chunks).boxed(),
            p,
            StreamDialect::OpenAiChat,
            "sbx".into(),
            "sha256:t".into(),
        );
        let out = collect_stream(guarded).await;
        assert!(out.contains("hello "));
        assert!(out.contains("world"));
        assert!(out.contains("[DONE]"));
        // Under-threshold text ⇒ exactly one end-of-stream scan.
        assert_eq!(scans.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn sse_guard_cuts_stream_on_violation_and_withholds_text() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(pipeline_with("BAD", ApplyTo::Output, scans, false));
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(sse_chunk("this is BAD content")),
            Ok(sse_chunk("more text that must never be seen")),
        ];
        let guarded = guard_sse_stream(
            futures::stream::iter(chunks).boxed(),
            p,
            StreamDialect::OpenAiChat,
            "sbx".into(),
            "sha256:t".into(),
        );
        let out = collect_stream(guarded).await;
        assert!(
            !out.contains("BAD content"),
            "flagged text must never reach the client: {out}"
        );
        assert!(out.contains("guardrail_blocked"));
        assert!(out.contains("data: [DONE]"));
    }

    #[tokio::test]
    async fn sse_guard_holds_text_until_scanned_across_threshold() {
        // Force a tiny threshold via a long first chunk: text length
        // over the default threshold triggers a mid-stream scan.
        let scans = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(pipeline_with("BAD", ApplyTo::Output, scans.clone(), false));
        let big = "x".repeat(STREAM_SCAN_THRESHOLD_CHARS + 10);
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            vec![Ok(sse_chunk(&big)), Ok(sse_chunk("tail"))];
        let guarded = guard_sse_stream(
            futures::stream::iter(chunks).boxed(),
            p,
            StreamDialect::OpenAiChat,
            "sbx".into(),
            "sha256:t".into(),
        );
        let out = collect_stream(guarded).await;
        assert!(out.contains(&big));
        assert!(out.contains("tail"));
        // One mid-stream scan (threshold) + one at end-of-stream for
        // the tail.
        assert_eq!(scans.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn sse_guard_cuts_on_scan_error() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(pipeline_with("BAD", ApplyTo::Output, scans, true));
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![Ok(sse_chunk("hello"))];
        let guarded = guard_sse_stream(
            futures::stream::iter(chunks).boxed(),
            p,
            StreamDialect::OpenAiChat,
            "sbx".into(),
            "sha256:t".into(),
        );
        let out = collect_stream(guarded).await;
        assert!(!out.contains("hello"), "unscanned text must be withheld");
        assert!(out.contains("guardrail_unavailable"));
    }

    #[tokio::test]
    async fn sse_guard_passes_non_text_frames_through_untouched() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(pipeline_with("BAD", ApplyTo::Output, scans.clone(), false));
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from(": keepalive\n\n")),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];
        let guarded = guard_sse_stream(
            futures::stream::iter(chunks).boxed(),
            p,
            StreamDialect::OpenAiChat,
            "sbx".into(),
            "sha256:t".into(),
        );
        let out = collect_stream(guarded).await;
        assert!(out.contains(": keepalive"));
        assert!(out.contains("[DONE]"));
        assert_eq!(
            scans.load(Ordering::SeqCst),
            0,
            "no text ⇒ no scan round-trips"
        );
    }

    #[tokio::test]
    async fn sse_guard_catches_data_prefix_without_space() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(pipeline_with("FORBIDDEN", ApplyTo::Output, scans, false));
        let event =
            serde_json::json!({ "choices": [ { "delta": { "content": "FORBIDDEN text" } } ] });
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            vec![Ok(Bytes::from(format!("data:{event}\n\n")))];
        let guarded = guard_sse_stream(
            futures::stream::iter(chunks).boxed(),
            p,
            StreamDialect::OpenAiChat,
            "sbx".into(),
            "sha256:t".into(),
        );
        let out = collect_stream(guarded).await;
        assert!(
            !out.contains("FORBIDDEN"),
            "spaceless data: events must still be scanned: {out}"
        );
        assert!(out.contains("guardrail_blocked"));
    }

    #[tokio::test]
    async fn sse_guard_state_bounds_scan_context_on_long_streams() {
        // Regression: `accumulated` must not grow without bound on
        // long-lived streams — after every clean scan the retained
        // context is trimmed to MAX_SCAN_CHARS (the most a future
        // scan can consume anyway).
        let scans = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(pipeline_with("BAD", ApplyTo::Output, scans.clone(), false));
        let mut state = SseGuardState::new(p, StreamDialect::OpenAiChat, 10);
        for _ in 0..40 {
            let step = state.on_chunk(sse_chunk(&"y".repeat(1000))).await;
            assert!(matches!(step, SseGuardStep::Release(_)));
        }
        assert!(
            scans.load(Ordering::SeqCst) >= 40,
            "every chunk over threshold scans"
        );
        assert!(
            state.accumulated.chars().count() <= MAX_SCAN_CHARS,
            "scan context must stay bounded, got {}",
            state.accumulated.chars().count()
        );
    }

    #[tokio::test]
    async fn sse_guard_handles_events_split_across_chunks() {
        let scans = Arc::new(AtomicUsize::new(0));
        let p = Arc::new(pipeline_with("FORBIDDEN", ApplyTo::Output, scans, false));
        let full = sse_chunk("this is FORBIDDEN text");
        let (a, b) = full.split_at(20);
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            vec![Ok(Bytes::copy_from_slice(a)), Ok(Bytes::copy_from_slice(b))];
        let guarded = guard_sse_stream(
            futures::stream::iter(chunks).boxed(),
            p,
            StreamDialect::OpenAiChat,
            "sbx".into(),
            "sha256:t".into(),
        );
        let out = collect_stream(guarded).await;
        assert!(
            !out.contains("FORBIDDEN"),
            "split-event text must still be caught: {out}"
        );
        assert!(out.contains("guardrail_blocked"));
    }
}
