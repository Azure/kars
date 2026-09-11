// kars Bridge BFF — Insights / efficiency metrics API.
//
// HONESTY CONTRACT (design note §24, UX honesty grammar):
// This endpoint returns only facts derivable from live cluster state — mission
// status distribution, tier mix, decision counts, receipt/log sizes. Runtime
// efficiency numbers (token cost, latency split, harness comparison) require a
// real agent run through the inference router, which a local kind cluster
// without an AI Foundry endpoint does not produce. Rather than fabricate zeros
// that imply measurement, we report `runtime_metrics_available: false` and the
// web layer renders the explicit "appears after a real mission runs" state.

use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::core::DynamicObject;
use serde::Serialize;
use serde_json::Value;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::{principal_can_view_all, require_owned_task};
use crate::state::AppState;

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}
fn upstream(e: kube::Error) -> AppError {
    AppError::Upstream(e.to_string())
}
fn spec(o: &DynamicObject) -> &Value {
    o.data.get("spec").unwrap_or(&Value::Null)
}
fn status(o: &DynamicObject) -> &Value {
    o.data.get("status").unwrap_or(&Value::Null)
}

#[derive(Debug, Serialize, Default)]
pub struct CountPair {
    pub label: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct InsightsDto {
    /// Missions (tasks) grouped by governance phase.
    pub missions_by_phase: Vec<CountPair>,
    /// Missions grouped by autonomy tier (1..5).
    pub missions_by_tier: Vec<CountPair>,
    /// Human decisions grouped by outcome.
    pub decisions: Vec<CountPair>,
    /// Total launched (executing) missions.
    pub launched: i64,
    /// Governance receipts issued.
    pub receipts_issued: i64,
    /// Size of the hash-chained inclusion log (entries).
    pub inclusion_log_size: i64,
    /// Delegated children rejected for amplifying authority (a safety signal).
    pub amplification_rejections: i64,
    /// Whether runtime efficiency metrics (token cost, latency) are available.
    /// False on a cluster with no real inference runs — drives the honest
    /// "needs a real run" empty state instead of fabricated zeros.
    pub runtime_metrics_available: bool,
    /// Human-readable reason runtime metrics are unavailable, when they are.
    pub runtime_metrics_note: Option<String>,
}

async fn gather_insights(
    cluster: &crate::kars::cluster::Cluster,
    owner_subject: Option<&str>,
) -> AppResult<InsightsDto> {
    let mut tasks = cluster.list_kind_all("KarsTask").await.map_err(upstream)?;
    let mut approvals = cluster
        .list_kind_all("KarsApproval")
        .await
        .map_err(upstream)?;
    let mut receipts = cluster
        .list_kind_all("KarsReceipt")
        .await
        .map_err(upstream)?;
    if let Some(owner) = owner_subject {
        let owns = |object: &DynamicObject| {
            object
                .metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("kars.azure.com/owner-sub"))
                .is_some_and(|subject| subject == owner)
        };
        tasks.retain(owns);
        approvals.retain(owns);
        let owned_tasks: std::collections::BTreeSet<String> = tasks
            .iter()
            .filter_map(|task| task.metadata.name.clone())
            .collect();
        receipts.retain(|receipt| {
            spec(receipt)
                .get("taskRef")
                .and_then(|reference| reference.get("name"))
                .and_then(|name| name.as_str())
                .is_some_and(|name| owned_tasks.contains(name))
        });
    }

    // Missions by phase.
    let mut phase_counts: std::collections::BTreeMap<String, i64> = Default::default();
    let mut tier_counts: std::collections::BTreeMap<i64, i64> = Default::default();
    let mut launched = 0i64;
    let mut amplification_rejections = 0i64;
    for t in &tasks {
        let phase = status(t)
            .get("phase")
            .and_then(|p| p.as_str())
            .unwrap_or("Pending")
            .to_string();
        *phase_counts.entry(phase).or_default() += 1;
        if let Some(tier) = spec(t)
            .get("envelope")
            .and_then(|e| e.get("tier"))
            .and_then(|x| x.as_i64())
        {
            *tier_counts.entry(tier).or_default() += 1;
        }
        let exec = status(t)
            .get("executionPhase")
            .and_then(|p| p.as_str())
            .unwrap_or("Idle");
        if exec != "Idle" {
            launched += 1;
        }
        // A degraded child whose Ready condition cites amplification is a
        // safety win worth surfacing.
        if let Some(conds) = status(t).get("conditions").and_then(|c| c.as_array()) {
            for c in conds {
                let msg = c.get("message").and_then(|m| m.as_str()).unwrap_or("");
                if msg.to_ascii_lowercase().contains("amplif") {
                    amplification_rejections += 1;
                }
            }
        }
    }

    // Decisions by outcome.
    let mut decision_counts: std::collections::BTreeMap<String, i64> = Default::default();
    for a in &approvals {
        let phase = status(a)
            .get("phase")
            .and_then(|p| p.as_str())
            .unwrap_or("Pending")
            .to_string();
        *decision_counts.entry(phase).or_default() += 1;
    }

    let log = cluster
        .receipt_log()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let inclusion_log_size = if owner_subject.is_some() {
        let visible: std::collections::BTreeSet<String> = receipts
            .iter()
            .filter_map(|receipt| {
                Some(format!(
                    "{}/{}",
                    receipt.metadata.namespace.as_ref()?,
                    receipt.metadata.name.as_ref()?
                ))
            })
            .collect();
        log.entries
            .iter()
            .filter(|entry| visible.contains(&entry.receipt))
            .count() as i64
    } else {
        log.entries.len() as i64
    };

    let to_pairs = |m: std::collections::BTreeMap<String, i64>| -> Vec<CountPair> {
        m.into_iter()
            .map(|(label, count)| CountPair { label, count })
            .collect()
    };

    // Runtime metrics ARE surfaced now: the efficiency frontier derives real
    // per-run token cost (from mission-output) and latency/cache/tool-fail (from
    // the execution trace). Report availability honestly from the data — true
    // once at least one delivered run carries real token telemetry.
    let outputs = cluster.list_mission_output_evidence().await;
    let runtime_available = outputs.iter().any(|record| {
        record
            .data
            .get("totalTokens")
            .and_then(|t| t.parse::<i64>().ok())
            .is_some_and(|t| t > 0)
    });

    Ok(InsightsDto {
        missions_by_phase: to_pairs(phase_counts),
        // Always emit all five autonomy tiers (1..=5), zero-filled, so the
        // "Autonomy mix" chart shows the full ladder and a missing tier reads as
        // "none granted" rather than silently vanishing from the axis.
        missions_by_tier: (1..=5)
            .map(|t| CountPair {
                label: format!("Tier {t}"),
                count: tier_counts.get(&t).copied().unwrap_or(0),
            })
            .collect(),
        decisions: to_pairs(decision_counts),
        launched,
        receipts_issued: receipts.len() as i64,
        inclusion_log_size,
        amplification_rejections,
        runtime_metrics_available: runtime_available,
        runtime_metrics_note: if runtime_available {
            None
        } else {
            Some(
                "Token cost and latency appear here once a mission runs and delivers — nothing is faked before then.".to_string(),
            )
        },
    })
}

/// `GET /api/insights` — fleet-wide efficiency + governance insights.
pub async fn get_insights(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<InsightsDto>> {
    let cluster = require_cluster(&state)?;
    let owner = (!principal_can_view_all(&principal)).then_some(principal.sub.as_str());
    Ok(Json(gather_insights(cluster, owner).await?))
}

// ─── Per-mission scorecard ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ScorecardDto {
    pub task: String,
    pub namespace: String,
    pub tier: Option<i64>,
    pub launched: bool,
    pub execution_phase: Option<String>,
    /// Budget ceiling (tokens) from the envelope, when set.
    pub token_budget: Option<i64>,
    /// Number of human decisions recorded for this mission.
    pub decisions_recorded: i64,
    pub approvals_granted: i64,
    pub approvals_denied: i64,
    /// Whether a signed receipt exists for this mission.
    pub receipt_issued: bool,
    /// Real tokens consumed by the latest captured mission run, when one exists.
    pub run_total_tokens: Option<i64>,
    pub run_prompt_tokens: Option<i64>,
    pub run_completion_tokens: Option<i64>,
    pub run_model: Option<String>,
    /// Runtime efficiency availability (see InsightsDto).
    pub runtime_metrics_available: bool,
    pub runtime_metrics_note: Option<String>,
}

/// `GET /api/namespaces/:ns/tasks/:name/scorecard` — per-mission efficiency.
pub async fn get_scorecard(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<ScorecardDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_task(cluster, &ns, &name, &principal).await?;
    let task = cluster
        .get_kind(&ns, "KarsTask", &name)
        .await
        .map_err(upstream)?
        .ok_or(AppError::NotFound)?;

    let approvals = cluster
        .list_kind(&ns, "KarsApproval")
        .await
        .map_err(upstream)?;
    let mut granted = 0i64;
    let mut denied = 0i64;
    let mut recorded = 0i64;
    for a in &approvals {
        let refs = spec(a)
            .get("taskRef")
            .and_then(|r| r.get("name"))
            .and_then(|n| n.as_str())
            == Some(name.as_str());
        if !refs {
            continue;
        }
        recorded += 1;
        match status(a).get("phase").and_then(|p| p.as_str()) {
            Some("Approved") => granted += 1,
            Some("Denied") => denied += 1,
            _ => {}
        }
    }

    let receipt_issued = cluster
        .get_kind(&ns, "KarsReceipt", &name)
        .await
        .map_err(upstream)?
        .is_some();

    let exec_phase = status(&task)
        .get("executionPhase")
        .and_then(|p| p.as_str())
        .map(|x| x.to_string());

    // Real per-mission token telemetry from the latest captured run, if any.
    let run = cluster.read_mission_output(&name).await;
    let run_total_tokens = run
        .as_ref()
        .and_then(|d| d.get("totalTokens"))
        .and_then(|v| v.parse().ok());
    let run_prompt_tokens = run
        .as_ref()
        .and_then(|d| d.get("promptTokens"))
        .and_then(|v| v.parse().ok());
    let run_completion_tokens = run
        .as_ref()
        .and_then(|d| d.get("completionTokens"))
        .and_then(|v| v.parse().ok());
    let run_model = run.as_ref().and_then(|d| d.get("model").cloned());
    let has_run = run_total_tokens.is_some();

    Ok(Json(ScorecardDto {
        task: name,
        namespace: ns,
        tier: spec(&task)
            .get("envelope")
            .and_then(|e| e.get("tier"))
            .and_then(|x| x.as_i64()),
        launched: exec_phase.as_deref().is_some_and(|p| p != "Idle"),
        execution_phase: exec_phase,
        token_budget: spec(&task)
            .get("envelope")
            .and_then(|e| e.get("budget"))
            .and_then(|b| b.get("tokens"))
            .and_then(|x| x.as_i64()),
        decisions_recorded: recorded,
        approvals_granted: granted,
        approvals_denied: denied,
        receipt_issued,
        run_total_tokens,
        run_prompt_tokens,
        run_completion_tokens,
        run_model,
        // A real run gives us real token numbers; latency/streaming telemetry is
        // still a named next step, so we surface tokens honestly and say what's
        // not yet wired.
        runtime_metrics_available: has_run,
        runtime_metrics_note: if has_run {
            None
        } else {
            Some(
                "Token burn and latency for this mission aren't surfaced until it runs — click 'Run mission' to capture a real governed run with real token cost.".to_string(),
            )
        },
    }))
}
