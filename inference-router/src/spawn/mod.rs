// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Sandbox spawn — create/list/delete ClawSandbox sub-agents via K8s API.
//!
//! The agent inside a sandbox has no kubectl or CLI access. This module exposes
//! HTTP endpoints that the plugin's `/azureclaw-spawn` slash command calls to
//! manage sub-agent sandboxes through the pod's ServiceAccount.

use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::{
    Api, Client, ResourceExt,
    api::{DynamicObject, ListParams, Patch, PatchParams, PostParams},
    discovery::ApiResource,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod docker;
pub use docker::{delete_sandbox_docker, list_sandboxes_docker};

fn default_true() -> bool {
    true
}

fn claw_sandbox_api_resource() -> ApiResource {
    ApiResource {
        group: "azureclaw.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "azureclaw.azure.com/v1alpha1".into(),
        kind: "ClawSandbox".into(),
        plural: "clawsandboxes".into(),
    }
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
    pub namespace: Option<String>,
    pub phase: Option<String>,
    pub model: Option<String>,
    pub governance: bool,
}

/// Create a ClawSandbox CRD for a sub-agent, or a Docker container in dev mode.
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
    let is_dev = std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true";
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

    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &claw_sandbox_api_resource());

    let model = req.model.as_deref().unwrap_or("gpt-4.1");
    let parent_isolation = std::env::var("SANDBOX_ISOLATION").unwrap_or_else(|_| "enhanced".into());
    let isolation = req.isolation.as_deref().unwrap_or(&parent_isolation);

    // Prevent downgrading from confidential parent
    if parent_isolation == "confidential" && isolation != "confidential" {
        return Err(format!(
            "Cannot spawn '{}' sub-agent from confidential parent — sub-agents must also be confidential",
            isolation,
        ));
    }

    // Build spec
    let mut spec = serde_json::json!({
        "openclaw": {
            "version": "2026.3.13",
            "config": {
                "agent": {
                    "model": format!("azure/{model}")
                }
            }
        },
        "sandbox": {
            "isolation": isolation,
            "readOnlyRootFilesystem": true,
            "runAsNonRoot": true,
            "allowPrivilegeEscalation": false,
        },
        "inference": {
            "provider": "azure-ai-foundry",
            "model": model,
            "contentSafety": true,
            "promptShields": true,
        },
        "networkPolicy": {
            "defaultDeny": true,
            "approvalRequired": true,
            "learnEgress": req.learn_egress,
        },
    });

    // Add token budget if specified
    if req.token_budget_daily.is_some() || req.token_budget_per_request.is_some() {
        let mut budget = serde_json::Map::new();
        if let Some(d) = req.token_budget_daily {
            budget.insert("daily".into(), serde_json::json!(d));
        }
        if let Some(p) = req.token_budget_per_request {
            budget.insert("perRequest".into(), serde_json::json!(p));
        }
        spec["inference"]["tokenBudget"] = serde_json::Value::Object(budget);
    }

    // Governance is always enabled (native in router)
    {
        let mut gov = serde_json::json!({
            "enabled": true,
            "toolPolicy": "default",
            "trustThreshold": req.trust_threshold.unwrap_or(500),
        });
        // Propagate trusted peers so the target auto-trusts the source at KNOCK time
        if let Some(ref peers) = req.trusted_peers {
            gov["trustedPeers"] = serde_json::json!(peers);
        }
        // Handoff targets need global registry mode for mesh communication
        if req.handoff.is_some() {
            gov["registryMode"] = serde_json::json!("global");
        }
        spec["governance"] = gov;
    }

    // Propagate Foundry agent tools from parent environment
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

    // Build labels — handoff targets use different labels than sub-agents
    let mut labels = BTreeMap::new();
    if req.handoff.is_some() {
        labels.insert(
            "azureclaw.azure.com/spawned-by".to_string(),
            "handoff".to_string(),
        );
        labels.insert(
            "azureclaw.azure.com/predecessor".to_string(),
            parent_name.to_string(),
        );
    } else {
        labels.insert(
            "azureclaw.azure.com/parent".to_string(),
            parent_name.to_string(),
        );
        labels.insert(
            "azureclaw.azure.com/spawned-by".to_string(),
            "agent".to_string(),
        );
    }

    let crd = serde_json::json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawSandbox",
        "metadata": {
            "name": req.agent_id,
            "namespace": namespace,
            "labels": labels,
        },
        "spec": spec,
    });

    let obj: kube::api::DynamicObject =
        serde_json::from_value(crd).map_err(|e| format!("Failed to build CRD: {e}"))?;

    match api.create(&PostParams::default(), &obj).await {
        Ok(_created) => {
            tracing::info!(parent = %parent_name, child = %req.agent_id, "Sub-agent sandbox created");

            // For handoff targets, propagate channel/plugin credentials to the
            // target namespace so the cloud agent gets Telegram, Slack, etc.
            if req.handoff.is_some() {
                let child_name = req.agent_id.clone();
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
                namespace: Some(format!("azureclaw-{}", req.agent_id)),
                phase: Some("Pending".into()),
                message: Some(format!(
                    "Sub-agent '{}' spawned (model: {}, governance: {}). Use AGT mesh to communicate.",
                    req.agent_id, model, req.governance
                )),
            })
        }
        Err(kube::Error::Api(resp)) if resp.code == 409 => {
            // Already exists — reuse rather than error
            tracing::info!(parent = %parent_name, child = %req.agent_id, "Sub-agent sandbox already exists — reusing");
            Ok(SpawnResponse {
                status: "created".into(),
                agent_id: req.agent_id.clone(),
                namespace: Some(format!("azureclaw-{}", req.agent_id)),
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

    let target_ns = format!("azureclaw-{}", child_name);
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
                "azureclaw.azure.com/managed-by": "handoff",
                "azureclaw.azure.com/predecessor": std::env::var("SANDBOX_NAME").unwrap_or_default(),
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
            &PatchParams::apply("azureclaw-handoff"),
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

    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &claw_sandbox_api_resource());

    let lp = ListParams::default().labels(&format!("azureclaw.azure.com/parent={parent_name}"));

    let list = api
        .list(&lp)
        .await
        .map_err(|e| format!("Failed to list sandboxes: {e}"))?;

    let entries: Vec<SubAgentEntry> = list
        .items
        .iter()
        .map(|obj| {
            let name = obj.name_any();
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
                .get("spec")
                .and_then(|s| s.get("inference"))
                .and_then(|i| i.get("model"))
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
pub async fn get_sandbox_status(name: &str) -> Result<SpawnResponse, String> {
    // Dev mode: query Docker Engine API instead of K8s
    if std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true" {
        return docker::get_sandbox_status_docker(name).await;
    }

    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client error: {e}"))?;

    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &claw_sandbox_api_resource());

    let obj = api
        .get(name)
        .await
        .map_err(|e| format!("Sandbox '{}' not found: {e}", name))?;
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

    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &claw_sandbox_api_resource());

    // Verify the sandbox was spawned by this parent (prevent deleting others' sandboxes)
    let obj = api
        .get(name)
        .await
        .map_err(|e| format!("Sandbox '{}' not found: {e}", name))?;
    let labels = obj.metadata.labels.as_ref();
    let actual_parent = labels
        .and_then(|l| l.get("azureclaw.azure.com/parent"))
        .map(String::as_str);

    if actual_parent != Some(parent_name) {
        return Err(format!(
            "Sandbox '{}' was not spawned by '{}' — cannot delete",
            name, parent_name
        ));
    }

    api.delete(name, &Default::default())
        .await
        .map_err(|e| format!("Failed to delete: {e}"))?;

    tracing::info!(parent = %parent_name, child = %name, "Sub-agent sandbox deleted");
    Ok(SpawnResponse {
        status: "deleted".into(),
        agent_id: name.to_string(),
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
    if std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true" {
        return docker::collect_sub_agent_snapshots_docker(parent_name).await;
    }

    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client error: {e}"))?;

    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client, &namespace, &claw_sandbox_api_resource());

    let lp = ListParams::default().labels(&format!("azureclaw.azure.com/parent={parent_name}"));
    let list = api
        .list(&lp)
        .await
        .map_err(|e| format!("Failed to list sub-agents: {e}"))?;

    let mut snapshots = Vec::new();

    for obj in &list.items {
        let name = obj.name_any();
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

        // Reconstruct SpawnRequest from CRD spec
        let model = spec
            .get("inference")
            .and_then(|i| i.get("model"))
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
            .and_then(|n| n.get("learnEgress"))
            .and_then(|l| l.as_bool())
            .unwrap_or(false);

        let isolation = spec
            .get("sandbox")
            .and_then(|s| s.get("isolation"))
            .and_then(|i| i.as_str())
            .map(String::from);

        let token_budget_daily = spec
            .get("inference")
            .and_then(|i| i.get("tokenBudget"))
            .and_then(|b| b.get("daily"))
            .and_then(|d| d.as_i64());

        let token_budget_per_request = spec
            .get("inference")
            .and_then(|i| i.get("tokenBudget"))
            .and_then(|b| b.get("perRequest"))
            .and_then(|p| p.as_i64());

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
            isolation,
            token_budget_daily,
            token_budget_per_request,
            trusted_peers,
            handoff: None, // Not a handoff spawn — regular sub-agent re-spawn
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
