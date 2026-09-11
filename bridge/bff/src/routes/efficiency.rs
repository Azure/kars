// kars Bridge BFF — cross-harness efficiency frontier (design note §3B, Pillar
// B). Built entirely from the REAL per-run telemetry the router captures on
// every mission run and the controller persists (mission-output ConfigMap +
// the per-round/per-tool execution trace). Groups runs by the model/harness
// route they took and computes the 2026 efficiency facts that let an operator
// see — and the orchestrator recommend — the best governed route per outcome.
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
    pub avg_tokens: i64,
    /// Tokens spent per *successfully delivered* outcome — the cost-per-outcome
    /// efficiency metric (lower is better).
    pub tokens_per_outcome: i64,
    /// Mean model rounds per delivered run (orchestration efficiency).
    pub avg_rounds: f64,
    /// Mean tool calls per delivered run (orchestration efficiency).
    pub avg_tool_calls: f64,

    // ── 2026 enrichments ────────────────────────────────────────────────────
    /// Mean prompt (input) tokens per delivered run.
    pub avg_prompt_tokens: i64,
    /// Mean completion (output) tokens per delivered run.
    pub avg_completion_tokens: i64,
    /// Fraction of tool calls that FAILED (ok=false) across delivered runs,
    /// 0..1 — rework/effort signal. Lower is better.
    pub tool_fail_rate: f64,
    /// Mean wall-clock duration (ms) of a delivered run — first trace event to
    /// last. The latency a human actually waits (minus human-approval waits).
    pub avg_wall_ms: i64,
    /// p95 wall-clock duration (ms) across delivered runs — tail latency.
    pub p95_wall_ms: i64,
    /// Mean time-to-first-action (ms): latency of the first model round — how
    /// quickly the agent starts doing something.
    pub avg_ttfa_ms: i64,
    /// pass^k reliability, 0..1: among packages RUN MORE THAN ONCE on this
    /// route, the fraction whose EVERY attempt was accepted. `None` when no
    /// package has repeated here yet (can't claim reliability from one shot).
    pub reliability_rate: Option<f64>,
    /// The k behind `reliability_rate` — the minimum attempt-count among the
    /// repeated packages counted (so "pass^k" is truthful about k).
    pub reliability_k: Option<i64>,
    /// Number of repeated packages behind `reliability_rate` (the sample size).
    pub reliability_samples: i64,
    /// USD per accepted outcome — `None` unless a price is configured for this
    /// route's model (see `KARS_MODEL_PRICES`). Never a fabricated price.
    pub usd_per_outcome: Option<f64>,
    /// Fraction of prompt (input) tokens served from the provider cache across
    /// delivered runs, 0..1 — higher means cheaper input. 0 when the trace has
    /// no cache info (older runs / cache-unaware router).
    pub cache_hit_rate: f64,
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
    pub total_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub rounds: i64,
    pub tool_calls: i64,
    pub tool_fail: i64,
    pub wall_ms: i64,
    pub ttfa_ms: i64,
    pub cached_tokens: i64,
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
        if let (Some(i), Some(o)) = (in_p, out_p) {
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

/// Normalize an objective into a stable package key for pass^k grouping:
/// lowercased, whitespace-collapsed, length-bounded. Empty when no objective.
fn package_key(objective: &str) -> String {
    let collapsed = objective
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    collapsed.chars().take(200).collect()
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
    let outputs = cluster.list_mission_output_evidence().await;
    let mut runs: Vec<RunMetrics> = Vec::with_capacity(outputs.len());

    for record in &outputs {
        let task = &record.task_name;
        let data = &record.data;
        if owner_subject
            .is_some_and(|owner| data.get("ownerSub").map(String::as_str) != Some(owner))
        {
            continue;
        }
        let route = data
            .get("model")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let harness = data.get("harness").cloned().unwrap_or_default();
        let total_tokens = data
            .get("totalTokens")
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0);
        let prompt_tokens = data
            .get("promptTokens")
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0);
        let completion_tokens = data
            .get("completionTokens")
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0);
        let ok = data.get("status").map(String::as_str) == Some("ok");
        let artifacts = data
            .get("artifactCount")
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0);
        // "accepted" is the HONEST outcome signal: a human approved the
        // deliverable (review status), not merely that the model emitted tokens.
        let assignment_identity = data
            .get("assignmentNonce")
            .cloned()
            .unwrap_or_else(|| record.evidence_key.clone());
        let task_review = cluster.read_review(task).await.unwrap_or_default();
        let review = if task_review.get("assignmentNonce") == Some(&assignment_identity) {
            task_review
        } else {
            Default::default()
        };
        let accepted = review.get("status").map(String::as_str) == Some("approved");
        // A run a human APPROVED was necessarily delivered — otherwise there'd be
        // nothing to approve. Folding `accepted` into `delivered` keeps the funnel
        // coherent (accepted ⊆ delivered) and stops "1 accepted, 0 delivered"
        // when a delivered run under-reports telemetry (e.g. tokens==0 &&
        // artifacts==0 on a harness whose token accounting lagged).
        let delivered = accepted || (ok && (total_tokens > 0 || artifacts > 0));
        let rounds = data
            .get("rounds")
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0);
        let tool_calls = data
            .get("toolCalls")
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0);
        let package = package_key(data.get("objective").map(String::as_str).unwrap_or(""));

        // Enrich with latency + tool-failure + cache from the execution trace.
        let trace = cluster.read_mission_trace(&record.evidence_key).await;
        let (wall_ms, ttfa_ms, tool_fail, cached_tokens) = match &trace {
            Some(t) => derive_from_trace(t),
            None => (0, 0, 0, 0),
        };
        let finish = trace.as_deref().map(last_finish_reason).unwrap_or_default();
        let fault = classify_fault(accepted, tool_fail, &finish).to_string();

        runs.push(RunMetrics {
            route,
            harness,
            package,
            delivered,
            accepted,
            total_tokens,
            prompt_tokens,
            completion_tokens,
            rounds,
            tool_calls,
            tool_fail,
            wall_ms,
            ttfa_ms,
            cached_tokens,
            fault,
        });
    }

    aggregate(runs, &load_price_table())
}

/// Pure aggregation of per-run metrics into the route frontier — unit-testable.
pub fn aggregate(runs: Vec<RunMetrics>, prices: &BTreeMap<String, (f64, f64)>) -> EfficiencyDto {
    #[derive(Default)]
    struct Acc {
        route: String,
        harness: String,
        runs: i64,
        delivered: i64,
        accepted: i64,
        tokens: i64,
        /// Delivered runs that actually REPORTED token usage (> 0). Token
        /// averages divide by this, not `delivered`, so a delivered run whose
        /// harness under-reported tokens (0) can't dilute the per-run cost and
        /// make a route look artificially cheap.
        tokens_n: i64,
        prompt: i64,
        completion: i64,
        rounds: i64,
        tool_calls: i64,
        tool_fail: i64,
        cached: i64,
        wall: Vec<i64>,
        ttfa: i64,
        ttfa_n: i64,
        // fault kind → count (among unaccepted runs).
        faults: BTreeMap<String, i64>,
        // package → attempts, each `accepted`.
        packages: BTreeMap<String, Vec<bool>>,
    }

    // Group by the composite (model × harness) route so harness efficiency is
    // measurable. The map key joins the two with a unit separator that can't
    // appear in a model/harness name.
    let mut by_route: BTreeMap<String, Acc> = BTreeMap::new();
    let total_runs = runs.len() as i64;

    for r in &runs {
        let key = format!("{}\u{1}{}", r.route, r.harness);
        let acc = by_route.entry(key).or_default();
        if acc.route.is_empty() {
            acc.route = r.route.clone();
            acc.harness = r.harness.clone();
        }
        acc.runs += 1;
        if r.accepted {
            acc.accepted += 1;
        }
        if !r.fault.is_empty() {
            *acc.faults.entry(r.fault.clone()).or_insert(0) += 1;
        }
        if r.delivered {
            acc.delivered += 1;
            acc.tokens += r.total_tokens;
            if r.total_tokens > 0 {
                acc.tokens_n += 1;
            }
            acc.prompt += r.prompt_tokens;
            acc.completion += r.completion_tokens;
            acc.rounds += r.rounds;
            acc.tool_calls += r.tool_calls;
            acc.tool_fail += r.tool_fail;
            acc.cached += r.cached_tokens;
            if r.wall_ms > 0 {
                acc.wall.push(r.wall_ms);
            }
            if r.ttfa_ms > 0 {
                acc.ttfa += r.ttfa_ms;
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
            let d = a.delivered.max(0);
            // Token averages use the count of runs that actually reported tokens
            // (tokens_n), not delivered (d), so zero-token deliveries don't dilute
            // the per-outcome cost. Rounds/tool-calls still divide by d.
            let tn = a.tokens_n.max(0);
            let tokens_per_outcome = if tn > 0 { a.tokens / tn } else { 0 };
            let usd_per_outcome = price_for(&route, prices).and_then(|(in_p, out_p)| {
                if tn == 0 {
                    return None;
                }
                let cost = (a.prompt as f64 / 1_000_000.0) * in_p
                    + (a.completion as f64 / 1_000_000.0) * out_p;
                Some(cost / tn as f64)
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
            let avg_wall = if a.wall.is_empty() {
                0
            } else {
                a.wall.iter().sum::<i64>() / a.wall.len() as i64
            };
            let p95_wall = percentile(&a.wall, 0.95);

            RouteEfficiency {
                route,
                harness: a.harness.clone(),
                runs: a.runs,
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
                avg_tokens: if tn > 0 { a.tokens / tn } else { 0 },
                tokens_per_outcome,
                avg_rounds: if d > 0 {
                    a.rounds as f64 / d as f64
                } else {
                    0.0
                },
                avg_tool_calls: if d > 0 {
                    a.tool_calls as f64 / d as f64
                } else {
                    0.0
                },
                avg_prompt_tokens: if tn > 0 { a.prompt / tn } else { 0 },
                avg_completion_tokens: if tn > 0 { a.completion / tn } else { 0 },
                tool_fail_rate: if a.tool_calls > 0 {
                    a.tool_fail as f64 / a.tool_calls as f64
                } else {
                    0.0
                },
                avg_wall_ms: avg_wall,
                p95_wall_ms: p95_wall,
                avg_ttfa_ms: if a.ttfa_n > 0 { a.ttfa / a.ttfa_n } else { 0 },
                reliability_rate,
                reliability_k,
                reliability_samples,
                usd_per_outcome,
                cache_hit_rate: if a.prompt > 0 {
                    (a.cached as f64 / a.prompt as f64).min(1.0)
                } else {
                    0.0
                },
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
            .then(a.tokens_per_outcome.cmp(&b.tokens_per_outcome))
    });

    // Recommend on the HONEST signal: acceptance dominates; reliability
    // (pass^k) rewards consistency; cost-per-outcome penalizes. Fall back to
    // delivery only when no route has any accepted run yet.
    let max_cost = routes
        .iter()
        .filter(|r| r.delivered > 0)
        .map(|r| r.tokens_per_outcome)
        .max()
        .unwrap_or(1)
        .max(1);
    let recommended_pick = routes
        .iter()
        .filter(|r| r.delivered > 0)
        .map(|r| {
            let cost_norm = r.tokens_per_outcome as f64 / max_cost as f64;
            let reliability = r.reliability_rate.unwrap_or(0.0);
            // Acceptance dominates; reliability and delivery reward; cost penalizes.
            let score =
                r.acceptance_rate + 0.3 * reliability + 0.2 * r.success_rate - 0.5 * cost_norm;
            (r.route.clone(), r.harness.clone(), score)
        })
        .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    let recommended = recommended_pick.as_ref().map(|(route, _, _)| route.clone());
    // The harness of the winning route — so compose can propose BOTH the model
    // and the harness that actually won (not the model with a default harness).
    let recommended_harness = recommended_pick
        .as_ref()
        .map(|(_, h, _)| h.clone())
        .filter(|h| !h.is_empty());

    // Honest basis + confidence. The DEFENSIBLE reason is delivery success; the
    // ideal signal (human acceptance) and pass^k reliability are often sparse
    // early, so we state them with sample sizes and flag low confidence rather
    // than star-recommending a route whose headline acceptance reads as 4%.
    let (recommended_basis, recommended_low_confidence) = recommended
        .as_ref()
        .and_then(|route| routes.iter().find(|r| &r.route == route))
        .map(|r| {
            let accepted = r.accepted;
            let rel_samples = r.reliability_samples;
            let low_conf = accepted < 3 || rel_samples < 3;
            let mut basis = format!(
                "Highest delivery success — {:.0}% ({}/{} runs delivered)",
                r.success_rate * 100.0,
                r.delivered,
                r.runs
            );
            if accepted < 3 {
                basis.push_str(&format!(
                    ". Human acceptance is still low-signal ({} of {} deliverables reviewed) — treat as best-available, not proven-best",
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
mod tests {
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
}
