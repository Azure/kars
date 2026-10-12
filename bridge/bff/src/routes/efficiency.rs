// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Cross-harness efficiency from ownership-fenced terminal evidence and retained
// attempts. Legacy measurements remain descriptive history, not route authority.
//
// 2026 metric grounding (SWE-bench Verified resolve-rate, Terminal-Bench 2.0
// task-resolution, τ²/τ³-bench pass^k reliability + fault attribution): the
// honest question is not "did the model emit tokens" but "did it produce a
// human-ACCEPTED outcome, reliably, at what cost and latency". We therefore
// measure, per route:
//   • Outcome    — acceptance-rate (human approved) and pass^k reliability
//                  (fraction of REPEATED packages accepted on every attempt).
//   • Cost       — tokens/outcome always; USD/outcome when a price is configured.
//   • Effort     — rounds, tool-calls, and tool-FAILURE rate per outcome.
//   • Latency    — wall-clock and compute ms per run, and time-to-first-action.
// Honest: a route with no completed runs simply doesn't appear; a metric with
// no data (e.g. USD with no price table, pass^k with no repeats) is reported as
// absent, never fabricated.

use axum::{
    Json,
    extract::{Extension, State},
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::principal_can_view_all;
use crate::state::AppState;

#[derive(Debug, Serialize, Clone, Default)]
pub struct RouteEfficiency {
    /// The model/harness route (e.g. `azure-openai/openai/gpt-4o`).
    pub route: String,
    /// The harness (agent runtime) this route ran on, e.g. "OpenClaw",
    /// "Hermes". "" for legacy runs with no harness recorded. Distinguishes two
    /// rows with the same model but different harness.
    pub harness: String,
    /// Total runs observed on this route.
    pub runs: i64,
    /// Runs with validated durable terminal evidence; legacy history is separate
    /// from recommendation authority even when it shares a model name.
    pub verified_runs: i64,
    pub measured_token_runs: i64,
    pub measured_tokens: Option<i64>,
    /// Runs that produced a substantive deliverable (tokens spent, ok).
    pub delivered: i64,
    /// Delivery success rate (delivered / runs), 0..1.
    pub success_rate: f64,
    /// Runs whose deliverable a human ACCEPTED (review approved) — the honest
    /// outcome signal (not "emitted tokens").
    pub accepted: i64,
    /// Acceptance rate (accepted / runs), 0..1 — the real success metric.
    pub acceptance_rate: f64,
    /// Mean total tokens across delivered runs.
    pub avg_tokens: Option<i64>,
    /// Tokens spent per *successfully delivered* outcome — the cost-per-outcome
    /// efficiency metric (lower is better).
    pub tokens_per_outcome: Option<i64>,
    /// Mean model rounds per delivered run (orchestration efficiency).
    pub avg_rounds: Option<f64>,
    /// Mean tool calls per delivered run (orchestration efficiency).
    pub avg_tool_calls: Option<f64>,

    // ── 2026 enrichments ────────────────────────────────────────────────────
    /// Mean prompt (input) tokens per delivered run.
    pub avg_prompt_tokens: Option<i64>,
    /// Mean completion (output) tokens per delivered run.
    pub avg_completion_tokens: Option<i64>,
    /// Fraction of tool calls that FAILED (ok=false) across delivered runs,
    /// 0..1 — rework/effort signal. Lower is better.
    pub tool_fail_rate: Option<f64>,
    /// Mean terminal startedAt-to-finishedAt duration (ms); legacy trace span
    /// when available. Not model compute time or time spent in human review.
    pub avg_wall_ms: Option<i64>,
    /// p95 wall-clock duration (ms) across delivered runs — tail latency.
    pub p95_wall_ms: Option<i64>,
    /// Mean time-to-first-action (ms): latency of the first model round — how
    /// quickly the agent starts doing something.
    pub avg_ttfa_ms: Option<i64>,
    /// pass^k reliability, 0..1: among packages RUN MORE THAN ONCE on this
    /// route, the fraction whose EVERY attempt was accepted. `None` when no
    /// package has repeated here yet (can't claim reliability from one shot).
    pub reliability_rate: Option<f64>,
    /// The k behind `reliability_rate` — the minimum attempt-count among the
    /// repeated packages counted (so "pass^k" is truthful about k).
    pub reliability_k: Option<i64>,
    /// Number of repeated packages behind `reliability_rate` (the sample size).
    pub reliability_samples: i64,
    /// USD per measured delivered outcome — `None` unless a price is configured for this
    /// route's model (see `KARS_MODEL_PRICES`). Never a fabricated price.
    pub usd_per_outcome: Option<f64>,
    /// Fraction of prompt (input) tokens served from the provider cache across
    /// delivered runs, 0..1. Absent when cache measurements are unavailable.
    pub cache_hit_rate: Option<f64>,
    /// The most common fault among this route's UNACCEPTED runs (agent vs
    /// environment vs policy vs capacity), or "" when none/all accepted — the
    /// honest "why does this route miss" signal. See `classify_fault`.
    pub top_fault: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct EfficiencyDto {
    pub routes: Vec<RouteEfficiency>,
    /// The recommended route: the best balance of high acceptance + reliability
    /// + low cost among routes with at least one delivered run. `None` until at
    ///   least one route has delivered.
    pub recommended: Option<String>,
    /// The harness of the recommended route — so a proposal can adopt BOTH the
    /// model and the harness that actually won, not the model with a default
    /// harness. `None` when no route has delivered or the harness is unrecorded.
    pub recommended_harness: Option<String>,
    /// An honest, human-readable basis for the recommendation — the DEFENSIBLE
    /// reason (e.g. "Highest delivery success 100% (26/26)"), plus caveats when
    /// the acceptance / reliability signal is still sparse. `None` when nothing
    /// is recommended yet.
    pub recommended_basis: Option<String>,
    /// True when the recommendation rests on thin evidence (few human-accepted
    /// outcomes or too few repeated packages for reliability). The UI must
    /// present it as "best available so far", NOT a confident star.
    pub recommended_low_confidence: bool,
    /// Total runs across all routes (the sample size behind the frontier).
    pub total_runs: i64,
    /// True when a model price table is configured, so USD figures are present.
    pub priced: bool,
}

/// `GET /api/efficiency` — the cross-harness efficiency frontier.
pub async fn get_efficiency(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<EfficiencyDto>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let owner = (!principal_can_view_all(&principal)).then_some(principal.sub.as_str());
    Ok(Json(compute_efficiency_for_owner(cluster, owner).await))
}

/// One run's derived metrics — the honest per-run record, assembled from the
/// mission-output ConfigMap (aggregate) enriched with the per-round/per-tool
/// execution trace (latency, tool failures). A pure value so aggregation is
/// unit-testable without a cluster.
#[derive(Debug, Clone, Default)]
pub struct RunMetrics {
    pub route: String,
    /// The harness (agent runtime) dimension of the route, e.g. "OpenClaw",
    /// "Hermes". Pairs with `route` (model) so the frontier compares harness
    /// efficiency, not just model. "" for legacy runs with no harness recorded.
    pub harness: String,
    /// Normalized package identity (objective) — groups repeats for pass^k.
    pub package: String,
    pub delivered: bool,
    pub accepted: bool,
    pub verified: bool,
    pub total_tokens: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub rounds: Option<i64>,
    pub tool_calls: Option<i64>,
    pub tool_fail: Option<i64>,
    pub wall_ms: Option<i64>,
    pub ttfa_ms: Option<i64>,
    pub cached_tokens: Option<i64>,
    /// Fault attribution when the run wasn't accepted ("" when accepted / no signal).
    pub fault: String,
}

/// Per-model price ($/1M input tokens, $/1M output tokens), loaded from the
/// `KARS_MODEL_PRICES` env var (JSON object: model-substring → {"in":N,"out":N}).
/// A route is priced when its string contains a configured key. Absent config
/// means USD figures are simply not reported — never guessed.
fn load_price_table() -> BTreeMap<String, (f64, f64)> {
    let mut table = BTreeMap::new();
    let Ok(raw) = std::env::var("KARS_MODEL_PRICES") else {
        return table;
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) else {
        return table;
    };
    for (k, v) in map {
        let in_p = v.get("in").and_then(|x| x.as_f64());
        let out_p = v.get("out").and_then(|x| x.as_f64());
        if let (Some(i), Some(o)) = (in_p, out_p)
            && i.is_finite()
            && o.is_finite()
            && i >= 0.0
            && o >= 0.0
            && !k.is_empty()
        {
            table.insert(k.to_lowercase(), (i, o));
        }
    }
    table
}

/// Match a route to a configured price by substring (case-insensitive).
fn price_for<'a>(route: &str, table: &'a BTreeMap<String, (f64, f64)>) -> Option<&'a (f64, f64)> {
    let r = route.to_lowercase();
    table
        .iter()
        .find(|(k, _)| r.contains(k.as_str()))
        .map(|(_, v)| v)
}

/// Legacy exact objective identity, never a sensitive prefix in a response.
fn package_key(objective: &str) -> String {
    if objective.trim().is_empty() {
        String::new()
    } else {
        crate::providers::signing::sha256_hex(objective)
    }
}

/// Derive latency + tool-failure + cache facts from a mission's execution trace
/// (`trace.json`: an array of `round` and `tool` events). Returns
/// `(wall_ms, ttfa_ms, tool_fail, cached_tokens)`. Robust to a missing/garbled
/// trace. `cached_tokens` sums the prompt tokens served from the provider cache
/// across rounds (present only on traces from a cache-aware router).
pub fn derive_from_trace(trace_json: &str) -> (i64, i64, i64, i64) {
    let Ok(Value::Array(events)) = serde_json::from_str::<Value>(trace_json) else {
        return (0, 0, 0, 0);
    };
    let mut ttfa_ms = 0i64;
    let mut tool_fail = 0i64;
    let mut cached = 0i64;
    let mut first_ts: Option<i64> = None;
    let mut last_ts: Option<i64> = None;
    let mut last_ms = 0i64;
    let mut seen_round = false;
    for ev in &events {
        let kind = ev.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        let ms = ev.get("ms").and_then(|m| m.as_i64()).unwrap_or(0);
        if let Some(ts) = ev.get("ts").and_then(|t| t.as_str()).and_then(parse_ts_ms) {
            if first_ts.is_none() {
                first_ts = Some(ts);
            }
            last_ts = Some(ts);
            last_ms = ms;
        }
        match kind {
            "round" => {
                if !seen_round {
                    seen_round = true;
                    ttfa_ms = ms;
                }
                cached += ev
                    .get("cached_tokens")
                    .and_then(|c| c.as_i64())
                    .unwrap_or(0);
            }
            "tool" if ev.get("ok").and_then(|o| o.as_bool()) == Some(false) => {
                tool_fail += 1;
            }
            _ => {}
        }
    }
    // Wall-clock = span between first and last event timestamps, plus the last
    // event's own duration (the final round's latency lands after its ts).
    let wall_ms = match (first_ts, last_ts) {
        (Some(a), Some(b)) if b >= a => (b - a) + last_ms,
        _ => 0,
    };
    (wall_ms, ttfa_ms, tool_fail, cached)
}

/// Parse an RFC3339 timestamp to epoch milliseconds. `None` when unparseable.
fn parse_ts_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// The finish reason of the LAST model round in a trace (empty when none).
pub fn last_finish_reason(trace_json: &str) -> String {
    let Ok(Value::Array(events)) = serde_json::from_str::<Value>(trace_json) else {
        return String::new();
    };
    events
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("round"))
        .filter_map(|e| e.get("finish_reason").and_then(|f| f.as_str()))
        .next_back()
        .unwrap_or("")
        .to_string()
}

/// Deterministic fault attribution for a run that did NOT reach a human-accepted
/// outcome (τ-bench-style: agent vs environment vs policy). Derived only from
/// real signals — the last finish reason and observed tool failures — never a
/// guess. Accepted runs have no fault. Honest buckets:
///   • `environment` — tools/external systems failed (tool_fail present).
///   • `policy` — the provider blocked output (content filter / refusal).
///   • `capacity` — the model hit its length/token ceiling before finishing.
///   • `incomplete` — ended without a clear terminal reason (agent gave up).
///   • `""` — accepted, or no signal to attribute.
fn classify_fault(accepted: bool, tool_fail: i64, finish: &str) -> &'static str {
    if accepted {
        return "";
    }
    let f = finish.to_lowercase();
    if f.contains("content_filter") || f.contains("refus") || f.contains("safety") {
        "policy"
    } else if f.contains("length") || f.contains("max_tokens") || f.contains("token") {
        "capacity"
    } else if tool_fail > 0 {
        "environment"
    } else if !finish.is_empty()
        && f != "stop"
        && f != "end_turn"
        && f != "tool_calls"
        && f != "tool_use"
    {
        "incomplete"
    } else {
        // Terminated cleanly but the human didn't accept — a quality miss, not a
        // mechanical fault we can attribute from signals alone.
        ""
    }
}

/// Compute the cross-harness efficiency frontier from real per-run telemetry.
/// Shared by the `/api/efficiency` route and the orchestrators (mission +
/// team) so the LLM's recommendation is grounded in the same learned facts the
/// operator sees — never a separate heuristic.
pub async fn compute_efficiency(cluster: &crate::kars::cluster::Cluster) -> EfficiencyDto {
    compute_efficiency_for_owner(cluster, None).await
}

pub async fn compute_efficiency_for_owner(
    cluster: &crate::kars::cluster::Cluster,
    owner_subject: Option<&str>,
) -> EfficiencyDto {
    // Reuse the maintained ownership/UID/commit fence, including historical
    // revisions. Raw output maps omit ownerSub on modern producer records.
    let outputs = cluster
        .list_mission_deliverables()
        .await
        .unwrap_or_default();
    let mut runs: Vec<RunMetrics> = Vec::with_capacity(outputs.len());

    for record in &outputs {
        let task = &record.output.task_name;
        let data = &record.output.data;
        if owner_subject
            .is_some_and(|owner| data.get("ownerSub").map(String::as_str) != Some(owner))
        {
            continue;
        }
        let review = cluster.read_review(task).await.unwrap_or_default();
        let attempt = match (data.get("taskUid"), data.get("assignmentNonce")) {
            (Some(uid), Some(nonce)) if data.contains_key("evidence.json") => {
                cluster.read_mission_attempt(uid, nonce).await
            }
            _ => None,
        };
        let trace = if !data.contains_key("evidence.json") {
            cluster
                .read_mission_trace(&record.output.evidence_key)
                .await
        } else {
            None
        };
        runs.push(normalize_run(
            data,
            &review,
            attempt.as_ref(),
            trace.as_deref(),
        ));
    }

    aggregate(runs, &load_price_table())
}

/// Inputs come from the fenced deliverable reader. Modern terminal evidence
/// never falls back to unbound flattened fields or a legacy trace.
fn normalize_run(
    data: &BTreeMap<String, String>,
    review: &BTreeMap<String, String>,
    attempt: Option<&k8s_openapi::api::core::v1::ConfigMap>,
    trace: Option<&str>,
) -> RunMetrics {
    use crate::routes::tasks::run_evidence;
    let result = run_evidence::mission_result(data);
    let modern = data.contains_key("evidence.json") || data.contains_key("runtimeBootId");
    let valid = result.run_evidence.is_some();
    let accepted = result.reviewable && accepted_revision(data, review);
    let mut run = RunMetrics {
        route: result
            .model
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "unknown".into()),
        delivered: result.reviewable,
        accepted,
        verified: valid,
        total_tokens: result.total_tokens,
        prompt_tokens: result.prompt_tokens,
        completion_tokens: result.completion_tokens,
        ..Default::default()
    };
    if modern {
        let Some(evidence) = result.run_evidence else {
            return RunMetrics {
                route: "unknown".into(),
                ..Default::default()
            };
        };
        run.rounds = evidence.rounds;
        run.wall_ms = parse_ts_ms(&evidence.finished_at)
            .zip(parse_ts_ms(&evidence.started_at))
            .and_then(|(end, start)| end.checked_sub(start))
            .filter(|n| *n >= 0);
        if let Some(metrics) =
            attempt.and_then(|cm| run_evidence::efficiency::from_attempt(data, cm))
        {
            run.harness = metrics.harness;
            run.package = metrics.package;
            run.tool_calls = metrics.tool_calls;
            run.tool_fail = metrics.tool_fail;
        }
    } else {
        let number = |key: &str| {
            data.get(key)
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|n| (0..=9_007_199_254_740_991).contains(n))
        };
        run.harness = data.get("harness").cloned().unwrap_or_default();
        run.package = package_key(data.get("objective").map(String::as_str).unwrap_or(""));
        run.rounds = number("rounds");
        run.tool_calls = number("toolCalls");
        if let Some(trace) = trace
            && let Ok(Value::Array(events)) = serde_json::from_str::<Value>(trace)
            && !events.is_empty()
        {
            let (wall, ttfa, fails, cached) = derive_from_trace(trace);
            run.wall_ms = (wall > 0).then_some(wall);
            run.ttfa_ms = (ttfa > 0).then_some(ttfa);
            run.tool_fail = events
                .iter()
                .any(|e| e.get("kind").and_then(Value::as_str) == Some("tool"))
                .then_some(fails);
            let round_events: Vec<_> = events
                .iter()
                .filter(|e| e.get("kind").and_then(Value::as_str) == Some("round"))
                .collect();
            run.cached_tokens = (!round_events.is_empty()
                && round_events.iter().all(|e| {
                    e.get("cached_tokens")
                        .and_then(Value::as_i64)
                        .is_some_and(|n| n >= 0)
                }))
            .then_some(cached);
            run.fault = classify_fault(accepted, fails, &last_finish_reason(trace)).into();
        }
    }
    run
}

fn accepted_revision(data: &BTreeMap<String, String>, review: &BTreeMap<String, String>) -> bool {
    use crate::routes::review::{ReviewEntry, review_matches_output};
    if review_matches_output(data, review) {
        return review.get("status").map(String::as_str) == Some("approved");
    }
    let Some(nonce) = data
        .get("assignmentNonce")
        .filter(|nonce| !nonce.is_empty())
    else {
        return false;
    };
    if data
        .get("taskUid")
        .is_none_or(|uid| uid.is_empty() || review.get("taskUid") != Some(uid))
    {
        return false;
    }
    // The maintained review journal records the exact reviewed nonce. A later
    // revision must not erase that acceptance or transfer it to another Task.
    let history = review
        .get("history")
        .filter(|raw| raw.len() <= 1024 * 1024)
        .and_then(|raw| serde_json::from_str::<Vec<ReviewEntry>>(raw).ok())
        .unwrap_or_default();
    history
        .iter()
        .filter(|entry| entry.assignment_nonce.as_ref() == Some(nonce))
        .filter_map(|entry| parse_ts_ms(&entry.decided_at).map(|at| (at, entry)))
        .max_by_key(|(at, _)| *at)
        .is_some_and(|(_, entry)| entry.attested && entry.decision == "approve")
}

/// Pure aggregation of per-run metrics into the route frontier — unit-testable.
pub fn aggregate(runs: Vec<RunMetrics>, prices: &BTreeMap<String, (f64, f64)>) -> EfficiencyDto {
    #[derive(Default)]
    struct Acc {
        route: String,
        harness: String,
        runs: i64,
        verified: i64,
        delivered: i64,
        accepted: i64,
        tokens: i64,
        /// Delivered runs with measured usage (including a genuine zero).
        /// Missing usage never dilutes the measured average.
        tokens_n: i64,
        prompt: i64,
        completion: i64,
        split_n: i64,
        rounds: i64,
        rounds_n: i64,
        tool_calls: i64,
        tools_n: i64,
        tool_fail: i64,
        assessed_calls: i64,
        cached: i64,
        cache_prompt: i64,
        wall: Vec<i64>,
        ttfa: i64,
        ttfa_n: i64,
        // fault kind → count (among unaccepted runs).
        faults: BTreeMap<String, i64>,
        // package → attempts, each `accepted`.
        packages: BTreeMap<String, Vec<bool>>,
    }

    let mut by_route: BTreeMap<(String, String), Acc> = BTreeMap::new();
    let total_runs = runs.len() as i64;

    for r in &runs {
        let key = (r.route.clone(), r.harness.clone());
        let acc = by_route.entry(key).or_default();
        if acc.route.is_empty() {
            acc.route = r.route.clone();
            acc.harness = r.harness.clone();
        }
        acc.runs += 1;
        acc.verified += i64::from(r.verified);
        if r.accepted {
            acc.accepted += 1;
        }
        if !r.fault.is_empty() {
            *acc.faults.entry(r.fault.clone()).or_insert(0) += 1;
        }
        if r.delivered {
            acc.delivered += 1;
            if let Some(tokens) = r.total_tokens.filter(|n| *n >= 0) {
                acc.tokens += tokens;
                acc.tokens_n += 1;
            }
            if let Some((prompt, completion)) = r
                .prompt_tokens
                .zip(r.completion_tokens)
                .filter(|(p, c)| *p >= 0 && *c >= 0 && p.checked_add(*c) == r.total_tokens)
            {
                acc.prompt += prompt;
                acc.completion += completion;
                acc.split_n += 1;
            }
            if let Some(rounds) = r.rounds.filter(|n| *n >= 0) {
                acc.rounds += rounds;
                acc.rounds_n += 1;
            }
            if let Some(calls) = r.tool_calls.filter(|n| *n >= 0) {
                acc.tool_calls += calls;
                acc.tools_n += 1;
                if let Some(fails) = r.tool_fail.filter(|n| *n >= 0 && *n <= calls) {
                    acc.tool_fail += fails;
                    acc.assessed_calls += calls;
                }
            }
            if let Some((cached, prompt)) = r
                .cached_tokens
                .zip(r.prompt_tokens)
                .filter(|(c, p)| *c >= 0 && *c <= *p && *p > 0)
            {
                acc.cached += cached;
                acc.cache_prompt += prompt;
            }
            if let Some(wall) = r.wall_ms.filter(|n| *n >= 0) {
                acc.wall.push(wall);
            }
            if let Some(ttfa) = r.ttfa_ms.filter(|n| *n >= 0) {
                acc.ttfa += ttfa;
                acc.ttfa_n += 1;
            }
        }
        if !r.package.is_empty() {
            acc.packages
                .entry(r.package.clone())
                .or_default()
                .push(r.accepted);
        }
    }

    let mut routes: Vec<RouteEfficiency> = by_route
        .into_values()
        .map(|mut a| {
            let route = a.route.clone();
            // Each average uses its own measured population, never all deliveries.
            let tn = a.tokens_n.max(0);
            let tokens_per_outcome = (tn > 0).then(|| a.tokens / tn);
            let usd_per_outcome = price_for(&route, prices).and_then(|(in_p, out_p)| {
                if a.split_n == 0
                    || !in_p.is_finite()
                    || !out_p.is_finite()
                    || *in_p < 0.0
                    || *out_p < 0.0
                {
                    return None;
                }
                let cost = (a.prompt as f64 / 1_000_000.0) * in_p
                    + (a.completion as f64 / 1_000_000.0) * out_p;
                let cost = cost / a.split_n as f64;
                cost.is_finite().then_some(cost)
            });
            // pass^k reliability from repeated packages on this route.
            let repeated: Vec<&Vec<bool>> = a.packages.values().filter(|v| v.len() >= 2).collect();
            let (reliability_rate, reliability_k, reliability_samples) = if repeated.is_empty() {
                (None, None, 0)
            } else {
                let fully = repeated.iter().filter(|v| v.iter().all(|&x| x)).count();
                let k = repeated.iter().map(|v| v.len()).min().unwrap_or(2) as i64;
                (
                    Some(fully as f64 / repeated.len() as f64),
                    Some(k),
                    repeated.len() as i64,
                )
            };
            a.wall.sort_unstable();
            let avg_wall =
                (!a.wall.is_empty()).then(|| a.wall.iter().sum::<i64>() / a.wall.len() as i64);
            let p95_wall = (!a.wall.is_empty()).then(|| percentile(&a.wall, 0.95));

            RouteEfficiency {
                route,
                harness: a.harness.clone(),
                runs: a.runs,
                verified_runs: a.verified,
                measured_token_runs: tn,
                measured_tokens: (tn > 0).then_some(a.tokens),
                delivered: a.delivered,
                success_rate: if a.runs > 0 {
                    a.delivered as f64 / a.runs as f64
                } else {
                    0.0
                },
                accepted: a.accepted,
                acceptance_rate: if a.runs > 0 {
                    a.accepted as f64 / a.runs as f64
                } else {
                    0.0
                },
                avg_tokens: tokens_per_outcome,
                tokens_per_outcome,
                avg_rounds: (a.rounds_n > 0).then(|| a.rounds as f64 / a.rounds_n as f64),
                avg_tool_calls: (a.tools_n > 0).then(|| a.tool_calls as f64 / a.tools_n as f64),
                avg_prompt_tokens: (a.split_n > 0).then(|| a.prompt / a.split_n),
                avg_completion_tokens: (a.split_n > 0).then(|| a.completion / a.split_n),
                tool_fail_rate: (a.assessed_calls > 0)
                    .then(|| a.tool_fail as f64 / a.assessed_calls as f64),
                avg_wall_ms: avg_wall,
                p95_wall_ms: p95_wall,
                avg_ttfa_ms: (a.ttfa_n > 0).then(|| a.ttfa / a.ttfa_n),
                reliability_rate,
                reliability_k,
                reliability_samples,
                usd_per_outcome,
                cache_hit_rate: (a.cache_prompt > 0)
                    .then(|| a.cached as f64 / a.cache_prompt as f64),
                top_fault: a
                    .faults
                    .iter()
                    .max_by_key(|(_, n)| **n)
                    .map(|(k, _)| k.clone())
                    .unwrap_or_default(),
            }
        })
        .collect();

    // Sort the frontier: productive routes (any delivery) first, then best
    // ACCEPTANCE, then most reliable, then cheapest per outcome.
    routes.sort_by(|a, b| {
        (b.delivered > 0)
            .cmp(&(a.delivered > 0))
            .then(
                b.acceptance_rate
                    .partial_cmp(&a.acceptance_rate)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                b.reliability_rate
                    .unwrap_or(0.0)
                    .partial_cmp(&a.reliability_rate.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(b.success_rate.total_cmp(&a.success_rate))
            .then(
                a.tokens_per_outcome
                    .unwrap_or(i64::MAX)
                    .cmp(&b.tokens_per_outcome.unwrap_or(i64::MAX)),
            )
            .then(a.route.cmp(&b.route))
            .then(a.harness.cmp(&b.harness))
    });

    // Use the same lexicographic order as the displayed frontier. Unknown
    // costs never become cheap, and unverified history cannot select a route.
    let recommended_pick = routes.iter().find(|r| {
        r.delivered > 0
            && r.verified_runs == r.runs
            && r.route != "unknown"
            && !r.harness.is_empty()
            && r.measured_token_runs == r.delivered
            && r.tokens_per_outcome.is_some()
    });
    let recommended = recommended_pick.map(|r| r.route.clone());
    // The harness of the winning route — so compose can propose BOTH the model
    // and the harness that actually won (not the model with a default harness).
    let recommended_harness = recommended_pick
        .map(|r| r.harness.clone())
        .filter(|h| !h.is_empty());

    // The basis describes the actual ordering and the exact winning pair.
    let (recommended_basis, recommended_low_confidence) = recommended_pick
        .map(|r| {
            let accepted = r.accepted;
            let rel_samples = r.reliability_samples;
            let low_conf = accepted < 3 || rel_samples < 3 || r.acceptance_rate < 0.5;
            let mut basis = format!(
                "Best available measured route by acceptance, repeated-package reliability, delivery, then token cost: {} on {} — {}/{} accepted, {}/{} delivered",
                r.route, r.harness, r.accepted, r.runs,
                r.delivered,
                r.runs
            );
            if accepted < 3 {
                basis.push_str(&format!(
                    ". Human acceptance is still low-signal ({} of {} deliverables approved) — treat as best-available, not proven-best",
                    accepted, r.delivered
                ));
            }
            if rel_samples < 3 {
                basis.push_str(&format!(
                    ". Reliability (pass^k) has only {} repeated package(s) so far",
                    rel_samples
                ));
            }
            basis.push('.');
            (Some(basis), low_conf)
        })
        .unwrap_or((None, false));

    EfficiencyDto {
        priced: !prices.is_empty(),
        routes,
        recommended,
        recommended_harness,
        recommended_basis,
        recommended_low_confidence,
        total_runs,
    }
}

/// Nearest-rank percentile of a pre-sorted slice. Empty → 0.
fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests;
