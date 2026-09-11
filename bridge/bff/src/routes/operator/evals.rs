// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use kube::core::DynamicObject;
use serde::Serialize;
use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::{created_of, name_of, ns_of, require_cluster, s, spec, status, upstream};

// ─── KarsEval — safety/quality lifecycle (conformance evals) ─────────────────

#[derive(Debug, Serialize)]
pub struct EvalResultDto {
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub errored: i64,
    pub corpus_name: Option<String>,
    pub corpus_digest: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EvalDto {
    pub name: String,
    pub namespace: String,
    pub display_name: Option<String>,
    /// The sandbox this eval targets (spec.targetSandboxRef).
    pub target_sandbox: Option<String>,
    /// The corpus replayed — `builtin:<name>` or an OCI ref.
    pub corpus: Option<String>,
    /// Reconcile phase (Ready / Degraded / Pending).
    pub phase: Option<String>,
    /// Optional cron schedule (recurring eval), when set.
    pub schedule: Option<String>,
    pub last_run_at: Option<String>,
    /// The most recent verdict (pass/fail counts), when a run completed.
    pub last_result: Option<EvalResultDto>,
    pub created: Option<String>,
}

fn to_eval_result(v: &Value) -> Option<EvalResultDto> {
    if !v.is_object() {
        return None;
    }
    Some(EvalResultDto {
        total: v.get("total").and_then(|x| x.as_i64()).unwrap_or(0),
        passed: v.get("passed").and_then(|x| x.as_i64()).unwrap_or(0),
        failed: v.get("failed").and_then(|x| x.as_i64()).unwrap_or(0),
        errored: v.get("errored").and_then(|x| x.as_i64()).unwrap_or(0),
        corpus_name: s(v, "corpusName"),
        corpus_digest: s(v, "corpusDigest"),
        completed_at: s(v, "completedAt"),
    })
}

fn to_eval(o: &DynamicObject) -> EvalDto {
    let sp = spec(o);
    let st = status(o);
    let corpus = sp.get("corpus").and_then(|c| {
        c.get("builtin")
            .and_then(|b| b.as_str())
            .map(|b| format!("builtin:{b}"))
            .or_else(|| {
                c.get("bundleRef")
                    .and_then(|r| r.get("repository"))
                    .and_then(|x| x.as_str())
                    .map(|x| x.to_string())
            })
    });
    EvalDto {
        name: name_of(o),
        namespace: ns_of(o),
        display_name: s(sp, "displayName"),
        target_sandbox: sp
            .get("targetSandboxRef")
            .and_then(|r| r.get("name"))
            .and_then(|x| x.as_str())
            .map(|x| x.to_string()),
        corpus,
        phase: s(st, "phase"),
        schedule: s(sp, "schedule"),
        last_run_at: s(st, "lastRunAt"),
        last_result: st.get("lastResult").and_then(to_eval_result),
        created: created_of(o),
    }
}

/// `GET /api/operator/evals` — the KarsEval safety/quality lifecycle: every
/// conformance eval, which sandbox it targets, and its latest real verdict
/// (pass/fail against the replayed corpus). Honest empty when none exist.
pub async fn list_evals(State(state): State<AppState>) -> AppResult<Json<Vec<EvalDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster.list_kind_all("KarsEval").await.map_err(upstream)?;
    let mut dtos: Vec<EvalDto> = items.iter().map(to_eval).collect();
    dtos.sort_by(|a, b| b.last_run_at.cmp(&a.last_run_at).then(a.name.cmp(&b.name)));
    Ok(Json(dtos))
}

/// Operator request to configure + launch a safety eval.
#[derive(Debug, serde::Deserialize)]
pub struct CreateEvalRequest {
    /// The sandbox to evaluate (spec.targetSandboxRef).
    pub target_sandbox: String,
    /// Builtin corpus name, e.g. `jailbreak-baseline` (spec.corpus.builtin).
    pub corpus: String,
    /// Optional cron schedule for a recurring eval; one-shot when omitted.
    pub schedule: Option<String>,
    /// Runner image override. On a dev cluster this must be the locally-loaded
    /// `kars-conformance-runner:dev`; in prod the controller default applies.
    pub runner_image: Option<String>,
    /// Human label.
    pub display_name: Option<String>,
    /// Run immediately (stamp the run-now annotation). Default true.
    pub run_now: Option<bool>,
}

/// `POST /api/operator/evals` — configure and (by default) launch a safety eval
/// against a sandbox. Operator-only surface; the controller spawns the runner
/// Job that replays the corpus and records the real verdict.
pub async fn create_eval(
    State(state): State<AppState>,
    Json(req): Json<CreateEvalRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let sandbox = req.target_sandbox.trim();
    let corpus = req.corpus.trim();
    if sandbox.is_empty() || corpus.is_empty() {
        return Err(AppError::BadRequest(
            "target_sandbox and corpus are required".into(),
        ));
    }
    // Deterministic, readable name so re-running the same eval updates in place.
    let name = format!("{sandbox}-{}", corpus.replace([':', '_', '/'], "-"));
    let mut spec = serde_json::json!({
        "targetSandboxRef": { "name": sandbox },
        "corpus": { "builtin": corpus },
    });
    if let Some(img) = req
        .runner_image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        spec["runnerImage"] = serde_json::json!(img);
    }
    if let Some(sch) = req
        .schedule
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        spec["schedule"] = serde_json::json!(sch);
    }
    if let Some(dn) = req
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        spec["displayName"] = serde_json::json!(dn);
    }
    let mut annotations = serde_json::Map::new();
    if req.run_now.unwrap_or(true) {
        annotations.insert("kars.azure.com/run-now".into(), serde_json::json!("true"));
    }
    let body = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsEval",
        "metadata": { "name": name, "annotations": annotations },
        "spec": spec.clone(),
    });
    cluster
        .apply_kind("kars-system", "KarsEval", body, true)
        .await
        .map_err(upstream)?;
    Ok(Json(serde_json::json!({ "created": true, "name": name })))
}

/// A single eval case with what it tests and its latest verdict.
#[derive(Debug, Serialize)]
pub struct EvalCaseDto {
    pub id: String,
    pub tags: Vec<String>,
    /// Plain-language summary of the adversarial probe this case sends.
    pub probe: Option<String>,
    /// The expected decision (what a safe agent SHOULD do), e.g. "Blocked".
    pub expected: Option<String>,
    /// What the router ACTUALLY decided on the last run, when known.
    pub actual: Option<String>,
    /// The actual decision's reason (e.g. why it was blocked/allowed) — surfaces
    /// WHY a case failed (e.g. blocked by a transport error, not content safety).
    pub actual_reason: Option<String>,
    /// Latest verdict: true=passed, false=failed, None=not yet run OR errored.
    pub pass: Option<bool>,
    /// True when the case could NOT be evaluated (target unreachable / transport
    /// error). Distinct from a policy failure — inconclusive, shown amber.
    pub errored: bool,
}

#[derive(Debug, Serialize)]
pub struct EvalReportDto {
    pub name: String,
    pub corpus: Option<String>,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    /// Cases the runner could not evaluate (target unreachable). Inconclusive,
    /// not counted as failures — surfaced so the UI never conflates "couldn't
    /// reach the sandbox" with "the sandbox let a jailbreak through".
    pub errored: usize,
    pub completed_at: Option<String>,
    /// Whether the controller captured PER-CASE verdicts for the last run. False
    /// for runs that predate per-case reporting (only counts survive) — the UI
    /// then shows the baseline cases without verdicts and invites a re-run.
    pub per_case_available: bool,
    pub cases: Vec<EvalCaseDto>,
}

/// `GET /api/operator/evals/{name}/report` — the DETAILED eval report: every case
/// in the corpus (what it probes, the expected decision) merged with the latest
/// per-case verdict (pass/fail, and what the router actually did). Sourced from
/// the corpus ConfigMap (definitions) + the report ConfigMap (verdicts) the
/// controller persists — real, never fabricated. Empty verdicts until a run.
pub async fn eval_report(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<EvalReportDto>> {
    let cluster = require_cluster(&state)?;
    // Corpus definitions (what each case tests).
    let corpus_raw = cluster
        .configmap_data(&format!("karseval-{name}-corpus"))
        .await
        .and_then(|d| d.get("corpus.json").cloned());
    // Per-case verdicts from the last run (may be absent before first run).
    let report_raw = cluster
        .configmap_data(&format!("karseval-{name}-report"))
        .await
        .and_then(|d| d.get("report.json").cloned());

    // Index verdicts by case id.
    let report_json: Option<serde_json::Value> = report_raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let per_case_available = report_json.is_some();
    let mut verdicts: std::collections::BTreeMap<String, serde_json::Value> = Default::default();
    let mut completed_at = None;
    let (mut total, mut passed, mut failed, mut errored) = (0usize, 0usize, 0usize, 0usize);
    if let Some(r) = &report_json {
        completed_at = r
            .get("completedAt")
            .and_then(|v| v.as_str())
            .map(String::from);
        total = r.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        passed = r.get("passed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        failed = r.get("failed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        errored = r.get("errored").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if let Some(arr) = r.get("results").and_then(|v| v.as_array()) {
            for c in arr {
                if let Some(id) = c.get("caseId").and_then(|v| v.as_str()) {
                    verdicts.insert(id.to_string(), c.clone());
                }
            }
        }
    }
    // Fall back to the KarsEval's own status counts when no per-case report exists
    // (an older run) so the detail's totals never contradict the summary card.
    if !per_case_available
        && let Ok(items) = cluster.list_kind_all("KarsEval").await
        && let Some(ev) = items.iter().find(|o| name_of(o) == name)
    {
        if let Some(lr) = status(ev).get("lastResult") {
            total = lr.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            passed = lr.get("passed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            failed = lr.get("failed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            errored = lr.get("errored").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        }
        completed_at = s(status(ev), "lastRunAt");
    }

    let corpus_json: Option<serde_json::Value> = corpus_raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let corpus_name = corpus_json
        .as_ref()
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let mut cases: Vec<EvalCaseDto> = Vec::new();
    if let Some(arr) = corpus_json
        .as_ref()
        .and_then(|c| c.get("cases"))
        .and_then(|v| v.as_array())
    {
        for case in arr {
            let id = case
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let tags = case
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let expected = case
                .get("expect")
                .and_then(|e| e.get("decision"))
                .and_then(|v| v.as_str())
                .map(String::from);
            // Summarise the probe: the last user message in the scenario.
            let probe = case
                .get("scenario")
                .and_then(|s| s.get("messages"))
                .and_then(|m| m.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .rev()
                        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                })
                .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                .map(|s| s.chars().take(160).collect::<String>());
            let v = verdicts.get(&id);
            let pass = v.and_then(|c| c.get("pass")).and_then(|p| p.as_bool());
            let errored = v
                .and_then(|c| c.get("errored"))
                .and_then(|e| e.as_bool())
                .unwrap_or(false);
            let actual = v
                .and_then(|c| c.get("actual"))
                .and_then(|a| a.get("decision"))
                .and_then(|d| d.as_str())
                .map(String::from);
            let actual_reason = v
                .and_then(|c| c.get("actual"))
                .and_then(|a| a.get("reason"))
                .and_then(|d| d.as_str())
                .map(|s| s.chars().take(240).collect::<String>());
            cases.push(EvalCaseDto {
                id,
                tags,
                probe,
                expected,
                actual,
                actual_reason,
                pass,
                errored,
            });
        }
    }

    Ok(Json(EvalReportDto {
        name,
        corpus: corpus_name,
        total,
        passed,
        failed,
        errored,
        completed_at,
        per_case_available,
        cases,
    }))
}
