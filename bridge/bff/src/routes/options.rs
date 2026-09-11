// kars Bridge BFF — launch-package options.
//
// The editable launch package (§20 of the design note) must be composed from
// **real cluster facts**, never a hard-coded menu. This endpoint enumerates the
// composable substrate a task-giver can pick from: the models this cluster is
// configured to serve, the agent runtimes the controller can materialize, the
// isolation levels, and the existing `ToolPolicy` / `McpServer` / `KarsMemory`
// objects the blueprint composes by reference. Absent CRDs surface as empty
// lists (the web layer renders the honesty grammar), never as errors.

use axum::Json;
use axum::extract::State;
use kube::core::DynamicObject;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}
fn upstream(e: kube::Error) -> AppError {
    AppError::Upstream(e.to_string())
}
fn name_of(o: &DynamicObject) -> String {
    o.metadata.name.clone().unwrap_or_default()
}
fn ns_of(o: &DynamicObject) -> String {
    o.metadata.namespace.clone().unwrap_or_default()
}

/// A model the agent can reason with — a provider tag + deployment name, the
/// exact pair that lands on `InferencePolicy.spec.modelPreference.primary`.
#[derive(Debug, Serialize)]
pub struct ModelOption {
    pub provider: String,
    pub deployment: String,
    /// True for the controller's configured default (used when a package leaves
    /// the model unset) so the UI can pre-select and label it.
    pub is_default: bool,
    /// Short human detail (e.g. "Anthropic · 1.0M ctx · powerful") when the
    /// provider exposes it — GitHub Copilot's live catalog does. `None` for
    /// providers with no metadata. Shown in the Model catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// An agent runtime/harness the controller can materialize. `wired` reflects
/// whether the substrate has a real, end-to-end adapter today; `status` adds
/// the honest validation tier (`validated` = exercised end-to-end on kars
/// clusters; `available` = adapter wired, less battle-tested; `unavailable` =
/// declared but no adapter yet). `note` is a one-line human explanation.
#[derive(Debug, Serialize)]
pub struct RuntimeOption {
    pub kind: String,
    pub label: String,
    pub wired: bool,
    pub status: String,
    pub note: String,
}

/// The inference provider this cluster inherits from its setup (GitHub Copilot,
/// GitHub Models, or Azure AI Foundry) — surfaced so the UI states it as fact
/// rather than pretending the Bridge picks it.
#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub note: String,
}

/// A composable CRD the blueprint references by name (tool policy, MCP server,
/// shared memory), projected to just what the picker needs.
#[derive(Debug, Clone, Serialize)]
pub struct RefOption {
    pub name: String,
    pub namespace: String,
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_schema_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualified_routes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct IsolationOption {
    pub value: String,
    pub label: String,
    pub note: String,
}

/// The full composable palette for the launch package.
#[derive(Debug, Serialize)]
pub struct Options {
    pub models: Vec<ModelOption>,
    /// The controller default deployment, surfaced explicitly so the UI can say
    /// "leave unset → uses <default>" honestly.
    pub default_model: Option<String>,
    /// The inherited inference provider for this cluster (None when unreadable).
    pub provider: Option<ProviderInfo>,
    pub runtimes: Vec<RuntimeOption>,
    pub isolation: Vec<IsolationOption>,
    pub tool_policies: Vec<RefOption>,
    pub mcp_servers: Vec<RefOption>,
    /// Operator-curated MCP profiles — named vetted bundles of McpServers users
    /// can add as a set (the operator vets the grouping once; users pick it).
    pub mcp_profiles: Vec<McpProfileOption>,
    pub memories: Vec<RefOption>,
    /// Attested capability bundles (KarsSkill) a team role can acquire, so the
    /// team composer can offer a real skill picker instead of teaching a concept
    /// with no control behind it.
    pub skills: Vec<RefOption>,
}

/// A named, operator-vetted bundle of McpServers offered to users as a set.
#[derive(Debug, Serialize, Clone)]
pub struct McpProfileOption {
    pub name: String,
    pub summary: Option<String>,
    pub servers: Vec<String>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct QualifiedRoute {
    runtime: String,
    provider: String,
    deployment: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default = "default_qualified_parallelism")]
    max_parallel: i32,
    #[serde(default)]
    min_total_tokens: Option<i64>,
    #[serde(default)]
    resource: Option<QualifiedResource>,
    evidence: QualificationEvidence,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct QualifiedResource {
    kind: String,
    name: String,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    schema_digest: Option<String>,
    #[serde(default)]
    version_digest: Option<String>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct QualificationEvidence {
    task: String,
    run_id: String,
    digest: String,
}

#[derive(Debug, Clone)]
struct QualifiedResourceSelection {
    kind: String,
    name: String,
    backend: Option<String>,
    schema_digest: Option<String>,
    version_digest: Option<String>,
}

fn default_qualified_parallelism() -> i32 {
    1
}

fn string_at(value: &serde_json::Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(|entry| entry.as_str())
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
    })
}

fn bool_at(value: &serde_json::Value, pointers: &[&str]) -> Option<bool> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_bool))
}

fn string_array_at(value: &serde_json::Value, pointers: &[&str]) -> Vec<String> {
    pointers
        .iter()
        .find_map(|pointer| {
            value.pointer(pointer).and_then(|entry| {
                entry.as_array().map(|values| {
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|entry| !entry.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
            })
        })
        .unwrap_or_default()
}

fn status_observes_current_generation(resource: &DynamicObject) -> bool {
    resource
        .data
        .pointer("/status/observedGeneration")
        .and_then(serde_json::Value::as_i64)
        == resource.metadata.generation
}

fn readiness_summary(resource: &DynamicObject) -> Option<String> {
    let phase = string_at(&resource.data, &["/status/phase"]);
    let ready = resource
        .data
        .pointer("/status/conditions")
        .and_then(serde_json::Value::as_array)
        .and_then(|conditions| {
            conditions.iter().find_map(|condition| {
                (condition.get("type").and_then(serde_json::Value::as_str) == Some("Ready")).then(
                    || {
                        let status = condition
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                        let message = condition
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|message| !message.is_empty())
                            .map(str::to_string);
                        (status, message)
                    },
                )
            })
        });
    let observed = status_observes_current_generation(resource);
    match (phase, ready) {
        (Some(phase), Some((Some(status), Some(message)))) => Some(format!(
            "{}{}{}",
            phase,
            if observed { "" } else { " (stale generation)" },
            if status == "True" {
                String::new()
            } else {
                format!(" — {message}")
            }
        )),
        (Some(phase), Some((Some(status), None))) => Some(format!(
            "{}{}{}",
            phase,
            if observed { "" } else { " (stale generation)" },
            if status == "True" {
                String::new()
            } else {
                format!(" — Ready={status}")
            }
        )),
        (Some(phase), _) => Some(format!(
            "{}{}",
            phase,
            if observed { "" } else { " (stale generation)" }
        )),
        (None, Some((Some(status), Some(message)))) => Some(format!("Ready={status} — {message}")),
        (None, Some((Some(status), None))) => Some(format!("Ready={status}")),
        _ => None,
    }
}

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

fn resource_qualification_routes_in(
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

fn resource_is_qualified_in(
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

fn resource_qualification_routes(
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

fn mcp_resource_selection(option: &RefOption) -> QualifiedResourceSelection {
    QualifiedResourceSelection {
        kind: "mcp".into(),
        name: option.name.clone(),
        backend: None,
        schema_digest: option.tool_schema_digest.clone(),
        version_digest: None,
    }
}

fn memory_resource_selection(option: &RefOption) -> QualifiedResourceSelection {
    QualifiedResourceSelection {
        kind: "memory".into(),
        name: option.name.clone(),
        backend: option.backend.clone(),
        schema_digest: option.compiled_digest.clone(),
        version_digest: None,
    }
}

fn skill_resource_selection(option: &RefOption) -> QualifiedResourceSelection {
    QualifiedResourceSelection {
        kind: "skill".into(),
        name: option.name.clone(),
        backend: None,
        schema_digest: None,
        version_digest: option.version_digest.clone(),
    }
}

fn channel_resource_selection(channel: &str) -> QualifiedResourceSelection {
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

pub(crate) fn mcp_server_option(resource: &DynamicObject) -> RefOption {
    let mode = string_at(&resource.data, &["/status/mode"]).or_else(|| {
        bool_at(&resource.data, &["/spec/managed"])
            .map(|managed| if managed { "Managed" } else { "External" }.to_string())
    });
    let discovered_tools = string_array_at(&resource.data, &["/status/discoveredTools"]);
    let tool_schema_digest =
        string_at(&resource.data, &["/status/toolSchemaDigest"]).or_else(|| {
            let signature = serde_json::json!({
                "mode": mode.clone(),
                "endpoint": string_at(
                    &resource.data,
                    &["/status/endpoint", "/spec/url", "/spec/endpoint"],
                ),
                "allowed_tools": string_array_at(&resource.data, &["/spec/allowedTools"]),
                "discovered_tools": discovered_tools.clone(),
            });
            serde_json::to_vec(&signature)
                .ok()
                .map(|bytes| format!("sha256:{:x}", Sha256::digest(bytes)))
        });
    let mut option = RefOption {
        name: name_of(resource),
        namespace: ns_of(resource),
        summary: string_at(
            &resource.data,
            &["/status/endpoint", "/spec/url", "/spec/endpoint"],
        ),
        mode,
        discovered_tools,
        tool_schema_digest,
        compiled_digest: None,
        backend: None,
        readiness: readiness_summary(resource),
        version: None,
        recipe: None,
        version_digest: None,
        qualified_routes: Vec::new(),
    };
    option.qualified_routes =
        resource_qualification_routes(&mcp_resource_selection(&option)).unwrap_or_default();
    option
}

pub(crate) fn memory_option(resource: &DynamicObject) -> RefOption {
    let mut option = RefOption {
        name: name_of(resource),
        namespace: ns_of(resource),
        summary: string_at(
            &resource.data,
            &["/spec/displayName", "/spec/storeName", "/status/storeName"],
        ),
        mode: None,
        discovered_tools: Vec::new(),
        tool_schema_digest: None,
        compiled_digest: string_at(
            &resource.data,
            &[
                "/status/compiledDigest",
                "/status/compiled/digest",
                "/status/resolvedDigest",
                "/status/specDigest",
            ],
        ),
        backend: string_at(
            &resource.data,
            &[
                "/status/backend",
                "/spec/backend",
                "/status/binding/backend",
                "/spec/binding/backend",
                "/status/provider",
                "/spec/provider",
            ],
        )
        .or_else(|| Some("foundry".into())),
        readiness: readiness_summary(resource),
        version: None,
        recipe: None,
        version_digest: None,
        qualified_routes: Vec::new(),
    };
    option.qualified_routes =
        resource_qualification_routes(&memory_resource_selection(&option)).unwrap_or_default();
    option
}

pub(crate) fn skill_option(resource: &DynamicObject) -> RefOption {
    let mut option = RefOption {
        name: name_of(resource),
        namespace: ns_of(resource),
        summary: string_at(&resource.data, &["/spec/summary"]),
        mode: None,
        discovered_tools: Vec::new(),
        tool_schema_digest: None,
        compiled_digest: None,
        backend: None,
        readiness: string_at(&resource.data, &["/status/phase"]),
        version: string_at(&resource.data, &["/spec/version"]),
        recipe: string_at(&resource.data, &["/spec/recipe"]),
        version_digest: string_at(&resource.data, &["/status/versionDigest"]),
        qualified_routes: Vec::new(),
    };
    option.qualified_routes =
        resource_qualification_routes(&skill_resource_selection(&option)).unwrap_or_default();
    option
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

fn route_qualification_gap_in(
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

fn route_is_qualified_in(
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

/// Infer a provider tag from a deployment string when none is recorded — a
/// `github-models`-style `openai/<model>` carries its vendor in the prefix. A
/// bare deployment name (the Copilot/Foundry form) carries no vendor, so we tag
/// it with the cluster's ACTUAL inherited provider (`github-copilot`,
/// `github-models`, or a Foundry/Azure provider) rather than guessing
/// `azure-openai`. This is what makes a composed mission/team stamp the real
/// provider the router serves — never a misleading default.
pub(crate) fn provider_for(
    deployment: &str,
    recorded: Option<&str>,
    cluster_default: Option<&str>,
) -> String {
    if let Some(p) = recorded {
        return p.to_string();
    }
    match deployment.split_once('/') {
        Some(("openai", _)) => "github-models".to_string(),
        Some((vendor, _)) => vendor.to_string(),
        None => cluster_default.unwrap_or("azure-openai").to_string(),
    }
}

/// `GET /api/options` — the composable launch-package palette, from live state.
pub async fn get_options(State(state): State<AppState>) -> AppResult<Json<Options>> {
    let cluster = require_cluster(&state)?;
    Ok(Json(build_options(cluster).await?))
}

/// Build the real composable building blocks from live cluster state. Shared by
/// the `/api/options` route and the orchestrator (`/compose`), so the LLM only
/// ever proposes models, tool policies, MCP servers, isolation levels, and
/// memory stores that genuinely exist on this cluster.
pub async fn build_options(cluster: &crate::kars::cluster::Cluster) -> AppResult<Options> {
    // Models: the controller-configured default + catalog, deduped against any
    // distinct models already pinned on existing InferencePolicies (real,
    // in-use facts). Order: default first, then catalog, then discovered.
    let (default_model, catalog) = cluster.controller_models().await;
    // The cluster's inherited inference provider — the authoritative tag for any
    // catalog model that doesn't carry its own vendor prefix. Fetched up front
    // so every offered model is stamped with the provider the router actually
    // serves (e.g. `github-copilot`), not a neutral guess.
    let provider = cluster
        .controller_provider()
        .await
        .map(|(id, label, note)| ProviderInfo { id, label, note });
    let cluster_provider_id: Option<String> = provider.as_ref().map(|p| p.id.clone());
    // The operator's declared, served set — the only models we trust enough to
    // offer. Discovered InferencePolicy models are surfaced ONLY if they are
    // also in this set, so a stale policy pinning an unserved model (e.g. a
    // decommissioned deployment) can't leak a broken option into the picker.
    let catalog_set: std::collections::BTreeSet<String> = catalog
        .iter()
        .cloned()
        .chain(default_model.clone())
        .collect();
    let mut models: Vec<ModelOption> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Detail blurbs (deployment → "vendor · ctx · category") for models whose
    // provider exposes them; applied in a post-pass so push_model stays simple.
    let mut details: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let cpid = cluster_provider_id.clone();
    let mut push_model = |deployment: String, provider: Option<&str>, is_default: bool| {
        if deployment.is_empty() || !seen.insert(deployment.clone()) {
            return;
        }
        let provider = provider_for(&deployment, provider, cpid.as_deref());
        models.push(ModelOption {
            provider,
            deployment,
            is_default,
            detail: None,
        });
    };
    if let Some(def) = default_model.clone() {
        push_model(def, None, true);
    }
    for dep in catalog {
        push_model(dep, None, false);
    }
    // Live GitHub Copilot catalog — when Copilot is the cluster's default
    // provider, surface the seat's ACTUAL served models (gpt-5.6, opus-4.8,
    // gemini-3.1-pro, …) instead of only the static KARS_MODEL_CATALOG. This
    // is the SAME set the wizard's Copilot discovery shows, so the Model
    // catalogue, the orchestrator's menu, and the manual-override picker all
    // reflect what Copilot really serves — refreshing itself as GitHub adds
    // models. Cached (5-min TTL); a transient Copilot failure leaves the
    // static catalog intact (best-effort, never blanks the list).
    if cluster_provider_id.as_deref() == Some("github-copilot")
        && let Some(token) = cluster.controller_copilot_token().await
    {
        for (dep, _recommended, detail) in
            crate::routes::operator::copilot_catalog_cached(&token).await
        {
            if let Some(d) = detail {
                details.entry(dep.clone()).or_insert(d);
            }
            push_model(dep, Some("github-copilot"), false);
        }
    }
    for ip in cluster
        .list_kind_all("InferencePolicy")
        .await
        .map_err(upstream)?
    {
        let primary = ip
            .data
            .get("spec")
            .and_then(|s| s.get("modelPreference"))
            .and_then(|m| m.get("primary"));
        if let Some(p) = primary {
            let dep = p.get("deployment").and_then(|d| d.as_str()).unwrap_or("");
            // Only surface a discovered model if the operator's catalog declares
            // it — never an arbitrary (possibly unserved) pinned deployment.
            if !catalog_set.contains(dep) {
                continue;
            }
            let prov = p.get("provider").and_then(|d| d.as_str());
            push_model(dep.to_string(), prov, false);
        }
    }

    // Additional providers (§ inference-provider-wizard): every model the
    // operator explicitly declared when connecting a provider beyond the
    // single default — e.g. a Foundry deployment or a GitHub Models id —
    // tagged with ITS OWN provider, not the cluster default. These are
    // operator-declared (trusted) the same way the default catalog is, so
    // they don't need the catalog_set gate above. This is what lets
    // InferencePolicy's model picker offer "gpt-4.1 via Foundry" alongside
    // "opus-4.8 via GitHub Copilot" with no change to that editor — it
    // already keys options by `provider::deployment`.
    let provider_keys = cluster
        .read_secret_all("kars-system", "kars-inference-providers")
        .await
        .map_err(upstream)?;
    let mut declared_models: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (key, value) in &provider_keys {
        let Some(tag_part) = key
            .strip_prefix("KARS_PROVIDER_")
            .and_then(|r| r.strip_suffix("_MODELS"))
        else {
            continue;
        };
        let tag = tag_part.to_ascii_lowercase().replace('_', "-");
        declared_models.insert(
            tag,
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        );
    }
    for (tag, deployments) in declared_models {
        for dep in deployments {
            push_model(dep, Some(tag.as_str()), false);
        }
    }

    // Apply detail blurbs (only some providers expose them).
    if !details.is_empty() {
        for m in models.iter_mut() {
            if m.detail.is_none()
                && let Some(d) = details.get(&m.deployment)
            {
                m.detail = Some(d.clone());
            }
        }
    }

    // Runtimes: the controller wires adapters for several harnesses, but a
    // harness is only RUNNABLE here when its container image is configured and
    // the controller has registry credentials that cover the image.
    // `wired` now means "can start a pod here" — so the UI never lets a user (or
    // the orchestrator) pick a harness that would ErrImagePull. `status`:
    //   ready       = runnable on this cluster
    //   needs_image = adapter exists, but no image configured here
    //   unavailable = no adapter at all (SemanticKernel)
    let runnable = cluster.runnable_runtimes().await;
    let mk = |kind: &str, label: &str, ready_note: &str| {
        let is_runnable = runnable.contains(kind);
        RuntimeOption {
            kind: kind.into(),
            label: label.into(),
            wired: is_runnable,
            status: if is_runnable {
                "ready".into()
            } else {
                "needs_image".into()
            },
            note: if is_runnable {
                ready_note.into()
            } else {
                "Adapter wired, but its runtime image or registry pull credential is unavailable on this cluster.".into()
            },
        }
    };
    let runtimes = vec![
        mk(
            "OpenClaw",
            "OpenClaw",
            "Autonomous — the default kars harness, exercised end-to-end (full mesh + spawn). Runs missions and standing teams.",
        ),
        mk(
            "Hermes",
            "Hermes (Nous Research)",
            "Autonomous — executes a delivered objective in-process and replies (plugins, 20+ channels, native MCP). Verified end-to-end.",
        ),
        mk(
            "Anthropic",
            "Anthropic Claude Agent SDK",
            "Adapter only (pins the governed router) — you supply the agent logic. Not a turnkey autonomous harness; auto-corrected to OpenClaw for missions/teams.",
        ),
        mk(
            "OpenAIAgents",
            "OpenAI Agents SDK",
            "Adapter only (routes through the inference sidecar) — you supply the agent logic. Not turnkey autonomous; auto-corrected to OpenClaw for missions/teams.",
        ),
        mk(
            "MicrosoftAgentFramework",
            "Microsoft Agent Framework",
            "Adapter only (MAF Python, first-party AGT integration) — you supply the agent logic. Not turnkey autonomous; auto-corrected to OpenClaw.",
        ),
        mk(
            "LangGraph",
            "LangGraph",
            "Adapter only (Python + TypeScript, pins the router) — you supply the graph. Not turnkey autonomous; auto-corrected to OpenClaw.",
        ),
        mk(
            "PydanticAi",
            "Pydantic-AI",
            "Adapter only (provider-agnostic, pins the router at bootstrap) — you supply the agent. Not turnkey autonomous; auto-corrected to OpenClaw.",
        ),
        mk(
            "BYO",
            "Bring-your-own runtime",
            "Autonomous by contract — any image honoring the BYO contract (UID 1000, router at 127.0.0.1:8443, consumes the objective + delivers).",
        ),
        RuntimeOption {
            kind: "SemanticKernel".into(),
            label: "Semantic Kernel".into(),
            wired: false,
            status: "unavailable".into(),
            note: "Declared on the substrate but no adapter is wired yet.".into(),
        },
    ];

    let isolation = vec![
        IsolationOption {
            value: "standard".into(),
            label: "Standard".into(),
            note: "Namespaced sandbox, default-deny egress, seccomp.".into(),
        },
        IsolationOption {
            value: "enhanced".into(),
            label: "Enhanced".into(),
            note: "Hardened profile for sensitive work.".into(),
        },
        IsolationOption {
            value: "confidential".into(),
            label: "Confidential".into(),
            note: "Confidential compute (CVM) where the node pool supports it.".into(),
        },
    ];

    let tool_policies = cluster
        .list_kind_all("ToolPolicy")
        .await
        .map_err(upstream)?
        .iter()
        .map(|o| RefOption {
            name: name_of(o),
            namespace: ns_of(o),
            summary: o
                .data
                .get("spec")
                .and_then(|s| s.get("appliesTo"))
                .and_then(|a| a.get("tool"))
                .and_then(|t| t.as_str())
                .map(|t| format!("tools {t}")),
            mode: None,
            discovered_tools: Vec::new(),
            tool_schema_digest: None,
            compiled_digest: None,
            backend: None,
            readiness: None,
            version: None,
            recipe: None,
            version_digest: None,
            qualified_routes: Vec::new(),
        })
        .collect();

    let mut mcp_servers: Vec<RefOption> = cluster
        .list_kind_all("McpServer")
        .await
        .map_err(upstream)?
        .iter()
        .map(mcp_server_option)
        .collect();
    // Collapse duplicate registrations in the SAME namespace that point at the
    // same endpoint URL. Identical endpoints in different namespaces are
    // distinct workspace grants and must survive namespace filtering.
    // the audit saw two identical Playwright servers offered side by side, which
    // is confusing and invites a redundant grant. Keep the first per URL; servers
    // with no URL are always kept (nothing to compare on).
    {
        let mut seen_urls: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        mcp_servers.retain(|s| match s.summary.as_deref() {
            Some(url) if !url.is_empty() => {
                seen_urls.insert((s.namespace.clone(), url.to_string()))
            }
            _ => true,
        });
    }

    let memories = cluster
        .list_kind_all("KarsMemory")
        .await
        .map_err(upstream)?
        .iter()
        .map(memory_option)
        .collect();

    let skills = cluster
        .list_kind_all("KarsSkill")
        .await
        .map_err(upstream)?
        .iter()
        // Operator trust gate: users may only assign skills an operator has
        // approved AND locked to the skill's current version digest. A pending,
        // never-approved, or changed-since-approval skill is withheld until
        // (re)approved — the same rule the operator console enforces.
        .filter(|o| {
            let review = o
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/skill-review"))
                .map(String::as_str);
            let locked = o
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/skill-locked-digest"));
            let digest = o
                .data
                .get("status")
                .and_then(|s| s.get("versionDigest"))
                .and_then(|d| d.as_str());
            review == Some("approved") && locked.is_some() && locked.map(String::as_str) == digest
        })
        .map(skill_option)
        .collect();

    // Operator-curated MCP profiles (vetted bundles). Only surface servers that
    // still exist on the cluster, so a deleted McpServer can't linger in a bundle.
    let known_servers: std::collections::BTreeSet<String> =
        mcp_servers.iter().map(|r| r.name.clone()).collect();
    let mcp_profiles: Vec<McpProfileOption> = {
        let raw = cluster.read_mcp_profiles().await;
        let parsed: Vec<crate::routes::operator::McpProfileDto> =
            serde_json::from_str(&raw).unwrap_or_default();
        parsed
            .into_iter()
            .map(|p| McpProfileOption {
                name: p.name,
                summary: p.summary,
                servers: p
                    .servers
                    .into_iter()
                    .filter(|s| known_servers.contains(s))
                    .collect(),
            })
            .collect()
    };

    Ok(Options {
        models,
        default_model,
        provider,
        runtimes,
        isolation,
        tool_policies,
        mcp_servers,
        mcp_profiles,
        memories,
        skills,
    })
}

#[cfg(test)]
mod qualification_tests {
    use super::{
        QualifiedResourceSelection, channel_resource_selection, mcp_resource_selection,
        resource_is_qualified_in, resource_qualification_routes_in, route_is_qualified_in,
        route_qualification_gap_in,
    };
    use std::collections::BTreeSet;

    #[test]
    fn qualification_requires_route_capabilities_constraints_and_evidence() {
        let routes = r#"[
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["delegation","filesystem-read","shell","network","artifacts","telemetry"],
            "max_parallel":1,
            "min_total_tokens":128144,
            "evidence":{
              "task":"openclaw-proof",
              "run_id":"run-1",
              "digest":"sha256:abc"
            }

          }
        ]"#;
        let required = ["delegation", "shell", "telemetry"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        assert!(
            route_is_qualified_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &required,
                1,
                Some(128_144)
            )
            .expect("valid routes")
        );
        assert!(
            route_is_qualified_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &required,
                1,
                None
            )
            .expect("an uncapped route is not below the retained minimum")
        );
        let unsupported = ["delegation", "memory"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        assert!(
            !route_is_qualified_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &unsupported,
                1,
                Some(128_144)
            )
            .expect("valid routes")
        );
        assert!(
            !route_is_qualified_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &required,
                2,
                Some(128_144)
            )
            .expect("valid routes")
        );
        assert!(
            !route_is_qualified_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &required,
                1,
                Some(100_000)
            )
            .expect("valid routes")
        );
        assert!(route_is_qualified_in("{", "OpenClaw", "x", "y", &required, 1, None).is_err());
    }

    #[test]
    fn qualification_gap_reports_only_capabilities_missing_from_closest_record() {
        let routes = r#"[
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["delegation","web-search","network","artifacts","telemetry"],
            "max_parallel":1,
            "min_total_tokens":300000,
            "evidence":{"task":"research-proof","run_id":"run-1","digest":"sha256:abc"}
          }
        ]"#;
        let required = [
            "artifacts",
            "delegation",
            "mcp",
            "network",
            "telemetry",
            "web-search",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
        assert_eq!(
            route_qualification_gap_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &required,
                1,
                Some(300000),
            )
            .expect("gap"),
            ["mcp".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn resource_qualification_requires_matching_current_digest_and_ignores_generic_routes() {
        let routes = r#"[
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["mcp","network","telemetry"],
            "max_parallel":1,
            "evidence":{"task":"generic-proof","run_id":"run-1","digest":"sha256:generic"}
          },
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["mcp","network","telemetry"],
            "max_parallel":1,
            "resource":{"kind":"mcp","name":"playwright","schema_digest":"sha256:tools-v1"},
            "evidence":{"task":"mcp-proof","run_id":"run-2","digest":"sha256:mcp"}
          }
        ]"#;
        let selection = QualifiedResourceSelection {
            kind: "mcp".into(),
            name: "playwright".into(),
            backend: None,
            schema_digest: Some("sha256:tools-v1".into()),
            version_digest: None,
        };
        assert!(
            resource_is_qualified_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &selection,
            )
            .expect("resource qualification")
        );
        let mismatched = QualifiedResourceSelection {
            schema_digest: Some("sha256:tools-v2".into()),
            ..selection.clone()
        };
        assert!(
            !resource_is_qualified_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &mismatched,
            )
            .expect("resource qualification")
        );
        let missing_digest = QualifiedResourceSelection {
            schema_digest: None,
            ..selection
        };
        assert!(
            !resource_is_qualified_in(
                routes,
                "OpenClaw",
                "local-inference",
                "gpt-oss-120b",
                &missing_digest,
            )
            .expect("resource qualification")
        );
    }

    #[test]
    fn resource_route_summary_lists_only_matching_resource_records() {
        let routes = r#"[
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["skill","telemetry"],
            "max_parallel":1,
            "resource":{"kind":"channel","name":"telegram"},
            "evidence":{"task":"channel-proof","run_id":"run-1","digest":"sha256:chan"}
          },
          {
            "runtime":"Hermes",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["mcp","telemetry"],
            "max_parallel":1,
            "resource":{"kind":"mcp","name":"playwright","schema_digest":"sha256:tools-v1"},
            "evidence":{"task":"mcp-proof","run_id":"run-2","digest":"sha256:mcp"}
          }
        ]"#;
        let selection = channel_resource_selection("telegram");
        assert_eq!(
            resource_qualification_routes_in(routes, &selection).expect("summary"),
            vec!["OpenClaw · local-inference::gpt-oss-120b".to_string()]
        );
        let mcp = mcp_resource_selection(&super::RefOption {
            name: "playwright".into(),
            namespace: "demo".into(),
            summary: None,
            mode: None,
            discovered_tools: Vec::new(),
            tool_schema_digest: Some("sha256:tools-v1".into()),
            compiled_digest: None,
            backend: None,
            readiness: None,
            version: None,
            recipe: None,
            version_digest: None,
            qualified_routes: Vec::new(),
        });
        assert_eq!(
            resource_qualification_routes_in(routes, &mcp).expect("summary"),
            vec!["Hermes · local-inference::gpt-oss-120b".to_string()]
        );
    }
}
