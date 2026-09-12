// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::Api;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::team::KarsTeam;
use crate::routes::options::build_options;
use crate::routes::tasks::require_cluster;

use super::validation::{
    apply_team_git_write, build_roster, normalize_autonomous_runtime, normalize_lifecycle_mode,
    normalize_mcp_servers, normalize_model_fallback_routes, reject_reserved_role_names,
    validate_mcp_servers, validate_team_model_routes, validate_warm_idle_seconds,
};
use super::{
    CreateRole, CreateTeamRequest, TeamModelRoutes, UpdateTeamRequest, require_owned_team,
};

/// `POST /api/namespaces/:ns/teams` — create a standing team. The controller
/// validates the envelope; cadence drives the autonomous tick. Defaults are
/// conservative (tier 3, ceiling=tier, depth 1) so a team can't self-amplify.
pub async fn create_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(mut b): Json<CreateTeamRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    cluster
        .credential_grant(&ns)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if b.name.trim().is_empty() || b.charter.trim().len() < 8 {
        return Err(AppError::BadRequest(
            "name and a real charter are required".into(),
        ));
    }
    reject_reserved_role_names(&b.name, &b.roles)?;
    let execution_plan = b
        .execution_plan
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("a typed execution_plan is required".into()))?;
    crate::routes::compose::validate_execution_plan(execution_plan)
        .map_err(AppError::BadRequest)?;
    let roster_names = b
        .roles
        .iter()
        .map(|role| role.name.trim())
        .collect::<std::collections::BTreeSet<_>>();
    let plan_names = execution_plan
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if roster_names != plan_names {
        return Err(AppError::BadRequest(
            "execution_plan role names must exactly match the team roster".into(),
        ));
    }
    b.mcp_servers = normalize_mcp_servers(&b.mcp_servers)?;
    validate_mcp_servers(cluster, &ns, &b.mcp_servers).await?;
    normalize_autonomous_runtime(&mut b.runtime);
    for role in &mut b.roles {
        normalize_autonomous_runtime(&mut role.runtime);
    }
    let options = build_options(cluster).await?;
    if b.model
        .as_deref()
        .is_none_or(|model| model.trim().is_empty())
    {
        b.model = options
            .models
            .iter()
            .find(|model| model.is_default)
            .or_else(|| options.models.first())
            .map(|model| format!("{}::{}", model.provider, model.deployment));
    }
    b.model_fallbacks = normalize_model_fallback_routes(&b.model_fallbacks, b.model.as_deref())?;
    validate_team_model_routes(
        &options,
        TeamModelRoutes {
            namespace: &ns,
            runtime: b.runtime.as_deref(),
            model: b.model.as_deref(),
            model_fallbacks: &b.model_fallbacks,
            roles: &b.roles,
            execution_plan,
            mcp_servers: &b.mcp_servers,
            memory: b.memory.as_deref(),
        },
    )?;
    b.created_by = Some(principal.name.clone());
    let created_by = principal.name.clone();
    let git_write = crate::routes::github::authorize_git_write(
        cluster,
        &ns,
        &principal,
        b.git_write_repos.as_deref(),
    )
    .await?;
    // Aggregate inference-budget gate (cluster + workspace + user): a launched
    // team immediately kicks off a run (token spend), so block starting new work
    // when a budget at any tier is strict/over-buffer. A paused team passes.
    if b.launch.unwrap_or(false) {
        crate::routes::budgets::enforce_launch_budget(cluster, &ns, &created_by).await?;
    }
    let tier = b.tier.unwrap_or(3).clamp(1, 5);
    let ceiling = b.authority_ceiling.unwrap_or(tier).clamp(1, tier);
    // Governance: create PAUSED unless the operator explicitly opts into
    // launching. A paused team does not auto-kickoff (the controller mints the
    // initial run only when `!paused`), so "Launch" is a genuine human approval
    // — clicking Run now / Resume — not an automatic side-effect of Create.
    let paused = !b.launch.unwrap_or(false);
    let mut spec = serde_json::json!({
        "charter": b.charter, "paused": paused, "envelope": { "tier": tier, "authorityCeiling": ceiling, "delegationDepth": b.delegation_depth.unwrap_or(1) },
    });
    if let Some(mode) = normalize_lifecycle_mode(b.lifecycle_mode.as_deref())? {
        spec["lifecycleMode"] = serde_json::json!(mode);
    }
    if let Some(seconds) = validate_warm_idle_seconds(b.warm_idle_seconds)? {
        spec["warmIdleSeconds"] = serde_json::json!(seconds);
    }
    if let Some(r) = &b.reporting_to {
        spec["reportingTo"] = serde_json::json!(r);
    }
    if let Some(d) = b
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        spec["displayName"] = serde_json::json!(d);
    }
    if let Some(c) = &b.knowledge_commons {
        spec["knowledgeCommons"] = serde_json::json!(c);
    }
    // cadence_minutes == 0 (or absent) means a cadence-LESS "run on demand" team:
    // the CRD requires everyMinutes >= 1 when the cadence field is present, so we
    // OMIT it entirely rather than write an invalid everyMinutes: 0 (which the
    // apiserver rejects 422). A cadence-less team is minted once on creation
    // (kickoff) and thereafter only runs via "Run now".
    if let Some(m) = b.cadence_minutes
        && m >= 1
    {
        spec["cadence"] = serde_json::json!({ "everyMinutes": m });
    }
    // Every team run must be governed by a real ToolPolicy. Without one the run
    // sandbox is created with governance disabled, the agent's AGT engine starts
    // with an empty policy set and fails closed, and the run hangs until the
    // dispatch times out. Resolve the requested policy (or the cluster default
    // `kars-default`) and pin it on the team's run blueprint.
    let tool_policy = resolve_team_tool_policy(cluster, &ns, b.tool_policy.as_deref()).await;
    if let Some(tp) = &tool_policy {
        spec["blueprint"] = serde_json::json!({ "toolPolicy": tp });
    }
    if !spec["blueprint"].is_object() {
        spec["blueprint"] = serde_json::json!({});
    }
    spec["blueprint"]["executionPlan"] = serde_json::to_value(execution_plan.clone().into_crd())
        .map_err(|error| {
            AppError::BadRequest(format!("execution_plan could not be serialized: {error}"))
        })?;
    // Team-level harness: the runtime every minted run executes on. Correct a
    // bootstrap-only adapter (no autonomous task loop) to OpenClaw — a standing
    // run must be able to run autonomously. Hermes/BYO are autonomous and pass
    // through. The controller inherits this via the team's run blueprint.
    if let Some(rt) = b
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let rt = if crate::routes::compose::is_non_autonomous_harness(rt) {
            "OpenClaw"
        } else {
            rt
        };
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["runtime"] = serde_json::json!(rt);
    }
    if let Some(model) = b.model.as_deref().map(str::trim).filter(|s| !s.is_empty())
        && let Some((provider, deployment)) = model.split_once("::")
    {
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["model"] =
            serde_json::json!({"provider": provider, "deployment": deployment});
    }
    let model_fallbacks = b
        .model_fallbacks
        .iter()
        .map(|route| route.trim())
        .filter(|route| !route.is_empty())
        .filter_map(|route| route.split_once("::"))
        .map(|(provider, deployment)| {
            serde_json::json!({"provider": provider, "deployment": deployment})
        })
        .collect::<Vec<_>>();
    spec["blueprint"]["modelFallbacks"] = serde_json::json!(model_fallbacks);
    if let Some(memory) = b.memory.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["memory"] = serde_json::json!(memory);
    }
    if !b.egress.is_empty() {
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["egress"] = serde_json::json!(
            b.egress
                .iter()
                .filter_map(|entry| {
                    let host = entry.host.trim();
                    (!host.is_empty())
                        .then(|| serde_json::json!({"host": host, "port": entry.port}))
                })
                .collect::<Vec<_>>()
        );
    }
    if let Some(mode) = b.egress_mode.as_deref().map(str::trim) {
        let mode = match mode.to_ascii_lowercase().as_str() {
            "strict" => "Strict",
            "learning" | "learn" => "Learn",
            _ => {
                return Err(AppError::BadRequest(
                    "egress_mode must be 'learning' or 'strict'".into(),
                ));
            }
        };
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["egressMode"] = serde_json::json!(mode);
    }
    apply_team_git_write(&mut spec, git_write.as_ref().map(|(config, _)| config))?;
    if let Some((_, binding)) = &git_write {
        spec["blueprint"]["githubBinding"] =
            serde_json::to_value(binding).map_err(|error| AppError::Upstream(error.to_string()))?;
    }
    if !b.mcp_servers.is_empty() {
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["mcpServers"] = serde_json::json!(
            b.mcp_servers
                .iter()
                .map(|server| server.trim())
                .filter(|server| !server.is_empty())
                .collect::<Vec<_>>()
        );
    }
    if !b.roles.is_empty() {
        spec["roster"] = serde_json::json!(build_roster(&b.roles));
    }
    if let Some(ttl) = b.run_retention_ttl_seconds {
        spec["runRetentionTtlSeconds"] = serde_json::json!(ttl);
    }
    let body = serde_json::json!({ "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTeam", "metadata": {"name": b.name.trim(), "namespace": ns}, "spec": spec });
    let mut body = body;
    // Stamp the creator for per-user budget attribution (propagated onto runs).
    if body["metadata"]["annotations"].is_null() {
        body["metadata"]["annotations"] = serde_json::json!({});
    }
    body["metadata"]["annotations"]["kars.azure.com/created-by"] = serde_json::json!(created_by);
    body["metadata"]["annotations"]["kars.azure.com/owner-sub"] = serde_json::json!(principal.sub);
    body["metadata"]["annotations"]["kars.azure.com/owner-name"] =
        serde_json::json!(principal.name);
    let active = !body["spec"]["paused"].as_bool().unwrap_or(false);
    body["spec"]["paused"] = serde_json::json!(true);
    let captured = cluster
        .create_kind(&ns, "KarsTeam", body)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    cluster
        .finish_created_credentials(
            &crate::kars::credentials::Target {
                kind: "KarsTeam".into(),
                namespace: ns.clone(),
                name: captured.name_any(),
                uid: captured
                    .uid()
                    .ok_or_else(|| AppError::Upstream("Team CREATE omitted UID".into()))?,
            },
            active,
        )
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(Json(
        serde_json::json!({"created": true, "name": b.name.trim()}),
    ))
}

/// Resolve the governance policy to pin on a team's run blueprint: the
/// requested policy when it exists, else the cluster default (`kars-default`),
/// else the first installed policy. Returns `None` only when the cluster has no
/// ToolPolicy at all (nothing we can assign).
async fn resolve_team_tool_policy(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    requested: Option<&str>,
) -> Option<String> {
    if let Some(r) = requested.map(str::trim).filter(|r| !r.is_empty())
        && cluster
            .get_kind(ns, "ToolPolicy", r)
            .await
            .ok()
            .flatten()
            .is_some()
    {
        return Some(r.to_string());
    }
    if cluster
        .get_kind(
            ns,
            "ToolPolicy",
            crate::routes::compose::DEFAULT_TOOL_POLICY,
        )
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return Some(crate::routes::compose::DEFAULT_TOOL_POLICY.to_string());
    }
    cluster
        .list_kind_all("ToolPolicy")
        .await
        .ok()?
        .into_iter()
        .find(|policy| policy.namespace().as_deref() == Some(ns))
        .map(|policy| policy.name_any())
}

/// `PATCH /api/namespaces/:ns/teams/:name` — edit charter, cadence, reporting,
/// or pause. Envelope-raising fields are out of scope here (promote handles
/// governed tier changes); this is the non-amplifying day-to-day edit.
pub async fn update_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(mut b): Json<UpdateTeamRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    normalize_autonomous_runtime(&mut b.runtime);
    if let Some(roles) = &mut b.roles {
        for role in roles {
            normalize_autonomous_runtime(&mut role.runtime);
        }
    }
    let execution_plan_changed = b.execution_plan.is_some();
    if b.runtime.is_some()
        || b.model.is_some()
        || b.model_fallbacks.is_some()
        || b.memory.is_some()
        || b.roles.is_some()
        || b.mcp_servers.is_some()
        || b.execution_plan.is_some()
    {
        let options = build_options(cluster).await?;
        if b.model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty())
        {
            b.model = options
                .models
                .iter()
                .find(|model| model.is_default)
                .or_else(|| options.models.first())
                .map(|model| format!("{}::{}", model.provider, model.deployment));
        }
        let existing_runtime = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.runtime.as_deref());
        let existing_model = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.model.as_ref())
            .map(|model| format!("{}::{}", model.provider, model.deployment));
        let existing_model_fallbacks = team
            .spec
            .blueprint
            .as_ref()
            .map(|blueprint| {
                blueprint
                    .model_fallbacks
                    .iter()
                    .map(|model| format!("{}::{}", model.provider, model.deployment))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if b.model.is_some() || b.model_fallbacks.is_some() {
            b.model_fallbacks = Some(normalize_model_fallback_routes(
                b.model_fallbacks
                    .as_deref()
                    .unwrap_or(existing_model_fallbacks.as_slice()),
                b.model.as_deref().or(existing_model.as_deref()),
            )?);
        }
        let existing_roles = team
            .spec
            .roster
            .iter()
            .map(|role| CreateRole {
                name: role.name.clone(),
                system_prompt: role.system_prompt.clone(),
                runtime: role
                    .blueprint
                    .as_ref()
                    .and_then(|blueprint| blueprint.runtime.clone()),
                model: role
                    .blueprint
                    .as_ref()
                    .and_then(|blueprint| blueprint.model.as_ref())
                    .map(|model| format!("{}::{}", model.provider, model.deployment)),
                skills: role.skills.clone(),
            })
            .collect::<Vec<_>>();
        let existing_execution_plan = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.execution_plan.as_ref())
            .map(crate::routes::tasks::ExecutionPlanDto::from_crd);
        let existing_mcp_servers = team
            .spec
            .blueprint
            .as_ref()
            .map(|blueprint| blueprint.mcp_servers.clone())
            .unwrap_or_default();
        let existing_memory = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.memory.as_deref());
        let execution_plan = b
            .execution_plan
            .as_ref()
            .or(existing_execution_plan.as_ref())
            .ok_or_else(|| {
            AppError::BadRequest(
                "this team cannot change runtime/model/roles/MCP until it has a typed execution_plan"
                    .into(),
            )
        })?;
        crate::routes::compose::validate_execution_plan(execution_plan)
            .map_err(AppError::BadRequest)?;
        let effective_roles = b.roles.as_deref().unwrap_or(existing_roles.as_slice());
        let roster_names = effective_roles
            .iter()
            .map(|role| role.name.trim())
            .collect::<std::collections::BTreeSet<_>>();
        let plan_names = execution_plan
            .roles
            .iter()
            .map(|role| role.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if roster_names != plan_names {
            return Err(AppError::BadRequest(
                "execution_plan role names must exactly match the team roster".into(),
            ));
        }
        validate_team_model_routes(
            &options,
            TeamModelRoutes {
                namespace: &ns,
                runtime: b.runtime.as_deref().or(existing_runtime),
                model: b.model.as_deref().or(existing_model.as_deref()),
                model_fallbacks: b
                    .model_fallbacks
                    .as_deref()
                    .unwrap_or(existing_model_fallbacks.as_slice()),
                roles: effective_roles,
                execution_plan,
                mcp_servers: b
                    .mcp_servers
                    .as_deref()
                    .unwrap_or(existing_mcp_servers.as_slice()),
                memory: b.memory.as_deref().or(existing_memory),
            },
        )?;
    }
    if b.paused == Some(false) {
        crate::routes::budgets::enforce_launch_budget(cluster, &ns, &principal.name).await?;
    }
    let mut spec = serde_json::Map::new();
    if let Some(c) = &b.charter {
        spec.insert("charter".into(), serde_json::json!(c));
    }
    if let Some(p) = b.paused {
        spec.insert("paused".into(), serde_json::json!(p));
    }
    if let Some(r) = &b.reporting_to {
        spec.insert("reportingTo".into(), serde_json::json!(r));
    }
    if let Some(mode) = normalize_lifecycle_mode(b.lifecycle_mode.as_deref())? {
        spec.insert("lifecycleMode".into(), serde_json::json!(mode));
    }
    if let Some(seconds) = validate_warm_idle_seconds(b.warm_idle_seconds)? {
        spec.insert("warmIdleSeconds".into(), serde_json::json!(seconds));
    }
    if let Some(m) = b.cadence_minutes {
        if m >= 1 {
            spec.insert("cadence".into(), serde_json::json!({"everyMinutes": m}));
        } else {
            // 0 = passive / run-on-demand: clear the cadence entirely (a merge
            // patch null removes the field) so the team actually stops auto-
            // running, honouring the "0 = passive" label instead of silently
            // leaving the previous cadence in place.
            spec.insert("cadence".into(), serde_json::Value::Null);
        }
    }
    if let Some(roles) = &b.roles {
        reject_reserved_role_names(&name, roles)?;
        spec.insert("roster".into(), serde_json::json!(build_roster(roles)));
    }
    let mut blueprint = serde_json::Map::new();
    if let Some(rt) = b
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let rt = if crate::routes::compose::is_non_autonomous_harness(rt) {
            "OpenClaw"
        } else {
            rt
        };
        blueprint.insert("runtime".into(), serde_json::json!(rt));
    }
    if let Some(model) = b.model.as_deref().map(str::trim) {
        if model.is_empty() {
            blueprint.insert("model".into(), serde_json::Value::Null);
        } else if let Some((provider, deployment)) = model.split_once("::") {
            blueprint.insert(
                "model".into(),
                serde_json::json!({"provider": provider, "deployment": deployment}),
            );
        } else {
            return Err(AppError::BadRequest(
                "model must be encoded as provider::deployment".into(),
            ));
        }
    }
    if let Some(fallbacks) = &b.model_fallbacks {
        let mut seen = std::collections::BTreeSet::new();
        let mut routes = Vec::new();
        for fallback in fallbacks {
            let fallback = fallback.trim();
            if fallback.is_empty() || !seen.insert(fallback.to_string()) {
                continue;
            }
            let Some((provider, deployment)) = fallback.split_once("::") else {
                return Err(AppError::BadRequest(
                    "model_fallbacks entries must be encoded as provider::deployment".into(),
                ));
            };
            routes.push(serde_json::json!({
                "provider": provider,
                "deployment": deployment,
            }));
        }
        if routes.len() > 8 {
            return Err(AppError::BadRequest(
                "model_fallbacks may contain at most 8 unique routes".into(),
            ));
        }
        blueprint.insert("modelFallbacks".into(), serde_json::json!(routes));
    }
    if let Some(memory) = b.memory.as_deref() {
        blueprint.insert(
            "memory".into(),
            if memory.trim().is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::json!(memory.trim())
            },
        );
    }
    if let Some(servers) = &b.mcp_servers {
        let servers = normalize_mcp_servers(servers)?;
        validate_mcp_servers(cluster, &ns, &servers).await?;
        blueprint.insert("mcpServers".into(), serde_json::json!(servers));
    }
    if let Some(repos) = &b.git_write_repos {
        let git_write =
            crate::routes::github::authorize_git_write(cluster, &ns, &principal, Some(repos))
                .await?;
        blueprint.insert(
            "githubBinding".into(),
            git_write
                .as_ref()
                .map(|(_, binding)| serde_json::to_value(binding))
                .transpose()
                .map_err(|error| AppError::Upstream(error.to_string()))?
                .unwrap_or(serde_json::Value::Null),
        );
        blueprint.insert(
            "gitWrite".into(),
            git_write
                .map(|(grant, _)| {
                    serde_json::to_value(grant).map_err(|e| AppError::Upstream(e.to_string()))
                })
                .transpose()?
                .unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(egress) = &b.egress {
        let entries = egress
            .iter()
            .filter_map(|entry| {
                let host = entry.host.trim();
                (!host.is_empty()).then(|| serde_json::json!({"host": host, "port": entry.port}))
            })
            .collect::<Vec<_>>();
        blueprint.insert("egress".into(), serde_json::json!(entries));
    }
    if let Some(mode) = b.egress_mode.as_deref().map(str::trim) {
        let mode = match mode.to_ascii_lowercase().as_str() {
            "strict" => "Strict",
            "learning" | "learn" => "Learn",
            _ => {
                return Err(AppError::BadRequest(
                    "egress_mode must be 'learning' or 'strict'".into(),
                ));
            }
        };
        blueprint.insert("egressMode".into(), serde_json::json!(mode));
    }
    if let Some(execution_plan) = &b.execution_plan {
        blueprint.insert(
            "executionPlan".into(),
            serde_json::to_value(execution_plan.clone().into_crd()).map_err(|error| {
                AppError::BadRequest(format!("execution_plan could not be serialized: {error}"))
            })?,
        );
    }
    if !blueprint.is_empty() {
        // Merge-patch the nested blueprint so runtime/MCP edits preserve the
        // team's existing toolPolicy/model and can be changed together.
        spec.insert("blueprint".into(), serde_json::Value::Object(blueprint));
    }
    if let Some(ttl) = b.run_retention_ttl_seconds {
        spec.insert("runRetentionTtlSeconds".into(), serde_json::json!(ttl));
    }
    let api: Api<KarsTeam> = cluster.teams(&ns);
    api.patch(
        &name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(serde_json::json!({"spec": spec})),
    )
    .await
    .map_err(|e| AppError::Upstream(e.to_string()))?;
    if execution_plan_changed {
        cluster
            .merge_patch_kind(
                &ns,
                "KarsTask",
                &format!("{name}-principal"),
                serde_json::json!({
                    "metadata": {
                        "annotations": {
                            "kars.azure.com/retry-not-before": null
                        }
                    }
                }),
            )
            .await
            .map_err(|error| {
                AppError::Upstream(format!(
                    "team plan was updated but its retry park could not be cleared: {error}"
                ))
            })?;
    }
    Ok(Json(serde_json::json!({"updated": true})))
}
