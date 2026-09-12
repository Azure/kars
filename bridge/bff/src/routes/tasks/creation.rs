use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::{Api, PostParams};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::{KarsTask, KarsTaskSpec, LocalObjectRef, TaskBudget, TaskEnvelope};
use crate::state::AppState;

use super::mapping::to_detail;
use super::{
    BlueprintDto, CreateTaskRequest, ModelDto, TaskDetailDto, map_kube_err, require_cluster,
};

fn validate_mission_fallback_route(
    options: &crate::routes::options::Options,
    blueprint: &BlueprintDto,
    runtime: &str,
    model: &ModelDto,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
    total_tokens: Option<i64>,
) -> AppResult<()> {
    if !options
        .models
        .iter()
        .any(|option| option.provider == model.provider && option.deployment == model.deployment)
    {
        return Err(AppError::BadRequest(format!(
            "fallback model route `{}::{}` is not present in the live model catalogue",
            model.provider, model.deployment
        )));
    }
    match crate::routes::options::route_qualification(
        runtime,
        &model.provider,
        &model.deployment,
        required_capabilities,
        max_parallel,
        total_tokens,
    ) {
        Ok(true) => {}
        Ok(false) => {
            return Err(AppError::BadRequest(format!(
                "fallback route `{runtime} · {}::{}` lacks atomic qualification for capabilities: {}",
                model.provider,
                model.deployment,
                required_capabilities
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Err(error) => {
            return Err(AppError::Upstream(format!(
                "route qualification configuration error: {error}"
            )));
        }
    }
    let route = crate::routes::options::route_label(runtime, &model.provider, &model.deployment);
    for server in &blueprint.mcp_servers {
        let option = options
            .mcp_servers
            .iter()
            .find(|option| option.name == *server)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "MCP server `{server}` is not present in the live options catalogue"
                ))
            })?;
        if !crate::routes::options::mcp_server_qualified_for_route(
            runtime,
            &model.provider,
            &model.deployment,
            option,
        )
        .map_err(|error| {
            AppError::Upstream(format!(
                "resource qualification configuration error: {error}"
            ))
        })? {
            return Err(AppError::BadRequest(format!(
                "MCP server `{server}` lacks current resource qualification for fallback {route}"
            )));
        }
    }
    if let Some(memory) = blueprint
        .memory
        .as_deref()
        .filter(|memory| !memory.is_empty())
    {
        let option = options
            .memories
            .iter()
            .find(|option| option.name == memory)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "memory `{memory}` is not present in the live options catalogue"
                ))
            })?;
        if !crate::routes::options::memory_binding_qualified_for_route(
            runtime,
            &model.provider,
            &model.deployment,
            option,
        )
        .map_err(|error| {
            AppError::Upstream(format!(
                "resource qualification configuration error: {error}"
            ))
        })? {
            return Err(AppError::BadRequest(format!(
                "memory `{memory}` lacks current resource qualification for fallback {route}"
            )));
        }
    }
    for skill in &blueprint.skills {
        let option = options
            .skills
            .iter()
            .find(|option| option.name == *skill)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "skill `{skill}` is not present in the approved live catalogue"
                ))
            })?;
        if !crate::routes::options::skill_version_qualified_for_route(
            runtime,
            &model.provider,
            &model.deployment,
            option,
        )
        .map_err(|error| {
            AppError::Upstream(format!(
                "resource qualification configuration error: {error}"
            ))
        })? {
            return Err(AppError::BadRequest(format!(
                "skill `{skill}` lacks current version qualification for fallback {route}"
            )));
        }
    }
    Ok(())
}

/// `POST /api/namespaces/:ns/tasks` — create a task.
///
/// The BFF never sets status — it submits the spec and lets the controller
/// validate the envelope and stamp the digest. Admission (CEL) rejects an
/// amplifying envelope here, which we surface as a 422-style upstream error.
pub async fn create_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(mut req): Json<CreateTaskRequest>,
) -> AppResult<Json<TaskDetailDto>> {
    let cluster = require_cluster(&state)?;
    cluster.credential_grant(&ns).await.map_err(map_kube_err)?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    // The caller cannot choose attribution; it is derived from the verified
    // Bridge session inserted by auth middleware.
    req.created_by = Some(principal.name.clone());
    let created_by = principal.name.clone();
    if let Some(plan) = req
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.execution_plan.as_ref())
    {
        crate::routes::compose::validate_execution_plan(plan).map_err(AppError::BadRequest)?;
        req.envelope.delegation_depth = 1;
    } else if let Some(delegation) = req.delegation.as_ref() {
        crate::routes::compose::validate_delegation(delegation).map_err(AppError::BadRequest)?;
        req.envelope.delegation_depth = i32::from(delegation.mode == "principal-specialists");
    }
    let mut harness_correction: Option<String> = None;
    if let Some(blueprint) = req.blueprint.as_mut()
        && let Some(runtime) = blueprint.runtime.as_deref()
        && crate::routes::compose::is_non_autonomous_harness(runtime)
    {
        harness_correction = Some(format!(
            "harness {runtime} is a bootstrap-only adapter (no autonomous task loop) and cannot run a one-shot mission; corrected to OpenClaw"
        ));
        blueprint.runtime = Some("OpenClaw".to_string());
    }
    let git_write = crate::routes::github::authorize_git_write(
        cluster,
        &ns,
        &principal,
        req.git_write_repos.as_deref(),
    )
    .await?;
    if req.blueprint.as_ref().is_some_and(|blueprint| {
        blueprint.runtime.as_deref().unwrap_or("OpenClaw") != "OpenClaw"
            && !blueprint.skills.is_empty()
    }) {
        return Err(AppError::BadRequest(
            "controller-mounted file skills are currently supported only by OpenClaw".into(),
        ));
    }
    if req
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.tool_policy.as_deref())
        == Some("kars-team-member")
    {
        return Err(AppError::BadRequest(
            "kars-team-member is reserved for declared standing-team specialists".into(),
        ));
    }
    if let Some(blueprint) = req.blueprint.as_mut() {
        if blueprint.model_fallbacks.len() > 8 {
            return Err(AppError::BadRequest(
                "model_fallbacks may contain at most 8 routes".into(),
            ));
        }
        if blueprint.model.is_none() {
            let options = crate::routes::options::build_options(cluster).await?;
            blueprint.model = options
                .models
                .iter()
                .find(|model| model.is_default)
                .or_else(|| options.models.first())
                .map(|model| ModelDto {
                    provider: model.provider.clone(),
                    deployment: model.deployment.clone(),
                });
        }
    }
    if let Some(blueprint) = req.blueprint.as_ref()
        && let Some(model) = blueprint.model.as_ref()
    {
        let options = crate::routes::options::build_options(cluster).await?;
        let served = options.models.iter().any(|option| {
            option.provider == model.provider && option.deployment == model.deployment
        });
        if !served {
            return Err(AppError::BadRequest(format!(
                "model route `{}::{}` is not present in the live model catalogue",
                model.provider, model.deployment
            )));
        }
        let runtime = blueprint.runtime.as_deref().unwrap_or("OpenClaw");
        if !cluster.runnable_runtimes().await.contains(runtime) {
            return Err(AppError::BadRequest(format!(
                "runtime `{runtime}` cannot start on this cluster"
            )));
        }
        let (required_capabilities, max_parallel) =
            crate::routes::validate::qualification_requirements(blueprint, None);
        let total_tokens = req
            .envelope
            .budget
            .as_ref()
            .and_then(|budget| budget.tokens);
        match crate::routes::options::route_qualification(
            runtime,
            &model.provider,
            &model.deployment,
            &required_capabilities,
            max_parallel,
            total_tokens,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err(AppError::BadRequest(format!(
                    "runtime/model route `{runtime} · {}::{}` lacks qualification evidence for capabilities: {}",
                    model.provider,
                    model.deployment,
                    required_capabilities
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            Err(error) => {
                return Err(AppError::Upstream(format!(
                    "route qualification configuration error: {error}"
                )));
            }
        }
        fn find_resource<'a>(
            items: &'a [crate::routes::options::RefOption],
            name: &str,
        ) -> Option<&'a crate::routes::options::RefOption> {
            items.iter().find(|option| option.name == name)
        }
        for server in &blueprint.mcp_servers {
            let Some(option) = find_resource(&options.mcp_servers, server) else {
                return Err(AppError::BadRequest(format!(
                    "MCP server `{server}` is not present in the live options catalogue"
                )));
            };
            match crate::routes::options::mcp_server_qualified_for_route(
                runtime,
                &model.provider,
                &model.deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "MCP server `{server}` lacks retained resource qualification for {} at current schema {}",
                        crate::routes::options::route_label(
                            runtime,
                            &model.provider,
                            &model.deployment
                        ),
                        option.tool_schema_digest.as_deref().unwrap_or("missing")
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
        if let Some(memory) = blueprint
            .memory
            .as_deref()
            .filter(|memory| !memory.is_empty())
        {
            let Some(option) = find_resource(&options.memories, memory) else {
                return Err(AppError::BadRequest(format!(
                    "memory `{memory}` is not present in the live options catalogue"
                )));
            };
            match crate::routes::options::memory_binding_qualified_for_route(
                runtime,
                &model.provider,
                &model.deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "memory `{memory}` lacks retained resource qualification for {} at backend {} / compiled digest {}",
                        crate::routes::options::route_label(
                            runtime,
                            &model.provider,
                            &model.deployment
                        ),
                        option.backend.as_deref().unwrap_or("missing"),
                        option.compiled_digest.as_deref().unwrap_or("missing"),
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
        for skill in &blueprint.skills {
            let Some(option) = find_resource(&options.skills, skill) else {
                return Err(AppError::BadRequest(format!(
                    "skill `{skill}` is not present in the approved live catalogue"
                )));
            };
            match crate::routes::options::skill_version_qualified_for_route(
                runtime,
                &model.provider,
                &model.deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "skill `{skill}` lacks retained resource qualification for {} at version digest {}",
                        crate::routes::options::route_label(
                            runtime,
                            &model.provider,
                            &model.deployment
                        ),
                        option.version_digest.as_deref().unwrap_or("missing")
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for fallback in &blueprint.model_fallbacks {
            let key = format!("{}::{}", fallback.provider, fallback.deployment);
            if key == format!("{}::{}", model.provider, model.deployment) || !seen.insert(key) {
                continue;
            }
            validate_mission_fallback_route(
                &options,
                blueprint,
                runtime,
                fallback,
                &required_capabilities,
                max_parallel,
                total_tokens,
            )?;
        }
    }

    // Aggregate inference-budget gate (cluster + workspace + user). A launched
    // mission consumes inference tokens, so a strict/over-buffer budget at any
    // tier blocks starting new work. Draft (unlaunched) missions don't run yet,
    // so they pass — the gate re-applies when they run.
    if req.launch {
        crate::routes::budgets::enforce_launch_budget(cluster, &ns, &created_by).await?;
    }

    // Default the tool policy to `kars-default` when neither the request envelope
    // nor the blueprint pins one. This is not cosmetic: the AGT mesh transport the
    // run's delivery rides on requires a mounted ToolPolicy. With governance OFF
    // the sandbox mounts no policy, the AGT engine fails closed, and the agent can
    // never send its `task_response` back to the controller — the run streams live
    // but NEVER delivers (no output ConfigMap, endless re-dispatch). Every bridge
    // mission must be governed; `kars-default` is the cluster's baseline policy.
    // An explicit blueprint tool policy still wins (governance_spec prefers it), so
    // we only inject the default when the blueprint carries none.
    let blueprint_has_tool_policy = req
        .blueprint
        .as_ref()
        .and_then(|b| b.tool_policy.as_ref())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let tool_policy_ref = req
        .envelope
        .tool_policy
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| (!blueprint_has_tool_policy).then(|| "kars-default".to_string()))
        .map(|name| LocalObjectRef { name });

    // ── Hard capability match, defense-in-depth ─────────────────────────────
    // A direct mission is one-shot autonomous; a bootstrap-only adapter has no
    // task-execution loop and delivers nothing. The compose flow already
    // corrects this, but a manually-edited package could still name one — so
    // enforce it again at creation: rewrite the harness to OpenClaw and record
    // the correction as a governance annotation on the task so it survives into
    // the run and the receipt/decision view. (Hermes/BYO are autonomous — kept.)
    let mut blueprint = req.blueprint.map(BlueprintDto::into_crd);
    if let Some((git_write, binding)) = git_write {
        let blueprint = blueprint.get_or_insert_with(Default::default);
        blueprint.git_write = Some(git_write);
        blueprint.github_binding = Some(binding);
    }
    let spec = KarsTaskSpec {
        objective: req.objective,
        display_name: req.display_name,
        execution: req.launch.then_some(crate::kars::task::TaskExecution {
            launch: true,
            runtime: None,
        }),
        blueprint,
        parent_ref: req
            .parent
            .filter(|s| !s.is_empty())
            .map(|name| LocalObjectRef { name }),
        envelope: TaskEnvelope {
            tier: req.envelope.tier,
            authority_ceiling: req.envelope.authority_ceiling,
            delegation_depth: req.envelope.delegation_depth,
            budget: req.envelope.budget.map(|b| TaskBudget {
                scope: b.scope,
                tokens: b.tokens,
                usd_micros: b.usd_micros,
            }),
            tool_policy_ref,
            egress_allowlist_ref: req
                .envelope
                .egress_allowlist
                .filter(|s| !s.is_empty())
                .map(|name| LocalObjectRef { name }),
        },
        retention_ttl_seconds: req.retention_ttl_seconds,
    };
    let mut task = KarsTask::new(&req.name, spec);
    if let Some(plan) = task
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.execution_plan.as_ref())
    {
        let total_tokens = task
            .spec
            .envelope
            .budget
            .as_ref()
            .and_then(|budget| budget.tokens)
            .ok_or_else(|| {
                AppError::BadRequest(
                    "execution-plan missions require an explicit total token budget".into(),
                )
            })?;
        let (principal_tokens, child_tokens) =
            crate::routes::compose::delegation_budget_allocation(total_tokens, plan.roles.len())
                .map_err(AppError::BadRequest)?;
        let annotations = task
            .metadata
            .annotations
            .get_or_insert_with(Default::default);
        annotations.insert(
            "kars.azure.com/mission-budget-total".into(),
            total_tokens.to_string(),
        );
        annotations.insert(
            "kars.azure.com/mission-principal-budget".into(),
            principal_tokens.to_string(),
        );
        annotations.insert(
            "kars.azure.com/mission-child-budget".into(),
            child_tokens.to_string(),
        );
        annotations.insert(
            "kars.azure.com/mission-specialist-count".into(),
            plan.roles.len().to_string(),
        );
        annotations.insert(
            "kars.azure.com/mission-decomposition".into(),
            "execution-plan/v1".into(),
        );
    }
    // Record the capability correction on the task so it's durable and surfaces in
    // the governed record (the run reads task annotations; the receipt/decision
    // view can attest the harness was corrected rather than silently swapped).
    if let Some(reason) = &harness_correction {
        task.metadata
            .annotations
            .get_or_insert_with(Default::default)
            .insert(
                "kars.azure.com/harness-corrected".to_string(),
                reason.clone(),
            );
    }
    // Stamp the creator for per-user budget attribution.
    task.metadata
        .annotations
        .get_or_insert_with(Default::default)
        .insert("kars.azure.com/created-by".to_string(), created_by.clone());
    let annotations = task
        .metadata
        .annotations
        .get_or_insert_with(Default::default);
    annotations.insert(
        "kars.azure.com/owner-sub".to_string(),
        principal.sub.clone(),
    );
    annotations.insert(
        "kars.azure.com/owner-name".to_string(),
        principal.name.clone(),
    );
    let launch = task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch);
    if let Some(execution) = task.spec.execution.as_mut() {
        execution.launch = false;
    }
    let created = api
        .create(&PostParams::default(), &task)
        .await
        .map_err(map_kube_err)?;
    cluster
        .finish_created_credentials(
            &crate::kars::credentials::Target {
                kind: "KarsTask".into(),
                namespace: ns.clone(),
                name: created.name_any(),
                uid: created
                    .uid()
                    .ok_or_else(|| AppError::Upstream("Task CREATE omitted UID".into()))?,
            },
            launch,
        )
        .await
        .map_err(map_kube_err)?;
    let created = api.get(&created.name_any()).await.map_err(map_kube_err)?;
    Ok(Json(to_detail(
        &created,
        Vec::new(),
        Vec::new(),
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        None,
    )))
}
