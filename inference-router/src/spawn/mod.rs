// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Sandbox spawn — create/list/delete KarsSandbox sub-agents via K8s API.
//!
//! The agent inside a sandbox has no kubectl or CLI access. This module exposes
//! HTTP endpoints that the plugin's `/kars-spawn` slash command calls to
//! manage sub-agent sandboxes through the pod's ServiceAccount.

use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::{
    Api, Client, ResourceExt,
    api::{DynamicObject, ListParams, Patch, PatchParams, PostParams},
    discovery::ApiResource,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

mod docker;

#[cfg(test)]
mod dev_profile_test;
#[cfg(test)]
mod mcp_inherit_test;
pub use docker::{delete_sandbox_docker, list_sandboxes_docker};

fn default_true() -> bool {
    true
}

fn kars_sandbox_api_resource() -> ApiResource {
    ApiResource {
        group: "kars.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsSandbox".into(),
        plural: "karssandboxes".into(),
    }
}

fn kars_api_resource(kind: &str, plural: &str) -> ApiResource {
    ApiResource {
        group: "kars.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: kind.into(),
        plural: plural.into(),
    }
}

async fn is_verified_team_roster_spawn(
    client: &Client,
    namespace: &str,
    parent: &DynamicObject,
    role: Option<&str>,
) -> Option<bool> {
    if parent
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("kars.azure.com/team-role"))
        .map(String::as_str)
        == Some("member")
    {
        return Some(false);
    }
    let Some(task_name) = parent
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("kars.azure.com/karstask"))
    else {
        return None;
    };
    let tasks: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        namespace,
        &kars_api_resource("KarsTask", "karstasks"),
    );
    let Ok(task) = tasks.get(task_name).await else {
        return Some(false);
    };
    let annotations = task.metadata.annotations.as_ref();
    let team_associated = annotations
        .and_then(|annotations| annotations.get("kars.azure.com/team"))
        .is_some();
    let taskforce = annotations
        .and_then(|annotations| annotations.get("kars.azure.com/team-role"))
        .map(String::as_str)
        == Some("taskforce");
    if !team_associated {
        return None;
    }
    if !taskforce {
        return Some(false);
    }
    let Some(owner) = task.metadata.owner_references.as_ref().and_then(|owners| {
        owners
            .iter()
            .find(|owner| owner.kind == "KarsTeam" && owner.controller == Some(true))
    }) else {
        return Some(false);
    };
    let teams: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        namespace,
        &kars_api_resource("KarsTeam", "karsteams"),
    );
    let Ok(team) = teams.get(&owner.name).await else {
        return Some(false);
    };
    if team.metadata.uid.as_deref() != Some(owner.uid.as_str()) {
        return Some(false);
    }
    let Some(roster) = task
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kars.azure.com/effective-roster"))
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
    else {
        return Some(false);
    };
    Some(
        role.map(str::trim)
            .filter(|role| !role.is_empty())
            .is_some_and(|role| roster.iter().any(|member| member == role)),
    )
}

const LOGICAL_AGENT_ID_ANNOTATION: &str = "kars.azure.com/logical-agent-id";

fn scoped_child_name(parent_name: &str, logical_agent_id: &str) -> String {
    let candidate = format!("{parent_name}-{logical_agent_id}");
    // The controller creates namespace `kars-<sandbox>`; keep the sandbox name
    // at <=58 so the prefixed namespace also satisfies the 63-byte DNS limit.
    const MAX_NAME: usize = 58;
    if candidate.len() <= MAX_NAME {
        return candidate;
    }
    let digest = Sha256::digest(candidate.as_bytes());
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    );
    let prefix_len = MAX_NAME - suffix.len() - 1;
    let prefix = candidate[..prefix_len].trim_end_matches('-');
    format!("{prefix}-{suffix}")
}

fn logical_agent_id(obj: &DynamicObject) -> String {
    obj.metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(LOGICAL_AGENT_ID_ANNOTATION))
        .cloned()
        .unwrap_or_else(|| obj.name_any())
}

fn spawn_parent(obj: &DynamicObject) -> Option<&str> {
    obj.metadata.labels.as_ref().and_then(|labels| {
        labels
            .get("kars.azure.com/parent")
            .or_else(|| labels.get("kars.azure.com/predecessor"))
            .map(String::as_str)
    })
}

fn apply_spawn_identity(crd: &mut serde_json::Value, resource_name: &str, logical_agent_id: &str) {
    crd["metadata"]["name"] = serde_json::Value::String(resource_name.to_string());
    if !crd["metadata"]["annotations"].is_object() {
        crd["metadata"]["annotations"] = serde_json::json!({});
    }
    crd["metadata"]["annotations"][LOGICAL_AGENT_ID_ANNOTATION] =
        serde_json::Value::String(logical_agent_id.to_string());
}

fn child_matches_parent(
    obj: &DynamicObject,
    parent_name: &str,
    parent_uid: &str,
    logical_name: &str,
) -> bool {
    if obj.metadata.deletion_timestamp.is_some()
        || spawn_parent(obj) != Some(parent_name)
        || logical_agent_id(obj) != logical_name
    {
        return false;
    }
    let bound_uid = obj
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kars.azure.com/spawn-parent-uid"))
        .map(String::as_str)
        .or_else(|| {
            obj.metadata
                .owner_references
                .as_ref()
                .and_then(|owners| owners.iter().find(|owner| owner.name == parent_name))
                .map(|owner| owner.uid.as_str())
        });
    bound_uid == Some(parent_uid)
}

async fn find_existing_child(
    api: &Api<DynamicObject>,
    parent_name: &str,
    parent_uid: &str,
    logical_agent_id: &str,
) -> Result<Option<(String, DynamicObject)>, String> {
    let scoped = scoped_child_name(parent_name, logical_agent_id);
    for (index, resource_name) in [scoped.as_str(), logical_agent_id].into_iter().enumerate() {
        let Some(obj) = api
            .get_opt(resource_name)
            .await
            .map_err(|e| format!("Failed to inspect child sandbox: {e}"))?
        else {
            continue;
        };
        if !child_matches_parent(&obj, parent_name, parent_uid, logical_agent_id) {
            if index == 1 {
                // Legacy global-name child owned by another parent. Ignore it;
                // the scoped name is independent and safe to create.
                continue;
            }
            return Err(format!(
                "Sandbox resource collision for '{logical_agent_id}' — existing object is not this parent incarnation's child"
            ));
        }
        return Ok(Some((resource_name.to_string(), obj)));
    }
    Ok(None)
}

/// Request body for `POST /sandbox/spawn`.
///
/// The canonical identifier for a sub-agent on the wire is `agent_id` (a
/// DNS-safe k8s metadata.name, 1–63 chars, `[a-z0-9-]`). The serde alias
/// `name` remains accepted on deserialise for backward compatibility with
/// any in-flight plugin or client that hasn't been updated yet; the alias
/// will be retired once all callers have migrated.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnRequest {
    /// Name for the sub-agent sandbox (must be DNS-safe). Canonical wire
    /// name is `agent_id`; `name` is accepted as a deserialise-only alias.
    #[serde(alias = "name")]
    pub agent_id: String,
    /// Model deployment to use (default: gpt-4.1).
    pub model: Option<String>,
    /// Enable AGT governance (default: true).
    #[serde(default = "default_true")]
    pub governance: bool,
    /// Trust threshold for AGT mesh (default: 500).
    pub trust_threshold: Option<i32>,
    /// Enable egress learn mode (default: false).
    #[serde(default)]
    pub learn_egress: bool,
    /// Deliberately inherit the parent's already-approved endpoint set. Defaults
    /// to false so spawned agents start zero-trust and must request additional
    /// access unless the principal explicitly delegates existing network scope.
    #[serde(default)]
    pub inherit_parent_egress: bool,
    /// Inherit only when the router verifies that a KarsTeam taskforce is
    /// spawning an exact role declared in its roster.
    #[serde(default)]
    pub auto_inherit_team_egress: bool,
    /// Isolation level: standard | enhanced | confidential.
    pub isolation: Option<String>,
    /// Daily token budget.
    pub token_budget_daily: Option<i64>,
    /// Per-request token budget.
    pub token_budget_per_request: Option<i64>,
    /// Trusted peer AMIDs — parent-verified agents that the sub-agent should
    /// auto-trust (parent + siblings). Passed securely via env var at spawn time,
    /// not self-reported. Format: "name:AMID,name:AMID,..."
    pub trusted_peers: Option<String>,
    /// Handoff metadata — when present, spawn targets AKS even in dev mode.
    pub handoff: Option<HandoffMeta>,
    /// Runtime kind override — `OpenClaw` (default), `Hermes`, etc.
    /// When unset, the child inherits the parent's runtime by reading
    /// the `KARS_RUNTIME_KIND` env var on the spawning router. This
    /// lets a Hermes parent spawn Hermes children without the LLM
    /// having to know the runtime kind explicitly. The accepted values
    /// match the controller's `RuntimeKind` enum exactly.
    pub runtime_kind: Option<String>,
    /// Optional persona/role descriptor for the sub-agent (e.g.
    /// "data analyst", "technical writer", "auditor"). Surfaces in
    /// the parent's local peer roster (Hermes plugin) and in the
    /// child's AGT registry record's `capabilities` field so siblings
    /// can find each other by role. Free-form string; not used by the
    /// spawn-control plane itself. Currently consumed only by the
    /// Hermes runtime; OpenClaw silently ignores it.
    pub role: Option<String>,
}

/// Handoff metadata attached to a spawn request.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffMeta {
    /// "restore" = target will receive state from predecessor via mesh.
    pub mode: String,
    /// Name of the agent handing off.
    pub predecessor: Option<String>,
}

/// Response from spawn/status endpoints.
#[derive(Debug, Serialize)]
pub struct SpawnResponse {
    pub status: String,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Sub-agent entry for list response.
#[derive(Debug, Serialize)]
pub struct SubAgentEntry {
    pub agent_id: String,
    pub mesh_name: String,
    pub namespace: Option<String>,
    pub phase: Option<String>,
    pub model: Option<String>,
    pub governance: bool,
}

fn apply_parent_git_write(
    crd: &mut serde_json::Value,
    parent_repos: &str,
    connection_name: Option<&str>,
) {
    let parent_repos = parent_repos.trim();
    if parent_repos.is_empty() {
        return;
    }
    if let Some(connection_name) = connection_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let repos = parent_repos
            .split(',')
            .map(str::trim)
            .filter(|repo| !repo.is_empty())
            .collect::<Vec<_>>();
        crd["spec"]["gitWrite"] = serde_json::json!({
            "connectionConfigMapRef": {"name": connection_name},
            "repos": repos,
        });
        return;
    }
    if let Some(meta) = crd.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        let anns = meta
            .entry("annotations")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(o) = anns.as_object_mut() {
            o.insert(
                "kars.azure.com/git-write-repos".to_string(),
                serde_json::Value::String(parent_repos.to_string()),
            );
        }
    }
}

/// Create a KarsSandbox CRD for a sub-agent, or a Docker container in dev mode.
pub async fn create_sandbox(
    parent_name: &str,
    req: &SpawnRequest,
) -> Result<SpawnResponse, String> {
    // Validate name: must be DNS-safe
    if req.agent_id.is_empty() || req.agent_id.len() > 63 {
        return Err("name must be 1-63 characters".into());
    }
    if !req
        .agent_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("name must contain only lowercase alphanumeric characters and hyphens".into());
    }
    if req.agent_id.starts_with('-') || req.agent_id.ends_with('-') {
        return Err("name must not start or end with a hyphen".into());
    }

    // Dev mode: spawn sibling Docker container instead of K8s CRD.
    // Exception: handoff spawns always target AKS (the whole point is moving to cloud).
    let is_dev = std::env::var("KARS_DEV_MODE").unwrap_or_default() == "true";
    let is_handoff = req.handoff.as_ref().is_some_and(|h| h.mode == "restore");
    if is_dev && !is_handoff {
        return docker::create_sandbox_docker(parent_name, req).await;
    }
    if is_dev && is_handoff {
        tracing::info!(
            parent = %parent_name,
            child = %req.agent_id,
            "Handoff spawn — bypassing Docker dev mode, creating K8s CRD on AKS"
        );
    }

    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &namespace, &kars_sandbox_api_resource());

    // Sub-agents inherit the parent's model unless the spawn request explicitly
    // overrides it. The controller plumbs the parent's resolved
    // `inferenceRef`/`InferencePolicy.model` into both `AZURE_OPENAI_DEPLOYMENT`
    // (on the inference-router container, see reconciler/mod.rs ~line 1210)
    // and `OPENCLAW_MODEL` (on the agent container, see ~line 996). Reading
    // either gives us the parent's effective model — fall back to `DEFAULT_MODEL`
    // for parity with `RouterConfig::default_model` (config.rs ~line 97), and
    // only as a last resort to "gpt-4.1".
    //
    // Bug history: previously this hardcoded "gpt-4.1" as the unwrap_or fallback,
    // so any sub-agent spawned without an explicit `model` ran on gpt-4.1
    // regardless of the parent's choice. Symptom: operator UI showed parent's
    // model (e.g. gpt-5.4) but sub-agent inference logs showed
    // `inference:chat_completions:gpt-4.1`.
    let parent_model = std::env::var("AZURE_OPENAI_DEPLOYMENT")
        .or_else(|_| std::env::var("OPENCLAW_MODEL"))
        .or_else(|_| std::env::var("DEFAULT_MODEL"))
        .unwrap_or_else(|_| "gpt-4.1".into());
    let model = req.model.as_deref().unwrap_or(parent_model.as_str());
    let parent_isolation = std::env::var("SANDBOX_ISOLATION").unwrap_or_else(|_| "enhanced".into());
    let isolation = req.isolation.as_deref().unwrap_or(&parent_isolation);

    // Prevent downgrading from confidential parent
    if parent_isolation == "confidential" && isolation != "confidential" {
        return Err(format!(
            "Cannot spawn '{}' sub-agent from confidential parent — sub-agents must also be confidential",
            isolation,
        ));
    }

    // Build spec — matches the post-S10/S13 CRD schema:
    //   - `runtime` (required) — multi-runtime selector; sub-agents always
    //     spawn as OpenClaw with the controller's default image (`:latest`).
    //   - `inferenceRef` (required) — by-name reference to an
    //     InferencePolicy CR in the same namespace. Sub-agents reuse the
    //     parent's policy (`<parent>-inference`) so they inherit the same
    //     model preference, content-safety floor, prompt-shield setting,
    //     and token budgets without us needing to clone the CR.
    //   - `sandbox`, `governance`, `networkPolicy`, optional `agent` —
    //     unchanged structurally.
    // The legacy top-level `openclaw` and `inference` blocks were removed
    // from the schema in S10.A1 / S13; sending them now triggers
    // `additionalProperties: false` rejection at admission.
    //
    // Slice 2 DoD #6 — read parent's labels so user-defined tags
    // (e.g. `tier=prod`) propagate to the child. Best-effort: a
    // parent-fetch failure does not block spawn — we fall back to an
    // empty label map. Rationale: spawn-tracking labels alone are
    // still enough for the sub-agent to be functional; inherited
    // tags are a quality-of-life feature for operators, not a
    // governance gate.
    //
    // The SAME parent fetch also recovers, in one round-trip:
    //   - the parent's effective `governance.mcpServerRefs` (main's fix: so a
    //     spawned sub-agent doesn't silently lose MCP access), and
    //   - the parent's REAL `governance.toolPolicyRef`/`inferenceRef` names +
    //     uid (kars-bridge: so team-run sub-agents point at policies that
    //     actually exist and are garbage-collected when the parent goes away).
    let (
        parent_labels,
        parent_mcp_refs,
        parent_tool_policy,
        parent_inference,
        parent_endpoints,
        parent_egress_mode,
        parent_uid,
        verified_team_roster_spawn,
    ): (
        BTreeMap<String, String>,
        Vec<serde_json::Value>,
        Option<String>,
        Option<String>,
        Vec<serde_json::Value>,
        Option<String>,
        String,
        Option<bool>,
    ) = match api.get(parent_name).await {
        Ok(parent_obj) => {
            let verified_team_roster_spawn = is_verified_team_roster_spawn(
                &client,
                &namespace,
                &parent_obj,
                req.role.as_deref(),
            )
            .await;
            let labels = parent_obj.metadata.labels.clone().unwrap_or_default();
            let uid = parent_obj
                .metadata
                .uid
                .clone()
                .ok_or_else(|| "Parent KarsSandbox has no metadata.uid".to_string())?;
            let mcp_refs = parent_mcp_server_refs(&parent_obj.data);
            let spec = parent_obj.data.get("spec");
            let tool_policy = spec
                .and_then(|s| s.pointer("/governance/toolPolicyRef/name"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty());
            let inference = spec
                .and_then(|s| s.pointer("/inferenceRef/name"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty());
            let endpoints = spec
                .and_then(|s| s.pointer("/networkPolicy/allowedEndpoints"))
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default();
            let egress_mode = spec
                .and_then(|s| s.pointer("/networkPolicy/egressMode"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            (
                labels,
                mcp_refs,
                tool_policy,
                inference,
                endpoints,
                egress_mode,
                uid,
                verified_team_roster_spawn,
            )
        }
        Err(e) => {
            return Err(format!(
                "Could not fetch parent KarsSandbox '{parent_name}' for secure spawn: {e}"
            ));
        }
    };
    let inherit_parent_egress = match verified_team_roster_spawn {
        Some(role_is_declared) => {
            role_is_declared && (req.inherit_parent_egress || req.auto_inherit_team_egress)
        }
        None => req.inherit_parent_egress,
    };

    let mut crd = build_sub_agent_crd_with_labels(
        parent_name,
        &namespace,
        isolation,
        model,
        req,
        &parent_labels,
    );
    if verified_team_roster_spawn == Some(true)
        && let Some(labels) = crd
            .pointer_mut("/metadata/labels")
            .and_then(serde_json::Value::as_object_mut)
    {
        labels.insert(
            "kars.azure.com/team-role".into(),
            serde_json::Value::String("member".into()),
        );
    }
    let child_resource_name = scoped_child_name(parent_name, &req.agent_id);
    apply_spawn_identity(&mut crd, &child_resource_name, &req.agent_id);
    crd["metadata"]["annotations"]["kars.azure.com/spawn-parent-uid"] =
        serde_json::Value::String(parent_uid.clone());
    crd["metadata"]["annotations"]["kars.azure.com/egress-inheritance"] = serde_json::Value::String(
        if inherit_parent_egress {
            "inherit"
        } else {
            "request"
        }
        .into(),
    );

    // main: additive overlay — copy inherited MCP refs onto the child's
    // governance (the builder always emits `spec.governance`).
    if !parent_mcp_refs.is_empty()
        && let Some(gov) = crd
            .get_mut("spec")
            .and_then(|s| s.get_mut("governance"))
            .filter(|g| g.is_object())
    {
        let count = parent_mcp_refs.len();
        gov["mcpServerRefs"] = serde_json::Value::Array(parent_mcp_refs);
        tracing::info!(
            parent = %parent_name,
            child = %req.agent_id,
            count,
            "Sub-agent inherits parent MCP server refs"
        );
    }

    // kars-bridge: override the convention-derived refs with the parent's real
    // ones so the child inherits policies that actually exist (e.g.
    // `kars-default` shared by standing-team runs) rather than a 404-ing
    // derived name. When the parent carries NO explicit tool policy (a valid
    // posture — governance is still enforced via the AGT profile/trust
    // threshold), the child must not point at a convention-derived
    // `{parent}-toolpolicy` that does not exist, or it hangs
    // `Degraded: ToolPolicy ... not found`.
    apply_parent_refs(
        &mut crd,
        parent_tool_policy.as_deref(),
        parent_inference.as_deref(),
    );
    apply_parent_network_policy(
        &mut crd,
        &parent_endpoints,
        parent_egress_mode.as_deref(),
        inherit_parent_egress,
    );

    // Keyless git write (§14): a sub-agent inherits the principal's typed
    // connection reference and repo scope. The controller re-clamps the child
    // against that principal-specific ConfigMap. Legacy parents that do not
    // carry GIT_CONNECTION_CONFIG_MAP retain the annotation fallback.
    if let Ok(parent_repos) = std::env::var("GIT_WRITE_REPOS") {
        let connection_name = std::env::var("GIT_CONNECTION_CONFIG_MAP").ok();
        apply_parent_git_write(&mut crd, &parent_repos, connection_name.as_deref());
    }

    // kars-bridge: own the child by its parent sandbox so K8s garbage-collects
    // it when the parent goes away (run completes / task deleted / team
    // deleted). Without this, agent-spawned sub-agents outlive their parent run
    // as *orphans*. Skipped for handoff successors, which must OUTLIVE the
    // predecessor by design.
    if req.handoff.is_none() {
        apply_owner_reference(&mut crd, parent_name, &parent_uid);
    }

    if let Some((resource_name, existing)) =
        find_existing_child(&api, parent_name, &parent_uid, &req.agent_id).await?
    {
        let phase = existing
            .data
            .get("status")
            .and_then(|status| status.get("phase"))
            .and_then(|phase| phase.as_str())
            .unwrap_or("Pending")
            .to_string();
        tracing::info!(
            parent = %parent_name,
            child = %req.agent_id,
            resource = %resource_name,
            "Sub-agent sandbox already exists — reusing"
        );
        return Ok(SpawnResponse {
            status: "created".into(),
            agent_id: req.agent_id.clone(),
            mesh_name: Some(resource_name.clone()),
            namespace: Some(format!("kars-{resource_name}")),
            phase: Some(phase),
            message: Some(format!(
                "Sub-agent '{}' already exists (model: {}, governance: {}). Use AGT mesh to communicate.",
                req.agent_id, model, req.governance
            )),
        });
    }

    let obj: kube::api::DynamicObject =
        serde_json::from_value(crd).map_err(|e| format!("Failed to build CRD: {e}"))?;

    match api.create(&PostParams::default(), &obj).await {
        Ok(_created) => {
            tracing::info!(
                parent = %parent_name,
                child = %req.agent_id,
                resource = %child_resource_name,
                "Sub-agent sandbox created"
            );

            // For handoff targets, propagate channel/plugin credentials to the
            // target namespace so the cloud agent gets Telegram, Slack, etc.
            if req.handoff.is_some() {
                let child_name = child_resource_name.clone();
                let client_clone = Client::try_default().await.ok();
                if let Some(kc) = client_clone {
                    tokio::spawn(async move {
                        if let Err(e) = propagate_credentials(&kc, &child_name).await {
                            tracing::warn!(
                                child = %child_name,
                                "Credential propagation failed (non-fatal): {e}"
                            );
                        }
                    });
                }
            }

            Ok(SpawnResponse {
                status: "created".into(),
                agent_id: req.agent_id.clone(),
                mesh_name: Some(child_resource_name.clone()),
                namespace: Some(format!("kars-{child_resource_name}")),
                phase: Some("Pending".into()),
                message: Some(format!(
                    "Sub-agent '{}' spawned (model: {}, governance: {}). Use AGT mesh to communicate.",
                    req.agent_id, model, req.governance
                )),
            })
        }
        Err(kube::Error::Api(resp)) if resp.code == 409 => {
            let existing = api
                .get(&child_resource_name)
                .await
                .map_err(|e| format!("Failed to inspect existing sandbox: {e}"))?;
            if !child_matches_parent(&existing, parent_name, &parent_uid, &req.agent_id) {
                return Err(format!(
                    "Sandbox resource collision for '{}' — existing object is not this parent's logical child",
                    req.agent_id
                ));
            }
            tracing::info!(
                parent = %parent_name,
                child = %req.agent_id,
                resource = %child_resource_name,
                "Sub-agent sandbox already exists — reusing"
            );
            Ok(SpawnResponse {
                status: "created".into(),
                agent_id: req.agent_id.clone(),
                mesh_name: Some(child_resource_name.clone()),
                namespace: Some(format!("kars-{child_resource_name}")),
                phase: Some("Running".into()),
                message: Some(format!(
                    "Sub-agent '{}' already running (model: {}, governance: {}). Use AGT mesh to communicate.",
                    req.agent_id, model, req.governance
                )),
            })
        }
        Err(e) => {
            tracing::error!(parent = %parent_name, child = %req.agent_id, "Failed to create sandbox: {e}");
            Err(format!("Failed to create sandbox: {e}"))
        }
    }
}

// ── Credential propagation for handoff targets ──────────────────────────────
//
// The controller mounts `{name}-credentials` secret as envFrom (optional: true).
// For handoff targets we propagate channel/plugin credentials from the source's
// environment so the cloud agent inherits Telegram, Slack, etc.

/// Env vars that carry channel and plugin credentials (safe to propagate).
const CREDENTIAL_ENV_VARS: &[&str] = &[
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_ALLOW_FROM",
    "SLACK_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "WHATSAPP_ENABLED",
    "BRAVE_API_KEY",
    "TAVILY_API_KEY",
    "EXA_API_KEY",
    "FIRECRAWL_API_KEY",
    "PERPLEXITY_API_KEY",
];

async fn propagate_credentials(client: &Client, child_name: &str) -> Result<(), String> {
    // Collect credential env vars that are set in the current environment
    let mut creds: BTreeMap<String, String> = BTreeMap::new();
    for &var in CREDENTIAL_ENV_VARS {
        if let Ok(val) = std::env::var(var) {
            if !val.is_empty() {
                creds.insert(var.to_string(), val);
            }
        }
    }
    if creds.is_empty() {
        tracing::info!(child = %child_name, "No channel/plugin credentials to propagate");
        return Ok(());
    }

    let target_ns = format!("kars-{}", child_name);
    let secret_name = format!("{}-credentials", child_name);

    // Wait for the namespace to be created by the controller (up to 30s)
    let ns_api: Api<Namespace> = Api::all(client.clone());
    let mut ns_ready = false;
    for i in 0..15 {
        if ns_api.get_opt(&target_ns).await.ok().flatten().is_some() {
            ns_ready = true;
            break;
        }
        if i == 0 {
            tracing::info!(child = %child_name, "Waiting for namespace '{target_ns}' before creating credentials secret");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    if !ns_ready {
        return Err(format!("Namespace '{target_ns}' not created within 30s"));
    }

    // Build and apply the credentials secret
    let secret: Secret = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name,
            "namespace": target_ns,
            "labels": {
                "kars.azure.com/managed-by": "handoff",
                "kars.azure.com/predecessor": std::env::var("SANDBOX_NAME").unwrap_or_default(),
            }
        },
        "type": "Opaque",
        "stringData": creds,
    }))
    .map_err(|e| format!("Failed to build credentials secret: {e}"))?;

    let secret_api: Api<Secret> = Api::namespaced(client.clone(), &target_ns);
    secret_api
        .patch(
            &secret_name,
            &PatchParams::apply("kars-handoff"),
            &Patch::Apply(secret),
        )
        .await
        .map_err(|e| format!("Failed to create credentials secret: {e}"))?;

    tracing::info!(
        child = %child_name,
        creds = creds.len(),
        "Propagated {} credential(s) to {target_ns}/{secret_name}",
        creds.len()
    );
    Ok(())
}

/// List sub-agents spawned by a parent sandbox.
pub async fn list_sandboxes(parent_name: &str) -> Result<Vec<SubAgentEntry>, String> {
    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &kars_sandbox_api_resource());

    let lp = ListParams::default().labels(&format!("kars.azure.com/parent={parent_name}"));

    let list = api
        .list(&lp)
        .await
        .map_err(|e| format!("Failed to list sandboxes: {e}"))?;

    let entries: Vec<SubAgentEntry> = list
        .items
        .iter()
        .map(|obj| {
            let name = logical_agent_id(obj);
            let mesh_name = obj.name_any();
            let data = &obj.data;

            let phase = data
                .get("status")
                .and_then(|s| s.get("phase"))
                .and_then(|p| p.as_str())
                .map(String::from);

            let ns = data
                .get("status")
                .and_then(|s| s.get("namespace"))
                .and_then(|n| n.as_str())
                .map(String::from);

            let model = data
                .get("metadata")
                .and_then(|m| m.get("annotations"))
                .and_then(|a| a.get("kars.azure.com/model"))
                .and_then(|m| m.as_str())
                .map(String::from);

            let governance = data
                .get("spec")
                .and_then(|s| s.get("governance"))
                .and_then(|g| g.get("enabled"))
                .and_then(|e| e.as_bool())
                .unwrap_or(false);

            SubAgentEntry {
                agent_id: name,
                mesh_name,
                namespace: ns,
                phase,
                model,
                governance,
            }
        })
        .collect();

    Ok(entries)
}

/// Get status of a specific sub-agent sandbox.
pub async fn get_sandbox_status(parent_name: &str, name: &str) -> Result<SpawnResponse, String> {
    // Dev mode: query Docker Engine API instead of K8s
    if std::env::var("KARS_DEV_MODE").unwrap_or_default() == "true" {
        return docker::get_sandbox_status_docker(name).await;
    }

    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &kars_sandbox_api_resource());

    let parent_uid = api
        .get(parent_name)
        .await
        .map_err(|e| format!("Parent sandbox '{parent_name}' not found: {e}"))?
        .metadata
        .uid
        .ok_or_else(|| format!("Parent sandbox '{parent_name}' has no metadata.uid"))?;
    let Some((resource_name, obj)) =
        find_existing_child(&api, parent_name, &parent_uid, name).await?
    else {
        return Err(format!("Sandbox '{name}' not found"));
    };
    let data = &obj.data;

    let phase = data
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(|p| p.as_str())
        .map(String::from);

    let ns = data
        .get("status")
        .and_then(|s| s.get("namespace"))
        .and_then(|n| n.as_str())
        .map(String::from);

    Ok(SpawnResponse {
        status: "ok".into(),
        agent_id: name.to_string(),
        mesh_name: Some(resource_name),
        namespace: ns,
        phase,
        message: None,
    })
}

/// Delete a sub-agent sandbox.
pub async fn delete_sandbox(parent_name: &str, name: &str) -> Result<SpawnResponse, String> {
    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &kars_sandbox_api_resource());

    let parent_uid = api
        .get(parent_name)
        .await
        .map_err(|e| format!("Parent sandbox '{parent_name}' not found: {e}"))?
        .metadata
        .uid
        .ok_or_else(|| format!("Parent sandbox '{parent_name}' has no metadata.uid"))?;
    let Some((resource_name, _obj)) =
        find_existing_child(&api, parent_name, &parent_uid, name).await?
    else {
        return Err(format!("Sandbox '{name}' not found"));
    };

    api.delete(&resource_name, &Default::default())
        .await
        .map_err(|e| format!("Failed to delete: {e}"))?;

    tracing::info!(parent = %parent_name, child = %name, "Sub-agent sandbox deleted");
    Ok(SpawnResponse {
        status: "deleted".into(),
        agent_id: name.to_string(),
        mesh_name: Some(resource_name),
        namespace: None,
        phase: Some("Terminating".into()),
        message: Some(format!("Sub-agent '{}' is being torn down", name)),
    })
}

/// Collect sub-agent snapshots for handoff.
///
/// Lists all running sub-agents and reconstructs a `SpawnRequest` from each
/// CRD's spec so they can be re-spawned on the target host after restore.
pub async fn collect_sub_agent_snapshots(
    parent_name: &str,
) -> Result<Vec<crate::handoff::SubAgentSnapshot>, String> {
    // Dev mode (Docker): list sub-agent containers
    if std::env::var("KARS_DEV_MODE").unwrap_or_default() == "true" {
        return docker::collect_sub_agent_snapshots_docker(parent_name).await;
    }

    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &kars_sandbox_api_resource());

    let lp = ListParams::default().labels(&format!("kars.azure.com/parent={parent_name}"));
    let list = api
        .list(&lp)
        .await
        .map_err(|e| format!("Failed to list sub-agents: {e}"))?;

    let mut snapshots = Vec::new();

    for obj in &list.items {
        let name = logical_agent_id(obj);
        let spec = match obj.data.get("spec") {
            Some(s) => s,
            None => continue,
        };

        let phase = obj
            .data
            .get("status")
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .unwrap_or("Unknown");

        // Only include Running or Pending sub-agents (skip Terminating)
        if phase == "Terminating" {
            continue;
        }

        // Reconstruct SpawnRequest from CRD metadata + spec.
        // Model lives on the `kars.azure.com/model` annotation since
        // S13 (delegated to InferencePolicy on-CR).
        let model = obj
            .data
            .get("metadata")
            .and_then(|m| m.get("annotations"))
            .and_then(|a| a.get("kars.azure.com/model"))
            .and_then(|m| m.as_str())
            .map(String::from);

        let governance = spec
            .get("governance")
            .and_then(|g| g.get("enabled"))
            .and_then(|e| e.as_bool())
            .unwrap_or(true);

        let trust_threshold = spec
            .get("governance")
            .and_then(|g| g.get("trustThreshold"))
            .and_then(|t| t.as_i64())
            .map(|t| t as i32);

        let learn_egress = spec
            .get("networkPolicy")
            .and_then(|n| n.get("egressMode"))
            .and_then(|v| v.as_str())
            .map(|s| s.eq_ignore_ascii_case("Learn"))
            .unwrap_or(true); // CRD default = Learn

        let isolation = spec
            .get("sandbox")
            .and_then(|s| s.get("isolation"))
            .and_then(|i| i.as_str())
            .map(String::from);

        // Token budgets now live on the InferencePolicy CR, not the
        // sub-agent CRD. On restore, the new spawn will inherit the
        // parent's policy budgets — we no longer round-trip per-sub-agent.
        let token_budget_daily: Option<i64> = None;
        let token_budget_per_request: Option<i64> = None;

        let trusted_peers = spec
            .get("governance")
            .and_then(|g| g.get("trustedPeers"))
            .and_then(|p| p.as_str())
            .map(String::from);

        let spawn_config = SpawnRequest {
            agent_id: name.clone(),
            model,
            governance,
            trust_threshold,
            learn_egress,
            inherit_parent_egress: obj
                .data
                .get("metadata")
                .and_then(|m| m.get("annotations"))
                .and_then(|a| a.get("kars.azure.com/egress-inheritance"))
                .and_then(|value| value.as_str())
                == Some("inherit"),
            auto_inherit_team_egress: false,
            isolation,
            token_budget_daily,
            token_budget_per_request,
            trusted_peers,
            handoff: None, // Not a handoff spawn — regular sub-agent re-spawn
            // Restore the runtime kind from the captured CRD so re-spawn
            // preserves it (a Hermes parent's snapshots stay Hermes).
            runtime_kind: spec
                .get("runtime")
                .and_then(|r| r.get("kind"))
                .and_then(|k| k.as_str())
                .map(String::from),
            // Restore role from the captured CRD labels too — set at
            // spawn time by build_sub_agent_crd (kars.azure.com/role
            // label). None when the parent didn't pass a role.
            role: obj
                .data
                .get("metadata")
                .and_then(|m| m.get("labels"))
                .and_then(|l| l.get("kars.azure.com/role"))
                .and_then(|r| r.as_str())
                .map(String::from),
        };

        snapshots.push(crate::handoff::SubAgentSnapshot {
            agent_id: name.clone(),
            original_amid: String::new(), // Set by caller if registry available
            spawn_config,
            task_context: format!("Sub-agent '{name}' (phase: {phase})"),
            status: if phase == "Running" {
                "paused_at_checkpoint".to_string()
            } else {
                "pending".to_string()
            },
            checkpoint: None,
            workspace_tar: Vec::new(), // Workspace lives in the sub-agent's container
        });

        tracing::info!(
            parent = %parent_name,
            sub_agent = %name,
            phase = %phase,
            "Collected sub-agent snapshot for handoff"
        );
    }

    Ok(snapshots)
}

// ---------------------------------------------------------------------------
// Pure CRD builder (kept testable; called from `create_sandbox`)
// ---------------------------------------------------------------------------

/// Build the KarsSandbox CRD payload for a spawned sub-agent or handoff
/// target. Pure function — no I/O, no env vars except `FOUNDRY_AGENT_TOOLS`
/// — so it round-trips through JSON-shape contract tests below, catching
/// schema regressions to the pre-S10/S13 shape (`spec.openclaw`,
/// `spec.inference`, `governance.toolPolicy: <string>`,
/// top-level `spec.handoff`/`spec.model` — all rejected by
/// `additionalProperties: false` at admission).
///
/// **No-inherit invariant (Slice 3a/3b)**: the parent's
/// `spec.memoryRef` is deliberately NOT propagated onto the spawned
/// sub-agent. KarsMemory bindings are scoped to the agent that
/// declared them and must not flow through `handoff` or `spawn`. The
/// contract test `sub_agent_crd_never_inherits_memory_ref` asserts
/// this by construction — if a future caller adds a `memory_ref`
/// field to `SpawnRequest`, the test fails before the CRD ships.
/// Slice 2 DoD #6 — parent-label inheritance.
///
/// Pure label-merge: filter the parent's `metadata.labels` to drop
/// kars-controlled keys (anything starting with `kars.`
/// and the `app.kubernetes.io/*` tracking labels), then start the
/// child's label map from that filtered set. Spawn-tracking labels
/// (`parent`, `spawned-by`, `predecessor`) are written last so they
/// always win if the parent happened to carry a colliding key.
///
/// Operators who tag a parent with e.g. `tier=prod` /
/// `team=payments` / `env=staging` get those same tags on every
/// sub-agent the parent spawns — so a single `kubectl get
/// karssandbox -l tier=prod` returns the parent and every
/// descendant without the operator having to walk the
/// `kars.azure.com/parent` graph by hand.
pub(crate) fn inherit_parent_labels(
    parent_labels: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in parent_labels {
        // Drop labels we control — they get re-stamped per spawn
        // based on the *child's* role (handoff vs. agent vs. mesh)
        // and inheriting them would lie about the child's lineage.
        if k.starts_with("kars.azure.com/") || k.starts_with("app.kubernetes.io/") {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    out
}

/// Extract the parent sandbox's effective `governance.mcpServerRefs` so a
/// spawned sub-agent inherits the same MCP server access. Honors the
/// deprecated singular `mcpServerRef` (mirrors
/// `GovernanceConfig::effective_mcp_server_refs`). Operates on a
/// `DynamicObject`'s `data` value; empty when the parent references none.
fn parent_mcp_server_refs(parent_data: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(gov) = parent_data.get("spec").and_then(|s| s.get("governance")) else {
        return Vec::new();
    };
    if let Some(arr) = gov.get("mcpServerRefs").and_then(|v| v.as_array())
        && !arr.is_empty()
    {
        return arr.clone();
    }
    if let Some(singular) = gov.get("mcpServerRef").filter(|v| v.is_object()) {
        return vec![singular.clone()];
    }
    Vec::new()
}

/// Stamp an ownerReference on a spawned child CRD so K8s garbage-collects it
/// when the parent sandbox is deleted. Same-namespace only (spawn always
/// creates the child in the parent's namespace), which is the requirement for
/// owner-based GC. `controller:false` + `blockOwnerDeletion:false` — the parent
/// doesn't reconcile the child, it only anchors its lifetime, and deleting the
/// parent must never be blocked waiting on the child.
pub(crate) fn apply_owner_reference(
    crd: &mut serde_json::Value,
    parent_name: &str,
    parent_uid: &str,
) {
    if let Some(meta) = crd
        .pointer_mut("/metadata")
        .and_then(serde_json::Value::as_object_mut)
    {
        meta.insert(
            "ownerReferences".to_string(),
            serde_json::json!([{
                "apiVersion": "kars.azure.com/v1alpha1",
                "kind": "KarsSandbox",
                "name": parent_name,
                "uid": parent_uid,
                "controller": false,
                "blockOwnerDeletion": false,
            }]),
        );
    }
}

/// Reconcile a freshly-built sub-agent CRD's governance/inference refs with
/// the *parent's actual* refs.
///
/// The CRD builder derives `{parent_name}-toolpolicy` / `{parent_name}-inference`
/// by convention and always stamps `governance.enabled = true`. That derivation
/// holds for sandboxes with a dedicated per-sandbox policy, but breaks for:
///   - standing-team runs / any sandbox that references a SHARED policy
///     (e.g. `kars-default`) — the derived name then 404s and the child hangs
///     `Degraded: ToolPolicy ... not found (cross-namespace refs not supported)`.
///   - parents with NO governance block at all (team runs) — the derived
///     `{parent}-toolpolicy` never exists, and the CRD's CEL rule
///     (`toolPolicyRef.name must be set when governance.enabled=true`) forbids
///     simply dropping it. We fall back to the cluster-wide default policy
///     `kars-default`, a real, safe policy that is guaranteed present.
///
/// `parent_tool_policy`/`parent_inference` are the parent's real ref names (if any).
pub(crate) fn apply_parent_refs(
    crd: &mut serde_json::Value,
    parent_tool_policy: Option<&str>,
    parent_inference: Option<&str>,
) {
    // The cluster-wide default AGT ToolPolicy, always installed by the
    // controller. Used when the parent carries no explicit policy so the
    // child still satisfies the `enabled=true ⇒ toolPolicyRef set` CEL rule
    // while pointing at a policy that actually exists.
    const DEFAULT_TOOL_POLICY: &str = "kars-default";

    let resolved = parent_tool_policy
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_TOOL_POLICY);
    if let Some(gov) = crd.pointer_mut("/spec/governance/toolPolicyRef/name") {
        *gov = serde_json::Value::String(resolved.to_string());
    }

    if let Some(name) = parent_inference.filter(|s| !s.is_empty()) {
        if let Some(inf) = crd.pointer_mut("/spec/inferenceRef/name") {
            *inf = serde_json::Value::String(name.to_string());
        }
    }
}

/// A child cannot use a broader network posture than its parent. Copy the
/// parent's approved endpoint set and preserve Strict mode even when the spawn
/// request asks for learn mode; a child may narrow authority later, never
/// silently lose required approved access or relax the parent boundary.
pub(crate) fn apply_parent_network_policy(
    crd: &mut serde_json::Value,
    parent_endpoints: &[serde_json::Value],
    parent_egress_mode: Option<&str>,
    inherit_endpoints: bool,
) {
    let Some(network) = crd.pointer_mut("/spec/networkPolicy") else {
        return;
    };
    if inherit_endpoints && !parent_endpoints.is_empty() {
        network["allowedEndpoints"] = serde_json::Value::Array(parent_endpoints.to_vec());
    }
    if parent_egress_mode.is_some_and(|mode| mode.eq_ignore_ascii_case("Strict")) {
        network["egressMode"] = serde_json::Value::String("Strict".into());
    }
}

pub(crate) fn build_sub_agent_crd_with_labels(
    parent_name: &str,
    namespace: &str,
    isolation: &str,
    model: &str,
    req: &SpawnRequest,
    parent_labels: &BTreeMap<String, String>,
) -> serde_json::Value {
    // Dev profile: when running under `kars dev` (docker or local-k8s
    // — the parent's router was launched with `KARS_DEV_PROFILE=true`),
    // relax sub-agent CRD defaults so first-run UX doesn't trip on
    // novel egress targets or wait for operator approval. Production
    // AKS deployments leave this env unset and the strict defaults
    // (egressMode=Strict, approvalRequired=true) stand.
    let dev_profile = matches!(
        std::env::var("KARS_DEV_PROFILE").as_deref(),
        Ok("1" | "true" | "True" | "TRUE" | "yes")
    );
    let learn_egress = req.learn_egress || dev_profile;
    let approval_required = !dev_profile;

    // Determine the runtime kind for the child sandbox. Priority order:
    //   1. Explicit `runtime_kind` field in the spawn request (overrides
    //      everything — lets a Hermes parent request an OpenClaw child
    //      or vice versa).
    //   2. `KARS_RUNTIME_KIND` env on the parent (controller sets this
    //      on every v1 runtime container as part of the runtime contract
    //      — see controller/src/reconciler/mod.rs `KARS_RUNTIME_KIND`).
    //      This is the common case: child inherits parent's runtime.
    //   3. Hard-coded "OpenClaw" fallback for backward compat with
    //      pre-multi-runtime tests + the original OpenClaw-only world.
    //
    // The runtime kind string MUST match the controller's `RuntimeKind`
    // enum exactly (case-sensitive) or the CRD admission webhook will
    // reject the spawn with a strict-decoding error.
    let runtime_kind: String = req
        .runtime_kind
        .clone()
        .or_else(|| std::env::var("KARS_RUNTIME_KIND").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "OpenClaw".to_string());
    let runtime_variant_key: &str = match runtime_kind.as_str() {
        "Hermes" => "hermes",
        "OpenAIAgents" => "openaiAgents",
        "MAF" => "maf",
        _ => "openclaw",
    };

    let mut spec = serde_json::json!({
        "runtime": {
            "kind": runtime_kind,
            runtime_variant_key: {},
        },
        "inferenceRef": {
            "name": format!("{parent_name}-inference")
        },
        "sandbox": {
            "isolation": isolation,
            "readOnlyRootFilesystem": true,
            "runAsNonRoot": true,
            "allowPrivilegeEscalation": false,
        },
        "networkPolicy": {
            "defaultDeny": true,
            "approvalRequired": approval_required,
            "egressMode": if learn_egress { "Learn" } else { "Strict" },
        },
    });

    if req.token_budget_daily.is_some() || req.token_budget_per_request.is_some() {
        tracing::warn!(
            parent = %parent_name,
            child = %req.agent_id,
            "Per-sub-agent token budgets ignored — sub-agent inherits parent InferencePolicy '{parent_name}-inference'",
        );
    }

    {
        let mut gov = serde_json::json!({
            "enabled": true,
            "toolPolicyRef": { "name": format!("{parent_name}-toolpolicy") },
            "trustThreshold": req.trust_threshold.unwrap_or(500),
        });
        if let Some(ref peers) = req.trusted_peers {
            gov["trustedPeers"] = serde_json::json!(peers);
        }
        if req.handoff.is_some() {
            gov["registryMode"] = serde_json::json!("global");
        }
        spec["governance"] = gov;
    }

    let mut agent_tools: Vec<String> = Vec::new();
    if let Ok(tools) = std::env::var("FOUNDRY_AGENT_TOOLS") {
        agent_tools = tools
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if !agent_tools.is_empty() {
        spec["agent"] = serde_json::json!({ "tools": agent_tools });
    }

    let mut labels = inherit_parent_labels(parent_labels);
    if req.handoff.is_some() {
        labels.insert(
            "kars.azure.com/spawned-by".to_string(),
            "handoff".to_string(),
        );
        labels.insert(
            "kars.azure.com/predecessor".to_string(),
            parent_name.to_string(),
        );
    } else {
        labels.insert("kars.azure.com/parent".to_string(), parent_name.to_string());
        labels.insert("kars.azure.com/spawned-by".to_string(), "agent".to_string());
    }

    // Surface the optional persona/role on the CRD so:
    //   1. The parent's local peer roster (Hermes plugin) can recover
    //      it on restart by listing children with kars.azure.com/parent
    //      and reading kars.azure.com/role.
    //   2. Sibling discovery via `kubectl get karssandbox -l kars.azure.com/role=auditor`
    //      works out of the box without an AGT registry round-trip.
    //   3. The handoff snapshot path in this same file can restore
    //      role: when a parent re-spawns its children after restart
    //      (search "Restore role from the captured CRD labels too").
    //
    // The label is intentionally short (no kars.azure.com/persona-description
    // or similar) — long descriptions go on the spec.governance.persona
    // field if/when added, but the label-form is what RBAC and selectors
    // operate on. Skipped when no role provided.
    if let Some(role) = req.role.as_deref()
        && !role.trim().is_empty()
    {
        labels.insert(
            "kars.azure.com/role".to_string(),
            // K8s labels must be ≤63 chars and match
            // [a-z0-9A-Z]([-_.a-z0-9A-Z]{0,61}[a-z0-9A-Z])?  — apply a
            // best-effort sanitizer rather than rejecting (the LLM's
            // free-form persona shouldn't fail the spawn over a space).
            role.trim()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                        c
                    } else {
                        '-'
                    }
                })
                .take(63)
                .collect::<String>(),
        );
    }

    let mut annotations = BTreeMap::new();
    annotations.insert("kars.azure.com/model".to_string(), model.to_string());

    serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSandbox",
        "metadata": {
            "name": req.agent_id,
            "namespace": namespace,
            "labels": labels,
            "annotations": annotations,
        },
        "spec": spec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_resource_names_are_parent_scoped_and_bounded() {
        let first = scoped_child_name("team-a-run-123", "security-reviewer");
        let second = scoped_child_name("team-b-run-456", "security-reviewer");
        assert_ne!(first, second);
        assert_eq!(first, "team-a-run-123-security-reviewer");

        let long_parent = "p".repeat(63);
        let long = scoped_child_name(&long_parent, "browser-evidence-reviewer");
        assert!(long.len() <= 58);
        assert!(format!("kars-{long}").len() <= 63);
        assert_eq!(
            long,
            scoped_child_name(&long_parent, "browser-evidence-reviewer")
        );
    }

    #[test]
    fn spawn_identity_separates_resource_name_from_logical_agent_id() {
        let mut crd = serde_json::json!({
            "metadata": {
                "name": "security-reviewer",
                "annotations": {"kars.azure.com/model": "gpt-oss-120b"}
            }
        });
        apply_spawn_identity(
            &mut crd,
            "team-a-run-123-security-reviewer",
            "security-reviewer",
        );
        assert_eq!(crd["metadata"]["name"], "team-a-run-123-security-reviewer");
        assert_eq!(
            crd["metadata"]["annotations"][LOGICAL_AGENT_ID_ANNOTATION],
            "security-reviewer"
        );
        assert_eq!(
            crd["metadata"]["annotations"]["kars.azure.com/model"],
            "gpt-oss-120b"
        );
    }

    #[test]
    fn spawned_child_inherits_typed_principal_git_connection() {
        let mut crd = serde_json::json!({"metadata": {}, "spec": {}});
        apply_parent_git_write(
            &mut crd,
            "owner/repo,owner/second",
            Some("kars-github-connection-0123456789abcdef"),
        );
        assert_eq!(
            crd["spec"]["gitWrite"]["connectionConfigMapRef"]["name"],
            "kars-github-connection-0123456789abcdef"
        );
        assert_eq!(crd["spec"]["gitWrite"]["repos"][0], "owner/repo");
        assert!(crd["metadata"]["annotations"].is_null());
    }

    #[test]
    fn spawned_child_keeps_legacy_annotation_without_typed_connection() {
        let mut crd = serde_json::json!({"metadata": {}, "spec": {}});
        apply_parent_git_write(&mut crd, "owner/repo", None);
        assert_eq!(
            crd["metadata"]["annotations"]["kars.azure.com/git-write-repos"],
            "owner/repo"
        );
        assert!(crd["spec"]["gitWrite"].is_null());
    }

    #[test]
    fn spawn_request_rejects_unknown_fields() {
        // deny_unknown_fields — a typo in the client payload must fail loudly
        // instead of silently ignoring the intended value.
        let payload = r#"{
            "agent_id": "child",
            "modl": "gpt-4o"
        }"#;
        let err = serde_json::from_str::<SpawnRequest>(payload).unwrap_err();
        assert!(
            err.to_string().contains("unknown field"),
            "expected unknown-field error, got: {err}"
        );
    }

    #[test]
    fn spawn_request_accepts_canonical_agent_id() {
        let payload = r#"{
            "agent_id": "child",
            "model": "gpt-4o",
            "governance": true,
            "trust_threshold": 500
        }"#;
        let req: SpawnRequest = serde_json::from_str(payload).unwrap();
        assert_eq!(req.agent_id, "child");
        assert_eq!(req.model.as_deref(), Some("gpt-4o"));
        assert_eq!(req.trust_threshold, Some(500));
    }

    #[test]
    fn spawn_request_accepts_legacy_name_alias() {
        // Backward compatibility: plugins still in-flight may send `name`.
        // serde(alias = "name") lets them keep working during migration.
        let payload = r#"{
            "name": "child",
            "model": "gpt-4o"
        }"#;
        let req: SpawnRequest = serde_json::from_str(payload).unwrap();
        assert_eq!(req.agent_id, "child");
    }

    #[test]
    fn spawn_request_rejects_both_name_and_agent_id() {
        // If both fields are present, serde treats it as a duplicate and errors.
        // This guards against a client sending inconsistent values.
        let payload = r#"{
            "agent_id": "one",
            "name": "two"
        }"#;
        let err = serde_json::from_str::<SpawnRequest>(payload).unwrap_err();
        assert!(
            err.to_string().contains("duplicate field"),
            "expected duplicate-field error, got: {err}"
        );
    }

    #[test]
    fn handoff_meta_rejects_unknown_fields() {
        let payload = r#"{"mode":"restore","predecessor":"p","extra":"smuggled"}"#;
        let err = serde_json::from_str::<HandoffMeta>(payload).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn apply_owner_reference_anchors_child_to_parent() {
        let mut crd = serde_json::json!({
            "metadata": { "name": "child", "namespace": "kars-system" }
        });
        apply_owner_reference(&mut crd, "orch-run-123", "uid-abc");
        let owner = &crd["metadata"]["ownerReferences"][0];
        assert_eq!(owner["kind"], "KarsSandbox");
        assert_eq!(owner["name"], "orch-run-123");
        assert_eq!(owner["uid"], "uid-abc");
        assert_eq!(owner["controller"], false);
        assert_eq!(owner["blockOwnerDeletion"], false);
    }

    #[test]
    fn apply_parent_refs_inherits_shared_policy() {
        // Parent references a SHARED policy (kars-default) — the child must
        // inherit that real name, not the convention-derived phantom.
        let mut crd = serde_json::json!({
            "spec": {
                "inferenceRef": { "name": "child-inference" },
                "governance": { "toolPolicyRef": { "name": "child-toolpolicy" } }
            }
        });
        apply_parent_refs(&mut crd, Some("kars-default"), Some("shared-inference"));
        assert_eq!(
            crd["spec"]["governance"]["toolPolicyRef"]["name"],
            "kars-default"
        );
        assert_eq!(crd["spec"]["inferenceRef"]["name"], "shared-inference");
    }

    #[test]
    fn apply_parent_refs_falls_back_to_default_when_parent_has_none() {
        // Parent carries no explicit tool policy (team runs have no governance
        // block). The child must NOT keep the derived `{parent}-toolpolicy`
        // (which does not exist → 404 Degraded), nor drop it (the CRD CEL rule
        // requires it when governance.enabled=true). It falls back to the
        // cluster-wide `kars-default`, which is guaranteed present.
        let mut crd = serde_json::json!({
            "spec": {
                "inferenceRef": { "name": "child-inference" },
                "governance": {
                    "enabled": true,
                    "toolPolicyRef": { "name": "orch-run-123-toolpolicy" }
                }
            }
        });
        apply_parent_refs(&mut crd, None, None);
        assert_eq!(
            crd["spec"]["governance"]["toolPolicyRef"]["name"], "kars-default",
            "must fall back to kars-default, satisfying the enabled⇒policy CEL rule"
        );
        assert_eq!(crd["spec"]["governance"]["enabled"], true);
        assert_eq!(crd["spec"]["inferenceRef"]["name"], "child-inference");
    }

    #[test]
    fn child_inherits_parent_approved_egress() {
        let mut crd = serde_json::json!({
            "spec": {
                "networkPolicy": {
                    "defaultDeny": true,
                    "egressMode": "Strict"
                }
            }
        });
        let endpoints = vec![
            serde_json::json!({"host": "mcp.deepwiki.com", "port": 443}),
            serde_json::json!({"host": "kubernetes.io"}),
        ];
        apply_parent_network_policy(&mut crd, &endpoints, Some("Strict"), true);
        assert_eq!(
            crd["spec"]["networkPolicy"]["allowedEndpoints"],
            serde_json::Value::Array(endpoints)
        );
    }

    #[test]
    fn strict_parent_cannot_be_relaxed_by_child_request() {
        let mut crd = serde_json::json!({
            "spec": {
                "networkPolicy": {
                    "defaultDeny": true,
                    "egressMode": "Learn"
                }
            }
        });
        let endpoints = vec![serde_json::json!({"host": "kubernetes.io"})];
        apply_parent_network_policy(&mut crd, &endpoints, Some("Strict"), false);
        assert_eq!(crd["spec"]["networkPolicy"]["egressMode"], "Strict");
        assert!(
            crd["spec"]["networkPolicy"]
                .get("allowedEndpoints")
                .is_none(),
            "request mode must not copy parent business egress"
        );
    }

    fn minimal_req(agent_id: &str) -> SpawnRequest {
        SpawnRequest {
            agent_id: agent_id.into(),
            model: None,
            governance: true,
            trust_threshold: None,
            learn_egress: false,
            inherit_parent_egress: false,
            auto_inherit_team_egress: false,
            isolation: None,
            token_budget_daily: None,
            token_budget_per_request: None,
            trusted_peers: None,
            handoff: None,
            runtime_kind: None,
            role: None,
        }
    }

    #[test]
    fn sub_agent_crd_uses_post_s10_s13_shape() {
        // Audit class-of-bug guard: the JSON we send to the API server
        // MUST use the post-S10/S13 shape. The legacy shape is silently
        // pruned by clusters whose CRD doesn't have
        // `additionalProperties: false`, but rejected at admission on
        // strict clusters — surfacing as a 422 at spawn time. This test
        // catches reverts to the legacy shape at `cargo test` time.
        //
        // Take the env lock so parallel tests that set KARS_RUNTIME_KIND
        // don't bleed into our assertion that the default is OpenClaw.
        let (lock, prior_env) = lock_env_for_default_assertion();
        let crd = build_sub_agent_crd_with_labels(
            "azclaw2",
            "kars-system",
            "enhanced",
            "gpt-5.4",
            &minimal_req("viz"),
            &BTreeMap::new(),
        );
        drop(lock);
        restore_env(prior_env);

        // 1. Top-level
        assert_eq!(crd["apiVersion"], "kars.azure.com/v1alpha1");
        assert_eq!(crd["kind"], "KarsSandbox");
        assert_eq!(
            crd["metadata"]["annotations"]["kars.azure.com/model"],
            "gpt-5.4"
        );

        let spec = &crd["spec"];

        // 2. Required post-S10/S13 fields present
        assert_eq!(spec["runtime"]["kind"], "OpenClaw");
        assert!(spec["runtime"]["openclaw"].is_object());
        assert_eq!(spec["inferenceRef"]["name"], "azclaw2-inference");
        assert_eq!(
            spec["governance"]["toolPolicyRef"]["name"],
            "azclaw2-toolpolicy"
        );

        // 3. Legacy fields absent (the audit's class of bugs)
        assert!(spec.get("openclaw").is_none(), "legacy spec.openclaw");
        assert!(spec.get("inference").is_none(), "legacy spec.inference");
        assert!(spec.get("model").is_none(), "legacy spec.model");
        assert!(
            spec.get("handoff").is_none(),
            "legacy top-level spec.handoff"
        );
        assert!(
            spec["governance"].get("toolPolicy").is_none(),
            "legacy governance.toolPolicy (string field)"
        );
    }

    #[test]
    fn sub_agent_inherits_parent_runtime_kind_from_env() {
        // Hermes parent → Hermes child via KARS_RUNTIME_KIND env on the
        // router. Without this, every Hermes parent silently spawns
        // OpenClaw children, breaking the Hermes-only mesh assumption
        // (different runtimes can still mesh, but the operator UX shows
        // a mixed tree which is wrong for "all-Hermes" scenarios).
        let _guard = serial_env_set("KARS_RUNTIME_KIND", "Hermes");
        let req = minimal_req("hermes-child");
        let crd = build_sub_agent_crd_with_labels(
            "hermes-parent",
            "kars-system",
            "standard",
            "gpt-5.4",
            &req,
            &BTreeMap::new(),
        );
        assert_eq!(crd["spec"]["runtime"]["kind"], "Hermes");
        assert!(
            crd["spec"]["runtime"]["hermes"].is_object(),
            "expected runtime.hermes variant object, got: {}",
            crd["spec"]["runtime"]
        );
        // Belt-and-braces: NO openclaw key on the runtime object for a
        // Hermes child (would otherwise trip strict CRD admission).
        assert!(
            crd["spec"]["runtime"]["openclaw"].is_null(),
            "runtime.openclaw must be absent for Hermes child"
        );
    }

    #[test]
    fn explicit_runtime_kind_request_overrides_env() {
        // An explicit field on SpawnRequest always wins — lets a
        // multi-runtime orchestrator override per child.
        let _guard = serial_env_set("KARS_RUNTIME_KIND", "Hermes");
        let mut req = minimal_req("oc-child");
        req.runtime_kind = Some("OpenClaw".to_string());
        let crd = build_sub_agent_crd_with_labels(
            "hermes-parent",
            "kars-system",
            "standard",
            "gpt-5.4",
            &req,
            &BTreeMap::new(),
        );
        assert_eq!(crd["spec"]["runtime"]["kind"], "OpenClaw");
        assert!(crd["spec"]["runtime"]["openclaw"].is_object());
    }

    /// Tiny RAII helper: set an env var for the duration of one test
    /// then restore the prior value. Necessary because the workspace
    /// runs `cargo test` with multiple parallel threads.
    ///
    /// IMPORTANT: combine with `_ENV_TEST_LOCK` to serialize tests that
    /// read/write `KARS_RUNTIME_KIND` — otherwise a parallel test can
    /// observe an env value set by another test and produce false
    /// negatives like `sub_agent_crd_uses_post_s10_s13_shape` seeing
    /// `runtime.kind == "Hermes"` instead of the default `"OpenClaw"`.
    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn serial_env_set(key: &'static str, val: &str) -> EnvGuard {
        let lock = ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior = std::env::var(key).ok();
        unsafe { std::env::set_var(key, val) };
        EnvGuard {
            key,
            prior,
            _lock: lock,
        }
    }
    /// Acquire the lock without changing any env — for tests that
    /// must assert the DEFAULT (unset) behaviour while parallel tests
    /// may otherwise have set the var.
    fn lock_env_for_default_assertion() -> (std::sync::MutexGuard<'static, ()>, Option<String>) {
        let lock = ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior = std::env::var("KARS_RUNTIME_KIND").ok();
        unsafe { std::env::remove_var("KARS_RUNTIME_KIND") };
        (lock, prior)
    }
    /// Restore env captured by `lock_env_for_default_assertion`.
    fn restore_env(prior: Option<String>) {
        match prior {
            Some(v) => unsafe { std::env::set_var("KARS_RUNTIME_KIND", v) },
            None => unsafe { std::env::remove_var("KARS_RUNTIME_KIND") },
        }
    }

    #[test]
    fn handoff_target_crd_uses_canonical_shape_and_labels() {
        // Same as above but for the handoff path — labels diverge but the
        // schema-required keys must be identical.
        let mut req = minimal_req("azclaw2-cloud");
        req.handoff = Some(HandoffMeta {
            mode: "restore".into(),
            predecessor: Some("azclaw2".into()),
        });
        let crd = build_sub_agent_crd_with_labels(
            "azclaw2",
            "kars-system",
            "enhanced",
            "gpt-5.4",
            &req,
            &BTreeMap::new(),
        );

        assert_eq!(crd["apiVersion"], "kars.azure.com/v1alpha1");
        assert_eq!(
            crd["metadata"]["labels"]["kars.azure.com/spawned-by"],
            "handoff"
        );
        assert_eq!(
            crd["metadata"]["labels"]["kars.azure.com/predecessor"],
            "azclaw2"
        );
        // Handoff MUST request global registry mode for mesh comms.
        assert_eq!(crd["spec"]["governance"]["registryMode"], "global");
        assert_eq!(crd["spec"]["inferenceRef"]["name"], "azclaw2-inference");
        // Legacy must still be absent.
        assert!(crd["spec"].get("handoff").is_none());
        assert!(crd["spec"].get("model").is_none());
    }

    #[test]
    fn sub_agent_crd_never_inherits_memory_ref() {
        // No-inherit invariant for KarsMemory (Slice 3a/3b):
        // a parent's compiled memory binding must NEVER flow through
        // sub-agent spawn or handoff. The builder takes no
        // `memory_ref` input from `SpawnRequest`, and `spec.memoryRef`
        // must be absent on the built CRD — period. This test pins
        // the invariant so a future field addition can't silently
        // break it.
        //
        // Both the regular spawn path and the handoff path are
        // exercised.
        for handoff in [None, Some("predecessor-x")] {
            let mut req = minimal_req("child");
            if let Some(predecessor) = handoff {
                req.handoff = Some(HandoffMeta {
                    mode: "restore".into(),
                    predecessor: Some(predecessor.into()),
                });
            }
            let crd = build_sub_agent_crd_with_labels(
                "parent-with-memory",
                "kars-parent",
                "default",
                "gpt-5.4",
                &req,
                &BTreeMap::new(),
            );
            assert!(
                crd["spec"].get("memoryRef").is_none(),
                "spec.memoryRef leaked into spawned sub-agent CRD (handoff={handoff:?}); \
                 KarsMemory bindings must not inherit (Slice 3a no-inherit rule)"
            );
            // Belt-and-suspenders: governance block must also not
            // carry a memoryRef.
            assert!(
                crd["spec"]["governance"].get("memoryRef").is_none(),
                "memoryRef snuck into spec.governance — same Slice 3a invariant applies"
            );
        }
    }

    // ── Slice 2 DoD #6 — parent label inheritance ────────────────────────

    #[test]
    fn inherit_parent_labels_drops_kars_controlled_keys() {
        let mut parent = BTreeMap::new();
        parent.insert("tier".to_string(), "prod".to_string());
        parent.insert("team".to_string(), "payments".to_string());
        parent.insert(
            "kars.azure.com/parent".to_string(),
            "grandparent".to_string(),
        );
        parent.insert("kars.azure.com/spawned-by".to_string(), "agent".to_string());
        parent.insert(
            "app.kubernetes.io/managed-by".to_string(),
            "controller".to_string(),
        );

        let inherited = inherit_parent_labels(&parent);

        assert_eq!(inherited.get("tier"), Some(&"prod".to_string()));
        assert_eq!(inherited.get("team"), Some(&"payments".to_string()));
        assert!(
            !inherited.contains_key("kars.azure.com/parent"),
            "kars-controlled label leaked: child must re-stamp its own parent ref"
        );
        assert!(
            !inherited.contains_key("kars.azure.com/spawned-by"),
            "kars-controlled spawned-by leaked: child role depends on the spawn call, not the parent's"
        );
        assert!(
            !inherited.contains_key("app.kubernetes.io/managed-by"),
            "k8s tracking label leaked"
        );
    }

    #[test]
    fn child_crd_inherits_user_labels_from_parent() {
        // The headline DoD #6 case: parent has labels.tier=prod;
        // child CR must come out with labels.tier=prod even though
        // the spawn request never mentions it.
        let mut parent_labels = BTreeMap::new();
        parent_labels.insert("tier".to_string(), "prod".to_string());
        parent_labels.insert("env".to_string(), "staging".to_string());

        let crd = build_sub_agent_crd_with_labels(
            "azclaw-parent",
            "kars-system",
            "enhanced",
            "gpt-5.4",
            &minimal_req("child"),
            &parent_labels,
        );

        let child_labels = &crd["metadata"]["labels"];
        assert_eq!(child_labels["tier"], "prod");
        assert_eq!(child_labels["env"], "staging");
        // Spawn-tracking labels must coexist with inherited ones.
        assert_eq!(child_labels["kars.azure.com/parent"], "azclaw-parent");
        assert_eq!(child_labels["kars.azure.com/spawned-by"], "agent");
    }

    #[test]
    fn handoff_child_also_inherits_user_labels() {
        // Handoff path takes a different branch in build_sub_agent_crd_with_labels
        // (predecessor instead of parent). The label-inheritance
        // behaviour must hold there too — operators don't care
        // whether the child arrived via spawn or via handoff; they
        // want their `tier=prod` tag to follow it.
        let mut parent_labels = BTreeMap::new();
        parent_labels.insert("tier".to_string(), "prod".to_string());

        let mut req = minimal_req("cloud-child");
        req.handoff = Some(HandoffMeta {
            mode: "restore".into(),
            predecessor: Some("local-parent".into()),
        });

        let crd = build_sub_agent_crd_with_labels(
            "local-parent",
            "kars-system",
            "enhanced",
            "gpt-5.4",
            &req,
            &parent_labels,
        );

        let child_labels = &crd["metadata"]["labels"];
        assert_eq!(child_labels["tier"], "prod");
        assert_eq!(child_labels["kars.azure.com/spawned-by"], "handoff");
        assert_eq!(child_labels["kars.azure.com/predecessor"], "local-parent");
        // The handoff path intentionally omits the `parent` label
        // (predecessor takes its place semantically) — make sure
        // inheritance does not accidentally restore it.
        assert!(
            child_labels.get("kars.azure.com/parent").is_none()
                || child_labels["kars.azure.com/parent"].is_null(),
            "handoff path must not stamp the `parent` label"
        );
    }

    #[test]
    fn spawn_tracking_labels_win_over_parent_labels_on_collision() {
        // Defence-in-depth: if a parent somehow carried
        // `kars.azure.com/parent=evil` (shouldn't happen — we
        // filter it — but belt-and-suspenders), the child's
        // re-stamped value must win. This pins the ordering.
        let mut parent_labels = BTreeMap::new();
        parent_labels.insert("kars.azure.com/parent".to_string(), "evil".to_string());
        parent_labels.insert("tier".to_string(), "prod".to_string());

        let crd = build_sub_agent_crd_with_labels(
            "real-parent",
            "kars-system",
            "enhanced",
            "gpt-5.4",
            &minimal_req("child"),
            &parent_labels,
        );

        assert_eq!(
            crd["metadata"]["labels"]["kars.azure.com/parent"], "real-parent",
            "spawn-tracking label must win on collision"
        );
        assert_eq!(crd["metadata"]["labels"]["tier"], "prod");
    }

    #[test]
    fn empty_parent_labels_is_a_noop() {
        // The fallback path when parent fetch fails: empty map in,
        // child CR comes out with only the spawn-tracking labels.
        // This pins that the inheritance code path doesn't crash or
        // add spurious keys on the no-labels case.
        let crd = build_sub_agent_crd_with_labels(
            "parent",
            "kars-system",
            "enhanced",
            "gpt-5.4",
            &minimal_req("child"),
            &BTreeMap::new(),
        );

        let labels = crd["metadata"]["labels"]
            .as_object()
            .expect("labels must be an object");
        // Exactly the two spawn-tracking keys, nothing else.
        assert_eq!(labels.len(), 2);
        assert!(labels.contains_key("kars.azure.com/parent"));
        assert!(labels.contains_key("kars.azure.com/spawned-by"));
    }
}
