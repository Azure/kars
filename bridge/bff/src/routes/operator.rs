// kars Bridge BFF — Operator Console API.
//
// The operator surface reads the same CRDs as the user Workspace but projects
// them into resource/policy/audit language. Every field here is read from live
// cluster state via the dynamic API (no typed consumer per CRD) and projected
// into a stable browser DTO. Absent CRDs surface as empty lists, never errors —
// the honesty grammar (empty vs not-wired) lives in the web layer.

use axum::Json;
use axum::extract::{Extension, State};
use kube::core::DynamicObject;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

fn upstream(e: kube::Error) -> AppError {
    AppError::Upstream(e.to_string())
}

// ─── helpers over DynamicObject ──────────────────────────────────────────────

fn name_of(o: &DynamicObject) -> String {
    o.metadata.name.clone().unwrap_or_default()
}
fn ns_of(o: &DynamicObject) -> String {
    o.metadata.namespace.clone().unwrap_or_default()
}
fn created_of(o: &DynamicObject) -> Option<String> {
    o.metadata
        .creation_timestamp
        .as_ref()
        .map(|t| t.0.to_rfc3339())
}
/// Whether this sandbox is owned by a KarsTask — true for every mission/team-
/// run sandbox, false for a standing sandbox with no task behind it (e.g. the
/// Bridge's own orchestrator sandbox). Such a sandbox never gets a
/// mission-output ConfigMap, so it must be excluded from the "executing" test
/// below (it would otherwise look permanently "not yet delivered").
fn has_task_owner(o: &DynamicObject) -> bool {
    o.metadata
        .owner_references
        .as_ref()
        .is_some_and(|refs| refs.iter().any(|r| r.kind == "KarsTask"))
}
fn spec(o: &DynamicObject) -> &Value {
    o.data.get("spec").unwrap_or(&Value::Null)
}
fn status(o: &DynamicObject) -> &Value {
    o.data.get("status").unwrap_or(&Value::Null)
}
fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|x| x.to_string())
}
fn label(o: &DynamicObject, key: &str) -> Option<String> {
    o.metadata.labels.as_ref().and_then(|l| l.get(key).cloned())
}
fn annotation(o: &DynamicObject, key: &str) -> Option<String> {
    o.metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(key).cloned())
}

// Skill-admission annotation keys — the operator trust gate (§ skills workflow):
// a user-uploaded skill is only usable once an operator has scanned + approved
// it, which LOCKS the approval to the exact version digest at approval time.
// Any later change to the skill breaks the lock and returns it to review.
const ANN_REVIEW: &str = "kars.azure.com/skill-review";
const ANN_LOCKED_DIGEST: &str = "kars.azure.com/skill-locked-digest";
const ANN_APPROVED_BY: &str = "kars.azure.com/skill-approved-by";
const ANN_APPROVED_AT: &str = "kars.azure.com/skill-approved-at";

// ─── Sandbox fleet ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SandboxDto {
    pub name: String,
    pub namespace: String,
    pub runtime_namespace: Option<String>,
    pub phase: Option<String>,
    pub runtime: Option<String>,
    pub isolation: Option<String>,
    /// The governing ToolPolicy (AGT capability bounds) this agent runs under —
    /// registry inventory: "which policy governs this agent". From
    /// spec.governance.toolPolicyRef.
    pub tool_policy: Option<String>,
    /// The InferencePolicy binding its model route + token budget. From
    /// spec.inferenceRef.
    pub inference_policy: Option<String>,
    /// Whether AGT governance is enabled (fails closed on an empty policy set).
    pub governed: bool,
    /// Owning standing team (label), if any — the "who owns this" registry column.
    pub team: Option<String>,
    /// Parent sandbox name when this is a spawned sub-agent (label-derived).
    pub parent: Option<String>,
    pub message: Option<String>,
    pub created: Option<String>,
    /// For a Running sandbox: whether its run has produced ANY real activity
    /// (model rounds / tool calls). `Some(false)` = the pod is Running but idle
    /// — e.g. a chat-gateway harness waiting for input, or a hung run. Surfaced
    /// so a green "Running" never masks a stalled agent (audit f23). `None` when
    /// the sandbox isn't Running (the signal doesn't apply).
    pub working: Option<bool>,
    /// Whether this sandbox is CURRENTLY executing a task (Running AND no
    /// terminal mission-output yet) — the same "live" test the Workspace's
    /// Active-agents page uses. Distinct from `working` above: a sandbox can
    /// have `working: true` (it did real work) and still be `executing: false`
    /// (it already delivered and is simply lingering before teardown/
    /// retention) — the exact case that made "Sandboxes: 2 running" and
    /// "Active agents: 0 working" look contradictory when they're both true.
    pub executing: Option<bool>,
    pub cpu_millicores: Option<f64>,
    pub memory_bytes: Option<u64>,
    /// Standard K8s conditions, surfaced for the troubleshooting table.
    pub conditions: Vec<ConditionDto>,
}

#[derive(Debug, Serialize)]
pub struct ConditionDto {
    pub type_: String,
    pub status: String,
    pub reason: Option<String>,
    pub message: Option<String>,
}

fn conditions_of(o: &DynamicObject) -> Vec<ConditionDto> {
    status(o)
        .get("conditions")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| ConditionDto {
                    type_: s(c, "type").unwrap_or_default(),
                    status: s(c, "status").unwrap_or_default(),
                    reason: s(c, "reason"),
                    message: s(c, "message"),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn to_sandbox(o: &DynamicObject) -> SandboxDto {
    let sp = spec(o);
    SandboxDto {
        name: name_of(o),
        namespace: ns_of(o),
        runtime_namespace: s(status(o), "namespace"),
        phase: s(status(o), "phase"),
        runtime: sp
            .get("runtime")
            .and_then(|r| r.get("kind"))
            .and_then(|k| k.as_str())
            .map(|x| x.to_string()),
        isolation: sp
            .get("sandbox")
            .and_then(|sb| sb.get("isolation"))
            .and_then(|i| i.as_str())
            .map(|x| x.to_string()),
        tool_policy: sp
            .get("governance")
            .and_then(|g| g.get("toolPolicyRef"))
            .and_then(|r| r.get("name"))
            .and_then(|n| n.as_str())
            .map(|x| x.to_string()),
        inference_policy: sp
            .get("inferenceRef")
            .and_then(|r| r.get("name"))
            .and_then(|n| n.as_str())
            .map(|x| x.to_string()),
        governed: sp
            .get("governance")
            .and_then(|g| g.get("enabled"))
            .and_then(|e| e.as_bool())
            .unwrap_or(false),
        team: label(o, "kars.azure.com/team"),
        parent: label(o, "kars.azure.com/parent").or_else(|| s(sp, "parentSandbox")),
        message: s(status(o), "message"),
        created: created_of(o),
        working: None,
        executing: None,
        cpu_millicores: None,
        memory_bytes: None,
        conditions: conditions_of(o),
    }
}

fn cpu_millicores(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if let Some(value) = raw.strip_suffix('n') {
        return value.parse::<f64>().ok().map(|value| value / 1_000_000.0);
    }
    if let Some(value) = raw.strip_suffix('u') {
        return value.parse::<f64>().ok().map(|value| value / 1_000.0);
    }
    if let Some(value) = raw.strip_suffix('m') {
        return value.parse::<f64>().ok();
    }
    raw.parse::<f64>().ok().map(|value| value * 1_000.0)
}

fn memory_bytes(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    for (suffix, multiplier) in [
        ("Ki", 1_024_f64),
        ("Mi", 1_048_576_f64),
        ("Gi", 1_073_741_824_f64),
        ("Ti", 1_099_511_627_776_f64),
        ("K", 1_000_f64),
        ("M", 1_000_000_f64),
        ("G", 1_000_000_000_f64),
    ] {
        if let Some(value) = raw.strip_suffix(suffix) {
            return value
                .parse::<f64>()
                .ok()
                .map(|value| (value * multiplier) as u64);
        }
    }
    raw.parse::<u64>().ok()
}

fn metric_usage(metric: &DynamicObject) -> (f64, u64) {
    metric
        .data
        .get("containers")
        .and_then(Value::as_array)
        .map(|containers| {
            containers
                .iter()
                .fold((0.0, 0_u64), |(cpu, memory), container| {
                    let usage = container.get("usage").unwrap_or(&Value::Null);
                    (
                        cpu + usage
                            .get("cpu")
                            .and_then(Value::as_str)
                            .and_then(cpu_millicores)
                            .unwrap_or(0.0),
                        memory
                            + usage
                                .get("memory")
                                .and_then(Value::as_str)
                                .and_then(memory_bytes)
                                .unwrap_or(0),
                    )
                })
        })
        .unwrap_or((0.0, 0))
}

fn inherit_sandbox_context(sandboxes: &mut [SandboxDto]) {
    let by_name: HashMap<(String, String), usize> = sandboxes
        .iter()
        .enumerate()
        .map(|(index, sandbox)| ((sandbox.namespace.clone(), sandbox.name.clone()), index))
        .collect();
    let resolved: Vec<(Option<String>, Option<bool>)> = sandboxes
        .iter()
        .enumerate()
        .map(|(index, sandbox)| {
            let mut team = sandbox.team.clone();
            let mut executing = sandbox.executing;
            let mut cursor = index;
            let mut visited = vec![false; sandboxes.len()];
            visited[cursor] = true;

            while let Some(parent) = sandboxes[cursor].parent.as_ref() {
                let parent_key = (sandboxes[cursor].namespace.clone(), parent.clone());
                let Some(parent_index) = by_name.get(&parent_key).copied() else {
                    break;
                };
                if visited[parent_index] {
                    break;
                }
                visited[parent_index] = true;
                let parent = &sandboxes[parent_index];
                if team.is_none() {
                    team = parent.team.clone();
                }
                if parent.executing.is_some() {
                    executing = parent.executing;
                }
                cursor = parent_index;
            }
            (team, executing)
        })
        .collect();

    for (sandbox, (team, executing)) in sandboxes.iter_mut().zip(resolved) {
        sandbox.team = team;
        if sandbox.parent.is_some() {
            let observed_working =
                sandbox.phase.as_deref() == Some("Running") && sandbox.working == Some(true);
            sandbox.executing =
                executing.map(|parent_executing| parent_executing && observed_working);
        }
    }
}

#[derive(Debug, Serialize)]
pub struct NodeCapacityDto {
    pub name: String,
    pub cpu_usage_millicores: Option<f64>,
    pub cpu_allocatable_millicores: Option<f64>,
    pub memory_usage_bytes: Option<u64>,
    pub memory_allocatable_bytes: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub memory_percent: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct CapacityDto {
    pub metrics_available: bool,
    pub metrics_error: Option<String>,
    pub team_max_concurrent_runs: usize,
    pub global_active_runs_limit: usize,
    pub active_team_runs: usize,
    pub pod_metrics_available: bool,
    pub pod_metrics_error: Option<String>,
    pub nodes: Vec<NodeCapacityDto>,
}

/// `GET /api/operator/sandboxes` — the fleet, across all namespaces.
pub async fn list_sandboxes(State(state): State<AppState>) -> AppResult<Json<Vec<SandboxDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind_all("KarsSandbox")
        .await
        .map_err(upstream)?;
    let mut dtos: Vec<SandboxDto> = items.iter().map(to_sandbox).collect();
    let task_teams: HashMap<(String, String), String> = cluster
        .list_kind_all("KarsTask")
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|task| {
            label(&task, "kars.azure.com/team").map(|team| ((ns_of(&task), name_of(&task)), team))
        })
        .collect();
    let pod_metrics = cluster
        .list_metrics_all("PodMetrics", "pods")
        .await
        .unwrap_or_default();
    let usage: HashMap<(String, String), (f64, u64)> = pod_metrics
        .iter()
        .map(|metric| ((ns_of(metric), name_of(metric)), metric_usage(metric)))
        .collect();
    // Stall signal (audit f23): for each Running sandbox, check whether its run
    // has produced any real activity. A Running-but-empty sandbox is idle or
    // hung (a chat-gateway harness waiting for input, or a stalled loop) — the
    // operator must be able to tell that apart from a green "Running".
    for (o, d) in items.iter().zip(dtos.iter_mut()) {
        if d.team.is_none()
            && let Some(task_name) = label(o, "kars.azure.com/karstask")
        {
            d.team = task_teams.get(&(d.namespace.clone(), task_name)).cloned();
        }
        if let Some(runtime_namespace) = d.runtime_namespace.as_deref() {
            let (cpu, memory) = usage
                .iter()
                .filter(|((namespace, pod), _)| {
                    namespace == runtime_namespace && pod.starts_with(&d.name)
                })
                .fold((0.0, 0_u64), |(cpu, memory), (_, usage)| {
                    (cpu + usage.0, memory + usage.1)
                });
            if cpu > 0.0 || memory > 0 {
                d.cpu_millicores = Some(cpu);
                d.memory_bytes = Some(memory);
            }
        }
        if d.phase.as_deref() == Some("Running") {
            let persisted_activity = cluster
                .read_mission_trace(&d.name)
                .await
                .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let has_activity =
                persisted_activity || !cluster.sandbox_live_trace(&d.name).await.is_empty();
            d.working = Some(has_activity);
            // "Executing right now" — the same test the Workspace's Active
            // agents page uses (live iff Running AND no terminal mission-output
            // yet). A sandbox that already delivered still shows `working: true`
            // (it DID real work) but `executing: false` (nothing left to do,
            // just lingering before teardown/retention) — this is what makes
            // "Sandboxes: N running" and "Active agents: 0 working" both
            // correct at once instead of reading as a contradiction. Only
            // applies to a task-owned sandbox — a standing sandbox with no
            // KarsTask (e.g. the Bridge's own orchestrator) never gets a
            // mission-output ConfigMap, so it would otherwise look permanently
            // "not yet delivered" and inflate this count.
            if has_task_owner(o) {
                let delivered = cluster.read_mission_output(&d.name).await.is_some();
                d.executing = Some(!delivered);
            }
        }
    }
    inherit_sandbox_context(&mut dtos);
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

pub async fn capacity(State(state): State<AppState>) -> AppResult<Json<CapacityDto>> {
    let cluster = require_cluster(&state)?;
    let nodes = cluster.list_nodes().await.map_err(upstream)?;
    let metrics = cluster.list_metrics_all("NodeMetrics", "nodes").await;
    let (metrics_api_available, metrics_api_error, metric_map) = match metrics {
        Ok(items) => (
            true,
            None,
            items
                .into_iter()
                .map(|metric| {
                    let usage = metric.data.get("usage").cloned().unwrap_or(Value::Null);
                    (
                        name_of(&metric),
                        (
                            usage
                                .get("cpu")
                                .and_then(Value::as_str)
                                .and_then(cpu_millicores),
                            usage
                                .get("memory")
                                .and_then(Value::as_str)
                                .and_then(memory_bytes),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>(),
        ),
        Err(error) => (false, Some(error.to_string()), HashMap::new()),
    };
    let node_count = nodes.len();
    let covered_nodes = nodes
        .iter()
        .filter(|node| {
            node.metadata
                .name
                .as_ref()
                .is_some_and(|name| metric_map.contains_key(name))
        })
        .count();
    let metrics_available = metrics_api_available && node_count > 0 && covered_nodes == node_count;
    let metrics_error = if !metrics_api_available {
        metrics_api_error
    } else if node_count == 0 {
        Some("the cluster reported no nodes".into())
    } else if covered_nodes != node_count {
        Some(format!(
            "node metrics coverage is partial ({covered_nodes}/{node_count})"
        ))
    } else {
        None
    };
    let nodes = nodes
        .into_iter()
        .map(|node| {
            let name = node.metadata.name.unwrap_or_default();
            let allocatable = node.status.and_then(|status| status.allocatable);
            let cpu_allocatable = allocatable
                .as_ref()
                .and_then(|values| values.get("cpu"))
                .and_then(|value| cpu_millicores(&value.0));
            let memory_allocatable = allocatable
                .as_ref()
                .and_then(|values| values.get("memory"))
                .and_then(|value| memory_bytes(&value.0));
            let (cpu_usage, memory_usage) = metric_map.get(&name).cloned().unwrap_or((None, None));
            NodeCapacityDto {
                name,
                cpu_usage_millicores: cpu_usage,
                cpu_allocatable_millicores: cpu_allocatable,
                memory_usage_bytes: memory_usage,
                memory_allocatable_bytes: memory_allocatable,
                cpu_percent: cpu_usage
                    .zip(cpu_allocatable)
                    .filter(|(_, allocatable)| *allocatable > 0.0)
                    .map(|(usage, allocatable)| usage / allocatable * 100.0),
                memory_percent: memory_usage
                    .zip(memory_allocatable)
                    .filter(|(_, allocatable)| *allocatable > 0)
                    .map(|(usage, allocatable)| usage as f64 / allocatable as f64 * 100.0),
            }
        })
        .collect();
    let team_max_concurrent_runs = cluster
        .controller_env_value("KARS_TEAM_MAX_CONCURRENT_RUNS")
        .await
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    let global_active_runs_limit = cluster
        .controller_env_value("KARS_TEAM_GLOBAL_ACTIVE_RUNS_LIMIT")
        .await
        .and_then(|value| value.parse().ok())
        .unwrap_or(6);
    let active_team_runs = cluster
        .list_kind_all("KarsTask")
        .await
        .unwrap_or_default()
        .iter()
        .filter(|task| {
            let annotations = task.metadata.annotations.as_ref();
            let taskforce = annotations
                .and_then(|values| values.get("kars.azure.com/team-role"))
                .is_some_and(|role| role == "taskforce");
            let launched = task
                .data
                .pointer("/spec/execution/launch")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !taskforce || !launched {
                return false;
            }
            let requested =
                annotations.and_then(|values| values.get("kars.azure.com/run-requested"));
            let completed =
                annotations.and_then(|values| values.get("kars.azure.com/run-completed"));
            let delivery_pending = requested.is_some() && requested != completed;
            let assignment_active = task
                .data
                .pointer("/status/assignment/state")
                .and_then(Value::as_str)
                .is_some_and(|state| matches!(state, "Assigned" | "Running"));
            let execution_active = task
                .data
                .pointer("/status/executionPhase")
                .and_then(Value::as_str)
                .is_some_and(|phase| matches!(phase, "Launching" | "Running"));
            delivery_pending || assignment_active || execution_active
        })
        .count();
    let (pod_metrics_available, pod_metrics_error) =
        match cluster.list_metrics_all("PodMetrics", "pods").await {
            Ok(items) if !items.is_empty() => (true, None),
            Ok(_) => (
                false,
                Some("the metrics API returned no pod samples".into()),
            ),
            Err(error) => (false, Some(error.to_string())),
        };
    Ok(Json(CapacityDto {
        metrics_available,
        metrics_error,
        team_max_concurrent_runs,
        global_active_runs_limit,
        active_team_runs,
        pod_metrics_available,
        pod_metrics_error,
        nodes,
    }))
}

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

// ─── MCP servers (connected services) ────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct McpServerDto {
    pub name: String,
    pub namespace: String,
    pub url: Option<String>,
    pub phase: Option<String>,
    pub mode: Option<String>,
    pub endpoint: Option<String>,
    pub workload_ref: Option<String>,
    pub discovered_tools: Vec<String>,
    pub tool_schema_digest: Option<String>,
    pub production: Option<bool>,
    pub allowed_tools: Vec<String>,
    pub created: Option<String>,
    /// Raw `spec` for Edit-form prefill.
    pub spec: serde_json::Value,
}

fn to_mcp(o: &DynamicObject) -> McpServerDto {
    let sp = spec(o);
    let st = status(o);
    let allowed_tools = sp
        .get("allowedTools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    McpServerDto {
        name: name_of(o),
        namespace: ns_of(o),
        url: s(st, "endpoint").or_else(|| s(sp, "url")),
        phase: s(st, "phase"),
        mode: s(st, "mode"),
        endpoint: s(st, "endpoint"),
        workload_ref: s(st, "workloadRef"),
        discovered_tools: st
            .get("discoveredTools")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        tool_schema_digest: s(st, "toolSchemaDigest"),
        production: sp.get("productionMode").and_then(|p| p.as_bool()),
        allowed_tools,
        created: created_of(o),
        spec: sp.clone(),
    }
}

/// `GET /api/operator/mcpservers` — registered MCP servers (connected services).
pub async fn list_mcpservers(State(state): State<AppState>) -> AppResult<Json<Vec<McpServerDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster.list_kind_all("McpServer").await.map_err(upstream)?;
    let mut dtos: Vec<McpServerDto> = items.iter().map(to_mcp).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

// ─── Tool policies ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ToolPolicyDto {
    pub name: String,
    pub namespace: String,
    pub phase: Option<String>,
    pub version_hash: Option<String>,
    /// What this policy applies to (sandbox/tool scope), in plain terms.
    pub applies_to: Option<String>,
    /// Whether the policy carries an AGT governance profile (the rule set that
    /// allows/denies/rate-limits capabilities).
    pub has_governance_profile: bool,
    /// Allowed tool / MCP identifiers, when expressed as a flat list.
    pub allowed: Vec<String>,
    pub created: Option<String>,
    /// The raw `spec` object, so the console can prefill the Edit form with the
    /// exact current spec (edit = re-apply with changed fields via SSA).
    pub spec: serde_json::Value,
}

fn to_toolpolicy(o: &DynamicObject) -> ToolPolicyDto {
    let sp = spec(o);
    let mut allowed: Vec<String> = Vec::new();
    if let Some(arr) = sp.get("allow").and_then(|a| a.as_array()) {
        allowed.extend(arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())));
    }
    if let Some(arr) = sp.get("tools").and_then(|a| a.as_array()) {
        allowed.extend(arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())));
    }
    // The real ToolPolicy scopes via `appliesTo` (sandbox labels + tool glob)
    // and governs capabilities through an embedded AGT profile. Project that
    // into a plain summary rather than an empty list.
    let applies_to = sp.get("appliesTo").map(|a| {
        let tool = a.get("tool").and_then(|t| t.as_str()).unwrap_or("*");
        // Render the FULL sandbox selector, not just the well-known sandbox
        // label, so a policy scoped by other labels isn't misreported as "*".
        let labels = a
            .get("sandboxMatchLabels")
            .and_then(|l| l.as_object())
            .map(|m| {
                m.iter()
                    .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|s| !s.is_empty());
        match labels {
            Some(sel) => format!("sandbox [{sel}] · tools {tool}"),
            None => format!("all sandboxes · tools {tool}"),
        }
    });
    let has_governance_profile = sp.get("agtProfile").is_some();
    ToolPolicyDto {
        name: name_of(o),
        namespace: ns_of(o),
        phase: s(status(o), "phase"),
        version_hash: s(status(o), "versionHash"),
        applies_to,
        has_governance_profile,
        allowed,
        created: created_of(o),
        spec: sp.clone(),
    }
}

/// `GET /api/operator/toolpolicies` — tool/MCP authorization policies.
pub async fn list_toolpolicies(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<ToolPolicyDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind_all("ToolPolicy")
        .await
        .map_err(upstream)?;
    let mut dtos: Vec<ToolPolicyDto> = items.iter().map(to_toolpolicy).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

// ─── Inference policies ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct InferencePolicyDto {
    pub name: String,
    pub namespace: String,
    pub phase: Option<String>,
    pub version_hash: Option<String>,
    pub sandbox: Option<String>,
    pub daily_token_budget: Option<i64>,
    pub content_safety: bool,
    pub created: Option<String>,
    /// The raw spec, so the console's visual editor can pre-fill an edit
    /// (name/sandbox/tokens/content-safety/model-preference) instead of a
    /// hand-authored JSON blob.
    pub spec: Value,
}

fn to_inferencepolicy(o: &DynamicObject) -> InferencePolicyDto {
    let sp = spec(o);
    InferencePolicyDto {
        name: name_of(o),
        namespace: ns_of(o),
        phase: s(status(o), "phase"),
        version_hash: s(status(o), "versionHash"),
        sandbox: sp
            .get("appliesTo")
            .and_then(|a| a.get("sandboxName"))
            .and_then(|x| x.as_str())
            .map(|x| x.to_string()),
        daily_token_budget: sp
            .get("tokenBudget")
            .and_then(|t| t.get("dailyTokens"))
            .and_then(|x| x.as_i64()),
        // Content safety is enforced when the floor actually sets a severity
        // threshold or requires Prompt Shields — an empty `contentSafety: {}`
        // object is not protection, so don't report it as enabled.
        content_safety: sp
            .get("contentSafety")
            .map(|cs| {
                ["hate", "selfHarm", "sexual", "violence"]
                    .iter()
                    .any(|k| cs.get(*k).and_then(|v| v.as_str()).is_some())
                    || cs.get("requirePromptShields").and_then(|v| v.as_bool()) == Some(true)
            })
            .unwrap_or(false),
        created: created_of(o),
        spec: sp.clone(),
    }
}

/// `GET /api/operator/inferencepolicies` — inference governance policies.
pub async fn list_inferencepolicies(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<InferencePolicyDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind_all("InferencePolicy")
        .await
        .map_err(upstream)?;
    let mut dtos: Vec<InferencePolicyDto> = items.iter().map(to_inferencepolicy).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

/// `POST /api/operator/inferencepolicies` — create (or Server-Side-Apply edit) a
/// standalone InferencePolicy the operator authors directly (e.g. a policy
/// scoped to a selector with a token budget + content-safety floor). The
/// per-sandbox `<task>-inference` policies remain controller-generated; this is
/// the "I should be able to create inference policies" capability.
pub async fn create_inferencepolicy(
    State(state): State<AppState>,
    Json(req): Json<ApplyCrdRequest>,
) -> AppResult<Json<serde_json::Value>> {
    apply_governance(require_cluster(&state)?, "InferencePolicy", req).await
}

#[derive(serde::Deserialize)]
pub struct PatchInferenceBudgetRequest {
    /// New daily token cap for this policy (0 clears the cap).
    pub daily_tokens: i64,
}

/// `PATCH /api/operator/inferencepolicies/{name}` — edit a policy's daily token
/// budget in place (a merge patch on `spec.tokenBudget.dailyTokens`). For a
/// controller-generated policy the durable source is the mission's envelope
/// budget, so the reconciler may re-derive it; for an operator-authored policy
/// the edit sticks.
pub async fn patch_inferencepolicy(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(req): Json<PatchInferenceBudgetRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let budget = if req.daily_tokens > 0 {
        serde_json::json!({ "dailyTokens": req.daily_tokens })
    } else {
        serde_json::Value::Null
    };
    let patch = serde_json::json!({ "spec": { "tokenBudget": budget } });
    cluster
        .merge_patch_kind("kars-system", "InferencePolicy", &name, patch)
        .await
        .map_err(apply_err)?;
    Ok(Json(serde_json::json!({ "patched": true, "name": name })))
}

/// `DELETE /api/operator/inferencepolicies/{name}` — remove an operator-authored
/// policy. (A controller-generated one will be recreated by the reconciler.)
pub async fn delete_inferencepolicy(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    delete_governance(require_cluster(&state)?, "InferencePolicy", &name, None).await
}

// ─── Egress (allowlists + temporary approvals) ───────────────────────────────

#[derive(Debug, Serialize)]
pub struct EgressApprovalDto {
    pub name: String,
    pub namespace: String,
    pub sandbox: Option<String>,
    pub phase: Option<String>,
    pub reason: Option<String>,
    pub hosts: Vec<String>,
    pub expires_at: Option<String>,
    pub created: Option<String>,
}

fn to_egress_approval(o: &DynamicObject) -> EgressApprovalDto {
    let sp = spec(o);
    let hosts = sp
        .get("hosts")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let host = e.get("host").and_then(|x| x.as_str())?;
                    let port = e.get("port").and_then(|x| x.as_i64());
                    Some(match port {
                        Some(p) => format!("{host}:{p}"),
                        None => host.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    EgressApprovalDto {
        name: name_of(o),
        namespace: ns_of(o),
        sandbox: s(sp, "sandbox"),
        phase: s(status(o), "phase"),
        reason: s(sp, "reason"),
        hosts,
        expires_at: s(status(o), "expiresAt"),
        created: created_of(o),
    }
}

/// `GET /api/operator/egress` — temporary egress approvals across the fleet.
pub async fn list_egress(State(state): State<AppState>) -> AppResult<Json<Vec<EgressApprovalDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind_all("EgressApproval")
        .await
        .map_err(upstream)?;
    let mut dtos: Vec<EgressApprovalDto> = items.iter().map(to_egress_approval).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

// ─── Audit: receipts + inclusion log + checkpoint ────────────────────────────

#[derive(Debug, Serialize)]
pub struct ReceiptSummaryDto {
    pub name: String,
    pub namespace: String,
    pub task: Option<String>,
    pub envelope_digest: Option<String>,
    pub key_id: Option<String>,
    pub inclusion_seq: Option<i64>,
    pub created: Option<String>,
    /// At-a-glance verdict from the receipt's claim matrix (`spec.claims`, which
    /// the CRD already carries) — `verified` (all required non-regulatory claims
    /// PASS), `failed` (any FAIL), `partial` (required evidence incomplete), or
    /// `none` (no claims). Regulatory maturity is advisory and shown in detail.
    pub verdict: String,
}

/// Reduce a receipt's `(class, status)` claim pairs to an overall verdict.
/// Any FAIL/ERROR ⇒ "failed". Otherwise the badge reflects the CRYPTOGRAPHIC
/// claims (integrity + conformance + completeness) — the "regulatory" claim and
/// any "OMITTED" status are advisory V0-maturity disclosures that must NOT block
/// a "verified" verdict (else every receipt reads "partial" forever). `class`
/// is expected lowercased, `status` uppercased.
fn receipt_verdict(claims: &[(String, String)]) -> &'static str {
    if claims.is_empty() {
        return "none";
    }
    if claims.iter().any(|(_, s)| s == "FAIL" || s == "ERROR") {
        return "failed";
    }
    let core: Vec<&(String, String)> = claims
        .iter()
        .filter(|(class, status)| class != "regulatory" && status != "OMITTED")
        .collect();
    if !core.is_empty() && core.iter().all(|(_, s)| s == "PASS" || s == "OK") {
        "verified"
    } else {
        "partial"
    }
}

fn to_receipt_summary(o: &DynamicObject) -> ReceiptSummaryDto {
    let sp = spec(o);
    // The regulatory claim is a V0 maturity dimension — it is ALWAYS "PARTIAL"
    // or "OMITTED" until an external KMS/transparency anchor lands (a named V1
    // follow-up), and "OMITTED" is an honest disclosure, not a verification
    // failure. Treating either as blocking meant NO receipt could ever read
    // "Verified" (every one showed "Partial"), making the verdict useless. So
    // the badge reflects the CRYPTOGRAPHIC claims (integrity + conformance +
    // completeness); the regulatory/omitted maturity is still shown in detail.
    let claims: Vec<(String, String)> = sp
        .get("claims")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let status = c
                        .get("status")
                        .and_then(|s| s.as_str())?
                        .to_ascii_uppercase();
                    let class = c
                        .get("class")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    Some((class, status))
                })
                .collect()
        })
        .unwrap_or_default();
    let verdict = receipt_verdict(&claims).to_string();
    ReceiptSummaryDto {
        name: name_of(o),
        namespace: ns_of(o),
        task: sp
            .get("taskRef")
            .and_then(|r| r.get("name"))
            .and_then(|n| n.as_str())
            .map(|x| x.to_string()),
        envelope_digest: s(sp, "envelopeDigest"),
        key_id: s(sp, "keyId"),
        inclusion_seq: status(o).get("inclusionSeq").and_then(|x| x.as_i64()),
        created: created_of(o),
        verdict,
    }
}

#[derive(Debug, Serialize)]
pub struct AuditDto {
    pub receipts: Vec<ReceiptSummaryDto>,
    pub inclusion_log_size: i64,
    pub checkpoint: Option<CheckpointSummaryDto>,
    /// Real cryptographic integrity verdict — the whole hash chain recomputed
    /// and the signed checkpoint verified against the published anchor. Drives
    /// the audit banner so it reflects verification, not field presence.
    pub integrity: crate::routes::receipts::LogIntegrity,
}

#[derive(Debug, Serialize)]
pub struct CheckpointSummaryDto {
    pub tree_size: i64,
    pub root_hash: String,
    pub key_id: String,
    pub published_at: Option<String>,
}

/// `GET /api/operator/audit` — the audit substrate: every governance receipt,
/// the inclusion-log size, and the signed checkpoint (signed tree head).
pub async fn get_audit(State(state): State<AppState>) -> AppResult<Json<AuditDto>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind_all("KarsReceipt")
        .await
        .map_err(upstream)?;
    let mut receipts: Vec<ReceiptSummaryDto> = items.iter().map(to_receipt_summary).collect();
    receipts.sort_by_key(|a| a.inclusion_seq);

    let log = cluster
        .receipt_log()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let inclusion_log_size = log.entries.len() as i64;

    let checkpoint = log.checkpoint.as_ref().and_then(|d| {
        let tree_size = d.get("treeSize")?.parse::<i64>().ok()?;
        Some(CheckpointSummaryDto {
            tree_size,
            root_hash: d.get("rootHash").cloned().unwrap_or_default(),
            key_id: d.get("keyId").cloned().unwrap_or_default(),
            published_at: d.get("publishedAt").cloned(),
        })
    });

    Ok(Json(AuditDto {
        receipts,
        inclusion_log_size,
        checkpoint,
        integrity: crate::routes::receipts::verify_log_integrity(&log),
    }))
}

// ─── Skills & Profiles (predefined building blocks for customers) ────────────

#[derive(Debug, Serialize)]
pub struct SkillDto {
    pub name: String,
    pub namespace: String,
    pub version: Option<String>,
    pub summary: Option<String>,
    pub bounding_policy: Option<String>,
    pub phase: Option<String>,
    pub version_digest: Option<String>,
    pub attestation_verified: Option<bool>,
    // ── Operator trust gate ──────────────────────────────────────────────────
    /// Admission verdict: "approved" once an operator has signed off, else the
    /// skill is treated as pending review.
    pub review: String,
    /// The version digest the approval is locked to (from status at approval).
    pub locked_digest: Option<String>,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
    /// True when approved AND the locked digest still matches the current
    /// version digest — i.e. usable by users. False if never approved or the
    /// skill changed since approval (lock broken → back to review).
    pub usable: bool,
    /// Raw `spec` for Edit-form prefill.
    pub spec: serde_json::Value,
}

fn to_skill(o: &DynamicObject) -> SkillDto {
    let sp = spec(o);
    let version_digest = s(status(o), "versionDigest");
    let review = annotation(o, ANN_REVIEW).unwrap_or_else(|| "pending".into());
    let locked_digest = annotation(o, ANN_LOCKED_DIGEST);
    // Usable only when explicitly approved and the lock still matches the live
    // digest. When the skill has no digest yet (not scanned), it can't be usable.
    let usable = review == "approved" && locked_digest.is_some() && locked_digest == version_digest;
    SkillDto {
        name: name_of(o),
        namespace: ns_of(o),
        version: s(sp, "version"),
        summary: s(sp, "summary"),
        bounding_policy: s(sp, "boundingPolicy"),
        phase: s(status(o), "phase"),
        version_digest,
        attestation_verified: status(o)
            .get("attestationVerified")
            .and_then(|v| v.as_bool()),
        review,
        locked_digest,
        approved_by: annotation(o, ANN_APPROVED_BY),
        approved_at: annotation(o, ANN_APPROVED_AT),
        usable,
        spec: sp.clone(),
    }
}

#[derive(Debug, Serialize, serde::Deserialize, Clone)]
pub struct McpProfileDto {
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    /// Names of the operator-vetted McpServers this profile bundles.
    pub servers: Vec<String>,
}

/// `GET /api/operator/mcp-profiles` — the operator-curated MCP bundles users
/// can pick from (a named, vetted set of McpServers, so users compose from
/// approved groupings rather than assembling servers one by one).
pub async fn list_mcp_profiles(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<McpProfileDto>>> {
    let cluster = require_cluster(&state)?;
    let raw = cluster.read_mcp_profiles().await;
    let profiles: Vec<McpProfileDto> = serde_json::from_str(&raw).unwrap_or_default();
    Ok(Json(profiles))
}

/// `PUT /api/operator/mcp-profiles` — upsert a profile by name. Validates that
/// every referenced server is a real McpServer on the cluster, so a profile can
/// never bundle a non-existent (unvetted) server.
pub async fn put_mcp_profile(
    State(state): State<AppState>,
    Json(req): Json<McpProfileDto>,
) -> AppResult<Json<Vec<McpProfileDto>>> {
    let cluster = require_cluster(&state)?;
    if req.name.trim().is_empty() {
        return Err(AppError::BadRequest("profile name is required".into()));
    }
    // Real McpServers on the cluster — the vetted universe a profile may draw from.
    let known: std::collections::BTreeSet<String> = cluster
        .list_kind_all("McpServer")
        .await
        .map_err(upstream)?
        .iter()
        .map(name_of)
        .collect();
    for s in &req.servers {
        if !known.contains(s) {
            return Err(AppError::BadRequest(format!(
                "server '{s}' is not a registered McpServer — vet it first"
            )));
        }
    }
    let raw = cluster.read_mcp_profiles().await;
    let mut profiles: Vec<McpProfileDto> = serde_json::from_str(&raw).unwrap_or_default();
    profiles.retain(|p| p.name != req.name);
    profiles.push(req);
    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    let json = serde_json::to_string(&profiles).unwrap_or_else(|_| "[]".into());
    cluster
        .write_mcp_profiles(&json)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(profiles))
}

/// `DELETE /api/operator/mcp-profiles/:name` — remove a profile.
pub async fn delete_mcp_profile(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<Vec<McpProfileDto>>> {
    let cluster = require_cluster(&state)?;
    let raw = cluster.read_mcp_profiles().await;
    let mut profiles: Vec<McpProfileDto> = serde_json::from_str(&raw).unwrap_or_default();
    profiles.retain(|p| p.name != name);
    let json = serde_json::to_string(&profiles).unwrap_or_else(|_| "[]".into());
    cluster
        .write_mcp_profiles(&json)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(profiles))
}

pub async fn list_skills(State(state): State<AppState>) -> AppResult<Json<Vec<SkillDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster.list_kind_all("KarsSkill").await.map_err(upstream)?;
    let mut dtos: Vec<SkillDto> = items.iter().map(to_skill).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

/// Annotation recording who uploaded a user-submitted skill (provenance for the
/// operator reviewing it).
const ANN_UPLOADED_BY: &str = "kars.azure.com/skill-uploaded-by";
const ANN_UPLOADED_BY_SUB: &str = "kars.azure.com/skill-uploaded-by-sub";

#[derive(serde::Deserialize)]
pub struct SubmitSkillRequest {
    /// DNS-1123 object name (kebab-case).
    pub name: String,
    pub display_name: Option<String>,
    pub version: String,
    pub summary: String,
    /// The bounding tool policy — must be one the operator already vetted; it
    /// caps what the skill's recipe can do. Users pick from the approved set.
    pub bounding_policy: String,
    pub recipe: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// The skill PACKAGE files — flat filenames (SKILL.md + scripts). Stored as
    /// the `karsskill-<name>` ConfigMap and mounted into a granting sandbox.
    #[serde(default)]
    pub files: Vec<SkillFile>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillFile {
    /// Flat filename (no path separators) — e.g. `SKILL.md`, `triage.sh`.
    pub name: String,
    pub content: String,
}

/// `POST /api/skills` — USER skill submission. A team member uploads a skill
/// package; it lands as a `KarsSkill` that starts life PENDING REVIEW (never
/// usable until an operator scans + approves it). This is the user side of the
/// trust gate: users propose capability, operators vet + sign, then it's
/// grantable. The BFF never marks a user-submitted skill approved.
pub async fn submit_skill(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<SubmitSkillRequest>,
) -> AppResult<Json<SkillDto>> {
    let cluster = require_cluster(&state)?;
    let name = req.name.trim();
    if !is_dns1123_label(name) {
        return Err(AppError::BadRequest(
            "skill name must be a DNS-1123 label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    if req.version.trim().is_empty() {
        return Err(AppError::BadRequest("version is required".into()));
    }
    if req.summary.trim().len() < 8 {
        return Err(AppError::BadRequest("a real summary is required".into()));
    }
    if req.bounding_policy.trim().is_empty() {
        return Err(AppError::BadRequest(
            "a bounding tool policy is required — it caps what the skill may do".into(),
        ));
    }
    let ns = "kars-system".to_string();
    let mut spec = serde_json::json!({
        "version": req.version.trim(),
        "summary": req.summary.trim(),
        "boundingPolicy": req.bounding_policy.trim(),
    });
    if let Some(dn) = req.display_name.as_ref().filter(|s| !s.trim().is_empty()) {
        spec["displayName"] = serde_json::json!(dn.trim());
    }
    if let Some(r) = req.recipe.as_ref().filter(|s| !s.trim().is_empty()) {
        spec["recipe"] = serde_json::json!(r.trim());
    }
    if !req.mcp_servers.is_empty() {
        spec["mcpServers"] = serde_json::json!(req.mcp_servers);
    }
    // Validate + collect the package files. Standard Agent Skills use
    // subdirectories (scripts/, references/, assets/) referenced relatively from
    // SKILL.md. ConfigMap keys can't contain '/', so we accept relative paths
    // here and path-encode '/'→'__' only when writing the ConfigMap; the sandbox
    // entrypoint decodes them back on mount so the on-disk tree matches exactly.
    // A real skill package is at least a SKILL.md at the root.
    let mut files: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for f in &req.files {
        let fname = f.name.trim();
        let bad_segments = fname.split('/').any(|seg| {
            seg.is_empty()
                || !seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
        if fname.is_empty()
            || fname.starts_with('/')
            || fname.ends_with('/')
            || fname.contains("..")
            || fname.contains("__") // reserved as the CM path separator
            || fname.len() > 253
            || bad_segments
        {
            return Err(AppError::BadRequest(format!(
                "invalid skill file path '{fname}': use relative paths (letters, digits, . _ - and / for subdirs); no '..', no '__', no leading/trailing '/'"
            )));
        }
        files.insert(fname.to_string(), f.content.clone());
    }
    if !files.is_empty() {
        // A package the agent can actually USE must carry a SKILL.md — OpenClaw
        // auto-discovers `<name>/SKILL.md` and reads its frontmatter `description`
        // to know when to invoke the skill. Without it the files are dead weight.
        let skill_md = files.get("SKILL.md");
        match skill_md {
            None => {
                return Err(AppError::BadRequest(
                    "a skill package must include a SKILL.md — the agent discovers the skill from it".into(),
                ));
            }
            Some(md) if !md.contains("description:") => {
                return Err(AppError::BadRequest(
                    "SKILL.md must have YAML frontmatter with a `description:` — that's how the agent knows when to use the skill".into(),
                ));
            }
            _ => {}
        }
        spec["package"] = serde_json::json!(true);
        spec["files"] = serde_json::json!(files.keys().cloned().collect::<Vec<_>>());
        use sha2::{Digest, Sha256};
        let configmap_data: std::collections::BTreeMap<String, String> = files
            .iter()
            .map(|(path, content)| (path.replace('/', "__"), content.clone()))
            .collect();
        let canonical = serde_json::to_vec(&configmap_data)
            .map_err(|e| AppError::Internal(anyhow::Error::new(e)))?;
        spec["packageDigest"] = serde_json::json!(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(&canonical))
        ));
    }
    let uploader = principal.name;
    let uploader_sub = principal.sub;
    let body = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSkill",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "app.kubernetes.io/managed-by": "kars-bridge" },
            // Explicitly PENDING — the operator trust gate must approve it before
            // it is usable. Never set review=approved on the user path.
            "annotations": {
                ANN_REVIEW: "pending",
                ANN_UPLOADED_BY: uploader,
                ANN_UPLOADED_BY_SUB: uploader_sub,
            },
        },
        "spec": spec,
    });
    let applied = cluster
        .apply_kind(&ns, "KarsSkill", body, false)
        .await
        .map_err(apply_err)?;
    // Persist the package files as the karsskill-<name> ConfigMap so the
    // controller can mount them into a granting sandbox. ConfigMap keys can't
    // contain '/', so subdirectory paths are encoded '/'→'__'; the sandbox
    // entrypoint decodes them back to the real tree on mount.
    if !files.is_empty() {
        let cm_files: std::collections::BTreeMap<String, String> = files
            .iter()
            .map(|(path, content)| (path.replace('/', "__"), content.clone()))
            .collect();
        let package_digest = spec
            .get("packageDigest")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("package digest missing")))?;
        cluster
            .write_skill_package(name, &cm_files, package_digest)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    }
    Ok(Json(to_skill(&applied)))
}

/// Locate a skill by name across namespaces, returning `(namespace, object)`.
async fn find_skill(
    cluster: &crate::kars::cluster::Cluster,
    name: &str,
) -> AppResult<(String, DynamicObject)> {
    let items = cluster.list_kind_all("KarsSkill").await.map_err(upstream)?;
    items
        .into_iter()
        .find(|o| name_of(o) == name)
        .map(|o| (ns_of(&o), o))
        .ok_or(AppError::NotFound)
}

/// `POST /api/operator/skills/:name/approve` — the operator admission gate.
/// Records the operator's approval and LOCKS it to the skill's current version
/// digest, after which users can assign the skill. Requires the skill to have
/// been scanned (a version digest present) and its attestation to have verified
/// — an operator can't approve a skill the controller hasn't validated. Any
/// later change to the skill breaks the lock and returns it to review.
pub async fn approve_skill(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(_req): Json<ApproveSkillRequest>,
) -> AppResult<Json<SkillDto>> {
    let cluster = require_cluster(&state)?;
    let (ns, obj) = find_skill(cluster, &name).await?;
    let uploader_subject = obj
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANN_UPLOADED_BY_SUB))
        .cloned();
    if uploader_subject.as_deref() == Some(principal.sub.as_str()) {
        return Err(AppError::Forbidden(
            "skill submitter cannot approve their own package".into(),
        ));
    }
    let generation = obj.metadata.generation.unwrap_or_default();
    let observed_generation = status(&obj)
        .get("observedGeneration")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();
    if observed_generation != generation {
        return Err(AppError::Conflict(
            "skill changed after its last controller scan; wait for the current generation".into(),
        ));
    }
    let digest = s(status(&obj), "versionDigest").ok_or_else(|| {
        AppError::BadRequest(
            "skill has not been scanned yet (no version digest) — the controller must validate it before approval".into(),
        )
    })?;
    // Honest gate: don't let an operator approve a skill whose attestation the
    // controller could not verify.
    if status(&obj)
        .get("attestationVerified")
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        return Err(AppError::BadRequest(
            "skill attestation did not verify — cannot approve until the scan passes".into(),
        ));
    }
    let by = principal.name;
    let now = chrono::Utc::now().to_rfc3339();
    let updated = cluster
        .annotate_kind(
            &ns,
            "KarsSkill",
            &name,
            &[
                (ANN_REVIEW, Some("approved".into())),
                (ANN_LOCKED_DIGEST, Some(digest)),
                (ANN_APPROVED_BY, Some(by)),
                (ANN_APPROVED_AT, Some(now)),
            ],
        )
        .await
        .map_err(upstream)?;
    Ok(Json(to_skill(&updated)))
}

/// `POST /api/operator/skills/:name/revoke` — withdraw approval, returning the
/// skill to review (users immediately stop seeing it).
pub async fn revoke_skill(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<SkillDto>> {
    let cluster = require_cluster(&state)?;
    let (ns, _) = find_skill(cluster, &name).await?;
    let updated = cluster
        .annotate_kind(
            &ns,
            "KarsSkill",
            &name,
            &[
                (ANN_REVIEW, Some("pending".into())),
                (ANN_LOCKED_DIGEST, None),
                (ANN_APPROVED_BY, None),
                (ANN_APPROVED_AT, None),
            ],
        )
        .await
        .map_err(upstream)?;
    Ok(Json(to_skill(&updated)))
}

#[derive(Debug, serde::Deserialize)]
pub struct ApproveSkillRequest {}

#[derive(Debug, Serialize)]
pub struct ProfileRoleDto {
    pub name: String,
    pub system_prompt: Option<String>,
    pub skills: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileDto {
    pub name: String,
    pub namespace: String,
    pub domain: Option<String>,
    pub phase: Option<String>,
    pub template_digest: Option<String>,
    // Instantiation fields — so the team composer can prefill a whole team from
    // a profile (the profile is a vetted org template, not a dead-end record).
    pub display_name: Option<String>,
    pub charter_template: Option<String>,
    pub tier: Option<i32>,
    pub tool_policy: Option<String>,
    pub knowledge_commons: Option<String>,
    pub roles: Vec<ProfileRoleDto>,
    /// Raw spec for Edit-form prefill.
    pub spec: serde_json::Value,
}

fn to_profile(o: &DynamicObject) -> ProfileDto {
    let sp = spec(o);
    let roles = sp
        .get("roles")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let name = r.get("name")?.as_str()?.to_string();
                    Some(ProfileRoleDto {
                        name,
                        system_prompt: r
                            .get("systemPrompt")
                            .and_then(|s| s.as_str())
                            .map(String::from),
                        skills: r
                            .get("skills")
                            .and_then(|s| s.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    ProfileDto {
        name: name_of(o),
        namespace: ns_of(o),
        domain: s(sp, "domain"),
        phase: s(status(o), "phase"),
        template_digest: s(status(o), "templateDigest"),
        display_name: s(sp, "displayName"),
        charter_template: s(sp, "charterTemplate"),
        tier: sp
            .get("defaultEnvelope")
            .and_then(|e| e.get("tier"))
            .and_then(|t| t.as_i64())
            .map(|t| t as i32),
        tool_policy: s(sp, "toolPolicy"),
        knowledge_commons: s(sp, "knowledgeCommons"),
        roles,
        spec: sp.clone(),
    }
}

pub async fn list_profiles(State(state): State<AppState>) -> AppResult<Json<Vec<ProfileDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind_all("KarsProfile")
        .await
        .map_err(upstream)?;
    let mut dtos: Vec<ProfileDto> = items.iter().map(to_profile).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

/// `PUT /api/operator/profiles` — author/edit a `KarsProfile` (SSA).
pub async fn put_profile(
    State(state): State<AppState>,
    Json(req): Json<ApplyCrdRequest>,
) -> AppResult<Json<serde_json::Value>> {
    apply_governance(require_cluster(&state)?, "KarsProfile", req).await
}

/// `DELETE /api/operator/profiles/:name` — remove a `KarsProfile`.
pub async fn delete_profile(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    delete_governance(require_cluster(&state)?, "KarsProfile", &name, None).await
}

// ─── Credentials (secure repo/system access for agents) ──────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRequest {
    /// The agent/team this credential is for (becomes `<target>-credentials`).
    pub target: String,
    pub kind: String,
    pub namespace: String,
    #[serde(default)]
    pub target_uid: Option<String>,
    /// The env var name the agent reads (e.g. GITHUB_TOKEN, BRAVE_API_KEY).
    pub key: String,
    /// The secret value. Stored only in the K8s Secret; never read back.
    pub value: String,
    #[serde(default)]
    pub review: Option<String>,
}

/// A DNS-1123 label (lowercase alphanumeric + hyphens, must start/end
/// alphanumeric, ≤63 chars) — the constraint on the `kars-<target>` namespace
/// derived below, so an invalid target is rejected before it reaches the API
/// server as an opaque 422.
pub(super) fn is_dns1123_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// A POSIX-ish environment variable name: letters/digits/underscore, not
/// starting with a digit. Agents read the credential under this name.
pub(super) fn is_env_key(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(super) fn credential_write_error(error: kube::Error) -> AppError {
    match error {
        kube::Error::Api(status) if status.code == 409 => AppError::Conflict(
            "Credential authority changed or a source already exists. Refresh credential metadata and review before resubmitting; a source write may already be stored. No automatic retry or rollback was attempted.".into(),
        ),
        kube::Error::Api(status) => {
            AppError::Upstream(format!("Credential write: Kubernetes status {}", status.code))
        }
        _ => AppError::Upstream("Credential write transport or serialization failed".into()),
    }
}

/// `POST /api/operator/credentials` — write a governed workspace source and
/// bind its actual UID to the reviewed target. Values are write-only.
/// A binding conflict may follow a committed source write; report 409 without
/// retrying the transaction or deleting a source whose delivery is uncertain.
pub async fn put_credential(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    Json(req): Json<CredentialRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let target = req.target.trim();
    let key = req.key.trim();
    if target.is_empty() || key.is_empty() || req.value.is_empty() {
        return Err(AppError::BadRequest(
            "target, key and value are required".into(),
        ));
    }
    // Validate client-supplied names client-side of the API server, so the
    // failure is an actionable 400 rather than an opaque Kubernetes 422.
    if !is_dns1123_label(target) {
        return Err(AppError::BadRequest(
            "target must be a DNS-1123 label (lowercase letters, digits, hyphens; not starting/ending with a hyphen; ≤63 chars)".into(),
        ));
    }
    if !is_env_key(key) {
        return Err(AppError::BadRequest(
            "key must be a valid environment variable name (letters, digits, underscore; not starting with a digit)".into(),
        ));
    }
    if !is_dns1123_label(&req.namespace)
        || !["KarsSandbox", "KarsTask", "KarsTeam"].contains(&req.kind.as_str())
    {
        return Err(AppError::BadRequest("An explicit workspace namespace and KarsSandbox/KarsTask/KarsTeam target kind are required".into()));
    }
    if req.review.is_some() {
        let principal = principal.ok_or_else(|| {
            AppError::Forbidden(
                "A verified operator is required for reviewed credential writes".into(),
            )
        })?;
        return super::credential_review::write(&state, &principal.0, req).await;
    }
    Ok(Json(
        cluster
            .write_agent_credentials(
                &req.namespace,
                &req.kind,
                target,
                req.target_uid.as_deref(),
                std::collections::BTreeMap::from([(key.to_string(), req.value)]),
                Vec::new(),
            )
            .await
            .map_err(credential_write_error)?,
    ))
}

// ─── Provider onboarding (model providers for missions + envelope gen) ───────

#[derive(Debug, serde::Deserialize)]
pub struct ProviderRequest {
    /// "github-models" | "azure-openai" | "foundry".
    pub kind: String,
    /// Auth mode: "api" (key), "workload" (workload identity), "agentid".
    pub auth: String,
    pub endpoint: Option<String>,
    /// Comma-separated deployment ids to expose in the catalog.
    pub models: String,
    /// Optional key when auth=api; stored write-only in kars-system.
    pub key: Option<String>,
}

/// `POST /api/operator/providers` — onboard a model provider. Sets the catalog
/// the controller serves, records the endpoint, and (for api auth) stores the
/// key as a write-only secret. Workload/agentid auth store no secret — the
/// controller authenticates via its identity. The catalog feeds both mission
/// models and envelope generation. Patches the controller deployment env.
pub async fn put_provider(
    State(state): State<AppState>,
    Json(req): Json<ProviderRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    // GUARD: this endpoint only ever wires FOUNDRY_ENDPOINT + AZURE_OPENAI_API_KEY
    // onto the controller (set_controller_catalog below). GitHub Copilot needs a
    // COPILOT_GITHUB_TOKEN (exchanged for a Copilot JWT by the router's copilot_auth
    // path) and GitHub Models needs its catalog endpoint recognized by the router's
    // is_github_models() host check — neither is wired by this route. Silently
    // "succeeding" here would tell the operator the cluster default changed when it
    // did not. Reject until real backend wiring exists; both kinds work correctly
    // today via the "additional provider" flow (POST .../providers/additional),
    // which does propagate a real per-provider tag + credential to every sandbox.
    if req.kind == "github-copilot" || req.kind == "github-models" {
        return Err(AppError::BadRequest(format!(
            "{} can't be set as the cluster's default provider from this form yet \
             (it only wires an Azure-style endpoint/key). Add it as an additional \
             provider instead — every sandbox can already route to it per-request \
             via an InferencePolicy model preference.",
            if req.kind == "github-copilot" {
                "GitHub Copilot"
            } else {
                "GitHub Models"
            }
        )));
    }
    let models: Vec<&str> = req
        .models
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if models.is_empty() {
        return Err(AppError::BadRequest(
            "at least one model deployment is required".into(),
        ));
    }
    let mut key_secret: Option<(String, String)> = None;
    if req.auth == "api" {
        if let Some(k) = req.key.as_deref().filter(|k| !k.trim().is_empty()) {
            let secret = format!("kars-provider-{}", req.kind);
            cluster.upsert_secret("kars-system", &secret, serde_json::json!({
                "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                "metadata": {"name": secret, "namespace": "kars-system", "labels": {"app.kubernetes.io/managed-by": "kars-bridge"}},
                "stringData": {"API_KEY": k},
            })).await.map_err(upstream)?;
            key_secret = Some((secret, "API_KEY".to_string()));
        } else {
            return Err(AppError::BadRequest(
                "auth=api requires a provider API key".into(),
            ));
        }
    }
    let key_ref = key_secret.as_ref().map(|(s, k)| (s.as_str(), k.as_str()));
    cluster
        .set_controller_catalog(&models.join(","), req.endpoint.as_deref(), key_ref)
        .await
        .map_err(upstream)?;
    Ok(Json(
        serde_json::json!({"onboarded": true, "kind": req.kind, "auth": req.auth, "models": models,
        "note": if key_secret.is_some() {
            "Catalog updated and the API key wired into the controller via secretKeyRef (AZURE_OPENAI_API_KEY), which the controller propagates to sandbox pods. The controller is rolling to pick it up."
        } else {
            "Catalog updated; the controller is rolling. workload/agentid auth use the controller's own identity — no key stored."
        }}),
    ))
}

/// One discoverable model, browser-facing.
#[derive(Debug, Serialize)]
pub struct DiscoveredModelDto {
    /// The exact id to feed back into `ProviderRequest.models` (e.g. `openai/gpt-4o`).
    pub id: String,
    /// Human label, when richer than the id (e.g. "OpenAI GPT-4o").
    pub label: Option<String>,
    /// True for a highlighted/pre-selected pick. For GitHub Copilot this is
    /// every model in Copilot's own `powerful` picker category (the flagship
    /// tier), derived LIVE from the `/models` endpoint — not a hand-picked id
    /// that goes stale. Absent/false for GitHub Models / Azure OpenAI, which
    /// have no "best pick" signal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recommended: bool,
    /// Short human detail (e.g. "Anthropic · 1.0M ctx · powerful"), when the
    /// provider exposes it (GitHub Copilot's live catalog does). Shown in the
    /// Model catalogue so a model isn't just an opaque id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The same editor/integration headers the router's `copilot_auth` sends, so
/// the seat sees a consistent client identity across discovery and inference.
const COPILOT_EDITOR_VERSION: &str = "vscode/1.107.0";
const COPILOT_INTEGRATION_ID: &str = "vscode-chat";
/// Public OAuth client id for the GitHub Copilot device-flow integration — the
/// SAME id the CLI's `copilotDeviceLogin` uses (cli/src/github-copilot.ts). A
/// token minted through this flow is authorized for the `copilot_internal/v2/
/// token` exchange, unlike a stock `gh auth login` token (which 404s there).
const COPILOT_OAUTH_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

/// `POST /api/operator/providers/copilot/login/start` — begin the GitHub
/// device-flow OAuth so the operator can sign in to Copilot properly (no
/// hand-pasted token). Returns the user code + verification URL to show, and
/// the device code the client polls with.
pub async fn copilot_login_start() -> AppResult<Json<serde_json::Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let resp = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("User-Agent", "kars-bridge")
        .json(&serde_json::json!({ "client_id": COPILOT_OAUTH_CLIENT_ID, "scope": "read:user" }))
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("device-code request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Upstream(format!(
            "GitHub device-code returned {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("bad device-code JSON: {e}")))?;
    Ok(Json(serde_json::json!({
        "device_code": body.get("device_code").and_then(|v| v.as_str()).unwrap_or_default(),
        "user_code": body.get("user_code").and_then(|v| v.as_str()).unwrap_or_default(),
        "verification_uri": body.get("verification_uri").and_then(|v| v.as_str()).unwrap_or("https://github.com/login/device"),
        "interval": body.get("interval").and_then(|v| v.as_u64()).unwrap_or(5),
        "expires_in": body.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(900),
    })))
}

#[derive(Debug, serde::Deserialize)]
pub struct CopilotLoginPollRequest {
    pub device_code: String,
}

/// `POST /api/operator/providers/copilot/login/poll` — poll the device flow.
/// While the user hasn't approved yet, returns `{status:"pending"}`. On
/// approval it: (1) verifies the minted token is Copilot-entitled, (2) stores
/// it server-side as the Copilot provider credential (COPILOT_GITHUB_TOKEN in
/// the shared providers secret) — the token NEVER returns to the browser,
/// (3) busts the live-catalog cache, and (4) returns the seat's live model
/// list so the wizard can show it immediately.
pub async fn copilot_login_poll(
    State(state): State<AppState>,
    Json(req): Json<CopilotLoginPollRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let resp = client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .header("User-Agent", "kars-bridge")
        .json(&serde_json::json!({
            "client_id": COPILOT_OAUTH_CLIENT_ID,
            "device_code": req.device_code,
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        }))
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("device poll failed: {e}")))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("bad poll JSON: {e}")))?;

    if let Some(token) = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
    {
        // Verify the seat is genuinely Copilot-entitled before storing.
        copilot_jwt(token).await?;
        // Store server-side as the Copilot provider credential (never returned
        // to the browser). Also refresh the controller's default credential so
        // a cluster whose default IS Copilot starts working immediately.
        cluster
            .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
                keys.insert("COPILOT_GITHUB_TOKEN".to_string(), token.to_string());
            })
            .await
            .map_err(upstream)?;
        // Fresh token → invalidate any cached catalog for the old one.
        invalidate_copilot_catalog_cache();
        let models = copilot_catalog_cached(token).await;
        return Ok(Json(serde_json::json!({
            "status": "authorized",
            "models": models.iter().map(|(id, rec, detail)| serde_json::json!({"id": id, "recommended": rec, "detail": detail})).collect::<Vec<_>>(),
        })));
    }

    match body.get("error").and_then(|v| v.as_str()) {
        Some("authorization_pending") | Some("slow_down") => {
            Ok(Json(serde_json::json!({ "status": "pending" })))
        }
        Some("expired_token") => Err(AppError::Rejected(
            "The sign-in code expired before it was approved. Start again.".into(),
        )),
        Some("access_denied") => Err(AppError::Rejected(
            "Sign-in was cancelled on GitHub.".into(),
        )),
        Some(other) => Err(AppError::Upstream(format!(
            "GitHub device flow error: {other}"
        ))),
        None => Ok(Json(serde_json::json!({ "status": "pending" }))),
    }
}

/// Exchange a GitHub OAuth token / PAT for a short-lived Copilot JWT — the
/// exact same endpoint (and `chat_enabled` eligibility semantics) the CLI's
/// `checkCopilotEligibility` and the router's `copilot_auth` use. A 200 with a
/// token and `chat_enabled != false` means the router will actually be able to
/// serve inference for this seat, not merely that the token parses. Returns
/// the JWT so the caller can immediately query the live `/models` catalog with
/// it (no second exchange).
pub(crate) async fn copilot_jwt(gh_token: &str) -> Result<String, AppError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("token {gh_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "kars-bridge")
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("Copilot eligibility check failed: {e}")))?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err(AppError::Rejected(
            "This GitHub token isn't entitled to Copilot. Enable Copilot at https://github.com/settings/copilot, or use a token from an account with an active seat.".into(),
        ));
    }
    if !resp.status().is_success() {
        return Err(AppError::Upstream(format!(
            "Copilot token endpoint returned {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("bad Copilot token response: {e}")))?;
    if body.get("chat_enabled").and_then(|c| c.as_bool()) == Some(false) {
        return Err(AppError::Rejected(
            "Copilot subscription is active but Chat is disabled. Enable it at https://github.com/settings/copilot/features.".into(),
        ));
    }
    body.get("token")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Upstream("Copilot token endpoint returned no token".into()))
}

/// Parse GitHub Copilot's live `/models` response into the browser DTO. Pure
/// (no I/O) so it's unit-testable against a captured sample. Surfaces ONLY the
/// models a seat can actually reason with:
///   • `capabilities.type == "chat"` — excludes embeddings.
///   • `model_picker_enabled == true` — Copilot's own "show in picker" flag;
///     drops legacy/hidden aliases (gpt-4o, gpt-3.5-turbo, dated snapshots).
///   • policy absent, OR `policy.state == "enabled"` — a gated preview the
///     seat hasn't opted into is not usable, so it's hidden.
/// Ordering: Copilot's picker category (powerful → versatile → lightweight),
/// then context window desc, then id — so the flagship tier leads. Every
/// `powerful`-category model is marked `recommended` (pre-checked in the
/// wizard). This is entirely live: a new flagship (gpt-5.7, opus-4.9, …)
/// appears and is categorised by GitHub, with no code change here.
pub(crate) fn parse_copilot_models(body: &serde_json::Value) -> Vec<DiscoveredModelDto> {
    fn category_rank(cat: &str) -> u8 {
        match cat {
            "powerful" => 0,
            "versatile" => 1,
            "lightweight" => 2,
            _ => 3,
        }
    }
    let mut rows: Vec<(u8, u64, String, DiscoveredModelDto)> = Vec::new();
    let Some(data) = body.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    for m in data {
        let caps = m.get("capabilities");
        let is_chat = caps.and_then(|c| c.get("type")).and_then(|t| t.as_str()) == Some("chat");
        let picker = m
            .get("model_picker_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // policy absent => generally available; present => must be "enabled".
        let policy_ok = match m.get("policy") {
            None => true,
            Some(p) => p.get("state").and_then(|s| s.as_str()) == Some("enabled"),
        };
        if !(is_chat && picker && policy_ok) {
            continue;
        }
        let Some(id) = m
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(id);
        let vendor = m.get("vendor").and_then(|v| v.as_str()).unwrap_or("");
        let category = m
            .get("model_picker_category")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let ctx = caps
            .and_then(|c| c.get("limits"))
            .and_then(|l| l.get("max_context_window_tokens"))
            .and_then(|v| v.as_u64());
        let ctx_label = ctx
            .map(|c| {
                if c >= 1_000_000 {
                    format!("{:.1}M ctx", c as f64 / 1_000_000.0)
                } else {
                    format!("{}k ctx", c / 1000)
                }
            })
            .unwrap_or_default();
        let label = [vendor, &ctx_label, category]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ");
        let detail = if label.is_empty() {
            None
        } else {
            Some(label.clone())
        };
        rows.push((
            category_rank(category),
            ctx.unwrap_or(0),
            id.to_string(),
            DiscoveredModelDto {
                id: id.to_string(),
                label: (name != id || !label.is_empty()).then(|| {
                    if label.is_empty() {
                        name.to_string()
                    } else {
                        format!("{name} — {label}")
                    }
                }),
                recommended: category == "powerful",
                detail,
            },
        ));
    }
    // Sort: powerful first, then largest context, then id desc (newer version
    // numbers tend to sort higher) — purely presentational.
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
    rows.into_iter().map(|(_, _, _, dto)| dto).collect()
}

/// Fetch the live Copilot model catalog for a seat, given its exchanged JWT.
pub(crate) async fn fetch_copilot_models(jwt: &str) -> Result<Vec<DiscoveredModelDto>, AppError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let resp = client
        .get("https://api.githubcopilot.com/models")
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Editor-Version", COPILOT_EDITOR_VERSION)
        .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("Copilot /models request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Upstream(format!(
            "Copilot /models returned {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("bad Copilot /models JSON: {e}")))?;
    Ok(parse_copilot_models(&body))
}

/// A short-TTL cache for the live Copilot catalog so `build_options` (hit on
/// every Configuration page load AND every orchestrator compose) doesn't do a
/// token-exchange + /models round-trip each time. Keyed by the token so a
/// changed seat re-fetches; 5-minute freshness is plenty for a model list.
type CopilotCatalog = Vec<(String, bool, Option<String>)>;
type CachedCopilotCatalog = (String, std::time::Instant, CopilotCatalog);
static COPILOT_CATALOG_CACHE: std::sync::Mutex<Option<CachedCopilotCatalog>> =
    std::sync::Mutex::new(None);

/// Drop the cached Copilot catalog — call after a fresh sign-in so the next
/// `build_options` re-fetches against the new token immediately.
pub(crate) fn invalidate_copilot_catalog_cache() {
    *COPILOT_CATALOG_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = None;
}

/// Live (cached) Copilot model catalog for a seat token: `(deployment_id,
/// recommended, detail)` for every model the seat can actually use. Best-effort
/// — on any auth/network failure it returns the last good cache if still
/// present, else empty, so a transient Copilot outage never blanks the catalogue.
pub(crate) async fn copilot_catalog_cached(gh_token: &str) -> Vec<(String, bool, Option<String>)> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(300);
    {
        let guard = COPILOT_CATALOG_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some((tok, at, models)) = guard.as_ref()
            && tok == gh_token
            && at.elapsed() < TTL
        {
            return models.clone();
        }
    }
    let fetched = async {
        let jwt = copilot_jwt(gh_token).await.ok()?;
        let models = fetch_copilot_models(&jwt).await.ok()?;
        Some(
            models
                .into_iter()
                .map(|m| (m.id, m.recommended, m.detail))
                .collect::<Vec<_>>(),
        )
    }
    .await;
    match fetched {
        Some(models) => {
            let mut guard = COPILOT_CATALOG_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *guard = Some((
                gh_token.to_string(),
                std::time::Instant::now(),
                models.clone(),
            ));
            models
        }
        None => {
            // Fetch failed — reuse a still-present cache entry (even if stale)
            // rather than blanking the catalogue on a transient hiccup.
            let guard = COPILOT_CATALOG_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            guard
                .as_ref()
                .filter(|(tok, _, _)| tok == gh_token)
                .map(|(_, _, m)| m.clone())
                .unwrap_or_default()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct DiscoverModelsRequest {
    /// "github-models" | "azure-openai" | "github-copilot". (Foundry already
    /// discovers models via the existing /api/operator/foundry/verify.)
    pub kind: String,
    pub endpoint: Option<String>,
    pub key: Option<String>,
}

/// `POST /api/operator/providers/discover` — real, live model discovery so the
/// operator never hand-types a deployment id. GitHub Models queries the public
/// catalog (no auth). Azure OpenAI queries the data-plane `/openai/deployments`
/// endpoint using the operator-supplied endpoint + key (a live round-trip, so a
/// wrong key/endpoint surfaces as an immediate, actionable error). GitHub
/// Copilot exchanges the supplied token for a Copilot JWT (verifying the seat +
/// Chat entitlement live) and then queries the seat's LIVE `/models` catalog —
/// so the picker always reflects the models GitHub currently serves this seat
/// (gpt-5.6, claude-opus-4.8, gemini-3.1-pro, …), never a hand-maintained list
/// that goes stale.
pub async fn discover_models(
    Json(req): Json<DiscoverModelsRequest>,
) -> AppResult<Json<Vec<DiscoveredModelDto>>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match req.kind.as_str() {
        "github-models" => {
            let resp = client
                .get("https://models.github.ai/catalog/models")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
                .map_err(|e| {
                    AppError::Upstream(format!("GitHub Models catalog request failed: {e}"))
                })?;
            if !resp.status().is_success() {
                return Err(AppError::Upstream(format!(
                    "GitHub Models catalog returned {}",
                    resp.status()
                )));
            }
            let body: Vec<serde_json::Value> = resp
                .json()
                .await
                .map_err(|e| AppError::Upstream(format!("bad catalog JSON: {e}")))?;
            let models = body
                .iter()
                .filter_map(|m| {
                    let id = m.get("id").and_then(|v| v.as_str())?.to_string();
                    let name = m.get("name").and_then(|v| v.as_str()).map(str::to_string);
                    Some(DiscoveredModelDto {
                        id,
                        label: name,
                        recommended: false,
                        detail: None,
                    })
                })
                .collect();
            Ok(Json(models))
        }
        "azure-openai" => {
            let endpoint = req
                .endpoint
                .as_deref()
                .map(|e| e.trim().trim_end_matches('/'))
                .filter(|e| !e.is_empty())
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "endpoint is required to discover Azure OpenAI deployments".into(),
                    )
                })?;
            let key = req
                .key
                .as_deref()
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| AppError::BadRequest(
                    "an API key is required to discover deployments (workload/agentid auth can't be exercised from the browser — enter deployment ids manually, or discover once with a temporary key)".into(),
                ))?;
            let url = format!("{endpoint}/openai/deployments?api-version=2023-05-15");
            let resp = client
                .get(&url)
                .header("api-key", key)
                .send()
                .await
                .map_err(|e| AppError::Upstream(format!("Azure OpenAI request failed: {e}")))?;
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(AppError::Rejected(format!(
                    "Azure OpenAI rejected the discovery request ({status}) — check the endpoint and key: {body_text}"
                )));
            }
            let body: serde_json::Value = serde_json::from_str(&body_text)
                .map_err(|e| AppError::Upstream(format!("bad deployments JSON: {e}")))?;
            let models = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| {
                            let id = d.get("id").and_then(|v| v.as_str())?.to_string();
                            let base = d
                                .get("model")
                                .and_then(|v| v.as_str())
                                .map(|m| format!("deployment of {m}"));
                            Some(DiscoveredModelDto {
                                id,
                                label: base,
                                recommended: false,
                                detail: None,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(Json(models))
        }
        "github-copilot" => {
            let token = req
                .key
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .ok_or_else(|| AppError::BadRequest(
                    "a GitHub token (OAuth token or PAT with Copilot access) is required to verify the seat before showing the model catalog".into(),
                ))?;
            let jwt = copilot_jwt(token).await?;
            let models = fetch_copilot_models(&jwt).await?;
            Ok(Json(models))
        }
        other => Err(AppError::BadRequest(format!(
            "unknown provider kind '{other}' for discovery"
        ))),
    }
}

// ─── Multi-provider inference (§ inference-provider-wizard) ─────────────────
//
// The single "Inference provider" flow above (`put_provider`) sets the ONE
// default provider every mission inherits. This section manages ADDITIONAL
// providers that can be configured *at the same time* — e.g. GitHub Copilot
// as the default, Azure AI Foundry also connected — so an InferencePolicy's
// `modelPreference.primary.provider` can route a specific sandbox's calls to
// whichever one actually serves the model it needs (a sub-agent on gpt-4.1
// via Foundry, a principal on opus-4.8 via Copilot, in the SAME cluster).
//
// Storage: the `kars-inference-providers` Secret in `kars-system`. Its KEYS
// are the literal env var names `inference-router::config::Config::from_env`
// already parses generically (`KARS_PROVIDER_<TAG>_ENDPOINT` + optional
// `_API_KEY`/`_TOKEN`, or the well-known `COPILOT_GITHUB_TOKEN` for the
// GitHub Copilot special case) — no router-side change needed to support a
// provider added here. The controller mirrors this ONE secret into every
// sandbox's own namespace (the same mechanism already used for
// `kars-github-app`), and every sandbox's router picks whichever provider a
// request's InferencePolicy names — never all-or-nothing, never guessed from
// what's merely present in the env.
const INFERENCE_PROVIDERS_SECRET: &str = "kars-inference-providers";
const INFERENCE_PROVIDERS_NS: &str = "kars-system";

/// One additional provider, as surfaced to the operator (never the key/token
/// itself — `has_key` only tells you whether one is stored).
#[derive(Debug, Serialize)]
pub struct AdditionalProviderDto {
    pub tag: String,
    pub endpoint: Option<String>,
    pub has_key: bool,
    /// Deployment ids the operator declared this provider serves — these
    /// feed the shared model catalog (`GET /api/options`), tagged with this
    /// provider, so InferencePolicy's model picker can offer them.
    pub models: Vec<String>,
}

/// `GET /api/operator/providers/additional` — list every additional provider
/// configured on this cluster (beyond the single default from `put_provider`).
pub async fn list_additional_providers(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<AdditionalProviderDto>>> {
    let cluster = require_cluster(&state)?;
    let keys = cluster
        .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;
    let mut providers: std::collections::BTreeMap<String, AdditionalProviderDto> =
        std::collections::BTreeMap::new();
    for key in keys.keys() {
        if let Some(tag_part) = key
            .strip_prefix("KARS_PROVIDER_")
            .and_then(|r| r.strip_suffix("_ENDPOINT"))
        {
            let tag = tag_part.to_ascii_lowercase().replace('_', "-");
            providers
                .entry(tag.clone())
                .or_insert(AdditionalProviderDto {
                    tag,
                    endpoint: None,
                    has_key: false,
                    models: Vec::new(),
                });
        }
    }
    for (tag, dto) in providers.iter_mut() {
        let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
        dto.endpoint = keys
            .get(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"))
            .cloned();
        dto.has_key = keys.contains_key(&format!("KARS_PROVIDER_{tag_upper}_API_KEY"))
            || keys.contains_key(&format!("KARS_PROVIDER_{tag_upper}_TOKEN"));
        dto.models = keys
            .get(&format!("KARS_PROVIDER_{tag_upper}_MODELS"))
            .map(|m| {
                m.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
    }
    // GitHub Copilot is a special case (well-known endpoint, no
    // KARS_PROVIDER_*_ENDPOINT needed — see resolve_provider in the router).
    if keys.contains_key("COPILOT_GITHUB_TOKEN") {
        providers.insert(
            "github-copilot".to_string(),
            AdditionalProviderDto {
                tag: "github-copilot".to_string(),
                endpoint: Some("https://api.githubcopilot.com".to_string()),
                has_key: true,
                models: keys
                    .get("KARS_PROVIDER_GITHUB_COPILOT_MODELS")
                    .map(|m| {
                        m.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        );
    }
    Ok(Json(providers.into_values().collect()))
}

#[derive(Debug, serde::Deserialize)]
pub struct AdditionalProviderRequest {
    /// Lowercase, hyphenated tag (e.g. "foundry", "github-models"). The
    /// reserved tag "github-copilot" only needs `api_key` (its endpoint is
    /// the well-known Copilot API and is never user-editable).
    pub tag: String,
    pub endpoint: Option<String>,
    /// Dev-mode direct key/token (e.g. a GitHub Models PAT, or a second
    /// Azure OpenAI resource's key). Optional for providers that authenticate
    /// via Workload Identity in production (Foundry/Azure OpenAI need no key
    /// at all on AKS — see `inference-router::auth::WorkloadIdentityAuth`).
    pub api_key: Option<String>,
    /// Comma-separated deployment ids this provider serves — feeds the
    /// shared model catalog (`GET /api/options`), tagged with this provider,
    /// so InferencePolicy's model picker can offer "this model via THIS
    /// provider" without any change to that editor.
    pub models: Option<String>,
}

/// `PUT /api/operator/providers/additional` — add or update one additional
/// provider. Read-modify-write against the shared Secret so configuring one
/// provider never disturbs another already stored there.
pub async fn put_additional_provider(
    State(state): State<AppState>,
    Json(req): Json<AdditionalProviderRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let tag = req.tag.trim().to_ascii_lowercase();
    if !is_dns1123_label(&tag) {
        return Err(AppError::BadRequest(
            "tag must be lowercase letters, digits, hyphens (e.g. \"foundry\", \"github-models\")"
                .into(),
        ));
    }
    let is_copilot = tag == "github-copilot";
    if !is_copilot {
        let endpoint = req
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .ok_or_else(|| AppError::BadRequest("endpoint is required for this provider".into()))?;
        if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
            return Err(AppError::BadRequest("endpoint must be a URL".into()));
        }
    }
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let key_val = req
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string);
    let models: Vec<&str> = req
        .models
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if models.is_empty() {
        return Err(AppError::BadRequest(
            "at least one model deployment id is required (comma-separated) so InferencePolicy can offer it".into(),
        ));
    }
    let models_joined = models.join(",");
    cluster
        .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
            if is_copilot {
                if let Some(k) = key_val.clone() {
                    keys.insert("COPILOT_GITHUB_TOKEN".to_string(), k);
                }
                keys.insert(
                    "KARS_PROVIDER_GITHUB_COPILOT_MODELS".to_string(),
                    models_joined.clone(),
                );
            } else {
                if let Some(endpoint) = req
                    .endpoint
                    .as_deref()
                    .map(str::trim)
                    .filter(|e| !e.is_empty())
                {
                    keys.insert(
                        format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"),
                        endpoint.to_string(),
                    );
                }
                if let Some(k) = key_val.clone() {
                    keys.insert(format!("KARS_PROVIDER_{tag_upper}_API_KEY"), k);
                }
                keys.insert(
                    format!("KARS_PROVIDER_{tag_upper}_MODELS"),
                    models_joined.clone(),
                );
            }
        })
        .await
        .map_err(upstream)?;
    Ok(Json(serde_json::json!({
        "configured": true,
        "tag": tag,
        "note": "Every sandbox's router now has this provider available. Which one a given request actually uses is decided per-sandbox by its InferencePolicy.modelPreference — this alone doesn't make it the default."
    })))
}

/// `DELETE /api/operator/providers/additional/:tag` — remove one additional
/// provider's keys from the shared Secret (leaves other providers intact).
pub async fn delete_additional_provider(
    State(state): State<AppState>,
    axum::extract::Path(tag): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let tag = tag.trim().to_ascii_lowercase();
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let is_copilot = tag == "github-copilot";
    cluster
        .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
            if is_copilot {
                keys.remove("COPILOT_GITHUB_TOKEN");
                keys.remove("KARS_PROVIDER_GITHUB_COPILOT_MODELS");
            } else {
                keys.remove(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"));
                keys.remove(&format!("KARS_PROVIDER_{tag_upper}_API_KEY"));
                keys.remove(&format!("KARS_PROVIDER_{tag_upper}_TOKEN"));
                keys.remove(&format!("KARS_PROVIDER_{tag_upper}_MODELS"));
            }
        })
        .await
        .map_err(upstream)?;
    Ok(Json(serde_json::json!({"removed": true, "tag": tag})))
}

/// `POST /api/operator/providers/additional/:tag/promote` — make an already-
/// connected additional provider the cluster's DEFAULT (patches the
/// controller's own env — every mission that leaves its model unset inherits
/// this). Reads the tag's endpoint/key/models straight from
/// `kars-inference-providers` server-side (never exposed to the browser) and
/// re-points the SAME secret+key via `secretKeyRef` — no key duplication.
///
/// `github-copilot` is rejected: it authenticates via `COPILOT_GITHUB_TOKEN`
/// exchanged for a short-lived Copilot JWT, a completely different mechanism
/// than the endpoint+key shape every other provider here uses — the same
/// reason `put_provider` already refuses to set it as default from the other
/// form (see that handler's comment). Every other tag (Foundry, Azure OpenAI,
/// Custom, GitHub Models, and a local in-cluster model) is a plain
/// endpoint(+optional key), which is exactly what `set_controller_catalog`
/// wires — so promoting any of THOSE genuinely works.
pub async fn promote_additional_provider(
    State(state): State<AppState>,
    axum::extract::Path(tag): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let tag = tag.trim().to_ascii_lowercase();
    // GitHub Copilot IS promotable now — the wizard's device sign-in stores a
    // Copilot-authorized token, which `set_copilot_as_default` wires onto the
    // controller (KARS_PROVIDER + COPILOT_GITHUB_TOKEN), unlike the endpoint+key
    // shape every other provider uses.
    if tag == "github-copilot" {
        let keys = cluster
            .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
            .await
            .map_err(upstream)?;
        if !keys.contains_key("COPILOT_GITHUB_TOKEN") {
            return Err(AppError::BadRequest(
                "Sign in to GitHub Copilot first (Connect a provider → GitHub Copilot) — then it can be set as the cluster default.".into(),
            ));
        }
        let models = keys
            .get("KARS_PROVIDER_GITHUB_COPILOT_MODELS")
            .cloned()
            .unwrap_or_default();
        let models = if models.trim().is_empty() {
            // No explicit selection stored — fall back to the live catalog so
            // the default catalogue isn't empty.
            copilot_catalog_cached(keys.get("COPILOT_GITHUB_TOKEN").unwrap())
                .await
                .into_iter()
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>()
                .join(",")
        } else {
            models
        };
        cluster
            .set_copilot_as_default(&models)
            .await
            .map_err(upstream)?;
        return Ok(Json(serde_json::json!({
            "promoted": true,
            "tag": tag,
            "note": "GitHub Copilot is now the cluster default; the controller is rolling to pick it up. Every mission that leaves its model unset now inherits it."
        })));
    }
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let keys = cluster
        .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;
    let endpoint = keys
        .get(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"))
        .cloned()
        .ok_or_else(|| AppError::BadRequest(format!("no connected provider tagged {tag:?} with an endpoint (GitHub Models has a well-known endpoint but no explicit one is stored, so it can't be promoted this way either)")))?;
    let models = keys
        .get(&format!("KARS_PROVIDER_{tag_upper}_MODELS"))
        .cloned()
        .unwrap_or_default();
    if models.trim().is_empty() {
        return Err(AppError::BadRequest(format!(
            "{tag} has no declared models to promote"
        )));
    }
    let key_ref = if keys.contains_key(&format!("KARS_PROVIDER_{tag_upper}_API_KEY")) {
        Some((
            INFERENCE_PROVIDERS_SECRET,
            format!("KARS_PROVIDER_{tag_upper}_API_KEY"),
        ))
    } else {
        None
    };
    cluster
        .set_controller_catalog(
            &models,
            Some(&endpoint),
            key_ref.as_ref().map(|(s, k)| (*s, k.as_str())),
        )
        .await
        .map_err(upstream)?;
    Ok(Json(serde_json::json!({
        "promoted": true,
        "tag": tag,
        "note": "Cluster default updated; the controller is rolling to pick it up. Every mission that leaves its model unset now inherits this provider."
    })))
}

#[derive(Debug, serde::Deserialize)]
pub struct SetDefaultModelRequest {
    pub deployment: String,
    /// The provider tag that serves this model, as shown in the catalogue
    /// (e.g. "github-copilot", "foundry", "local-llama-3-2-1b-instruct").
    pub provider: String,
}

/// `POST /api/operator/models/default` — make one specific MODEL the cluster
/// default (what the Model catalogue's "Set as default" does). Promotes the
/// model's provider AND pins that model as the default (moved to the front of
/// the catalog, which `set_*_default` treats as KARS_TASK_DEFAULT_MODEL).
pub async fn set_default_model(
    State(state): State<AppState>,
    Json(req): Json<SetDefaultModelRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let deployment = req.deployment.trim().to_string();
    let provider = req.provider.trim().to_ascii_lowercase();
    if deployment.is_empty() {
        return Err(AppError::BadRequest(
            "a model deployment id is required".into(),
        ));
    }
    let keys = cluster
        .read_secret_all(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(upstream)?;

    // Reorder a comma list so `deployment` is first (becomes the default),
    // deduped; ensures the chosen model is present even if it wasn't listed.
    let reorder = |csv: &str| -> String {
        let mut out = vec![deployment.clone()];
        for m in csv.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if m != deployment {
                out.push(m.to_string());
            }
        }
        out.join(",")
    };

    if provider == "github-copilot" {
        if !keys.contains_key("COPILOT_GITHUB_TOKEN") {
            return Err(AppError::BadRequest(
                "Sign in to GitHub Copilot first.".into(),
            ));
        }
        let existing = keys
            .get("KARS_PROVIDER_GITHUB_COPILOT_MODELS")
            .cloned()
            .unwrap_or_default();
        let models = if existing.trim().is_empty() {
            // fall back to the live catalog so the catalog isn't just one model
            let mut live = copilot_catalog_cached(keys.get("COPILOT_GITHUB_TOKEN").unwrap())
                .await
                .into_iter()
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>()
                .join(",");
            if live.trim().is_empty() {
                live = deployment.clone();
            }
            reorder(&live)
        } else {
            reorder(&existing)
        };
        cluster
            .set_copilot_as_default(&models)
            .await
            .map_err(upstream)?;
        return Ok(Json(
            serde_json::json!({"ok": true, "default": deployment, "provider": provider}),
        ));
    }

    // Endpoint-based providers (foundry, azure-openai, custom, local-*): promote
    // via set_controller_catalog with the chosen model first.
    let tag_upper = provider.to_ascii_uppercase().replace('-', "_");
    let endpoint = keys
        .get(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"))
        .cloned()
        .ok_or_else(|| AppError::BadRequest(format!(
            "no connected provider {provider:?} with an endpoint serves {deployment:?} — connect it first"
        )))?;
    let existing = keys
        .get(&format!("KARS_PROVIDER_{tag_upper}_MODELS"))
        .cloned()
        .unwrap_or_default();
    let models = reorder(&existing);
    let key_ref = if keys.contains_key(&format!("KARS_PROVIDER_{tag_upper}_API_KEY")) {
        Some((
            INFERENCE_PROVIDERS_SECRET,
            format!("KARS_PROVIDER_{tag_upper}_API_KEY"),
        ))
    } else {
        None
    };
    cluster
        .set_controller_catalog(
            &models,
            Some(&endpoint),
            key_ref.as_ref().map(|(s, k)| (*s, k.as_str())),
        )
        .await
        .map_err(upstream)?;
    Ok(Json(
        serde_json::json!({"ok": true, "default": deployment, "provider": provider}),
    ))
}
//
// The Bridge is the author of the envelope's governance objects, not just a
// reader. Authoring uses Server-Side Apply (`cluster.apply_kind`) — the
// Kubernetes-native declarative upsert — so the SAME endpoint creates a new CRD
// and edits an existing one (re-apply with changed spec). The security boundary
// is RBAC on the Bridge ServiceAccount plus the CRD's admission/CEL validation;
// a rejected write surfaces the API server's own message via `AppError::Rejected`.

/// Apply-a-governance-CRD request. `spec` is the kind's raw `spec` object so the
/// operator can author every field; `force` opts into taking field ownership on
/// a 409 conflict (default: surface the conflict instead of clobbering).
#[derive(Debug, serde::Deserialize)]
pub struct ApplyCrdRequest {
    pub name: String,
    #[serde(default)]
    pub namespace: Option<String>,
    pub spec: serde_json::Value,
    #[serde(default)]
    pub force: bool,
}

/// Map a CRD-apply kube error to a client-safe AppError: admission/validation
/// (400/422) and field-ownership conflicts (409) are surfaced verbatim (safe —
/// they are the API server's own messages), RBAC denials (403) are made
/// actionable, everything else is an opaque upstream error.
fn apply_err(e: kube::Error) -> AppError {
    if let kube::Error::Api(ae) = &e {
        match ae.code {
            400 | 422 => return AppError::Rejected(ae.message.clone()),
            409 => {
                return AppError::Rejected(format!(
                    "field-ownership conflict: {} — another manager owns a field this apply sets; re-apply with force:true to take ownership",
                    ae.message
                ));
            }
            403 => {
                return AppError::Rejected(format!(
                    "forbidden: {} — the Bridge ServiceAccount lacks RBAC to write this resource",
                    ae.message
                ));
            }
            // Server-Side Apply surfaces schema-validation failures (an unknown
            // or misspelled spec field) as a 500 whose message IS actionable and
            // safe — e.g. "failed to create typed patch object (…): .spec.allow:
            // field not declared in schema". Without this, an operator authoring
            // a bad field gets an opaque "upstream dependency failed" instead of
            // the field to fix. Surface it as a rejection with the real message.
            500 if ae.message.contains("field not declared in schema")
                || ae.message.contains("failed to create typed patch object")
                || ae.message.contains("unknown field") =>
            {
                return AppError::Rejected(ae.message.clone());
            }
            _ => {}
        }
    }
    AppError::Upstream(e.to_string())
}

/// Shared apply path for the three governance kinds. Targets `kars-system` by
/// default (where the controller reads them).
async fn apply_governance(
    cluster: &crate::kars::cluster::Cluster,
    kind: &str,
    req: ApplyCrdRequest,
) -> AppResult<Json<serde_json::Value>> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if !req.spec.is_object() {
        return Err(AppError::BadRequest("spec must be a JSON object".into()));
    }
    let ns = req
        .namespace
        .as_deref()
        .unwrap_or("kars-system")
        .to_string();
    let body = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": kind,
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "app.kubernetes.io/managed-by": "kars-bridge" },
        },
        "spec": req.spec,
    });
    let applied = cluster
        .apply_kind(&ns, kind, body, req.force)
        .await
        .map_err(apply_err)?;
    Ok(Json(serde_json::json!({
        "applied": true,
        "kind": kind,
        "name": name_of(&applied),
        "namespace": ns,
        "note": "Server-Side Apply (field manager kars-bridge): created on first apply, edited on re-apply."
    })))
}

/// `PUT /api/operator/toolpolicies` — author/edit a `ToolPolicy` (SSA).
pub async fn put_toolpolicy(
    State(state): State<AppState>,
    Json(req): Json<ApplyCrdRequest>,
) -> AppResult<Json<serde_json::Value>> {
    apply_governance(require_cluster(&state)?, "ToolPolicy", req).await
}

/// `PUT /api/operator/mcpservers` — author/edit an `McpServer` (SSA).
pub async fn put_mcpserver(
    State(state): State<AppState>,
    Json(req): Json<ApplyCrdRequest>,
) -> AppResult<Json<serde_json::Value>> {
    apply_governance(require_cluster(&state)?, "McpServer", req).await
}

/// `PUT /api/operator/skills` — author/edit a `KarsSkill` (SSA).
pub async fn put_skill(
    State(state): State<AppState>,
    Json(req): Json<ApplyCrdRequest>,
) -> AppResult<Json<serde_json::Value>> {
    apply_governance(require_cluster(&state)?, "KarsSkill", req).await
}

/// Map a delete kube error: 404 → NotFound, 403 → actionable RBAC message,
/// everything else opaque upstream.
fn delete_err(e: kube::Error) -> AppError {
    if let kube::Error::Api(ae) = &e {
        match ae.code {
            404 => return AppError::NotFound,
            403 => {
                return AppError::Rejected(format!(
                    "forbidden: {} — the Bridge ServiceAccount lacks RBAC to delete this resource",
                    ae.message
                ));
            }
            _ => {}
        }
    }
    AppError::Upstream(e.to_string())
}

/// Shared delete path for a governance kind. Targets `kars-system` by default.
async fn delete_governance(
    cluster: &crate::kars::cluster::Cluster,
    kind: &str,
    name: &str,
    namespace: Option<&str>,
) -> AppResult<Json<serde_json::Value>> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    let ns = namespace.unwrap_or("kars-system");
    cluster
        .delete_kind(ns, kind, name)
        .await
        .map_err(delete_err)?;
    Ok(Json(serde_json::json!({
        "deleted": true,
        "kind": kind,
        "name": name,
        "namespace": ns,
        "note": "Deleted with foreground propagation — the controller's finalizers revoke downstream state before removal."
    })))
}

/// `DELETE /api/operator/toolpolicies/:name` — remove a `ToolPolicy`.
pub async fn delete_toolpolicy(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    delete_governance(require_cluster(&state)?, "ToolPolicy", &name, None).await
}

/// `DELETE /api/operator/mcpservers/:name` — remove an `McpServer`.
pub async fn delete_mcpserver(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    delete_governance(require_cluster(&state)?, "McpServer", &name, None).await
}

/// `DELETE /api/operator/skills/:name` — remove a `KarsSkill`.
pub async fn delete_skill(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    delete_governance(require_cluster(&state)?, "KarsSkill", &name, None).await
}

/// `DELETE /api/operator/egress/:name` — revoke a temporary `EgressApproval`.
/// The EgressApproval model is create-to-grant / delete-to-revoke, so deleting
/// the object is the authoritative revoke action (the controller reconciles the
/// sandbox allowlist back to its signed baseline on removal).
pub async fn delete_egress(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    delete_governance(require_cluster(&state)?, "EgressApproval", &name, None).await
}

// ─── datapath-completeness witness (optional eBPF) ───────────────────────────
//
// An independent, kernel-level attestation of what sandboxes ACTUALLY send on
// the network, cross-checked against the controller-declared egress allowlist.
// Produced out-of-band by the optional Inspektor Gadget witness
// (deploy/ebpf-witness/) and published to the `kars-datapath-witness` ConfigMap
// in kars-system. The Bridge only READS that ConfigMap — no eBPF/gadget
// dependency here. Absent ConfigMap => witness not enabled (honest empty), never
// an error.

#[derive(Serialize, Deserialize, Default)]
pub struct DatapathWitnessSandbox {
    pub namespace: String,
    pub sandbox: String,
    #[serde(default)]
    pub declared_hosts: Vec<String>,
    #[serde(default)]
    pub observed_dns: Vec<String>,
    #[serde(default)]
    pub observed_connects: u64,
    #[serde(default)]
    pub beyond_declared: Vec<String>,
    #[serde(default)]
    pub unused_declared: Vec<String>,
    pub verdict: String,
}

#[derive(Serialize)]
pub struct DatapathWitnessDto {
    /// True once the optional eBPF witness is installed and has published a
    /// verdict. False => not enabled (the web layer shows enable instructions).
    pub enabled: bool,
    pub generated_at: Option<String>,
    pub window_seconds: Option<u32>,
    pub sandboxes: Vec<DatapathWitnessSandbox>,
    /// How to turn the witness on — surfaced verbatim in the not-enabled state.
    pub install_hint: String,
}

#[derive(Deserialize)]
struct WitnessDoc {
    generated_at: Option<String>,
    window_seconds: Option<u32>,
    #[serde(default)]
    sandboxes: Vec<DatapathWitnessSandbox>,
}

pub async fn datapath_witness(
    State(state): State<AppState>,
) -> AppResult<Json<DatapathWitnessDto>> {
    let cluster = require_cluster(&state)?;
    let hint = "Enable the optional eBPF datapath witness on the cluster: \
                KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh --continuous"
        .to_string();

    let not_enabled = || DatapathWitnessDto {
        enabled: false,
        generated_at: None,
        window_seconds: None,
        sandboxes: Vec::new(),
        install_hint: hint.clone(),
    };

    let Some(body) = cluster
        .configmap_data("kars-datapath-witness")
        .await
        .and_then(|d| d.get("witness.json").cloned())
    else {
        return Ok(Json(not_enabled()));
    };

    match serde_json::from_str::<WitnessDoc>(&body) {
        Ok(doc) => Ok(Json(DatapathWitnessDto {
            enabled: true,
            generated_at: doc.generated_at,
            window_seconds: doc.window_seconds,
            sandboxes: doc.sandboxes,
            install_hint: hint,
        })),
        // Malformed payload is treated as not-enabled rather than a hard error —
        // the console must never 500 on optional-feature data.
        Err(_) => Ok(Json(not_enabled())),
    }
}

// ─── Diagnostics: live "what's actually broken right now" scan ────────────────
// The Troubleshooting page's real job: not a wiring/roadmap checklist, but the
// concrete problems an operator must act on — pods that won't start, containers
// crash-looping or stuck pulling an image, sandboxes the controller marked
// Degraded/Failed, and agents that came up but never went Ready. Every issue is
// read from live pod/CRD status and carries a plain remedy hint.

#[derive(Debug, Serialize)]
pub struct DiagnosticIssue {
    /// "critical" (blocks the workload) or "warning" (degraded but running).
    pub severity: String,
    /// Short machine-ish kind, e.g. "ImagePullBackOff", "CrashLoopBackOff",
    /// "PodPending", "NotReady", "SandboxDegraded", "HighRestarts".
    pub kind: String,
    /// The affected object, `namespace/name`.
    pub subject: String,
    /// The raw reason/phase from the cluster.
    pub reason: String,
    /// Human detail (container message / status message) when available.
    pub detail: Option<String>,
    /// A concrete next step for the operator.
    pub remedy: String,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsDto {
    pub issues: Vec<DiagnosticIssue>,
    pub scanned_pods: usize,
    pub scanned_sandboxes: usize,
    /// True when the scan found nothing wrong — the honest "all clear".
    pub healthy: bool,
}

/// `GET /api/operator/diagnostics` — the live problem scan behind Troubleshooting.
pub async fn get_diagnostics(State(state): State<AppState>) -> AppResult<Json<DiagnosticsDto>> {
    let cluster = require_cluster(&state)?;
    let mut issues: Vec<DiagnosticIssue> = Vec::new();

    // ── Pods: the ground truth for "won't start / not healthy". ──────────────
    let pods = cluster.all_pods().await;
    let scanned_pods = pods.len();
    for p in &pods {
        let ns = p.metadata.namespace.as_deref().unwrap_or("").to_string();
        let name = p.metadata.name.as_deref().unwrap_or("").to_string();
        let subject = format!("{ns}/{name}");
        let status = p.status.as_ref();
        let phase = status.and_then(|s| s.phase.as_deref()).unwrap_or("");
        let age_secs = status
            .and_then(|s| s.start_time.as_ref())
            .map(|t| (chrono::Utc::now() - t.0).num_seconds().max(0))
            .unwrap_or(0);

        // Container-level waiting reasons (image pull, crashloop, config error).
        let mut container_flagged = false;
        if let Some(cs) = status.and_then(|s| s.container_statuses.as_ref()) {
            for c in cs {
                if let Some(w) = c.state.as_ref().and_then(|st| st.waiting.as_ref()) {
                    let reason = w.reason.clone().unwrap_or_default();
                    let bad = matches!(
                        reason.as_str(),
                        "ImagePullBackOff"
                            | "ErrImagePull"
                            | "CrashLoopBackOff"
                            | "CreateContainerConfigError"
                            | "CreateContainerError"
                            | "InvalidImageName"
                            | "RunContainerError"
                    );
                    if bad {
                        container_flagged = true;
                        let remedy = match reason.as_str() {
                            "ImagePullBackOff" | "ErrImagePull" | "InvalidImageName" => {
                                "Image can't be pulled — check the image tag exists in the registry and the node has pull access."
                            }
                            "CrashLoopBackOff" | "RunContainerError" => {
                                "Container keeps exiting — check its logs (kubectl logs) for the crash cause."
                            }
                            _ => {
                                "Container config is invalid — check the ConfigMap/Secret mounts and env for this container."
                            }
                        };
                        issues.push(DiagnosticIssue {
                            severity: "critical".into(),
                            kind: reason.clone(),
                            subject: format!("{subject} · {}", c.name),
                            reason,
                            detail: w.message.clone(),
                            remedy: remedy.into(),
                        });
                    }
                }
                // A container restarting many times is a warning even if currently up.
                if c.restart_count >= 5 {
                    issues.push(DiagnosticIssue {
                        severity: "warning".into(),
                        kind: "HighRestarts".into(),
                        subject: format!("{subject} · {}", c.name),
                        reason: format!("{} restarts", c.restart_count),
                        detail: None,
                        remedy:
                            "Container is unstable — inspect its logs for the recurring failure."
                                .into(),
                    });
                }
            }
        }

        // Pod stuck Pending (unschedulable / image / volume) for > 60s.
        if phase == "Pending" && age_secs > 60 && !container_flagged {
            let msg = status
                .and_then(|s| s.conditions.as_ref())
                .and_then(|c| c.iter().find(|cond| cond.status == "False"))
                .and_then(|c| c.message.clone());
            issues.push(DiagnosticIssue {
                severity: "critical".into(),
                kind: "PodPending".into(),
                subject: subject.clone(),
                reason: "Pending".into(),
                detail: msg,
                remedy: "Pod can't be scheduled — check node capacity, taints, or unbound volumes (kubectl describe pod)."
                    .into(),
            });
        }

        // Running but not all containers Ready for > 120s (probes failing).
        if phase == "Running"
            && age_secs > 120
            && !container_flagged
            && let Some(cs) = status.and_then(|s| s.container_statuses.as_ref())
        {
            let total = cs.len();
            let ready = cs.iter().filter(|s| s.ready).count();
            if total > 0 && ready < total {
                issues.push(DiagnosticIssue {
                        severity: "warning".into(),
                        kind: "NotReady".into(),
                        subject: subject.clone(),
                        reason: format!("{ready}/{total} containers ready"),
                        detail: None,
                        remedy: "A container is up but failing its readiness probe — check the probe and the container's logs."
                            .into(),
                    });
            }
        }
    }

    // ── Sandboxes the controller itself flagged Degraded/Failed. ─────────────
    let sandboxes = cluster
        .list_kind_all("KarsSandbox")
        .await
        .unwrap_or_default();
    let scanned_sandboxes = sandboxes.len();
    for sb in &sandboxes {
        let phase = sb
            .data
            .get("status")
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .unwrap_or("");
        if matches!(phase, "Degraded" | "Failed") {
            let name = sb.metadata.name.as_deref().unwrap_or("").to_string();
            let msg = sb
                .data
                .get("status")
                .and_then(|s| s.get("message"))
                .and_then(|m| m.as_str())
                .map(String::from);
            issues.push(DiagnosticIssue {
                severity: if phase == "Failed" { "critical" } else { "warning" }.into(),
                kind: "SandboxDegraded".into(),
                subject: format!("kars-system/{name}"),
                reason: phase.to_string(),
                detail: msg,
                remedy: "The controller couldn't fully reconcile this sandbox — check the controller logs and the sandbox's referenced policies/secrets."
                    .into(),
            });
        }
        // Run-level stall detection (audit f24): the pod-level scan is blind to a
        // run whose sandbox is "Running" but whose run has FAILED/timed out. A
        // mission-output recorded with status=error is a definitive run failure
        // the operator must see even though the pod looks healthy.
        if phase == "Running" {
            let name = sb.metadata.name.as_deref().unwrap_or("").to_string();
            if let Some(out) = cluster.read_mission_output(&name).await
                && out.get("status").map(|s| s.as_str()) == Some("error")
            {
                let detail = out
                    .get("output")
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .or_else(|| out.get("error").cloned());
                issues.push(DiagnosticIssue {
                        severity: "warning".into(),
                        kind: "RunFailed".into(),
                        subject: format!("kars-system/{name}"),
                        reason: "run reported an error while the sandbox is still Running".into(),
                        detail,
                        remedy: "The agent's run did not complete (often a slow/absent agent or a chat-gateway harness that never executed the loop). Check the mission's Run tab, or re-run with the OpenClaw harness for autonomous missions."
                            .into(),
                    });
            }
        }
    }

    // Critical first, then warnings; stable within a severity.
    issues.sort_by(|a, b| {
        let rank = |s: &str| if s == "critical" { 0 } else { 1 };
        rank(&a.severity).cmp(&rank(&b.severity))
    });

    let healthy = issues.is_empty();
    Ok(Json(DiagnosticsDto {
        issues,
        scanned_pods,
        scanned_sandboxes,
        healthy,
    }))
}

// ─── Orchestrator health + the compose failover path ─────────────────────────
// The Bridge composer ("intent → package") runs its own inference. It prefers a
// DIRECT endpoint (BRIDGE_ORCHESTRATOR_* — scales for many teams) and otherwise
// routes through the standing `bridge-orchestrator` sandbox's router. This
// surfaces which path is live, the orchestrator sandbox's health, and — when the
// sandbox path is under strain — recommends configuring the direct endpoint
// (the "switch to inference-based orchestration under load" lever).

#[derive(Debug, Serialize)]
pub struct OrchestratorDto {
    /// Active compose inference path: "direct" (endpoint configured) or
    /// "sandbox" (routing through the orchestrator sandbox router), or "none".
    pub mode: String,
    /// Whether a direct BRIDGE_ORCHESTRATOR endpoint triple is configured.
    pub direct_configured: bool,
    /// Whether the standing orchestrator sandbox exists.
    pub sandbox_present: bool,
    /// The orchestrator sandbox phase (Running/Degraded/…), when present.
    pub sandbox_phase: Option<String>,
    /// Ready/total containers of the orchestrator pod, restarts, waiting reason.
    pub sandbox_ready: Option<String>,
    pub sandbox_restarts: Option<i32>,
    pub sandbox_waiting_reason: Option<String>,
    /// How many Running sandbox routers the composer can fall back through.
    pub router_candidates: usize,
    /// True when the operator should configure the direct endpoint (sandbox path
    /// is the only option and it's unhealthy or capacity is thin).
    pub recommend_direct: bool,
    /// Plain-language recommendation.
    pub note: String,
}

/// `GET /api/operator/orchestrator` — orchestrator health + compose failover path.
pub async fn get_orchestrator(State(state): State<AppState>) -> AppResult<Json<OrchestratorDto>> {
    let cluster = require_cluster(&state)?;

    let direct_configured = [
        "BRIDGE_ORCHESTRATOR_ENDPOINT",
        "BRIDGE_ORCHESTRATOR_TOKEN",
        "BRIDGE_ORCHESTRATOR_MODEL",
    ]
    .iter()
    .all(|k| {
        std::env::var(k)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    });

    // Orchestrator sandbox presence + health.
    let sandboxes = cluster
        .list_kind_all("KarsSandbox")
        .await
        .unwrap_or_default();
    let orch = sandboxes.iter().find(|sb| {
        sb.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/orchestrator"))
            .map(String::as_str)
            == Some("true")
    });
    let sandbox_present = orch.is_some();
    let sandbox_phase = orch.and_then(|o| {
        o.data
            .get("status")
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .map(String::from)
    });
    let health = if let Some(o) = orch {
        let name = o.metadata.name.clone().unwrap_or_default();
        cluster.sandbox_pod_health(&name).await
    } else {
        None
    };
    let (sandbox_ready, sandbox_restarts, sandbox_waiting_reason) = match &health {
        Some(h) => (
            Some(format!("{}/{}", h.ready_containers, h.total_containers)),
            Some(h.restarts),
            h.waiting_reason.clone(),
        ),
        None => (None, None, None),
    };

    let router_candidates = cluster.running_sandbox_candidates().await.len();

    let sandbox_healthy = sandbox_phase.as_deref() == Some("Running")
        && health
            .as_ref()
            .map(|h| h.ready_containers == h.total_containers && h.total_containers > 0)
            .unwrap_or(false);

    let mode = if direct_configured {
        "direct"
    } else if sandbox_present && router_candidates > 0 {
        "sandbox"
    } else {
        "none"
    }
    .to_string();

    // Recommend the direct endpoint when we're on the sandbox path and it's the
    // only option while being unhealthy or thin on router capacity.
    let recommend_direct = !direct_configured && !sandbox_healthy;

    let note = if direct_configured {
        "Composing via the direct inference endpoint — scales independently of any sandbox."
            .to_string()
    } else if !sandbox_present {
        "No orchestrator sandbox and no direct endpoint — the composer can't run. Set BRIDGE_ORCHESTRATOR_{ENDPOINT,TOKEN,MODEL} or let the Bridge provision the orchestrator sandbox.".to_string()
    } else if recommend_direct {
        "The orchestrator sandbox is present but not healthy enough to compose reliably. Repair it or configure BRIDGE_ORCHESTRATOR_{ENDPOINT,TOKEN,MODEL} for a direct inference path.".to_string()
    } else if router_candidates <= 1 {
        "Composing through the healthy orchestrator sandbox router. One router is sufficient for serial composition; configure BRIDGE_ORCHESTRATOR_{ENDPOINT,TOKEN,MODEL} only when you need independent capacity for many concurrent compose requests.".to_string()
    } else {
        "Composing via the orchestrator sandbox router — healthy. For many concurrent teams, a direct BRIDGE_ORCHESTRATOR endpoint scales better.".to_string()
    };

    Ok(Json(OrchestratorDto {
        mode,
        direct_configured,
        sandbox_present,
        sandbox_phase,
        sandbox_ready,
        sandbox_restarts,
        sandbox_waiting_reason,
        router_candidates,
        recommend_direct,
        note,
    }))
}

// ─── Integrations: kars-SRE agent + Headlamp plugin ──────────────────────────
// kars ships a real Headlamp plugin (tools/headlamp-plugin — /kars/sre and
// /kars/* views) and a real SRE agent (deploy/helm/kars/templates/sre.yaml,
// gated on sre.enabled; `kars sre install`). This surfaces whether each is
// active, deep-links into the existing plugin views, and gives the exact
// activation for what isn't enabled — rather than pretending to integrate.

#[derive(Debug, Serialize)]
pub struct IntegrationsDto {
    /// kars-SRE agent.
    pub sre_present: bool,
    pub sre_phase: Option<String>,
    pub sre_ready: Option<String>,
    /// The `kars sre install` activation command when SRE isn't enabled.
    pub sre_activate_cmd: String,
    /// Headlamp dashboard + kars plugin.
    pub headlamp_deployed: bool,
    pub headlamp_url: Option<String>,
    /// Deep-link paths into the kars Headlamp plugin (appended to headlamp_url).
    pub headlamp_paths: Vec<HeadlampLink>,
    /// How to install the plugin when Headlamp is present but the URL is unset.
    pub headlamp_install_hint: String,
}

#[derive(Debug, Serialize)]
pub struct HeadlampLink {
    pub label: String,
    pub path: String,
}

/// `GET /api/operator/integrations` — kars-SRE + Headlamp status & deep-links.
pub async fn get_integrations(State(state): State<AppState>) -> AppResult<Json<IntegrationsDto>> {
    let cluster = require_cluster(&state)?;

    // SRE agent: the `sre` KarsSandbox (deploy/helm/kars/templates/sre.yaml).
    let sandboxes = cluster
        .list_kind_all("KarsSandbox")
        .await
        .unwrap_or_default();
    let sre = sandboxes
        .iter()
        .find(|sb| sb.metadata.name.as_deref() == Some("sre"));
    let sre_present = sre.is_some();
    let sre_phase = sre.and_then(|o| {
        o.data
            .get("status")
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .map(String::from)
    });
    let sre_ready = if sre_present {
        cluster
            .sandbox_pod_health("sre")
            .await
            .map(|h| format!("{}/{}", h.ready_containers, h.total_containers))
    } else {
        None
    };

    // Headlamp: the `headlamp` Deployment in the `headlamp` namespace.
    let headlamp_deployed = cluster.deployment_exists("headlamp", "headlamp").await;

    Ok(Json(IntegrationsDto {
        sre_present,
        sre_phase,
        sre_ready,
        sre_activate_cmd: "kars sre install   # helm upgrade --reuse-values --set sre.enabled=true".into(),
        headlamp_deployed,
        headlamp_url: std::env::var("BRIDGE_HEADLAMP_URL").ok().filter(|u| !u.trim().is_empty()),
        headlamp_paths: vec![
            HeadlampLink { label: "SRE console".into(), path: "/kars/sre".into() },
            HeadlampLink { label: "Sandboxes".into(), path: "/kars/karssandboxes".into() },
            HeadlampLink { label: "Agent mesh".into(), path: "/kars/mesh".into() },
        ],
        headlamp_install_hint: "Build tools/headlamp-plugin (npm run build), kubectl cp dist into the headlamp pod at /headlamp/plugins/kars, then set BRIDGE_HEADLAMP_URL.".into(),
    }))
}

// ─── Local (in-cluster) inference — AI Runway ModelDeployment ────────────────
// See docs/local-inference.md (kars core). kars does NOT install or manage
// AI Runway/KAITO — an operator installs both once via their own real
// helm/kubectl commands, exactly like the GitHub App or Azure AI Foundry
// connection. This surface only detects presence and manages `ModelDeployment`
// objects on top, in the Bridge's own `kars-local-inference` namespace.

#[derive(Debug, Serialize)]
pub struct LocalInferenceStatusDto {
    /// Whether AI Runway's `modeldeployments.airunway.ai` CRD is present —
    /// i.e. whether an operator has installed it (see docs/local-inference.md).
    pub available: bool,
    /// Real, live-scanned count of nodes advertising `nvidia.com/gpu`
    /// capacity — never a hardcoded guess. Zero means only CPU-tier models
    /// can be offered.
    pub gpu_node_count: u32,
    /// Distinct GPU product names found via the NFD/GPU-feature-discovery
    /// `nvidia.com/gpu.product` node label, when present.
    pub gpu_products: Vec<String>,
}

/// `GET /api/operator/local-inference/status` — detect whether the cluster
/// can host an in-cluster model, and whether it has GPU capacity for the
/// larger tier. Never installs anything.
pub async fn local_inference_status(
    State(state): State<AppState>,
) -> AppResult<Json<LocalInferenceStatusDto>> {
    let cluster = require_cluster(&state)?;
    let available = cluster.local_inference_available().await;
    let gpu = cluster.gpu_node_summary().await.unwrap_or_default();
    Ok(Json(LocalInferenceStatusDto {
        available,
        gpu_node_count: gpu.gpu_node_count,
        gpu_products: gpu.gpu_products,
    }))
}

/// One curated, vetted model the wizard can offer without the operator
/// hand-typing a HuggingFace id or an AIKit image reference. Real values
/// verified live against AI Runway v0.7.0 + KAITO workspace chart 0.11.0 —
/// see docs/local-inference.md.
#[derive(Debug, Serialize, Clone)]
pub struct CuratedLocalModelDto {
    pub id: String,
    pub label: String,
    pub tier: String, // "cpu" | "gpu"
    pub params: String,
}

/// `GET /api/operator/local-inference/catalog` — the curated list + tier
/// availability (a GPU entry is still LISTED when no GPU node exists, so the
/// wizard can show it disabled with a clear reason, rather than silently
/// hiding an option and confusing an operator who just hasn't added GPU
/// nodes yet).
pub async fn local_inference_catalog() -> Json<Vec<CuratedLocalModelDto>> {
    Json(vec![
        CuratedLocalModelDto {
            id: "llama-3.2-1b-instruct".into(),
            label: "Llama 3.2 (1B, CPU)".into(),
            tier: "cpu".into(),
            params: "1B".into(),
        },
        CuratedLocalModelDto {
            id: "llama-3.2-3b-instruct".into(),
            label: "Llama 3.2 (3B, CPU)".into(),
            tier: "cpu".into(),
            params: "3B".into(),
        },
        CuratedLocalModelDto {
            id: "gemma-2-2b-instruct".into(),
            label: "Gemma 2 (2B, CPU)".into(),
            tier: "cpu".into(),
            params: "2B".into(),
        },
        CuratedLocalModelDto {
            id: "microsoft/Phi-4-mini-instruct".into(),
            label: "Phi-4-mini (GPU)".into(),
            tier: "gpu".into(),
            params: "3.8B".into(),
        },
        CuratedLocalModelDto {
            id: "meta-llama/Llama-3.1-8B-Instruct".into(),
            label: "Llama 3.1 (8B, GPU)".into(),
            tier: "gpu".into(),
            params: "8B".into(),
        },
        CuratedLocalModelDto {
            id: "mistralai/Mistral-7B-Instruct-v0.3".into(),
            label: "Mistral (7B, GPU)".into(),
            tier: "gpu".into(),
            params: "7B".into(),
        },
    ])
}

/// The AIKit CPU image for each curated CPU-tier model id — the `llamacpp`
/// engine needs an explicit pre-built image (there is no live HF→GGUF
/// resolution path), so this is the one place that mapping has to be
/// hardcoded. Free-text/advanced deployments must supply their own image.
fn aikit_image_for(model_id: &str) -> Option<&'static str> {
    match model_id {
        "llama-3.2-1b-instruct" => Some("ghcr.io/kaito-project/aikit/llama3.2:1b"),
        "llama-3.2-3b-instruct" => Some("ghcr.io/kaito-project/aikit/llama3.2:3b"),
        "gemma-2-2b-instruct" => Some("ghcr.io/kaito-project/aikit/gemma2:2b"),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
pub struct LocalModelDeploymentDto {
    pub name: String,
    pub namespace: String,
    pub managed: bool,
    pub model_id: Option<String>,
    pub engine: Option<String>,
    pub provider: Option<String>,
    pub phase: Option<String>,
    pub message: Option<String>,
    pub endpoint: Option<String>,
    pub created_at: Option<String>,
}

fn project_model_deployment(o: &DynamicObject) -> LocalModelDeploymentDto {
    let name = name_of(o);
    let namespace = ns_of(o);
    let managed = namespace == crate::kars::cluster::LOCAL_INFERENCE_NAMESPACE
        && label(o, "app.kubernetes.io/managed-by").as_deref() == Some("kars-bridge");
    let spec = o.data.get("spec");
    let status = o.data.get("status");
    let model_id = spec
        .and_then(|s| s.get("model"))
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .map(String::from);
    let engine = status
        .and_then(|s| s.get("engine"))
        .and_then(|e| e.get("type"))
        .and_then(Value::as_str)
        .map(String::from);
    let provider = status
        .and_then(|s| s.get("provider"))
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .map(String::from);
    let phase = status
        .and_then(|s| s.get("phase"))
        .and_then(Value::as_str)
        .map(String::from);
    let message = status
        .and_then(|s| s.get("message"))
        .and_then(Value::as_str)
        .map(String::from);
    // AI Runway publishes the routable Service in status when available. Fall
    // back to the ModelDeployment name and port 80 for older controller builds.
    let endpoint = if phase.as_deref() == Some("Running") {
        let service = status
            .and_then(|s| s.get("endpoint"))
            .and_then(|e| e.get("service"))
            .and_then(Value::as_str)
            .unwrap_or(&name);
        let port = status
            .and_then(|s| s.get("endpoint"))
            .and_then(|e| e.get("port"))
            .and_then(Value::as_u64)
            .unwrap_or(80);
        Some(format!(
            "http://{service}.{namespace}.svc.cluster.local:{port}"
        ))
    } else {
        None
    };
    LocalModelDeploymentDto {
        name,
        namespace,
        managed,
        model_id,
        engine,
        provider,
        phase,
        message,
        endpoint,
        created_at: created_of(o),
    }
}

/// `GET /api/operator/local-inference/deployments` — every ModelDeployment
/// the Bridge manages, with live status.
/// `GET /api/operator/local-inference/deployments` — every ModelDeployment
/// the Bridge manages, with live status. As a side effect, auto-registers
/// any newly-`Running` deployment as a normal additional inference provider
/// (tag `local-<name>`) — reusing the exact multi-provider mechanism proven
/// this session, so no router changes are needed: every sandbox's router
/// already knows how to dial an arbitrary custom OpenAI-compatible endpoint
/// once it's in `kars-inference-providers`. Idempotent (a re-list of an
/// already-wired deployment is a no-op re-write of the same values).
pub async fn list_local_model_deployments(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<LocalModelDeploymentDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster.list_model_deployments().await.map_err(upstream)?;
    let dtos: Vec<LocalModelDeploymentDto> = items.iter().map(project_model_deployment).collect();
    for d in &dtos {
        if d.managed
            && d.phase.as_deref() == Some("Running")
            && let (Some(endpoint), Some(model_id)) = (&d.endpoint, &d.model_id)
        {
            auto_wire_local_provider(cluster, &d.name, endpoint, model_id).await;
        }
    }
    Ok(Json(dtos))
}

/// Register a Running local ModelDeployment's Service as an additional
/// inference provider tagged `local-<name>`, no API key (in-cluster,
/// unauthenticated). Best-effort: a write failure here degrades to "the
/// model runs but isn't yet selectable from an InferencePolicy" rather than
/// failing the status poll the wizard depends on.
async fn auto_wire_local_provider(
    cluster: &crate::kars::cluster::Cluster,
    name: &str,
    endpoint: &str,
    model_id: &str,
) {
    let tag_upper = format!("LOCAL_{}", name.to_ascii_uppercase().replace('-', "_"));
    let endpoint = endpoint.to_string();
    let model_id = model_id.to_string();
    if let Err(e) = cluster
        .mutate_secret_keys(
            INFERENCE_PROVIDERS_NS,
            INFERENCE_PROVIDERS_SECRET,
            move |keys| {
                keys.insert(
                    format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"),
                    endpoint.clone(),
                );
                keys.insert(
                    format!("KARS_PROVIDER_{tag_upper}_MODELS"),
                    model_id.clone(),
                );
            },
        )
        .await
    {
        tracing::warn!(deployment = name, error = %e, "failed to auto-wire local model as an inference provider");
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateLocalModelDeploymentRequest {
    /// DNS-label name for this deployment (becomes the Service name kars
    /// wires into the inference-providers secret).
    pub name: String,
    /// A curated id (see `local_inference_catalog`) or, for the advanced
    /// free-text path, any HuggingFace model id.
    pub model_id: String,
    /// "cpu" or "gpu" — selects the engine/provider shape. Advanced/free-text
    /// requests must pick "cpu" (with an explicit `image`) or "gpu".
    pub tier: String,
    /// Required for tier=cpu when `model_id` isn't one of the curated ids
    /// (the llamacpp engine needs a pre-built AIKit/GGUF image — there's no
    /// live HF→GGUF resolution path).
    #[serde(default)]
    pub image: Option<String>,
    /// GPU count for tier=gpu. Default 1.
    #[serde(default)]
    pub gpu_count: Option<i64>,
}

/// `POST /api/operator/local-inference/deployments` — create (or update, via
/// SSA) a `ModelDeployment`. Rejects tier=cpu requests with no resolvable
/// image rather than creating a ModelDeployment doomed to fail validation
/// with an opaque upstream error.
pub async fn create_local_model_deployment(
    State(state): State<AppState>,
    Json(req): Json<CreateLocalModelDeploymentRequest>,
) -> AppResult<Json<LocalModelDeploymentDto>> {
    let cluster = require_cluster(&state)?;
    if !is_dns1123_label(&req.name) {
        return Err(AppError::BadRequest(
            "name must be lowercase letters, digits, hyphens".into(),
        ));
    }
    let spec = match req.tier.as_str() {
        "cpu" => {
            let image = req.image.as_deref().filter(|i| !i.trim().is_empty())
                .or_else(|| aikit_image_for(&req.model_id))
                .ok_or_else(|| AppError::BadRequest(
                    "a CPU deployment needs a pre-built AIKit image — pick a curated model or supply spec.image for an advanced/free-text one".into(),
                ))?;
            serde_json::json!({
                "model": {"id": req.model_id},
                "engine": {"type": "llamacpp"},
                "image": image,
            })
        }
        "gpu" => {
            serde_json::json!({
                "model": {"id": req.model_id},
                "resources": {"gpu": {"count": req.gpu_count.unwrap_or(1), "type": "nvidia.com/gpu"}},
            })
        }
        other => {
            return Err(AppError::BadRequest(format!(
                "tier must be \"cpu\" or \"gpu\", got {other:?}"
            )));
        }
    };
    let obj = cluster
        .apply_model_deployment(&req.name, spec)
        .await
        .map_err(upstream)?;
    Ok(Json(project_model_deployment(&obj)))
}

/// `GET /api/operator/local-inference/deployments/:name/status` — rich LIVE
/// status for the deploy progress tracker: a milestone-derived percentage,
/// real pod/container state, and the actual Kubernetes event stream (image
/// pull, scheduling, container start/fail) for this deployment's pods.
pub async fn local_deployment_live_status(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<crate::kars::cluster::LocalDeployLiveStatus>> {
    let cluster = require_cluster(&state)?;
    let status = cluster
        .local_deployment_live_status(&name)
        .await
        .map_err(upstream)?;
    Ok(Json(status))
}

/// `DELETE /api/operator/local-inference/deployments/:name` — undeploy a
/// local model. The Bridge also removes it from the connected-providers list
/// if it had been auto-wired (see `auto_wire_local_provider` in routes/run.rs
/// or the corresponding poll path) — callers should not assume the
/// InferencePolicy-facing tag disappears atomically with the CR.
pub async fn delete_local_model_deployment(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    cluster
        .delete_model_deployment(&name)
        .await
        .map_err(upstream)?;
    // Best-effort: also drop it from the additional-providers secret if it
    // was auto-wired. Not fatal if it wasn't (e.g. deleted before Ready).
    let tag = format!("local-{name}");
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let _ = cluster
        .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
            keys.remove(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"));
            keys.remove(&format!("KARS_PROVIDER_{tag_upper}_MODELS"));
        })
        .await;
    Ok(Json(serde_json::json!({"deleted": true, "name": name})))
}

#[cfg(test)]
mod tests {
    use super::{
        SandboxDto, apply_err, inherit_sandbox_context, is_dns1123_label, is_env_key,
        parse_copilot_models, project_model_deployment, receipt_verdict,
    };
    use crate::error::AppError;
    use kube::core::DynamicObject;
    use serde_json::json;

    #[test]
    fn projects_discovered_airunway_model_in_its_actual_namespace() {
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "airunway.ai/v1alpha1",
            "kind": "ModelDeployment",
            "metadata": {
                "name": "gpt-oss-120b",
                "namespace": "default"
            },
            "spec": {
                "model": {"id": "openai/gpt-oss-120b"}
            },
            "status": {
                "phase": "Running",
                "endpoint": {"service": "gpt-oss-120b", "port": 80},
                "engine": {"type": "vllm"},
                "provider": {"name": "kaito"}
            }
        }))
        .expect("valid dynamic object");

        let projected = project_model_deployment(&object);

        assert_eq!(projected.namespace, "default");
        assert!(!projected.managed);
        assert_eq!(
            projected.endpoint.as_deref(),
            Some("http://gpt-oss-120b.default.svc.cluster.local:80")
        );
        assert_eq!(projected.model_id.as_deref(), Some("openai/gpt-oss-120b"));
    }

    fn sandbox(
        name: &str,
        parent: Option<&str>,
        team: Option<&str>,
        executing: Option<bool>,
    ) -> SandboxDto {
        SandboxDto {
            name: name.to_string(),
            namespace: "kars-system".to_string(),
            runtime_namespace: None,
            phase: Some("Running".to_string()),
            runtime: None,
            isolation: None,
            tool_policy: None,
            inference_policy: None,
            governed: true,
            team: team.map(str::to_string),
            parent: parent.map(str::to_string),
            message: None,
            created: None,
            working: Some(false),
            executing,
            cpu_millicores: None,
            memory_bytes: None,
            conditions: Vec::new(),
        }
    }

    #[test]
    fn nested_subagents_inherit_root_team_and_execution() {
        let mut sandboxes = vec![
            sandbox("lead", None, Some("maintenance"), Some(true)),
            sandbox("specialist", Some("lead"), None, None),
            sandbox("worker", Some("specialist"), None, None),
        ];
        sandboxes[1].working = Some(true);
        sandboxes[2].working = Some(true);

        inherit_sandbox_context(&mut sandboxes);

        assert_eq!(sandboxes[0].team.as_deref(), Some("maintenance"));
        assert_eq!(sandboxes[0].executing, Some(true));
        assert_eq!(sandboxes[0].working, Some(false));
        for sandbox in &sandboxes[1..] {
            assert_eq!(sandbox.team.as_deref(), Some("maintenance"));
            assert_eq!(sandbox.executing, Some(true));
            assert_eq!(sandbox.working, Some(true));
        }
    }

    #[test]
    fn sandbox_context_does_not_cross_namespaces() {
        let mut first_lead = sandbox("lead", None, Some("team-a"), Some(true));
        first_lead.namespace = "namespace-a".into();
        let mut first_child = sandbox("worker", Some("lead"), None, None);
        first_child.namespace = "namespace-a".into();
        let mut second_lead = sandbox("lead", None, Some("team-b"), Some(false));
        second_lead.namespace = "namespace-b".into();
        let mut second_child = sandbox("worker", Some("lead"), None, None);
        second_child.namespace = "namespace-b".into();
        let mut sandboxes = vec![first_lead, first_child, second_lead, second_child];

        inherit_sandbox_context(&mut sandboxes);

        assert_eq!(sandboxes[1].team.as_deref(), Some("team-a"));
        assert_eq!(sandboxes[1].executing, Some(false));
        assert_eq!(sandboxes[3].team.as_deref(), Some("team-b"));
        assert_eq!(sandboxes[3].executing, Some(false));
    }

    #[test]
    fn parse_copilot_models_filters_and_categorises() {
        // A trimmed but faithful sample of the real /models response shape
        // (captured live 2026-07): a flagship chat model, a versatile one, an
        // embeddings model (must be dropped), a legacy non-picker chat model
        // (must be dropped), and a gated preview the seat hasn't enabled
        // (must be dropped).
        let body = json!({"data": [
            {
                "id": "claude-opus-4.8", "name": "Claude Opus 4.8", "vendor": "Anthropic",
                "model_picker_enabled": true, "model_picker_category": "powerful",
                "policy": {"state": "enabled"},
                "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 1_000_000}}
            },
            {
                "id": "gpt-5.6-terra", "name": "GPT-5.6 Terra", "vendor": "OpenAI",
                "model_picker_enabled": true, "model_picker_category": "versatile",
                "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 1_050_000}}
            },
            {
                "id": "text-embedding-3-small", "name": "Embedding V3 small", "vendor": "Azure OpenAI",
                "model_picker_enabled": false, "capabilities": {"type": "embeddings"}
            },
            {
                "id": "gpt-4o", "name": "GPT-4o", "vendor": "Azure OpenAI",
                "model_picker_enabled": false,
                "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 128_000}}
            },
            {
                "id": "some-preview", "name": "Gated Preview", "vendor": "OpenAI",
                "model_picker_enabled": true, "model_picker_category": "powerful",
                "policy": {"state": "unconfigured"},
                "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 200_000}}
            }
        ]});
        let out = parse_copilot_models(&body);
        let ids: Vec<&str> = out.iter().map(|m| m.id.as_str()).collect();
        // Only the two enabled, picker-enabled chat models survive — embeddings,
        // the legacy non-picker gpt-4o, and the un-enabled preview are dropped.
        assert_eq!(ids, vec!["claude-opus-4.8", "gpt-5.6-terra"]);
        // powerful sorts before versatile.
        assert!(out[0].recommended, "powerful model must be recommended");
        assert!(
            !out[1].recommended,
            "versatile model must not be recommended"
        );
        // Label carries the human name + context.
        assert!(out[0].label.as_deref().unwrap().contains("Claude Opus 4.8"));
        assert!(out[0].label.as_deref().unwrap().contains("1.0M ctx"));
    }

    #[test]
    fn parse_copilot_models_empty_on_missing_data() {
        assert!(parse_copilot_models(&json!({})).is_empty());
        assert!(parse_copilot_models(&json!({"data": []})).is_empty());
    }

    fn claim(class: &str, status: &str) -> (String, String) {
        (class.to_string(), status.to_string())
    }

    #[test]
    fn receipt_verdict_regulatory_and_omitted_are_advisory() {
        // The real V0 shape: crypto claims PASS, regulatory OMITTED. Must be
        // "verified" (regression: it used to read "partial" for every receipt).
        let v0 = vec![
            claim("integrity", "PASS"),
            claim("conformance", "PASS"),
            claim("completeness", "PASS"),
            claim("regulatory", "OMITTED"),
        ];
        assert_eq!(receipt_verdict(&v0), "verified");

        // Regulatory PARTIAL is likewise advisory.
        let v0b = vec![
            claim("integrity", "PASS"),
            claim("conformance", "PASS"),
            claim("completeness", "PASS"),
            claim("regulatory", "PARTIAL"),
        ];
        assert_eq!(receipt_verdict(&v0b), "verified");

        // A genuinely partial CORE claim (completeness) is still "partial".
        let partial = vec![
            claim("integrity", "PASS"),
            claim("conformance", "PASS"),
            claim("completeness", "PARTIAL"),
            claim("regulatory", "OMITTED"),
        ];
        assert_eq!(receipt_verdict(&partial), "partial");

        // Any FAIL/ERROR anywhere is "failed".
        let failed = vec![claim("integrity", "FAIL"), claim("conformance", "PASS")];
        assert_eq!(receipt_verdict(&failed), "failed");

        // No claims ⇒ "none".
        assert_eq!(receipt_verdict(&[]), "none");
    }

    fn api_err(code: u16, message: &str) -> kube::Error {
        kube::Error::Api(kube::core::ErrorResponse {
            status: "Failure".into(),
            message: message.into(),
            reason: "".into(),
            code,
        })
    }

    #[test]
    fn apply_err_surfaces_ssa_schema_failure() {
        // A Server-Side Apply schema rejection arrives as a 500 with an
        // actionable message — it must become a Rejected (422) carrying that
        // message, NOT an opaque Upstream (502).
        let e = api_err(
            500,
            "failed to create typed patch object (kars-system/qa; kars.azure.com/v1alpha1, Kind=ToolPolicy): .spec.allow: field not declared in schema",
        );
        match apply_err(e) {
            AppError::Rejected(m) => assert!(m.contains("field not declared in schema")),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn apply_err_keeps_opaque_500_opaque() {
        // A generic 500 with no actionable schema message stays Upstream.
        match apply_err(api_err(500, "etcdserver: request timed out")) {
            AppError::Upstream(_) => {}
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[test]
    fn apply_err_maps_validation_and_rbac() {
        assert!(matches!(
            apply_err(api_err(422, "bad")),
            AppError::Rejected(_)
        ));
        assert!(matches!(
            apply_err(api_err(403, "no")),
            AppError::Rejected(_)
        ));
        assert!(matches!(
            apply_err(api_err(409, "conflict")),
            AppError::Rejected(_)
        ));
    }

    #[test]
    fn dns1123_label_rules() {
        assert!(is_dns1123_label("repo-watch"));
        assert!(is_dns1123_label("a"));
        assert!(is_dns1123_label("team1"));
        assert!(!is_dns1123_label("")); // empty
        assert!(!is_dns1123_label("-lead")); // leading hyphen
        assert!(!is_dns1123_label("lead-")); // trailing hyphen
        assert!(!is_dns1123_label("Repo")); // uppercase
        assert!(!is_dns1123_label("a_b")); // underscore
        assert!(!is_dns1123_label(&"x".repeat(64))); // too long
    }

    #[test]
    fn env_key_rules() {
        assert!(is_env_key("GITHUB_TOKEN"));
        assert!(is_env_key("_x"));
        assert!(is_env_key("BRAVE_API_KEY"));
        assert!(!is_env_key("")); // empty
        assert!(!is_env_key("1TOKEN")); // leading digit
        assert!(!is_env_key("MY-KEY")); // hyphen
        assert!(!is_env_key("MY KEY")); // space
    }

    #[test]
    fn witness_doc_parses_real_aggregator_payload() {
        // The exact shape the aggregator publishes into kars-datapath-witness.
        let body = r#"{
          "generated_at": "2026-07-02T13:51:32Z",
          "window_seconds": 15,
          "gadget": "inspektor-gadget",
          "sandboxes": [
            {"namespace":"kars-demo","sandbox":"demo",
             "declared_hosts":["api.github.com"],
             "observed_dns":["api.github.com","example.com"],
             "observed_connects":4,
             "beyond_declared":["example.com"],
             "unused_declared":[],
             "verdict":"BEYOND-DECLARED"}
          ]
        }"#;
        let doc: super::WitnessDoc = serde_json::from_str(body).expect("parse");
        assert_eq!(doc.generated_at.as_deref(), Some("2026-07-02T13:51:32Z"));
        assert_eq!(doc.window_seconds, Some(15));
        assert_eq!(doc.sandboxes.len(), 1);
        let s = &doc.sandboxes[0];
        assert_eq!(s.sandbox, "demo");
        assert_eq!(s.verdict, "BEYOND-DECLARED");
        assert_eq!(s.beyond_declared, vec!["example.com"]);
        assert_eq!(s.observed_connects, 4);
    }

    #[test]
    fn witness_sandbox_tolerates_missing_optional_arrays() {
        // Defaults must hold so a partial payload never fails deserialization.
        let s: super::DatapathWitnessSandbox =
            serde_json::from_str(r#"{"namespace":"n","sandbox":"x","verdict":"LEARN"}"#)
                .expect("parse");
        assert_eq!(s.verdict, "LEARN");
        assert!(s.declared_hosts.is_empty());
        assert!(s.observed_dns.is_empty());
        assert_eq!(s.observed_connects, 0);
    }
}
