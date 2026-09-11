// kars Bridge BFF — launch options qualification.

use super::{Options, QualifiedResource, QualifiedResourceSelection, QualifiedRoute, RefOption};

fn route_matches(
    route: &QualifiedRoute,
    runtime: &str,
    provider: &str,
    deployment: &str,
    max_parallel: i32,
    total_tokens: Option<i64>,
) -> bool {
    route.runtime.eq_ignore_ascii_case(runtime)
        && route.provider == provider
        && route.deployment == deployment
        && max_parallel <= route.max_parallel
        && route
            .min_total_tokens
            .is_none_or(|minimum| total_tokens.is_none_or(|tokens| tokens >= minimum))
}

fn evidence_complete(route: &QualifiedRoute) -> bool {
    !route.evidence.task.trim().is_empty()
        && !route.evidence.run_id.trim().is_empty()
        && route.evidence.digest.starts_with("sha256:")
}

fn resource_capability(kind: &str) -> Option<&'static str> {
    match kind.to_ascii_lowercase().as_str() {
        "mcp" => Some("mcp"),
        "memory" => Some("memory"),
        _ => None,
    }
}

fn resource_requires_backend(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("memory")
}

fn resource_requires_schema_digest(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("mcp") || kind.eq_ignore_ascii_case("memory")
}

fn resource_requires_version_digest(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("skill")
}

fn resource_matches(
    required: &QualifiedResourceSelection,
    recorded: &QualifiedResource,
    capabilities: &std::collections::BTreeSet<String>,
) -> bool {
    if !recorded.kind.eq_ignore_ascii_case(&required.kind)
        || !recorded.name.eq_ignore_ascii_case(&required.name)
    {
        return false;
    }
    if let Some(capability) = resource_capability(&required.kind)
        && !capabilities.contains(capability)
    {
        return false;
    }
    if resource_requires_backend(&required.kind) && required.backend.is_none() {
        return false;
    }
    if resource_requires_schema_digest(&required.kind) && required.schema_digest.is_none() {
        return false;
    }
    if resource_requires_version_digest(&required.kind) && required.version_digest.is_none() {
        return false;
    }
    if let Some(backend) = required.backend.as_deref()
        && recorded.backend.as_deref() != Some(backend)
    {
        return false;
    }
    if let Some(schema_digest) = required.schema_digest.as_deref()
        && recorded.schema_digest.as_deref() != Some(schema_digest)
    {
        return false;
    }
    if let Some(version_digest) = required.version_digest.as_deref()
        && recorded.version_digest.as_deref() != Some(version_digest)
    {
        return false;
    }
    true
}

fn qualification_records(raw: &str) -> Result<Vec<QualifiedRoute>, String> {
    serde_json::from_str::<Vec<QualifiedRoute>>(raw)
        .map_err(|error| format!("BRIDGE_QUALIFICATION_RECORDS_JSON is invalid: {error}"))
}

fn qualification_records_from_env() -> Result<Vec<QualifiedRoute>, String> {
    let raw = std::env::var("BRIDGE_QUALIFICATION_RECORDS_JSON")
        .map_err(|_| "BRIDGE_QUALIFICATION_RECORDS_JSON is not configured".to_string())?;
    let mut records = qualification_records(&raw)?;
    if let Ok(additional) = std::env::var("BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON")
        && !additional.trim().is_empty()
    {
        records.extend(qualification_records(&additional).map_err(|error| {
            error.replace(
                "BRIDGE_QUALIFICATION_RECORDS_JSON",
                "BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON",
            )
        })?);
    }
    Ok(records)
}

fn qualification_records_raw_from_env() -> Result<String, String> {
    serde_json::to_string(&qualification_records_from_env()?)
        .map_err(|error| format!("qualification records could not be serialized: {error}"))
}

pub(crate) fn route_label(runtime: &str, provider: &str, deployment: &str) -> String {
    format!("{runtime} · {provider}::{deployment}")
}

pub(super) fn resource_qualification_routes_in(
    raw: &str,
    resource: &QualifiedResourceSelection,
) -> Result<Vec<String>, String> {
    let mut labels = qualification_records(raw)?
        .into_iter()
        .filter(evidence_complete)
        .filter_map(|route| {
            let capabilities = route
                .capabilities
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            route
                .resource
                .as_ref()
                .filter(|recorded| resource_matches(resource, recorded, &capabilities))
                .map(|_| route_label(&route.runtime, &route.provider, &route.deployment))
        })
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    Ok(labels)
}

pub(super) fn resource_is_qualified_in(
    raw: &str,
    runtime: &str,
    provider: &str,
    deployment: &str,
    resource: &QualifiedResourceSelection,
) -> Result<bool, String> {
    Ok(qualification_records(raw)?.into_iter().any(|route| {
        if !evidence_complete(&route)
            || !route_matches(&route, runtime, provider, deployment, 1, None)
        {
            return false;
        }
        let capabilities = route
            .capabilities
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        route
            .resource
            .as_ref()
            .is_some_and(|recorded| resource_matches(resource, recorded, &capabilities))
    }))
}

pub(super) fn resource_qualification_routes(
    resource: &QualifiedResourceSelection,
) -> Result<Vec<String>, String> {
    let raw = qualification_records_raw_from_env()?;
    resource_qualification_routes_in(&raw, resource)
}

fn resource_is_qualified(
    runtime: &str,
    provider: &str,
    deployment: &str,
    resource: &QualifiedResourceSelection,
) -> Result<bool, String> {
    let raw = qualification_records_raw_from_env()?;
    resource_is_qualified_in(&raw, runtime, provider, deployment, resource)
}

pub(super) fn mcp_resource_selection(option: &RefOption) -> QualifiedResourceSelection {
    QualifiedResourceSelection {
        kind: "mcp".into(),
        name: option.name.clone(),
        backend: None,
        schema_digest: option.tool_schema_digest.clone(),
        version_digest: None,
    }
}

pub(super) fn memory_resource_selection(option: &RefOption) -> QualifiedResourceSelection {
    QualifiedResourceSelection {
        kind: "memory".into(),
        name: option.name.clone(),
        backend: option.backend.clone(),
        schema_digest: option.compiled_digest.clone(),
        version_digest: None,
    }
}

pub(super) fn skill_resource_selection(option: &RefOption) -> QualifiedResourceSelection {
    QualifiedResourceSelection {
        kind: "skill".into(),
        name: option.name.clone(),
        backend: None,
        schema_digest: None,
        version_digest: option.version_digest.clone(),
    }
}

pub(super) fn channel_resource_selection(channel: &str) -> QualifiedResourceSelection {
    QualifiedResourceSelection {
        kind: "channel".into(),
        name: channel.to_ascii_lowercase(),
        backend: None,
        schema_digest: None,
        version_digest: None,
    }
}

pub(crate) fn mcp_server_qualified_for_route(
    runtime: &str,
    provider: &str,
    deployment: &str,
    option: &RefOption,
) -> Result<bool, String> {
    resource_is_qualified(
        runtime,
        provider,
        deployment,
        &mcp_resource_selection(option),
    )
}

pub(crate) fn memory_binding_qualified_for_route(
    runtime: &str,
    provider: &str,
    deployment: &str,
    option: &RefOption,
) -> Result<bool, String> {
    resource_is_qualified(
        runtime,
        provider,
        deployment,
        &memory_resource_selection(option),
    )
}

pub(crate) fn skill_version_qualified_for_route(
    runtime: &str,
    provider: &str,
    deployment: &str,
    option: &RefOption,
) -> Result<bool, String> {
    resource_is_qualified(
        runtime,
        provider,
        deployment,
        &skill_resource_selection(option),
    )
}

pub(crate) fn channel_adapter_qualified_for_route(
    runtime: &str,
    provider: &str,
    deployment: &str,
    channel: &str,
) -> Result<bool, String> {
    resource_is_qualified(
        runtime,
        provider,
        deployment,
        &channel_resource_selection(channel),
    )
}

pub(crate) fn resource_qualification_summary(options: &Options) -> Result<String, String> {
    let mut lines: Vec<String> = Vec::new();
    for server in &options.mcp_servers {
        let routes = resource_qualification_routes(&mcp_resource_selection(server))?;
        lines.push(format!(
            "  - MCP \"{}\"{}{}{}",
            server.name,
            server
                .tool_schema_digest
                .as_deref()
                .map(|digest| format!(" schema_digest={digest}"))
                .unwrap_or_else(|| " schema_digest=missing".into()),
            if server.discovered_tools.is_empty() {
                String::new()
            } else {
                format!(" tools=[{}]", server.discovered_tools.join(", "))
            },
            if routes.is_empty() {
                " qualified_on=(none)".into()
            } else {
                format!(" qualified_on=[{}]", routes.join("; "))
            }
        ));
    }
    for memory in &options.memories {
        let routes = resource_qualification_routes(&memory_resource_selection(memory))?;
        lines.push(format!(
            "  - MEMORY \"{}\"{}{}{}{}",
            memory.name,
            memory
                .backend
                .as_deref()
                .map(|backend| format!(" backend={backend}"))
                .unwrap_or_else(|| " backend=missing".into()),
            memory
                .compiled_digest
                .as_deref()
                .map(|digest| format!(" compiled_digest={digest}"))
                .unwrap_or_else(|| " compiled_digest=missing".into()),
            memory
                .readiness
                .as_deref()
                .map(|readiness| format!(" readiness={readiness}"))
                .unwrap_or_default(),
            if routes.is_empty() {
                " qualified_on=(none)".into()
            } else {
                format!(" qualified_on=[{}]", routes.join("; "))
            }
        ));
    }
    for skill in &options.skills {
        let routes = resource_qualification_routes(&skill_resource_selection(skill))?;
        lines.push(format!(
            "  - SKILL \"{}\"{}{}{}{}",
            skill.name,
            skill
                .version
                .as_deref()
                .map(|version| format!(" version={version}"))
                .unwrap_or_default(),
            skill
                .version_digest
                .as_deref()
                .map(|digest| format!(" version_digest={digest}"))
                .unwrap_or_else(|| " version_digest=missing".into()),
            skill
                .recipe
                .as_deref()
                .map(|recipe| format!(" recipe={}", recipe.chars().take(180).collect::<String>()))
                .unwrap_or_default(),
            if routes.is_empty() {
                " qualified_on=(none)".into()
            } else {
                format!(" qualified_on=[{}]", routes.join("; "))
            }
        ));
    }
    if lines.is_empty() {
        Ok("  (no MCP, memory, or approved-skill resources are available)".into())
    } else {
        Ok(lines.join("\n"))
    }
}

pub(crate) fn qualification_constraints_summary() -> Result<String, String> {
    let routes = qualification_records_from_env()?;
    if routes.is_empty() {
        return Ok("  (no qualified execution routes are retained)".into());
    }
    Ok(routes
        .into_iter()
        .map(|route| {
            format!(
                "  - {} · capabilities=[{}] · max_parallel={} · min_total_tokens={}{}",
                route_label(&route.runtime, &route.provider, &route.deployment),
                route.capabilities.join(","),
                route.max_parallel,
                route
                    .min_total_tokens
                    .map(|tokens| tokens.to_string())
                    .unwrap_or_else(|| "none".into()),
                route
                    .resource
                    .map(|resource| {
                        format!(
                            " · resource={}::{}{}{}{}",
                            resource.kind,
                            resource.name,
                            resource
                                .backend
                                .as_deref()
                                .map(|backend| format!(" backend={backend}"))
                                .unwrap_or_default(),
                            resource
                                .schema_digest
                                .as_deref()
                                .map(|digest| format!(" schema_digest={digest}"))
                                .unwrap_or_default(),
                            resource
                                .version_digest
                                .as_deref()
                                .map(|digest| format!(" version_digest={digest}"))
                                .unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub(crate) fn route_minimum_tokens(
    runtime: &str,
    provider: &str,
    deployment: &str,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
) -> Result<Option<i64>, String> {
    let routes = qualification_records_from_env()?;
    let matching = routes.into_iter().filter(|route| {
        let capabilities = route
            .capabilities
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        route_matches(route, runtime, provider, deployment, max_parallel, None)
            && required_capabilities.is_subset(&capabilities)
    });
    let mut minimum: Option<i64> = None;
    for route in matching {
        let Some(tokens) = route.min_total_tokens else {
            return Ok(None);
        };
        minimum = Some(minimum.map_or(tokens, |current| current.min(tokens)));
    }
    Ok(minimum)
}

pub(crate) fn route_qualification(
    runtime: &str,
    provider: &str,
    deployment: &str,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
    total_tokens: Option<i64>,
) -> Result<bool, String> {
    let raw = qualification_records_raw_from_env()?;
    route_is_qualified_in(
        &raw,
        runtime,
        provider,
        deployment,
        required_capabilities,
        max_parallel,
        total_tokens,
    )
}

pub(crate) fn route_qualification_gap(
    runtime: &str,
    provider: &str,
    deployment: &str,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
    total_tokens: Option<i64>,
) -> Result<std::collections::BTreeSet<String>, String> {
    let raw = qualification_records_raw_from_env()?;
    route_qualification_gap_in(
        &raw,
        runtime,
        provider,
        deployment,
        required_capabilities,
        max_parallel,
        total_tokens,
    )
}

pub(super) fn route_qualification_gap_in(
    raw: &str,
    runtime: &str,
    provider: &str,
    deployment: &str,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
    total_tokens: Option<i64>,
) -> Result<std::collections::BTreeSet<String>, String> {
    let routes = qualification_records(raw)?;
    let mut gaps = routes
        .into_iter()
        .filter_map(|route| {
            if !evidence_complete(&route)
                || !route_matches(
                    &route,
                    runtime,
                    provider,
                    deployment,
                    max_parallel,
                    total_tokens,
                )
            {
                return None;
            }
            let capabilities = route
                .capabilities
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            Some(
                required_capabilities
                    .difference(&capabilities)
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>(),
            )
        })
        .collect::<Vec<_>>();
    gaps.sort_by(|left, right| {
        left.len()
            .cmp(&right.len())
            .then_with(|| left.iter().cmp(right.iter()))
    });
    Ok(gaps
        .into_iter()
        .next()
        .unwrap_or_else(|| required_capabilities.clone()))
}

pub(super) fn route_is_qualified_in(
    raw: &str,
    runtime: &str,
    provider: &str,
    deployment: &str,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
    total_tokens: Option<i64>,
) -> Result<bool, String> {
    let routes = qualification_records(raw)?;
    Ok(routes.into_iter().any(|route| {
        let capabilities = route
            .capabilities
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        route_matches(
            &route,
            runtime,
            provider,
            deployment,
            max_parallel,
            total_tokens,
        ) && evidence_complete(&route)
            && required_capabilities.is_subset(&capabilities)
    }))
}
