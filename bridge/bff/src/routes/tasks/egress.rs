use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::Deserialize;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::require_owned_task;
use crate::state::AppState;

use super::{map_kube_err, require_cluster};

/// Body for a temporary egress request from a mission: the agent (or operator
/// on its behalf) asks to reach an extra website. Materialized as an
/// `EgressApproval` the controller reconciles through human approval — the BFF
/// never widens the sandbox's allowlist directly.
#[derive(Debug, Deserialize)]
pub struct EgressRequest {
    pub host: String,
    pub port: Option<u16>,
    pub reason: String,
    /// Time-to-live, e.g. "2h". Bounded by the cluster ceiling. Default "2h".
    pub ttl: Option<String>,
}

/// Normalize a human-friendly TTL (`"2h"`, `"30m"`, `"24h"`, `"1d"`, `"90s"`) to
/// the ISO-8601 duration the controller's `EgressApproval` reconciler requires
/// (`"PT2H"`, `"PT30M"`, `"P1D"`, `"PT90S"`). An already-ISO value (starts with
/// `P`) passes through uppercased. Unrecognized input falls back to `"PT2H"`
/// rather than emitting an invalid TTL that leaves the grant Pending forever.
pub(super) fn normalize_ttl(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return "PT2H".into();
    }
    if t.starts_with('P') || t.starts_with('p') {
        return t.to_ascii_uppercase();
    }
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: u64 = num.trim().parse().unwrap_or(0);
    if n == 0 {
        return "PT2H".into();
    }
    match unit.trim().to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" => format!("PT{n}S"),
        "m" | "min" | "mins" => format!("PT{n}M"),
        "h" | "hr" | "hrs" | "hour" | "hours" => format!("PT{n}H"),
        "d" | "day" | "days" => format!("P{n}D"),
        _ => "PT2H".into(),
    }
}

/// `POST /api/namespaces/:ns/tasks/:name/egress` — file a temporary egress
/// grant request for this mission's sandbox. Returns the created EgressApproval
/// name; it widens nothing until a human approves it.
pub async fn request_egress(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<EgressRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let host = req.host.trim().to_string();
    if host.is_empty() {
        return Err(AppError::BadRequest("host is required".into()));
    }
    if req.reason.trim().len() < 3 {
        return Err(AppError::BadRequest("reason is required".into()));
    }
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    task.status
        .as_ref()
        .and_then(|s| s.sandbox_ref.as_ref())
        .ok_or_else(|| AppError::BadRequest("mission has no running sandbox to widen".into()))?;
    let port = req.port.unwrap_or(443);
    let ttl = normalize_ttl(req.ttl.as_deref().unwrap_or("2h"));
    use sha2::{Digest, Sha256};
    let suffix = hex::encode(Sha256::digest(format!("{host}:{port}").as_bytes()));
    let approval_name = format!("{name}-eg-{}", &suffix[..12]);
    let task_uid = task
        .metadata
        .uid
        .clone()
        .ok_or_else(|| AppError::Upstream("task has no Kubernetes UID".into()))?;
    let body = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsApproval",
        "metadata": {
            "name": approval_name,
            "namespace": ns,
            "ownerReferences": [{
                "apiVersion": "kars.azure.com/v1alpha1",
                "kind": "KarsTask",
                "name": name,
                "uid": task_uid,
                "controller": true,
                "blockOwnerDeletion": true
            }],
            "labels": {
                "kars.azure.com/req-task": name,
                "kars.azure.com/req-kind": "egress"
            },
            "annotations": {
                "kars.azure.com/req-kind": "egress",
                "kars.azure.com/req-target": host,
                "kars.azure.com/req-port": port.to_string(),
                "kars.azure.com/req-ttl": ttl,
                "kars.azure.com/requested-by": principal.name,
                "kars.azure.com/requested-by-sub": principal.sub,
                "kars.azure.com/owner-sub": principal.sub,
                "kars.azure.com/owner-name": principal.name
            }
        },
        "spec": {
            "taskRef": {"name": name},
            "requestedBy": {
                "subject": principal.sub,
                "name": principal.name
            },
            "action": {
                "kind": "egress",
                "summary": format!("Allow the mission to reach {host}:{port}"),
                "detail": format!(
                    "{} Approving creates an exact, time-boxed {host}:{port} grant.",
                    req.reason.trim()
                )
            },
            "ttl": "PT24H"
        },
    });
    let created = cluster
        .apply_kind(&ns, "KarsApproval", body, false)
        .await
        .map_err(map_kube_err)?;
    Ok(Json(serde_json::json!({
        "requested": true,
        "name": created.metadata.name,
        "note": "Pending human approval. No egress is granted until a distinct operator approves it in the Bridge inbox."
    })))
}

/// `GET /api/namespaces/:ns/tasks/:name/egress/learned` — the domains the agent
/// has actually reached, observed by the router in Learn mode. This is the
/// evidence a customer reviews before promoting the mission to enforced.
pub async fn get_learned_egress(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let sandbox = task
        .status
        .as_ref()
        .and_then(|s| s.sandbox_ref.as_ref())
        .map(|r| r.name.clone());
    let Some(sandbox) = sandbox else {
        return Ok(Json(
            serde_json::json!({ "available": false, "reason": "no running sandbox yet", "domains": [] }),
        ));
    };
    let mode = cluster
        .sandbox_egress_mode(&sandbox)
        .await
        .unwrap_or_else(|| "Learn".into());
    let enforced = cluster.sandbox_allowlist(&sandbox).await;
    match cluster.sandbox_learned_domains(&sandbox).await {
        Ok(domains) => Ok(Json(
            serde_json::json!({ "available": true, "mode": mode, "domains": domains, "enforced": enforced }),
        )),
        Err(e) => Ok(Json(
            serde_json::json!({ "available": false, "mode": mode, "reason": e.to_string(), "domains": [], "enforced": enforced }),
        )),
    }
}

/// Body for flipping a mission's egress enforcement mode.
#[derive(Debug, Deserialize)]
pub struct EgressModeRequest {
    /// `"learning"` (clear the allowlist → controller runs Learn) or
    /// `"enforced"` (pin the allowlist → controller runs Strict).
    pub mode: String,
    /// The hosts to enforce when `mode == "enforced"`. Typically the reviewed
    /// subset of the learned domains.
    #[serde(default)]
    pub allow: Vec<String>,
    /// When true, UNION `allow` with the mission's current enforced allowlist
    /// instead of replacing it — so granting one host (e.g. from a blocker) can
    /// never silently wipe previously-approved hosts. The NetworkMode panel,
    /// which sets the full list deliberately, leaves this false (replace).
    #[serde(default)]
    pub merge: bool,
}

/// `POST /api/namespaces/:ns/tasks/:name/egress-mode` — promote a mission from
/// learning (monitoring) to enforced, or back. This drives the REAL lever: the
/// controller derives `egressMode: Strict` + an allowlist when the blueprint
/// names egress hosts, and `Learn` when it is empty. Operator-gated; the
/// controller re-reconciles the sandbox, so this is durable, not a UI toggle.
pub async fn set_egress_mode(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<EgressModeRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let enforced = match req.mode.as_str() {
        "enforced" | "strict" => true,
        "learning" | "learn" => false,
        _ => {
            return Err(AppError::BadRequest(
                "mode must be 'enforced' or 'learning'".into(),
            ));
        }
    };
    // Parse "host" or "host:port" into the blueprint egress shape.
    let egress: Vec<serde_json::Value> = if enforced {
        let mut parsed: Vec<serde_json::Value> = req
            .allow
            .iter()
            .filter_map(|h| {
                let h = h.trim();
                if h.is_empty() {
                    return None;
                }
                match h
                    .rsplit_once(':')
                    .and_then(|(host, p)| p.parse::<u16>().ok().map(|p| (host, p)))
                {
                    Some((host, port)) => Some(serde_json::json!({ "host": host, "port": port })),
                    None => Some(serde_json::json!({ "host": h })),
                }
            })
            .collect();
        // Additive grant: union with the mission's CURRENT enforced allowlist so
        // approving one host never clobbers the others (a k8s merge-patch of an
        // array replaces it wholesale, so we must merge here, before patching).
        if req.merge {
            let existing: Vec<serde_json::Value> = task
                .spec
                .blueprint
                .map(|b| b.egress)
                .unwrap_or_default()
                .into_iter()
                .map(|e| match e.port {
                    Some(p) => serde_json::json!({ "host": e.host, "port": p }),
                    None => serde_json::json!({ "host": e.host }),
                })
                .collect();
            let key = |v: &serde_json::Value| {
                format!(
                    "{}:{}",
                    v.get("host").and_then(|h| h.as_str()).unwrap_or(""),
                    v.get("port").and_then(|p| p.as_u64()).unwrap_or(0)
                )
            };
            let mut seen: std::collections::HashSet<String> = parsed.iter().map(key).collect();
            for e in existing {
                if seen.insert(key(&e)) {
                    parsed.push(e);
                }
            }
        }
        if parsed.is_empty() {
            return Err(AppError::BadRequest(
                "enforcing requires at least one allowed host — review the learned domains first"
                    .into(),
            ));
        }
        parsed
    } else {
        Vec::new()
    };
    // Patch the mission's blueprint egress; the controller compiles it into the
    // sandbox's networkPolicy (Strict + allowlist, or Learn when empty).
    let patch = serde_json::json!({ "spec": { "blueprint": { "egress": egress } } });
    cluster
        .tasks(&ns)
        .patch(
            &name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(patch),
        )
        .await
        .map_err(map_kube_err)?;
    Ok(Json(serde_json::json!({
        "updated": true,
        "mode": if enforced { "enforced" } else { "learning" },
        "note": if enforced {
            "Promoted to enforced — the sandbox will deny anything outside the approved allowlist on its next reconcile."
        } else {
            "Back to learning — the sandbox observes and records every domain it reaches without denying."
        }
    })))
}
