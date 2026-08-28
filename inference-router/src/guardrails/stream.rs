// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Streaming (SSE) hold-and-release output guard.

use std::sync::Arc;

use bytes::Bytes;
use futures::stream::BoxStream;
use futures::stream::StreamExt;

use super::{
    Direction, GuardrailError, GuardrailPipeline, GuardrailViolation, MAX_SCAN_CHARS,
    STREAM_SCAN_THRESHOLD_CHARS, STREAM_SCAN_THRESHOLD_ENV,
};

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
pub(crate) fn delta_text_from_event(
    dialect: StreamDialect,
    event: &serde_json::Value,
) -> Option<String> {
    match dialect {
        StreamDialect::OpenAiChat => {
            // Every choice, not just `choices[0]`, an OpenAI-compatible
            // upstream can emit multiple choices in one frame, and a
            // later choice's text must not slip through unscanned.
            let texts: Vec<&str> = event
                .get("choices")
                .and_then(|c| c.as_array())
                .map(|choices| {
                    choices
                        .iter()
                        .filter_map(|c| {
                            c.get("delta")
                                .and_then(|d| d.get("content"))
                                .and_then(|t| t.as_str())
                                .filter(|t| !t.is_empty())
                        })
                        .collect()
                })
                .unwrap_or_default();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
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

/// Client-facing SSE error frame for a guardrail cut. The OpenAI-shape
/// error object works for both dialects' SDK error paths.
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

/// Effective hold-and-release window size, clamped to
/// [`MAX_SCAN_CHARS`] so a window can never accumulate more text than
/// one scan covers.
fn stream_scan_threshold() -> usize {
    std::env::var(STREAM_SCAN_THRESHOLD_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &usize| *v > 0)
        .unwrap_or(STREAM_SCAN_THRESHOLD_CHARS)
        .min(MAX_SCAN_CHARS)
}

/// Hold-and-release state machine for one guarded SSE stream. Kept
/// separate from the stream adaptor so the release/hold/block logic
/// is unit-testable with a fake [`Guardrail`].
pub(crate) struct SseGuardState {
    pipeline: Arc<GuardrailPipeline>,
    dialect: StreamDialect,
    threshold: usize,
    /// Raw chunks held back until the text they carry has been
    /// covered by a scan.
    held: Vec<Bytes>,
    /// Carry buffer for a trailing incomplete UTF-8 sequence split
    /// across chunk boundaries, decoded once its continuation arrives
    /// so moderation scans the same code points the client receives.
    byte_carry: Vec<u8>,
    /// Carry buffer for `data:` lines split across chunk boundaries.
    line_carry: String,
    /// All delta text accumulated so far (scan context).
    pub(crate) accumulated: String,
    /// Chars of `accumulated` not yet covered by a scan.
    unscanned: usize,
}

/// What the state machine wants the adaptor to emit next.
pub(crate) enum SseGuardStep {
    /// Forward these bytes (possibly empty ⇒ nothing to emit yet).
    Release(Vec<Bytes>),
    /// Emit this terminal frame and drop the upstream stream.
    Cut(Bytes),
}

impl SseGuardState {
    pub(crate) fn new(
        pipeline: Arc<GuardrailPipeline>,
        dialect: StreamDialect,
        threshold: usize,
    ) -> Self {
        Self {
            pipeline,
            dialect,
            threshold,
            held: Vec::new(),
            byte_carry: Vec::new(),
            line_carry: String::new(),
            accumulated: String::new(),
            unscanned: 0,
        }
    }

    fn ingest_text(&mut self, chunk: &[u8]) {
        // Prepend any trailing incomplete UTF-8 bytes from the previous
        // chunk, then decode only the complete-code-point prefix. A
        // genuine incomplete sequence at the end is carried for the
        // next chunk; a real mid-stream invalid byte is decoded
        // lossily (matches the pre-existing behaviour for that
        // pathological case) rather than carried forever.
        let mut bytes = std::mem::take(&mut self.byte_carry);
        bytes.extend_from_slice(chunk);
        let decodable = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => bytes.len(),
        };
        self.byte_carry = bytes.split_off(decodable);
        self.line_carry.push_str(&String::from_utf8_lossy(&bytes));
        let (complete, rest) = match self.line_carry.rfind('\n') {
            Some(idx) => {
                let (c, r) = self.line_carry.split_at(idx + 1);
                (c.to_string(), r.to_string())
            }
            None => (String::new(), std::mem::take(&mut self.line_carry)),
        };
        self.line_carry = rest;
        for line in complete.lines() {
            self.ingest_line(line);
        }
    }

    fn ingest_line(&mut self, line: &str) {
        // SSE permits `data:` with or without a following space.
        let Some(payload) = line.trim().strip_prefix("data:") else {
            return;
        };
        let payload = payload.trim_start();
        if payload.is_empty() || payload == "[DONE]" {
            return;
        }
        match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(event) => {
                if let Some(text) = delta_text_from_event(self.dialect, &event) {
                    self.unscanned += text.chars().count();
                    self.accumulated.push_str(&text);
                }
                // Valid JSON with no delta text is a structural frame
                // (ping / role-only / stop) — nothing to scan.
            }
            // Unrecognised (non-JSON) frame: scan the raw payload so
            // it can't bypass the scan.
            Err(_) => {
                self.unscanned += payload.chars().count();
                self.accumulated.push_str(payload);
            }
        }
    }

    pub(crate) async fn on_chunk(&mut self, chunk: Bytes) -> SseGuardStep {
        self.ingest_text(&chunk);
        self.held.push(chunk);
        // A pending partial line, or a partial UTF-8 sequence, has
        // bytes in `held` whose text is not yet counted; never release
        // until it completes.
        if !self.line_carry.is_empty() || !self.byte_carry.is_empty() {
            return SseGuardStep::Release(Vec::new());
        }
        if self.unscanned < self.threshold {
            if self.unscanned == 0 {
                return SseGuardStep::Release(std::mem::take(&mut self.held));
            }
            return SseGuardStep::Release(Vec::new());
        }
        self.scan_and_release().await
    }

    async fn on_end(&mut self) -> SseGuardStep {
        // Flush any leftover incomplete UTF-8 bytes (lossily, the
        // stream ended mid-sequence) into the line carry first…
        if !self.byte_carry.is_empty() {
            let tail = std::mem::take(&mut self.byte_carry);
            self.line_carry.push_str(&String::from_utf8_lossy(&tail));
        }
        // …then flush a trailing unterminated line so it's scanned too.
        if !self.line_carry.is_empty() {
            let line = std::mem::take(&mut self.line_carry);
            self.ingest_line(&line);
        }
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

    /// After a clean scan, retain `MAX_SCAN_CHARS - threshold` chars
    /// of context: bounds memory and keeps the next scan in one window
    /// while overlapping for cross-boundary detection.
    fn trim_scan_context(&mut self) {
        let keep = MAX_SCAN_CHARS.saturating_sub(self.threshold).max(1);
        if self.accumulated.chars().count() <= keep {
            return;
        }
        let start = self
            .accumulated
            .char_indices()
            .rev()
            .nth(keep - 1)
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
