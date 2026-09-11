// kars Bridge BFF — hierarchical, editable inference token budgets.
//
// The user asked for a real budget HIERARCHY over inference token spend:
//
//   Cluster  ─▶  Workspace (org/user tenant = namespace)  ─▶  Sandbox
//
// The per-SANDBOX level already exists: the controller compiles an
// InferencePolicy from each task's envelope budget and the router enforces it
// (429) per sandbox. What was missing is the aggregate CLUSTER and WORKSPACE
// levels — a cluster-wide meter and cap, and per-workspace caps — plus the three
// enforcement MODES the user specified:
//
//   • passive  — never blocks; raises an alert when over budget.
//   • buffer   — allows up to `limit × (1 + bufferPercent/100)`, then blocks
//                (a cluster/org admin must raise the budget to proceed).
//   • strict   — blocks at 100% of the limit; only an admin can raise it.
//
// The cluster + workspace levels can't be enforced by a per-sandbox router (each
// router only sees its own sandbox), so the Bridge — which owns orchestration —
// enforces them at the point a mission/team is launched, against the REAL
// measured daily token utilization aggregated from completed runs. The config is
// stored in the `kars-inference-budgets` ConfigMap (cluster-native, editable).

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::error::{AppError, AppResult};
use crate::kars::cluster::Cluster;
use crate::state::AppState;

fn require_cluster(state: &AppState) -> AppResult<&Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

/// A single budget rule at one level of the hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetRule {
    /// Daily token cap. `0` (or absent) ⇒ no cap (measured, never blocked).
    #[serde(default)]
    pub daily_tokens: i64,
    /// Enforcement mode: `passive` | `buffer` | `strict`.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Buffer headroom (percent) allowed above `daily_tokens` in `buffer` mode.
    #[serde(default)]
    pub buffer_percent: i64,
}

fn default_mode() -> String {
    "passive".to_string()
}

impl BudgetRule {
    fn normalized(mut self) -> Self {
        self.mode = match self.mode.as_str() {
            "buffer" | "strict" | "passive" => self.mode,
            _ => "passive".to_string(),
        };
        if self.daily_tokens < 0 {
            self.daily_tokens = 0;
        }
        self.buffer_percent = self.buffer_percent.clamp(0, 1000);
        self
    }

    /// The effective hard cap (where enforcement blocks). For `buffer` mode this
    /// is `daily_tokens × (1 + bufferPercent/100)`; otherwise `daily_tokens`.
    fn hard_cap(&self) -> i64 {
        if self.daily_tokens == 0 {
            return 0; // uncapped
        }
        match self.mode.as_str() {
            "buffer" => self.daily_tokens + self.daily_tokens * self.buffer_percent / 100,
            _ => self.daily_tokens,
        }
    }
}

/// The persisted hierarchy (stored as `budgets.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetHierarchy {
    #[serde(default)]
    pub cluster: Option<BudgetRule>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, BudgetRule>,
    /// Per-USER caps, keyed by the `kars.azure.com/created-by` identity the Bridge
    /// stamps on each mission/team. The spec's "per-user" tier — genuinely
    /// functional (attributed from real run ownership), below the workspace tier.
    #[serde(default)]
    pub users: BTreeMap<String, BudgetRule>,
}

impl BudgetHierarchy {
    async fn load(cluster: &Cluster) -> Self {
        serde_json::from_str(&cluster.read_inference_budgets().await).unwrap_or_default()
    }
    async fn save(&self, cluster: &Cluster) -> AppResult<()> {
        let json = serde_json::to_string(self).map_err(|e| AppError::Internal(e.into()))?;
        cluster
            .write_inference_budgets(&json)
            .await
            .map_err(AppError::Internal)
    }
}

// ─── Usage aggregation ───────────────────────────────────────────────────────

/// Today's (UTC) measured token utilization, attributed to the full tenancy
/// hierarchy: the cluster total, a per-WORKSPACE (namespace) breakdown, and a
/// per-USER (created-by) breakdown — joined from the real run ownership, not a
/// hardcoded namespace. Same `totalTokens` the efficiency engine reads, scoped to
/// the UTC day so it composes with the per-sandbox daily budget model.
async fn usage_today(cluster: &Cluster) -> (i64, BTreeMap<String, i64>, BTreeMap<String, i64>) {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let meta = cluster.list_task_meta().await;
    let mut total: i64 = 0;
    let mut by_ns: BTreeMap<String, i64> = BTreeMap::new();
    let mut by_user: BTreeMap<String, i64> = BTreeMap::new();
    for record in cluster.list_mission_output_evidence().await {
        let task = record.task_name;
        let data = record.data;
        let finished = data.get("finishedAt").cloned().unwrap_or_default();
        if !finished.starts_with(&today) {
            continue;
        }
        let tokens = data
            .get("totalTokens")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        if tokens <= 0 {
            continue;
        }
        total += tokens;
        // Attribute to the OWNING task's namespace + creator (real ownership).
        let (ns, user) = meta
            .get(&task)
            .cloned()
            .unwrap_or_else(|| ("kars-system".to_string(), "unattributed".to_string()));
        *by_ns.entry(ns).or_insert(0) += tokens;
        *by_user.entry(user).or_insert(0) += tokens;
    }
    (total, by_ns, by_user)
}

// ─── DTOs ────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct BudgetLevelDto {
    pub scope: String,
    pub label: String,
    pub daily_tokens: i64,
    pub mode: String,
    pub buffer_percent: i64,
    pub used_today: i64,
    /// `ok` | `alert` (passive over budget) | `over_buffer` (buffer past
    /// headroom) | `blocking` (strict/at-hard-cap; launches refused).
    pub status: String,
    /// Fraction of the base daily cap used (0..>1), for the meter bar.
    pub percent: f64,
    pub hard_cap: i64,
}

#[derive(Serialize)]
pub struct BudgetAlert {
    pub scope: String,
    pub label: String,
    /// `alert` (passive over budget) | `over_buffer` | `blocking`.
    pub severity: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct BudgetsDto {
    pub cluster: Option<BudgetLevelDto>,
    pub cluster_used_today: i64,
    pub workspaces: Vec<BudgetLevelDto>,
    /// Per-user caps (the spec's per-user tier), keyed by created-by identity.
    pub users: Vec<BudgetLevelDto>,
    pub default_namespace: String,
    /// Namespaces that have measured usage today but no explicit budget yet
    /// (so the operator can add one with a click).
    pub unbudgeted_namespaces: Vec<String>,
    /// Users that have measured usage today but no explicit per-user budget yet.
    pub unbudgeted_users: Vec<String>,
    /// Active budget ALERTS — every level currently over its cap (passive),
    /// in buffer headroom, or blocking. This is the "raise alerts" surface:
    /// operators/admins see + act on breaches without hunting through meters.
    pub alerts: Vec<BudgetAlert>,
}

/// Build an alert for a level whose status indicates a breach (or `None` when ok).
fn alert_for(level: &BudgetLevelDto) -> Option<BudgetAlert> {
    let (severity, message) = match level.status.as_str() {
        "alert" => (
            "alert",
            format!(
                "{} is OVER its passive budget — {} of {} daily tokens ({}%). Alerting only; no work is blocked.",
                level.label,
                level.used_today,
                level.daily_tokens,
                (level.percent * 100.0).round() as i64
            ),
        ),
        "over_buffer_headroom" => (
            "over_buffer",
            format!(
                "{} is in its +{}% buffer headroom — {} of {} daily tokens. New work still runs, but an admin should review before it hits the hard cap ({}).",
                level.label,
                level.buffer_percent,
                level.used_today,
                level.daily_tokens,
                level.hard_cap
            ),
        ),
        "blocking" => (
            "blocking",
            format!(
                "{} is BLOCKING new work — {} of {} daily tokens (cap {}). Only a cluster/org admin can raise it.",
                level.label, level.used_today, level.daily_tokens, level.hard_cap
            ),
        ),
        _ => return None,
    };
    Some(BudgetAlert {
        scope: level.scope.clone(),
        label: level.label.clone(),
        severity: severity.to_string(),
        message,
    })
}

fn level_dto(scope: &str, label: &str, rule: &BudgetRule, used: i64) -> BudgetLevelDto {
    let hard = rule.hard_cap();
    let status = if rule.daily_tokens == 0 {
        "ok"
    } else if used >= hard && rule.mode != "passive" {
        "blocking"
    } else if used >= rule.daily_tokens {
        match rule.mode.as_str() {
            "passive" => "alert",
            "buffer" => "over_buffer_headroom",
            _ => "blocking",
        }
    } else {
        "ok"
    };
    let percent = if rule.daily_tokens > 0 {
        used as f64 / rule.daily_tokens as f64
    } else {
        0.0
    };
    BudgetLevelDto {
        scope: scope.to_string(),
        label: label.to_string(),
        daily_tokens: rule.daily_tokens,
        mode: rule.mode.clone(),
        buffer_percent: rule.buffer_percent,
        used_today: used,
        status: status.to_string(),
        percent,
        hard_cap: hard,
    }
}

// ─── Read ────────────────────────────────────────────────────────────────────

/// `GET /api/operator/inference-budgets` — the hierarchy + live measured usage.
pub async fn get_budgets(State(state): State<AppState>) -> AppResult<Json<BudgetsDto>> {
    let cluster = require_cluster(&state)?;
    let h = BudgetHierarchy::load(cluster).await;
    let (cluster_used, by_ns, by_user) = usage_today(cluster).await;
    let default_ns = "kars-system".to_string();

    let cluster_level = h
        .cluster
        .as_ref()
        .map(|r| level_dto("cluster", "Whole cluster", r, cluster_used));

    let mut workspaces: Vec<BudgetLevelDto> = h
        .workspaces
        .iter()
        .map(|(ns, r)| level_dto(ns, ns, r, *by_ns.get(ns).unwrap_or(&0)))
        .collect();
    workspaces.sort_by(|a, b| a.scope.cmp(&b.scope));

    let mut users: Vec<BudgetLevelDto> = h
        .users
        .iter()
        .map(|(u, r)| level_dto(u, u, r, *by_user.get(u).unwrap_or(&0)))
        .collect();
    users.sort_by(|a, b| a.scope.cmp(&b.scope));

    let unbudgeted: Vec<String> = by_ns
        .keys()
        .filter(|ns| !h.workspaces.contains_key(*ns))
        .cloned()
        .collect();
    let unbudgeted_users: Vec<String> = by_user
        .keys()
        .filter(|u| !h.users.contains_key(*u) && *u != "unattributed")
        .cloned()
        .collect();

    // Active alerts across every configured level (the "raise alerts" surface).
    let mut alerts: Vec<BudgetAlert> = Vec::new();
    if let Some(c) = &cluster_level {
        alerts.extend(alert_for(c));
    }
    for w in &workspaces {
        alerts.extend(alert_for(w));
    }
    for u in &users {
        alerts.extend(alert_for(u));
    }

    Ok(Json(BudgetsDto {
        cluster: cluster_level,
        cluster_used_today: cluster_used,
        workspaces,
        users,
        default_namespace: default_ns,
        unbudgeted_namespaces: unbudgeted,
        unbudgeted_users,
        alerts,
    }))
}

// ─── Write ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SetRuleRequest {
    #[serde(default)]
    pub daily_tokens: i64,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub buffer_percent: i64,
    /// When true, remove the rule entirely (uncap this level).
    #[serde(default)]
    pub clear: bool,
}

/// `PUT /api/operator/inference-budgets/cluster` — set/clear the cluster cap.
pub async fn set_cluster_budget(
    State(state): State<AppState>,
    Json(req): Json<SetRuleRequest>,
) -> AppResult<Json<BudgetsDto>> {
    let cluster = require_cluster(&state)?;
    let mut h = BudgetHierarchy::load(cluster).await;
    if req.clear {
        h.cluster = None;
    } else {
        h.cluster = Some(
            BudgetRule {
                daily_tokens: req.daily_tokens,
                mode: req.mode,
                buffer_percent: req.buffer_percent,
            }
            .normalized(),
        );
    }
    h.save(cluster).await?;
    get_budgets(State(state)).await
}

/// `PUT /api/operator/inference-budgets/workspaces/{ns}` — set/clear a workspace.
pub async fn set_workspace_budget(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Json(req): Json<SetRuleRequest>,
) -> AppResult<Json<BudgetsDto>> {
    if ns.trim().is_empty() {
        return Err(AppError::BadRequest(
            "workspace namespace is required".into(),
        ));
    }
    let cluster = require_cluster(&state)?;
    let mut h = BudgetHierarchy::load(cluster).await;
    if req.clear {
        h.workspaces.remove(&ns);
    } else {
        h.workspaces.insert(
            ns.clone(),
            BudgetRule {
                daily_tokens: req.daily_tokens,
                mode: req.mode,
                buffer_percent: req.buffer_percent,
            }
            .normalized(),
        );
    }
    h.save(cluster).await?;
    get_budgets(State(state)).await
}

/// `PUT /api/operator/inference-budgets/users/{user}` — set/clear a per-user cap.
pub async fn set_user_budget(
    State(state): State<AppState>,
    Path(user): Path<String>,
    Json(req): Json<SetRuleRequest>,
) -> AppResult<Json<BudgetsDto>> {
    if user.trim().is_empty() {
        return Err(AppError::BadRequest("user identity is required".into()));
    }
    let cluster = require_cluster(&state)?;
    let mut h = BudgetHierarchy::load(cluster).await;
    if req.clear {
        h.users.remove(&user);
    } else {
        h.users.insert(
            user.clone(),
            BudgetRule {
                daily_tokens: req.daily_tokens,
                mode: req.mode,
                buffer_percent: req.buffer_percent,
            }
            .normalized(),
        );
    }
    h.save(cluster).await?;
    get_budgets(State(state)).await
}

// ─── Enforcement (called at mission/team launch) ─────────────────────────────

/// Enforce the cluster + workspace + user budgets before a run is launched in
/// `ns` by `user`. Returns `Ok` to proceed (passive over-budget is allowed but
/// surfaces as an alert), or `AppError::Rejected` when a `buffer`/`strict` level
/// would be breached. The per-sandbox cap is separately enforced by the router;
/// this is the aggregate hierarchy gate the Bridge owns.
pub async fn enforce_launch_budget(cluster: &Cluster, ns: &str, user: &str) -> AppResult<()> {
    let h = BudgetHierarchy::load(cluster).await;
    if h.cluster.is_none() && h.workspaces.is_empty() && h.users.is_empty() {
        return Ok(()); // no hierarchy configured — nothing to enforce.
    }
    let (cluster_used, by_ns, by_user) = usage_today(cluster).await;

    if let Some(rule) = &h.cluster {
        check_level(rule, cluster_used, "cluster-wide")?;
    }
    if let Some(rule) = h.workspaces.get(ns) {
        let used = *by_ns.get(ns).unwrap_or(&0);
        check_level(rule, used, &format!("workspace “{ns}”"))?;
    }
    if let Some(rule) = h.users.get(user) {
        let used = *by_user.get(user).unwrap_or(&0);
        check_level(rule, used, &format!("user “{user}”"))?;
    }
    Ok(())
}

fn check_level(rule: &BudgetRule, used: i64, scope: &str) -> AppResult<()> {
    if rule.daily_tokens == 0 {
        return Ok(());
    }
    match rule.mode.as_str() {
        "passive" => Ok(()), // alert-only; never blocks a launch.
        "buffer" => {
            let hard = rule.hard_cap();
            if used >= hard {
                Err(AppError::Rejected(format!(
                    "The {scope} inference budget is exhausted — {used} of {} daily tokens used, past the +{}% buffer ({hard}). A cluster or org admin must raise the budget before new work can start today.",
                    rule.daily_tokens, rule.buffer_percent
                )))
            } else {
                Ok(()) // within buffer headroom — allowed.
            }
        }
        _ => {
            // strict
            if used >= rule.daily_tokens {
                Err(AppError::Rejected(format!(
                    "The {scope} inference budget is reached — {used} of {} daily tokens used (strict enforcement). Only a cluster or org admin can raise it; new work is blocked for today.",
                    rule.daily_tokens
                )))
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(daily: i64, mode: &str, buf: i64) -> BudgetRule {
        BudgetRule {
            daily_tokens: daily,
            mode: mode.into(),
            buffer_percent: buf,
        }
    }

    #[test]
    fn passive_never_blocks_but_alerts() {
        let r = rule(100, "passive", 0);
        assert!(check_level(&r, 150, "x").is_ok());
        let dto = level_dto("x", "x", &r, 150);
        assert_eq!(dto.status, "alert");
    }

    #[test]
    fn strict_blocks_at_limit() {
        let r = rule(100, "strict", 0);
        assert!(check_level(&r, 99, "x").is_ok());
        assert!(check_level(&r, 100, "x").is_err());
        assert_eq!(level_dto("x", "x", &r, 100).status, "blocking");
    }

    #[test]
    fn buffer_allows_headroom_then_blocks() {
        let r = rule(100, "buffer", 20); // hard cap 120
        assert!(check_level(&r, 100, "x").is_ok()); // within headroom
        assert!(check_level(&r, 119, "x").is_ok());
        assert!(check_level(&r, 120, "x").is_err()); // past +20%
        assert_eq!(r.hard_cap(), 120);
        assert_eq!(level_dto("x", "x", &r, 110).status, "over_buffer_headroom");
        assert_eq!(level_dto("x", "x", &r, 120).status, "blocking");
    }

    #[test]
    fn zero_is_uncapped() {
        let r = rule(0, "strict", 0);
        assert!(check_level(&r, 999_999, "x").is_ok());
        assert_eq!(level_dto("x", "x", &r, 999).status, "ok");
    }
}
