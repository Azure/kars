// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Guardrail unit tests (config, moderation, extraction, pipeline, SSE).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use futures::stream::{BoxStream, StreamExt};

use super::backend::{BuiltStage, scan_windows};
use super::stream::{SseGuardState, SseGuardStep, delta_text_from_event};
use super::*;

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
    ]);
    let stages = GuardrailStageCfg::from_compiled_json(&v).expect("valid stages");
    assert_eq!(stages.len(), 2);
    assert_eq!(stages[0].provider, "openai-moderation");
    assert_eq!(stages[0].apply_to, ApplyTo::Output);
    assert_eq!(stages[1].apply_to, ApplyTo::Both);
}

#[test]
fn stage_cfg_null_is_no_pipeline() {
    assert!(
        GuardrailStageCfg::from_compiled_json(&serde_json::Value::Null)
            .expect("null ⇒ ok")
            .is_empty()
    );
}

#[test]
fn stage_cfg_malformed_fails_closed() {
    // A declared-but-malformed block must be an error, never a
    // silent drop to "no guardrails".
    // Not an array:
    assert!(GuardrailStageCfg::from_compiled_json(&serde_json::json!({})).is_err());
    assert!(
        GuardrailStageCfg::from_compiled_json(&serde_json::json!("openai-moderation")).is_err()
    );
    // An entry missing a string provider:
    let missing = serde_json::json!([
        { "provider": "openai-moderation", "applyTo": "output" },
        { "applyTo": "input" }
    ]);
    assert!(GuardrailStageCfg::from_compiled_json(&missing).is_err());
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
    let event = serde_json::json!({ "choices": [ { "delta": { "content": "FORBIDDEN text" } } ] });
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

#[tokio::test]
async fn sse_guard_scans_non_json_data_frames() {
    // Upstream drift: a `data:` line whose payload isn't valid
    // JSON must still be scanned, not fast-released unscanned.
    let scans = Arc::new(AtomicUsize::new(0));
    let p = Arc::new(pipeline_with(
        "FORBIDDEN",
        ApplyTo::Output,
        scans.clone(),
        false,
    ));
    let chunks: Vec<Result<Bytes, std::io::Error>> =
        vec![Ok(Bytes::from("data: this is FORBIDDEN not-json\n\n"))];
    let guarded = guard_sse_stream(
        futures::stream::iter(chunks).boxed(),
        p,
        StreamDialect::OpenAiChat,
        "sbx".into(),
        "sha256:t".into(),
    );
    let out = collect_stream(guarded).await;
    assert!(
        !out.contains("FORBIDDEN not-json"),
        "non-JSON data frame must be scanned, not leaked: {out}"
    );
    assert!(out.contains("guardrail_blocked"));
    assert!(
        scans.load(Ordering::SeqCst) >= 1,
        "raw frame must be scanned"
    );
}

#[tokio::test]
async fn sse_guard_passes_json_structural_frames_without_scanning() {
    // Valid-JSON frames with no delta text (ping / role-only /
    // stop) carry no model text and are released without a scan
    // round-trip.
    let scans = Arc::new(AtomicUsize::new(0));
    let p = Arc::new(pipeline_with(
        "FORBIDDEN",
        ApplyTo::Output,
        scans.clone(),
        false,
    ));
    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::from(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        )),
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
    assert!(out.contains("\"role\":\"assistant\""));
    assert!(out.contains("[DONE]"));
    assert_eq!(
        scans.load(Ordering::SeqCst),
        0,
        "structural frames don't scan"
    );
}

#[tokio::test]
async fn sse_guard_withholds_bytes_when_split_inside_content_string() {
    // Regression (B1): a chunk boundary *inside* the JSON content
    // string leaves the delta text uncounted in line_carry while
    // its bytes sit in `held`. The guard must not fast-release
    // those bytes, or flagged model text ships unscanned.
    let scans = Arc::new(AtomicUsize::new(0));
    let p = Arc::new(pipeline_with("FORBIDDEN", ApplyTo::Output, scans, false));
    let full = sse_chunk("this is FORBIDDEN text");
    let s = String::from_utf8(full.to_vec()).unwrap();
    let cut = s.find("FORB").unwrap() + 2; // split mid-word, mid-string
    let (a, b) = s.split_at(cut);
    let chunks: Vec<Result<Bytes, std::io::Error>> =
        vec![Ok(Bytes::from(a.to_owned())), Ok(Bytes::from(b.to_owned()))];
    let guarded = guard_sse_stream(
        futures::stream::iter(chunks).boxed(),
        p,
        StreamDialect::OpenAiChat,
        "sbx".into(),
        "sha256:t".into(),
    );
    let out = collect_stream(guarded).await;
    assert!(
        !out.contains("FORB"),
        "no partial content bytes may reach the client unscanned: {out}"
    );
    assert!(out.contains("guardrail_blocked"));
}

#[test]
fn scan_windows_covers_every_char_without_truncation() {
    let short = "hello";
    assert_eq!(scan_windows(short), vec!["hello"]);

    let long: String = "a".repeat(MAX_SCAN_CHARS) + &"b".repeat(500);
    let windows = scan_windows(&long);
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].chars().count(), MAX_SCAN_CHARS);
    assert_eq!(windows[1].chars().count(), 500);
    assert_eq!(windows.concat(), long, "no char dropped across windows");
}

#[tokio::test]
async fn pipeline_scans_past_the_cap_via_windows() {
    // Content hidden past MAX_SCAN_CHARS must still be caught —
    // padding can't push it out of a truncated window anymore.
    let scans = Arc::new(AtomicUsize::new(0));
    let p = pipeline_with("NEEDLE", ApplyTo::Input, scans.clone(), false);
    let text = "x".repeat(MAX_SCAN_CHARS + 100) + " NEEDLE";
    let v = p
        .scan(&text, Direction::Input)
        .await
        .unwrap()
        .expect("needle past the cap is still flagged");
    assert_eq!(v.provider, "marker-test");
    assert!(scans.load(Ordering::SeqCst) >= 2, "must scan >1 window");
}

#[test]
fn delta_text_scans_every_choice_not_just_first() {
    // A multi-choice frame must surface all choices' text, or a
    // later choice ships unscanned.
    let event = serde_json::json!({
        "choices": [
            { "delta": { "content": "safe intro " } },
            { "delta": { "content": "FORBIDDEN payload" } }
        ]
    });
    let text = delta_text_from_event(StreamDialect::OpenAiChat, &event)
        .expect("multi-choice frame has text");
    assert!(text.contains("safe intro"), "first choice: {text}");
    assert!(text.contains("FORBIDDEN payload"), "second choice: {text}");
}

#[tokio::test]
async fn sse_guard_scans_utf8_split_across_chunks() {
    // Regression: a multi-byte code point split across chunk
    // boundaries must be reconstructed before scanning. With the
    // old per-chunk lossy decode, the split char became U+FFFD, so
    // a flagged multi-byte phrase evaded moderation while the
    // client received the intact bytes. The marker is CJK so the
    // scan only flags if the code points survived the split.
    let scans = Arc::new(AtomicUsize::new(0));
    let p = Arc::new(pipeline_with("爆弾", ApplyTo::Output, scans, false));
    let full = sse_chunk("plan: 爆弾 now");
    let bytes = full.to_vec();
    let s = std::str::from_utf8(&bytes).unwrap();
    // Split one byte into the 3-byte '爆' code point.
    let split = s.find('爆').unwrap() + 1;
    let (a, b) = bytes.split_at(split);
    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::copy_from_slice(a)),
        Ok(Bytes::copy_from_slice(b)),
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
    assert!(
        out.contains("guardrail_blocked"),
        "reconstructed multi-byte text must be flagged, got: {out}"
    );
    assert!(
        !out.contains('\u{FFFD}'),
        "no replacement char should appear from a split code point: {out}"
    );
}

#[tokio::test]
async fn sse_guard_passes_clean_utf8_split_intact() {
    // The benign counterpart: a clean stream whose multi-byte char
    // is split must still be delivered byte-for-byte (no U+FFFD,
    // no drops) once reconstructed.
    let scans = Arc::new(AtomicUsize::new(0));
    let p = Arc::new(pipeline_with("NOPE", ApplyTo::Output, scans, false));
    let full = sse_chunk("café ☕ ready");
    let bytes = full.to_vec();
    let s = std::str::from_utf8(&bytes).unwrap();
    let split = s.find('☕').unwrap() + 1; // mid 3-byte code point
    let (a, b) = bytes.split_at(split);
    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::copy_from_slice(a)),
        Ok(Bytes::copy_from_slice(b)),
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
    assert!(out.contains("café ☕ ready"), "intact delivery: {out}");
    assert!(!out.contains('\u{FFFD}'), "no corruption: {out}");
}
