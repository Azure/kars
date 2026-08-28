// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pluggable guardrail pipeline for `InferencePolicy.spec.guardrails[]`.
//!
//! Stages run around each governed call — request text pre-flight and
//! response text (buffered + streaming). First backend is OpenAI
//! Moderation; the [`Guardrail`] trait is the extension point.
//!
//! Fail-closed: a declared stage that can't be built (unknown backend
//! / missing credential) or errors at runtime blocks the request
//! rather than passing unscanned content.
//!
//! Streaming uses hold-and-release: SSE chunks are withheld until the
//! accumulated text reaches [`STREAM_SCAN_THRESHOLD_CHARS`] (or the
//! stream ends) and a scan clears it, so no model text reaches the
//! client unscanned; a flagged scan cuts the stream with an error
//! frame. Text over [`MAX_SCAN_CHARS`] is scanned in successive
//! windows ([`scan_windows`]), never truncated.
//!
//! Split across `backend` (moderation + pipeline + extraction) and
//! `stream` (SSE hold-and-release) to bound file size; this module
//! keeps the shared config/verdict/error types and re-exports both.

use async_trait::async_trait;

mod backend;
mod stream;

pub use backend::*;
pub use stream::*;

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

/// One compiled `guardrails[]` stage (`{provider, applyTo}`). Parsed
/// liberally; unknown-backend rejection happens at pipeline
/// construction, where a request exists to fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardrailStageCfg {
    pub provider: String,
    pub apply_to: ApplyTo,
}

impl GuardrailStageCfg {
    /// Parse the compiled `guardrails` block.
    ///
    /// `null`/absent ⇒ `Ok(empty)` (legitimately no pipeline). A
    /// present-but-malformed value, not an array, or an entry missing
    /// a string `provider`, is `Err`: a declared-but-unbuildable
    /// control must fail closed, never be silently dropped to "no
    /// guardrails" (which would also disable the sibling route-gap
    /// guard). The caller poisons the policy so every request refuses.
    pub fn from_compiled_json(v: &serde_json::Value) -> Result<Vec<Self>, String> {
        if v.is_null() {
            return Ok(Vec::new());
        }
        let Some(arr) = v.as_array() else {
            return Err(format!(
                "`guardrails` must be an array or null, got {}",
                json_kind(v)
            ));
        };
        let mut out = Vec::with_capacity(arr.len());
        for (i, stage) in arr.iter().enumerate() {
            let Some(provider) = stage.get("provider").and_then(|p| p.as_str()) else {
                return Err(format!(
                    "guardrails[{i}] missing string `provider`: {stage}"
                ));
            };
            out.push(Self {
                provider: provider.to_string(),
                apply_to: ApplyTo::parse(stage.get("applyTo").and_then(|a| a.as_str())),
            });
        }
        Ok(out)
    }
}

/// One-word JSON kind for error messages.
fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
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

#[cfg(test)]
mod tests;
