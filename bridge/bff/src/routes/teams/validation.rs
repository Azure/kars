// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Copyright (c) Pal Lakatos-Toth.

use crate::error::{AppError, AppResult};
use crate::routes::options::{ModelOption, Options, RefOption};

use super::{CreateRole, TeamModelRoutes};

pub(super) fn reject_budget_update<'de, D: serde::Deserializer<'de>>(_: D) -> Result<(), D::Error> {
    Err(serde::de::Error::custom(
        "Team lifetime budgets are immutable; a different limit requires a separately reviewed new Team",
    ))
}

pub(super) fn validate_team_budget(
    budget: Option<&crate::routes::tasks::BudgetDto>,
) -> AppResult<Option<crate::kars::task::TaskBudget>> {
    let Some(budget) = budget else {
        return Ok(None);
    };
    if !matches!(
        budget.scope,
        Some(crate::kars::task::BudgetScope::GovernedInference)
    ) {
        return Err(AppError::BadRequest(
            "a Team lifetime budget requires explicit GovernedInference scope".into(),
        ));
    }
    let limits = [budget.tokens, budget.usd_micros];
    if limits.iter().all(Option::is_none)
        || limits
            .iter()
            .flatten()
            .any(|limit| !(1..=9_007_199_254_740_991).contains(limit))
    {
        return Err(AppError::BadRequest("a Team lifetime budget requires positive safe whole-number limits; invalid or empty caps cannot be discarded".into()));
    }
    Ok(Some(crate::kars::task::TaskBudget {
        scope: budget.scope,
        tokens: budget.tokens,
        usd_micros: budget.usd_micros,
    }))
}

/// Build a `spec.roster` array from create/update role inputs, shared by team
/// creation and roster editing so both paths produce identical role shapes
/// (per-member systemPrompt + blueprint{runtime, model} + skills).
/// Reject roster role names that collide with the reserved task names the
/// controller derives from the team (`<team>-principal`). A role named
/// "principal" would otherwise re-materialize the principal task as a member
/// parented to itself, deadlocking the whole team. The controller also skips
/// such a role defensively, but rejecting here gives the operator a clear error
/// instead of a silently dropped role.
pub(super) fn reject_reserved_role_names(team: &str, roles: &[CreateRole]) -> AppResult<()> {
    let _ = team;
    for r in roles {
        let n = r.name.trim().to_ascii_lowercase();
        if n == "principal" {
            return Err(AppError::BadRequest(
                "role name 'principal' is reserved for the team's authority root — rename this role".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn build_roster(roles: &[CreateRole]) -> Vec<serde_json::Value> {
    roles
        .iter()
        .filter(|r| !r.name.trim().is_empty())
        .map(|r| {
            let mut role = serde_json::json!({ "name": r.name.trim() });
            if let Some(sp) = &r.system_prompt
                && !sp.trim().is_empty()
            {
                role["systemPrompt"] = serde_json::json!(sp.trim());
            }
            let mut bp = serde_json::Map::new();
            if let Some(rt) = &r.runtime
                && !rt.is_empty()
            {
                // Harness capability, defense-in-depth: a bootstrap-only adapter
                // (no autonomous task loop) can't run a standing member — a
                // hand-composed team could still name one, so correct it to
                // OpenClaw here too. Hermes/BYO are autonomous and pass through.
                let rt = if crate::routes::compose::is_non_autonomous_harness(rt) {
                    "OpenClaw"
                } else {
                    rt.as_str()
                };
                bp.insert("runtime".into(), serde_json::json!(rt));
            }
            if let Some(m) = &r.model
                && let Some((provider, deployment)) = m.split_once("::")
            {
                bp.insert(
                    "model".into(),
                    serde_json::json!({ "provider": provider, "deployment": deployment }),
                );
            }
            if !bp.is_empty() {
                role["blueprint"] = serde_json::Value::Object(bp);
            }
            if !r.skills.is_empty() {
                role["skills"] = serde_json::json!(r.skills);
            }
            role
        })
        .collect()
}

pub(super) fn normalize_mcp_servers(servers: &[String]) -> AppResult<Vec<String>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut normalized = Vec::new();
    for server in servers {
        let server = server.trim();
        if !server.is_empty() && seen.insert(server.to_string()) {
            normalized.push(server.to_string());
        }
    }
    if normalized.len() > 8 {
        return Err(AppError::BadRequest(
            "a team may connect at most 8 MCP servers".into(),
        ));
    }
    Ok(normalized)
}

pub(super) fn normalize_autonomous_runtime(runtime: &mut Option<String>) {
    if let Some(value) = runtime.as_deref()
        && (value.trim().is_empty() || crate::routes::compose::is_non_autonomous_harness(value))
    {
        *runtime = Some("OpenClaw".into());
    }
}

pub(super) fn normalize_model_fallback_routes(
    routes: &[String],
    primary: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut normalized = Vec::new();
    for route in routes {
        let route = route.trim();
        if route.is_empty()
            || primary.is_some_and(|primary| route == primary)
            || !seen.insert(route.to_string())
        {
            continue;
        }
        let valid = route
            .split_once("::")
            .is_some_and(|(provider, deployment)| {
                !provider.trim().is_empty() && !deployment.trim().is_empty()
            });
        if !valid {
            return Err(AppError::BadRequest(
                "model_fallbacks entries must be encoded as provider::deployment".into(),
            ));
        }
        normalized.push(route.to_string());
    }
    if normalized.len() > 8 {
        return Err(AppError::BadRequest(
            "model_fallbacks may contain at most 8 unique routes".into(),
        ));
    }
    Ok(normalized)
}

fn validate_model_route(models: &[ModelOption], route: &str) -> AppResult<()> {
    let route = route.trim();
    if route.is_empty() {
        return Ok(());
    }
    let valid = route
        .split_once("::")
        .is_some_and(|(provider, deployment)| {
            models
                .iter()
                .any(|model| model.provider == provider && model.deployment == deployment)
        });
    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "model route `{route}` is not present in the live model catalogue"
        )))
    }
}

fn option_named<'a>(items: &'a [RefOption], namespace: &str, name: &str) -> Option<&'a RefOption> {
    items
        .iter()
        .find(|option| option.name == name && option.namespace == namespace)
}

fn team_role_qualification_requirements(
    plan: &crate::routes::tasks::ExecutionPlanDto,
    role_name: &str,
) -> std::collections::BTreeSet<String> {
    let mut required =
        std::collections::BTreeSet::from(["team".to_string(), "telemetry".to_string()]);
    if let Some(role) = plan.roles.iter().find(|role| role.name == role_name) {
        for phase in &role.phases {
            required.extend(phase.capabilities.iter().cloned());
        }
    }
    required
}

pub(super) fn validate_team_model_routes(
    options: &Options,
    routes: TeamModelRoutes<'_>,
) -> AppResult<()> {
    let TeamModelRoutes {
        namespace,
        total_tokens,
        runtime,
        model,
        model_fallbacks,
        roles,
        execution_plan,
        mcp_servers,
        memory,
    } = routes;
    let memory = memory.map(str::trim).filter(|memory| !memory.is_empty());
    let default_route = options
        .models
        .iter()
        .find(|model| model.is_default)
        .or_else(|| options.models.first())
        .map(|model| format!("{}::{}", model.provider, model.deployment))
        .unwrap_or_default();
    let principal_route = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(default_route.as_str());
    validate_model_route(&options.models, principal_route)?;
    let principal_runtime = runtime
        .map(str::trim)
        .filter(|runtime| !runtime.is_empty())
        .unwrap_or("OpenClaw");
    let principal_model = principal_route.split_once("::").ok_or_else(|| {
        AppError::BadRequest(format!(
            "model route `{principal_route}` must use provider::deployment"
        ))
    })?;
    let principal_blueprint = crate::routes::tasks::BlueprintDto {
        runtime: Some(principal_runtime.to_string()),
        model: Some(crate::routes::tasks::ModelDto {
            provider: principal_model.0.to_string(),
            deployment: principal_model.1.to_string(),
        }),
        model_fallbacks: Vec::new(),
        instructions: None,
        tool_policy: None,
        mcp_servers: mcp_servers.to_vec(),
        egress: Vec::new(),
        egress_mode: None,
        isolation: None,
        memory: memory.map(str::to_string),
        skills: roles
            .iter()
            .flat_map(|role| role.skills.iter().cloned())
            .collect(),
        execution_plan: Some(execution_plan.clone()),
    };
    let (principal_required, principal_parallel) =
        crate::routes::validate::qualification_requirements(&principal_blueprint, Some("team"));
    validate_qualified_model_route(
        principal_runtime,
        principal_route,
        &principal_required,
        principal_parallel,
        total_tokens,
    )?;
    let principal_route_label = crate::routes::options::route_label(
        principal_runtime,
        principal_model.0,
        principal_model.1,
    );
    for server in mcp_servers {
        let option = option_named(&options.mcp_servers, namespace, server).ok_or_else(|| {
            AppError::BadRequest(format!(
                "MCP server `{server}` is not present in the live options catalogue"
            ))
        })?;
        match crate::routes::options::mcp_server_qualified_for_route(
            principal_runtime,
            principal_model.0,
            principal_model.1,
            option,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err(AppError::BadRequest(format!(
                    "MCP server `{server}` lacks retained resource qualification for {principal_route_label} at current schema {}",
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
    if let Some(memory) = memory.map(str::trim).filter(|memory| !memory.is_empty()) {
        let option = option_named(&options.memories, namespace, memory).ok_or_else(|| {
            AppError::BadRequest(format!(
                "memory `{memory}` is not present in the live options catalogue"
            ))
        })?;
        match crate::routes::options::memory_binding_qualified_for_route(
            principal_runtime,
            principal_model.0,
            principal_model.1,
            option,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err(AppError::BadRequest(format!(
                    "memory `{memory}` lacks retained resource qualification for {principal_route_label} at backend {} / compiled digest {}",
                    option.backend.as_deref().unwrap_or("missing"),
                    option.compiled_digest.as_deref().unwrap_or("missing")
                )));
            }
            Err(error) => {
                return Err(AppError::Upstream(format!(
                    "resource qualification configuration error: {error}"
                )));
            }
        }
    }
    for role in roles {
        let role_route = role
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or(principal_route);
        validate_model_route(&options.models, role_route)?;
        let role_runtime = role
            .runtime
            .as_deref()
            .map(str::trim)
            .filter(|runtime| !runtime.is_empty())
            .unwrap_or(principal_runtime);
        let role_required = team_role_qualification_requirements(execution_plan, &role.name);
        validate_qualified_model_route(role_runtime, role_route, &role_required, 1, total_tokens)?;
        let (provider, deployment) = role_route.split_once("::").ok_or_else(|| {
            AppError::BadRequest(format!(
                "model route `{role_route}` must use provider::deployment"
            ))
        })?;
        let role_route_label =
            crate::routes::options::route_label(role_runtime, provider, deployment);
        if role_required.contains("mcp") {
            for server in mcp_servers {
                let option =
                    option_named(&options.mcp_servers, namespace, server).ok_or_else(|| {
                        AppError::BadRequest(format!(
                            "MCP server `{server}` is not present in the live options catalogue"
                        ))
                    })?;
                match crate::routes::options::mcp_server_qualified_for_route(
                    role_runtime,
                    provider,
                    deployment,
                    option,
                ) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(AppError::BadRequest(format!(
                            "role `{}` MCP server `{server}` lacks retained resource qualification for {role_route_label} at current schema {}",
                            role.name,
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
        }
        if role_required.contains("memory")
            && let Some(memory) = memory.map(str::trim).filter(|memory| !memory.is_empty())
        {
            let option = option_named(&options.memories, namespace, memory).ok_or_else(|| {
                AppError::BadRequest(format!(
                    "memory `{memory}` is not present in the live options catalogue"
                ))
            })?;
            match crate::routes::options::memory_binding_qualified_for_route(
                role_runtime,
                provider,
                deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "role `{}` memory `{memory}` lacks retained resource qualification for {role_route_label} at backend {} / compiled digest {}",
                        role.name,
                        option.backend.as_deref().unwrap_or("missing"),
                        option.compiled_digest.as_deref().unwrap_or("missing")
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
        for skill in &role.skills {
            let option = option_named(&options.skills, namespace, skill).ok_or_else(|| {
                AppError::BadRequest(format!(
                    "skill `{skill}` is not present in the approved live catalogue"
                ))
            })?;
            match crate::routes::options::skill_version_qualified_for_route(
                role_runtime,
                provider,
                deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "role `{}` skill `{skill}` lacks retained resource qualification for {role_route_label} at current version digest {}",
                        role.name,
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
    }
    let mut seen_fallbacks = std::collections::BTreeSet::new();
    for fallback in model_fallbacks {
        let fallback = fallback.trim();
        if fallback.is_empty() {
            continue;
        }
        if !seen_fallbacks.insert(fallback.to_string()) {
            continue;
        }
        if seen_fallbacks.len() > 8 {
            return Err(AppError::BadRequest(
                "model_fallbacks may contain at most 8 unique routes".into(),
            ));
        }
        validate_model_route(&options.models, fallback)?;
        let fallback_roles = roles
            .iter()
            .cloned()
            .map(|mut role| {
                role.model = Some(fallback.to_string());
                role
            })
            .collect::<Vec<_>>();
        validate_team_model_routes(
            options,
            TeamModelRoutes {
                namespace,
                total_tokens,
                runtime,
                model: Some(fallback),
                model_fallbacks: &[],
                roles: &fallback_roles,
                execution_plan,
                mcp_servers,
                memory,
            },
        )?;
    }
    Ok(())
}

fn validate_qualified_model_route(
    runtime: &str,
    route: &str,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
    total_tokens: Option<i64>,
) -> AppResult<()> {
    let Some((provider, deployment)) = route.split_once("::") else {
        return Err(AppError::BadRequest(format!(
            "model route `{route}` must use provider::deployment"
        )));
    };
    match crate::routes::options::route_admitted(
        runtime,
        provider,
        deployment,
        required_capabilities,
        max_parallel,
        total_tokens,
    ) {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::BadRequest(format!(
            "runtime/model route `{runtime} · {provider}::{deployment}` has not passed the fresh E2E qualification matrix"
        ))),
        Err(error) => Err(AppError::Upstream(format!(
            "route qualification configuration error: {error}"
        ))),
    }
}

pub(super) fn normalize_lifecycle_mode(mode: Option<&str>) -> AppResult<Option<&'static str>> {
    let Some(mode) = mode.map(str::trim).filter(|mode| !mode.is_empty()) else {
        return Ok(None);
    };
    match mode.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
        "ephemeral" => Ok(Some("ephemeral")),
        "resourceoptimized" => Ok(Some("resourceOptimized")),
        "persistent" => Ok(Some("persistent")),
        _ => Err(AppError::BadRequest(
            "lifecycle_mode must be 'ephemeral', 'resourceOptimized', or 'persistent'".into(),
        )),
    }
}

pub(super) fn validate_warm_idle_seconds(seconds: Option<i64>) -> AppResult<Option<i64>> {
    match seconds {
        Some(seconds) if seconds < 0 => Err(AppError::BadRequest(
            "warm_idle_seconds must be non-negative".into(),
        )),
        value => Ok(value),
    }
}

pub(super) fn apply_team_git_write(
    spec: &mut serde_json::Value,
    git_write: Option<&crate::kars::task::GitWriteConfig>,
) -> AppResult<()> {
    let Some(git_write) = git_write else {
        return Ok(());
    };
    if !spec["blueprint"].is_object() {
        spec["blueprint"] = serde_json::json!({});
    }
    spec["blueprint"]["gitWrite"] =
        serde_json::to_value(git_write).map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(())
}

pub(super) async fn validate_mcp_servers(
    cluster: &crate::kars::cluster::Cluster,
    namespace: &str,
    servers: &[String],
) -> AppResult<()> {
    for server in servers {
        let Some(resource) = cluster
            .get_kind(namespace, "McpServer", server)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?
        else {
            return Err(AppError::BadRequest(format!(
                "MCP server `{server}` is not installed in namespace `{namespace}`"
            )));
        };
        let phase = resource
            .data
            .get("status")
            .and_then(|status| status.get("phase"))
            .and_then(|phase| phase.as_str());
        let observed_generation = resource
            .data
            .get("status")
            .and_then(|status| status.get("observedGeneration"))
            .and_then(|generation| generation.as_i64());
        if phase != Some("Ready") || observed_generation != resource.metadata.generation {
            return Err(AppError::BadRequest(format!(
                "MCP server `{server}` is not Ready for its current generation in namespace `{namespace}`"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_budget_absence_is_legacy_compatibility_not_enrollment() {
        for body in [
            serde_json::json!({"name":"example","charter":"Review work"}),
            serde_json::json!({"name":"example","charter":"Review work","budget":null}),
        ] {
            let request: super::super::CreateTeamRequest = serde_json::from_value(body).unwrap();
            assert!(
                validate_team_budget(request.budget.as_ref())
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn team_budget_preserves_reviewed_limits_and_crd_camel_case() {
        for (tokens, usd) in [
            (Some(2_000_000), None),
            (None, Some(5_000_000)),
            (Some(9_007_199_254_740_991_i64), Some(5_000_000)),
        ] {
            let input =
                serde_json::json!({"scope":"GovernedInference","tokens":tokens,"usd_micros":usd});
            let dto = serde_json::from_value(input).unwrap();
            let budget = validate_team_budget(Some(&dto)).unwrap().unwrap();
            let crd = serde_json::to_value(budget).unwrap();
            assert_eq!(crd["scope"], "GovernedInference");
            assert_eq!(
                crd.get("tokens").and_then(serde_json::Value::as_i64),
                tokens
            );
            assert_eq!(
                crd.get("usdMicros").and_then(serde_json::Value::as_i64),
                usd
            );
            assert!(crd.get("usd_micros").is_none());
        }
    }

    #[test]
    fn team_budget_rejects_invalid_or_unscoped_caps_without_dropping_them() {
        for input in [
            serde_json::json!({}),
            serde_json::json!({"scope":"GovernedInference"}),
            serde_json::json!({"tokens":1000}),
            serde_json::json!({"scope":null,"tokens":1000}),
        ] {
            let dto = serde_json::from_value(input).unwrap();
            assert!(validate_team_budget(Some(&dto)).is_err());
        }
        for field in ["tokens", "usd_micros"] {
            for invalid in [0, -1, 9_007_199_254_740_992_i64, i64::MAX] {
                let mut input = serde_json::json!({"scope":"GovernedInference","tokens":2000000,"usd_micros":5000000});
                input[field] = serde_json::json!(invalid);
                let dto = serde_json::from_value(input).unwrap();
                assert!(validate_team_budget(Some(&dto)).is_err());
            }
            for invalid in [
                serde_json::json!(1.5),
                serde_json::json!("2000"),
                serde_json::json!(true),
            ] {
                let mut input = serde_json::json!({"scope":"GovernedInference"});
                input[field] = invalid;
                assert!(serde_json::from_value::<crate::routes::tasks::BudgetDto>(input).is_err());
            }
        }
        assert!(
            serde_json::from_value::<crate::routes::tasks::BudgetDto>(
                serde_json::json!({"scope":"Planning","tokens":2000})
            )
            .is_err()
        );
    }

    #[test]
    fn team_update_cannot_change_or_remove_lifetime_budget() {
        assert!(
            serde_json::from_value::<super::super::UpdateTeamRequest>(
                serde_json::json!({"paused":true})
            )
            .is_ok()
        );
        for budget in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"scope":"GovernedInference","tokens":4000000}),
        ] {
            let error = serde_json::from_value::<super::super::UpdateTeamRequest>(
                serde_json::json!({"budget":budget}),
            )
            .unwrap_err();
            assert!(error.to_string().contains("immutable"));
        }
    }

    #[test]
    fn team_lifecycle_modes_are_normalized_and_idle_window_is_bounded() {
        assert_eq!(
            normalize_lifecycle_mode(Some("resource-optimized")).expect("mode"),
            Some("resourceOptimized")
        );
        assert_eq!(
            normalize_lifecycle_mode(Some("persistent")).expect("mode"),
            Some("persistent")
        );
        assert!(normalize_lifecycle_mode(Some("always-on")).is_err());
        assert_eq!(
            validate_warm_idle_seconds(Some(900)).expect("idle"),
            Some(900)
        );
        assert_eq!(validate_warm_idle_seconds(Some(0)).expect("idle"), Some(0));
        assert!(validate_warm_idle_seconds(Some(-1)).is_err());
    }

    #[test]
    fn team_models_require_exact_live_catalogue_pairs() {
        let models = vec![ModelOption {
            provider: "github-copilot".into(),
            deployment: "shared-name".into(),
            is_default: true,
            detail: None,
        }];
        assert!(validate_model_route(&models, "github-copilot::shared-name").is_ok());
        assert!(validate_model_route(&models, "").is_ok());
        assert!(validate_model_route(&models, "local-inference::shared-name").is_err());
        assert!(validate_model_route(&models, "shared-name").is_err());
    }

    #[test]
    fn team_budget_reaches_principal_member_and_fallback_qualification() {
        const CHILD: &str = "KARS_TEST_TEAM_BUDGET_ROUTE";
        let routes = [
            ("OpenClaw", "primary"),
            ("Hermes", "specialist"),
            ("OpenClaw", "fallback"),
            ("Hermes", "fallback"),
        ];
        let Ok(target) = std::env::var(CHILD) else {
            // Isolate environment-backed qualification without mutating the test process.
            for (index, _) in routes.iter().enumerate() {
                let records = routes.iter().enumerate().map(|(candidate, (runtime, model))| {
                    serde_json::json!({
                        "runtime": runtime, "provider": "test", "deployment": model,
                        "capabilities": ["team", "telemetry", "delegation"], "max_parallel": 1,
                        "min_total_tokens": if candidate == index { 2000 } else { 1000 },
                        "evidence": {"task": "test-fixture", "run_id": "test-run", "digest": "sha256:test-fixture"}
                    })
                }).collect::<Vec<_>>();
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "routes::teams::validation::tests::team_budget_reaches_principal_member_and_fallback_qualification",
                        "--nocapture",
                    ])
                    .env(CHILD, index.to_string())
                    .env("BRIDGE_ROUTE_QUALIFICATION_MODE", "required")
                    .env("BRIDGE_QUALIFICATION_RECORDS_JSON", serde_json::to_string(&records).unwrap())
                    .env_remove("BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };
        let options = Options {
            models: ["primary", "specialist", "fallback"]
                .into_iter()
                .map(|deployment| ModelOption {
                    provider: "test".into(),
                    deployment: deployment.into(),
                    is_default: deployment == "primary",
                    detail: None,
                })
                .collect(),
            default_model: None,
            provider: None,
            runtimes: vec![],
            isolation: vec![],
            tool_policies: vec![],
            mcp_servers: vec![],
            mcp_profiles: vec![],
            memories: vec![],
            skills: vec![],
        };
        let plan = serde_json::from_value(serde_json::json!({
            "schema": "kars.execution-plan/v1", "max_parallel": 1,
            "roles": [{"name": "writer", "objective": "Write briefing", "phases": [{
                "name": "draft", "objective": "Draft briefing", "capabilities": ["team", "telemetry"], "max_tool_calls": 0
            }]}],
            "synthesis": {"objective": "Combine briefing", "capabilities": [], "max_tool_calls": 0}
        })).unwrap();
        let roles = vec![CreateRole {
            name: "writer".into(),
            system_prompt: None,
            runtime: Some("Hermes".into()),
            model: Some("test::specialist".into()),
            skills: vec![],
        }];
        let fallbacks = vec!["test::fallback".into()];
        let validate = |tokens| {
            validate_team_model_routes(
                &options,
                TeamModelRoutes {
                    namespace: "kars-system",
                    total_tokens: Some(tokens),
                    runtime: Some("OpenClaw"),
                    model: Some("test::primary"),
                    model_fallbacks: &fallbacks,
                    roles: &roles,
                    execution_plan: &plan,
                    mcp_servers: &[],
                    memory: None,
                },
            )
        };
        validate(2500).expect("sufficient reviewed lifetime cap");
        let (runtime, model) = routes[target.parse::<usize>().unwrap()];
        let error = validate(1500).expect_err("insufficient cap must reach every route");
        assert!(
            error
                .to_string()
                .contains(&format!("{runtime} · test::{model}")),
            "{error}"
        );
    }

    #[test]
    fn team_mcp_servers_are_deduplicated_and_bounded() {
        assert_eq!(
            normalize_mcp_servers(&[
                " playwright ".into(),
                "playwright".into(),
                "everything".into()
            ])
            .expect("normalize"),
            vec!["playwright", "everything"]
        );
        let too_many = (0..9).map(|i| format!("mcp-{i}")).collect::<Vec<_>>();
        assert!(normalize_mcp_servers(&too_many).is_err());
    }

    #[test]
    fn team_git_write_is_applied_without_mcp_servers() {
        let mut spec = serde_json::json!({"charter": "Deliver a feature"});
        let git_write = crate::kars::task::GitWriteConfig {
            connection_config_map_ref: crate::kars::task::LocalObjectRef {
                name: "kars-github-connection-0123456789abcdef".into(),
            },
            repos: vec!["owner/repo".into()],
        };
        apply_team_git_write(&mut spec, Some(&git_write)).expect("git write applies");
        assert_eq!(
            spec["blueprint"]["gitWrite"]["connectionConfigMapRef"]["name"],
            "kars-github-connection-0123456789abcdef"
        );
        assert_eq!(spec["blueprint"]["gitWrite"]["repos"][0], "owner/repo");
        assert!(spec["blueprint"].get("mcpServers").is_none());
    }
}
