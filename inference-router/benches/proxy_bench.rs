// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Inference-router proxy hot-path bench (Phase 2 S16).
//!
//! Captures the proxy overhead per request — routing decision, auth-header
//! attach, and safety-evaluator dispatch — without making an upstream
//! call. Production p99 must stay under 5ms; this bench is the regression
//! tripwire.
//!
//! As with the controller bench, we do not stand up a real upstream
//! (would couple the bench to network noise). Instead we exercise the
//! deterministic in-process work that dominates the cold-path metric.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Simulated route lookup: a small static table mapping path prefixes to
/// upstream slot indices. Mirrors the `match path` ladder in the router's
/// `routes/mod.rs` plus the path-rewrite step.
fn route_lookup(path: &str) -> Option<usize> {
    if path.starts_with("/openai/") {
        Some(0)
    } else if path.starts_with("/agt/relay") {
        Some(1)
    } else if path.starts_with("/mcp/") {
        Some(2)
    } else if path.starts_with("/a2a/") {
        Some(3)
    } else if path.starts_with("/healthz") {
        Some(4)
    } else {
        None
    }
}

/// Build an upstream header set the way `proxy::build_upstream_headers`
/// does in production: clone request headers, inject `api-key` /
/// `Authorization`, drop hop-by-hop headers.
fn attach_auth_headers(req: &[(&str, &str)], token: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = req
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.to_ascii_lowercase().as_str(),
                "host" | "connection" | "content-length"
            )
        })
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    out.push(("api-key".to_string(), token.to_string()));
    out.push(("authorization".to_string(), format!("Bearer {token}")));
    out
}

/// Stub safety evaluator: returns `true` (allowed) iff the body does not
/// contain any of a small blocked-substring set. Mirrors the
/// `safety::evaluate` quick-path used before invoking Content Safety.
fn safety_quick_check(body: &[u8]) -> bool {
    const BAD: &[&[u8]] = &[b"<script>", b"DROP TABLE", b"$(curl"];
    !BAD.iter().any(|b| body.windows(b.len()).any(|w| w == *b))
}

fn bench_proxy(c: &mut Criterion) {
    let mut group = c.benchmark_group("proxy_overhead");
    let headers: Vec<(&str, &str)> = vec![
        ("content-type", "application/json"),
        ("user-agent", "openclaw/0.1"),
        ("accept", "application/json"),
        ("host", "ignored.invalid"),
    ];
    let body = br#"{"messages":[{"role":"user","content":"hello world"}]}"#;

    group.bench_function("route_lookup", |b| {
        b.iter(|| route_lookup(black_box("/openai/v1/chat/completions")));
    });
    group.bench_function("attach_auth_headers", |b| {
        b.iter(|| attach_auth_headers(black_box(&headers), black_box("test-token-zzz")));
    });
    group.bench_function("safety_quick_check", |b| {
        b.iter(|| safety_quick_check(black_box(body)));
    });
    group.finish();
}

criterion_group!(benches, bench_proxy);
criterion_main!(benches);
