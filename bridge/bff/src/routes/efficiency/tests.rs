use super::*;

fn run(
    route: &str,
    pkg: &str,
    accepted: bool,
    tokens: i64,
    wall: i64,
    ttfa: i64,
    fail: i64,
) -> RunMetrics {
    RunMetrics {
        route: route.into(),
        harness: "OpenClaw".into(),
        package: pkg.into(),
        delivered: tokens > 0,
        accepted,
        total_tokens: tokens,
        prompt_tokens: tokens / 2,
        completion_tokens: tokens / 2,
        rounds: 3,
        tool_calls: 4,
        tool_fail: fail,
        wall_ms: wall,
        ttfa_ms: ttfa,
        cached_tokens: 0,
        fault: String::new(),
    }
}

#[test]
fn derive_from_trace_computes_wall_ttfa_and_fails() {
    let trace = r#"[
          {"kind":"round","ms":800,"ts":"2026-07-02T10:00:00Z","cached_tokens":100},
          {"kind":"tool","ms":0,"ok":true,"ts":"2026-07-02T10:00:00Z"},
          {"kind":"tool","ms":0,"ok":false,"ts":"2026-07-02T10:00:01Z"},
          {"kind":"round","ms":1200,"ts":"2026-07-02T10:00:05Z","cached_tokens":200}
        ]"#;
    let (wall, ttfa, fail, cached) = derive_from_trace(trace);
    assert_eq!(ttfa, 800, "TTFA is the first round latency");
    assert_eq!(fail, 1, "one failed tool");
    // span 0s→5s = 5000ms + last round ms 1200
    assert_eq!(wall, 6200);
    assert_eq!(cached, 300, "cached tokens summed across rounds");
}

#[test]
fn derive_from_trace_is_robust_to_garbage() {
    assert_eq!(derive_from_trace("not json"), (0, 0, 0, 0));
    assert_eq!(derive_from_trace("{}"), (0, 0, 0, 0));
}

#[test]
fn cache_hit_rate_from_cached_tokens() {
    let mut r = run("A", "p1", true, 2000, 5000, 500, 0); // prompt = 1000
    r.cached_tokens = 800;
    let dto = aggregate(vec![r], &BTreeMap::new());
    assert!(
        (dto.routes[0].cache_hit_rate - 0.8).abs() < 1e-6,
        "800/1000 cached"
    );
}

#[test]
fn fault_classification_is_deterministic() {
    assert_eq!(classify_fault(true, 5, "length"), "", "accepted → no fault");
    assert_eq!(classify_fault(false, 0, "content_filter"), "policy");
    assert_eq!(classify_fault(false, 0, "length"), "capacity");
    assert_eq!(
        classify_fault(false, 3, "stop"),
        "environment",
        "tool failures → environment"
    );
    assert_eq!(
        classify_fault(false, 0, "stop"),
        "",
        "clean stop but unaccepted → quality miss, no mechanical fault"
    );
    assert_eq!(classify_fault(false, 0, ""), "", "no signal → unattributed");
}

#[test]
fn top_fault_is_the_dominant_one() {
    let mut r1 = run("A", "p1", false, 1000, 100, 50, 2);
    r1.fault = "environment".into();
    let mut r2 = run("A", "p2", false, 1000, 100, 50, 0);
    r2.fault = "environment".into();
    let mut r3 = run("A", "p3", false, 1000, 100, 50, 0);
    r3.fault = "capacity".into();
    let dto = aggregate(vec![r1, r2, r3], &BTreeMap::new());
    assert_eq!(dto.routes[0].top_fault, "environment");
}

#[test]
fn last_finish_reason_picks_final_round() {
    let trace = r#"[
          {"kind":"round","finish_reason":"tool_calls"},
          {"kind":"tool","ok":true},
          {"kind":"round","finish_reason":"length"}
        ]"#;
    assert_eq!(last_finish_reason(trace), "length");
    assert_eq!(last_finish_reason("garbage"), "");
}

#[test]
fn passk_reliability_only_counts_repeated_packages() {
    // route A: package p1 run twice (both accepted) → reliable; p2 once (ignored).
    let runs = vec![
        run("A", "p1", true, 1000, 5000, 500, 0),
        run("A", "p1", true, 1100, 5200, 400, 0),
        run("A", "p2", true, 900, 4000, 300, 0),
    ];
    let dto = aggregate(runs, &BTreeMap::new());
    let a = dto.routes.iter().find(|r| r.route == "A").unwrap();
    assert_eq!(a.reliability_samples, 1, "only p1 repeated");
    assert_eq!(a.reliability_rate, Some(1.0));
    assert_eq!(a.reliability_k, Some(2));
}

#[test]
fn passk_flags_inconsistent_package() {
    // p1 accepted once, rejected once → NOT fully reliable.
    let runs = vec![
        run("A", "p1", true, 1000, 5000, 500, 0),
        run("A", "p1", false, 1100, 5200, 400, 2),
    ];
    let dto = aggregate(runs, &BTreeMap::new());
    let a = dto.routes.iter().find(|r| r.route == "A").unwrap();
    assert_eq!(
        a.reliability_rate,
        Some(0.0),
        "inconsistent package fails pass^k"
    );
    assert_eq!(a.reliability_samples, 1);
}

#[test]
fn usd_per_outcome_only_when_priced() {
    let runs = vec![run("gpt-4o", "p1", true, 2_000_000, 5000, 500, 0)];
    // No prices → None.
    let dto = aggregate(runs.clone(), &BTreeMap::new());
    assert!(dto.routes[0].usd_per_outcome.is_none());
    assert!(!dto.priced);
    // Priced: 1M prompt @ $2.5 + 1M completion @ $10 = $12.5 over 1 outcome.
    let mut prices = BTreeMap::new();
    prices.insert("gpt-4o".to_string(), (2.5, 10.0));
    let dto = aggregate(runs, &prices);
    assert!(dto.priced);
    let usd = dto.routes[0].usd_per_outcome.unwrap();
    assert!((usd - 12.5).abs() < 1e-6, "got {usd}");
}

#[test]
fn tool_fail_rate_computed() {
    let runs = vec![run("A", "p1", true, 1000, 5000, 500, 2)]; // 2 fails of 4 calls
    let dto = aggregate(runs, &BTreeMap::new());
    assert!((dto.routes[0].tool_fail_rate - 0.5).abs() < 1e-6);
}
