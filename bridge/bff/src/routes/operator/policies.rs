// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use kube::core::DynamicObject;
use serde::Serialize;
use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::{created_of, name_of, ns_of, require_cluster, s, spec, status, upstream};

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
pub(super) fn apply_err(e: kube::Error) -> AppError {
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
pub(super) async fn apply_governance(
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
pub(super) async fn delete_governance(
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

#[cfg(test)]
mod tests {
    use super::apply_err;
    use crate::error::AppError;

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
}
