// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use kube::core::DynamicObject;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

use crate::error::AppResult;
use crate::state::AppState;

use super::{
    created_of, has_task_owner, label, name_of, ns_of, require_cluster, s, spec, status, upstream,
};

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

#[cfg(test)]
mod tests {
    use super::{SandboxDto, inherit_sandbox_context};

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
}
