//! Sandbox spawn — create/list/delete ClawSandbox sub-agents via K8s API.
//!
//! The agent inside a sandbox has no kubectl or CLI access. This module exposes
//! HTTP endpoints that the plugin's `/azureclaw-spawn` slash command calls to
//! manage sub-agent sandboxes through the pod's ServiceAccount.

use kube::{
    Api, Client, ResourceExt,
    api::{DynamicObject, ListParams, PostParams},
    discovery::ApiResource,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
#[derive(Debug, Deserialize)]
pub struct SpawnRequest {
    /// Name for the sub-agent sandbox (must be DNS-safe).
    pub name: String,
    /// Model deployment to use (default: gpt-4.1).
    pub model: Option<String>,
    /// Enable AGT governance (default: false).
    #[serde(default)]
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
}

/// Response from spawn/status endpoints.
#[derive(Debug, Serialize)]
pub struct SpawnResponse {
    pub status: String,
    pub name: String,
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
    pub name: String,
    pub namespace: Option<String>,
    pub phase: Option<String>,
    pub model: Option<String>,
    pub governance: bool,
}

/// Create a ClawSandbox CRD for a sub-agent.
pub async fn create_sandbox(
    parent_name: &str,
    req: &SpawnRequest,
) -> Result<SpawnResponse, String> {
    // Validate name: must be DNS-safe
    if req.name.is_empty() || req.name.len() > 63 {
        return Err("name must be 1-63 characters".into());
    }
    if !req.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("name must contain only lowercase alphanumeric characters and hyphens".into());
    }
    if req.name.starts_with('-') || req.name.ends_with('-') {
        return Err("name must not start or end with a hyphen".into());
    }

    let client = Client::try_default().await.map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> = Api::namespaced_with(
        client,
        &namespace,
        &claw_sandbox_api_resource(),
    );

    let model = req.model.as_deref().unwrap_or("gpt-4.1");
    let isolation = req.isolation.as_deref().unwrap_or("enhanced");

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

    // Add governance if enabled
    if req.governance {
        spec["governance"] = serde_json::json!({
            "enabled": true,
            "toolPolicy": "default",
            "trustThreshold": req.trust_threshold.unwrap_or(500),
        });
    }

    // Build labels
    let mut labels = BTreeMap::new();
    labels.insert("azureclaw.azure.com/parent".to_string(), parent_name.to_string());
    labels.insert("azureclaw.azure.com/spawned-by".to_string(), "agent".to_string());

    let crd = serde_json::json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawSandbox",
        "metadata": {
            "name": req.name,
            "namespace": namespace,
            "labels": labels,
        },
        "spec": spec,
    });

    let obj: kube::api::DynamicObject = serde_json::from_value(crd)
        .map_err(|e| format!("Failed to build CRD: {e}"))?;

    match api.create(&PostParams::default(), &obj).await {
        Ok(_created) => {
            tracing::info!(parent = %parent_name, child = %req.name, "Sub-agent sandbox created");
            Ok(SpawnResponse {
                status: "created".into(),
                name: req.name.clone(),
                namespace: Some(format!("azureclaw-{}", req.name)),
                phase: Some("Pending".into()),
                message: Some(format!(
                    "Sub-agent '{}' spawned (model: {}, governance: {}). Use AGT mesh to communicate.",
                    req.name, model, req.governance
                )),
            })
        }
        Err(kube::Error::Api(resp)) if resp.code == 409 => {
            Err(format!("Sandbox '{}' already exists", req.name))
        }
        Err(e) => {
            tracing::error!(parent = %parent_name, child = %req.name, "Failed to create sandbox: {e}");
            Err(format!("Failed to create sandbox: {e}"))
        }
    }
}

/// List sub-agents spawned by a parent sandbox.
pub async fn list_sandboxes(parent_name: &str) -> Result<Vec<SubAgentEntry>, String> {
    let client = Client::try_default().await.map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> = Api::namespaced_with(
        client,
        &namespace,
        &claw_sandbox_api_resource(),
    );

    let lp = ListParams::default()
        .labels(&format!("azureclaw.azure.com/parent={parent_name}"));

    let list = api.list(&lp).await.map_err(|e| format!("Failed to list sandboxes: {e}"))?;

    let entries: Vec<SubAgentEntry> = list.items.iter().map(|obj| {
        let name = obj.name_any();
        let data = &obj.data;

        let phase = data.get("status")
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .map(String::from);

        let ns = data.get("status")
            .and_then(|s| s.get("namespace"))
            .and_then(|n| n.as_str())
            .map(String::from);

        let model = data.get("spec")
            .and_then(|s| s.get("inference"))
            .and_then(|i| i.get("model"))
            .and_then(|m| m.as_str())
            .map(String::from);

        let governance = data.get("spec")
            .and_then(|s| s.get("governance"))
            .and_then(|g| g.get("enabled"))
            .and_then(|e| e.as_bool())
            .unwrap_or(false);

        SubAgentEntry { name, namespace: ns, phase, model, governance }
    }).collect();

    Ok(entries)
}

/// Get status of a specific sub-agent sandbox.
pub async fn get_sandbox_status(name: &str) -> Result<SpawnResponse, String> {
    let client = Client::try_default().await.map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> = Api::namespaced_with(
        client,
        &namespace,
        &claw_sandbox_api_resource(),
    );

    let obj = api.get(name).await.map_err(|e| format!("Sandbox '{}' not found: {e}", name))?;
    let data = &obj.data;

    let phase = data.get("status")
        .and_then(|s| s.get("phase"))
        .and_then(|p| p.as_str())
        .map(String::from);

    let ns = data.get("status")
        .and_then(|s| s.get("namespace"))
        .and_then(|n| n.as_str())
        .map(String::from);

    Ok(SpawnResponse {
        status: "ok".into(),
        name: name.to_string(),
        namespace: ns,
        phase,
        message: None,
    })
}

/// Delete a sub-agent sandbox.
pub async fn delete_sandbox(parent_name: &str, name: &str) -> Result<SpawnResponse, String> {
    let client = Client::try_default().await.map_err(|e| format!("K8s client error: {e}"))?;

    let namespace = std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api: Api<DynamicObject> = Api::namespaced_with(
        client,
        &namespace,
        &claw_sandbox_api_resource(),
    );

    // Verify the sandbox was spawned by this parent (prevent deleting others' sandboxes)
    let obj = api.get(name).await.map_err(|e| format!("Sandbox '{}' not found: {e}", name))?;
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

    api.delete(name, &Default::default()).await.map_err(|e| format!("Failed to delete: {e}"))?;

    tracing::info!(parent = %parent_name, child = %name, "Sub-agent sandbox deleted");
    Ok(SpawnResponse {
        status: "deleted".into(),
        name: name.to_string(),
        namespace: None,
        phase: Some("Terminating".into()),
        message: Some(format!("Sub-agent '{}' is being torn down", name)),
    })
}
