// kars Bridge BFF — launch-package options.
//
// The editable launch package (§20 of the design note) must be composed from
// **real cluster facts**, never a hard-coded menu. This endpoint enumerates the
// composable substrate a task-giver can pick from: the models this cluster is
// configured to serve, the agent runtimes the controller can materialize, the
// isolation levels, and the existing `ToolPolicy` / `McpServer` / `KarsMemory`
// objects the blueprint composes by reference. Absent CRDs surface as empty
// lists (the web layer renders the honesty grammar), never as errors.

use kube::core::DynamicObject;
use serde::Serialize;

mod palette;
mod projections;
mod qualification;

pub(crate) use palette::provider_for;
pub use palette::{build_options, get_options};
pub(crate) use projections::{mcp_server_option, memory_option, skill_option};
pub(crate) use qualification::{
    channel_adapter_qualified_for_route, mcp_server_qualified_for_route,
    memory_binding_qualified_for_route, qualification_constraints_summary,
    resource_qualification_summary, route_label, route_minimum_tokens, route_qualification,
    route_qualification_gap, skill_version_qualified_for_route,
};

#[cfg(test)]
use qualification::{
    channel_resource_selection, mcp_resource_selection, resource_is_qualified_in,
    resource_qualification_routes_in, route_is_qualified_in, route_qualification_gap_in,
};

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

#[cfg(test)]
mod qualification_tests;
