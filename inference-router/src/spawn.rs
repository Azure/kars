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
#[derive(Debug, Deserialize)]
pub struct SpawnRequest {
    /// Name for the sub-agent sandbox (must be DNS-safe).
    pub name: String,
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

/// Create a ClawSandbox CRD for a sub-agent, or a Docker container in dev mode.
pub async fn create_sandbox(
    parent_name: &str,
    req: &SpawnRequest,
) -> Result<SpawnResponse, String> {
    // Validate name: must be DNS-safe
    if req.name.is_empty() || req.name.len() > 63 {
        return Err("name must be 1-63 characters".into());
    }
    if !req
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("name must contain only lowercase alphanumeric characters and hyphens".into());
    }
    if req.name.starts_with('-') || req.name.ends_with('-') {
        return Err("name must not start or end with a hyphen".into());
    }

    // Dev mode: spawn sibling Docker container instead of K8s CRD
    if std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true" {
        return create_sandbox_docker(parent_name, req).await;
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

    // Add governance if enabled
    if req.governance {
        spec["governance"] = serde_json::json!({
            "enabled": true,
            "toolPolicy": "default",
            "trustThreshold": req.trust_threshold.unwrap_or(500),
        });
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

    // Build labels
    let mut labels = BTreeMap::new();
    labels.insert(
        "azureclaw.azure.com/parent".to_string(),
        parent_name.to_string(),
    );
    labels.insert(
        "azureclaw.azure.com/spawned-by".to_string(),
        "agent".to_string(),
    );

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

    let obj: kube::api::DynamicObject =
        serde_json::from_value(crd).map_err(|e| format!("Failed to build CRD: {e}"))?;

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
            // Already exists — reuse rather than error
            tracing::info!(parent = %parent_name, child = %req.name, "Sub-agent sandbox already exists — reusing");
            Ok(SpawnResponse {
                status: "created".into(),
                name: req.name.clone(),
                namespace: Some(format!("azureclaw-{}", req.name)),
                phase: Some("Running".into()),
                message: Some(format!(
                    "Sub-agent '{}' already running (model: {}, governance: {}). Use AGT mesh to communicate.",
                    req.name, model, req.governance
                )),
            })
        }
        Err(e) => {
            tracing::error!(parent = %parent_name, child = %req.name, "Failed to create sandbox: {e}");
            Err(format!("Failed to create sandbox: {e}"))
        }
    }
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
                name,
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
        return get_sandbox_status_docker(name).await;
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
        name: name.to_string(),
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
        name: name.to_string(),
        namespace: None,
        phase: Some("Terminating".into()),
        message: Some(format!("Sub-agent '{}' is being torn down", name)),
    })
}

// ── Docker dev-mode spawning ────────────────────────────────────────────────
//
// In dev mode the router runs inside the sandbox container which has no docker
// CLI. We talk to the Docker Engine API via the mounted Unix socket using curl.

/// Build a Docker Engine API JSON body for container creation.
fn docker_create_body(
    container_name: &str,
    req: &SpawnRequest,
    parent_name: &str,
) -> serde_json::Value {
    let model = req.model.as_deref().unwrap_or("gpt-4.1");
    let network = std::env::var("DOCKER_NETWORK").unwrap_or_else(|_| "azureclaw-dev".into());
    let relay_url = std::env::var("AGT_RELAY_URL").unwrap_or_default();
    let registry_url = std::env::var("AGT_REGISTRY_URL").unwrap_or_default();
    let endpoint = std::env::var("AZURE_OPENAI_ENDPOINT").unwrap_or_default();
    let foundry_endpoint = std::env::var("FOUNDRY_PROJECT_ENDPOINT").unwrap_or_default();
    let image =
        std::env::var("AZURECLAW_DEV_IMAGE").unwrap_or_else(|_| "azureclaw-sandbox:dev".into());

    let api_key = std::env::var("AZURE_OPENAI_API_KEY").unwrap_or_default();

    let mut env = vec![
        format!("OPENCLAW_MODEL={}", model),
        format!("AZURE_OPENAI_ENDPOINT={}", endpoint),
        format!("AZURE_OPENAI_API_KEY={}", api_key),
        format!("SANDBOX_NAME={}", req.name),
        "AZURECLAW_DEV_MODE=true".to_string(),
        format!("DOCKER_NETWORK={}", network),
        "EGRESS_LEARN_MODE=true".to_string(),
    ];

    if !foundry_endpoint.is_empty() {
        env.push(format!("FOUNDRY_PROJECT_ENDPOINT={}", foundry_endpoint));
    }

    if req.governance && !relay_url.is_empty() {
        env.push(format!("AGT_RELAY_URL={}", relay_url));
        env.push(format!("AGT_REGISTRY_URL={}", registry_url));
        env.push("AGT_GOVERNANCE_ENABLED=true".to_string());
        env.push(format!(
            "AGT_TRUST_THRESHOLD={}",
            req.trust_threshold.unwrap_or(500)
        ));
        // Pass parent identity so sub-agents can trust their parent and siblings
        env.push(format!("PARENT_SANDBOX={}", parent_name));
        // Pre-seeded trusted peers (parent-verified AMIDs, not self-reported)
        if let Some(ref peers) = req.trusted_peers {
            env.push(format!("AGT_TRUSTED_PEERS={}", peers));
        }
    }

    let mut labels = serde_json::Map::new();
    labels.insert("azureclaw.parent".into(), serde_json::json!(parent_name));
    labels.insert("azureclaw.spawned-by".into(), serde_json::json!("agent"));

    serde_json::json!({
        "Image": image,
        "Hostname": req.name,
        "Env": env,
        "Labels": labels,
        "HostConfig": {
            "ReadonlyRootfs": true,
            "CapAdd": ["NET_ADMIN"],
            "Tmpfs": { "/tmp": "rw,noexec,nosuid,size=512m" },
            "Binds": [
                "/var/run/docker.sock:/var/run/docker.sock",
                format!("{}-data:/sandbox", container_name),
            ],
            "NetworkMode": network,
        },
    })
}

/// Call Docker Engine API via curl --unix-socket.
async fn docker_api(method: &str, path: &str, body: Option<&str>) -> Result<String, String> {
    let mut args = vec![
        "--unix-socket".to_string(),
        "/var/run/docker.sock".into(),
        "-s".into(),
        "-S".into(),
        "-X".into(),
        method.into(),
    ];
    if body.is_some() {
        args.extend(["-H".into(), "Content-Type: application/json".into()]);
        args.extend(["-d".into(), body.unwrap().into()]);
    }
    // The hostname is ignored when using --unix-socket; "docker" is just a placeholder
    args.push(format!("http://docker/v1.44{}", path));

    let output = tokio::process::Command::new("curl")
        .args(&args)
        .output()
        .await
        .map_err(|e| format!("curl failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Docker API error: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Spawn a sub-agent as a sibling Docker container (dev mode only).
async fn create_sandbox_docker(
    parent_name: &str,
    req: &SpawnRequest,
) -> Result<SpawnResponse, String> {
    let container_name = format!("azureclaw-{}", req.name);
    let model = req.model.as_deref().unwrap_or("gpt-4.1");

    // Check if container already exists and is running — reuse it
    if let Ok(inspect_resp) =
        docker_api("GET", &format!("/containers/{}/json", container_name), None).await
    {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&inspect_resp) {
            let is_running = info
                .get("State")
                .and_then(|s| s.get("Running"))
                .and_then(|r| r.as_bool())
                .unwrap_or(false);
            if is_running {
                tracing::info!(parent = %parent_name, child = %req.name, "Sub-agent container already running — reusing");
                return Ok(SpawnResponse {
                    status: "created".into(),
                    name: req.name.clone(),
                    namespace: Some(container_name),
                    phase: Some("Running".into()),
                    message: Some(format!(
                        "Sub-agent '{}' already running (model: {}, governance: {}). Use AGT mesh to communicate.",
                        req.name, model, req.governance
                    )),
                });
            }
        }
        // Container exists but not running — remove it
        let _ = docker_api(
            "DELETE",
            &format!("/containers/{}?force=true", container_name),
            None,
        )
        .await;
    }

    // Create container
    let body = docker_create_body(&container_name, req, parent_name);
    let body_str = serde_json::to_string(&body).map_err(|e| format!("JSON error: {e}"))?;
    let create_resp = docker_api(
        "POST",
        &format!("/containers/create?name={}", container_name),
        Some(&body_str),
    )
    .await?;

    // Parse response for container ID
    let resp: serde_json::Value = serde_json::from_str(&create_resp)
        .map_err(|e| format!("Docker create parse error: {e} — response: {create_resp}"))?;

    if let Some(msg) = resp.get("message").and_then(|m| m.as_str()) {
        return Err(format!("Docker create failed: {}", msg));
    }

    // Start container
    docker_api(
        "POST",
        &format!("/containers/{}/start", container_name),
        None,
    )
    .await
    .map_err(|e| format!("Docker start failed: {e}"))?;

    tracing::info!(parent = %parent_name, child = %req.name, "Sub-agent container spawned (dev mode)");
    Ok(SpawnResponse {
        status: "created".into(),
        name: req.name.clone(),
        namespace: Some(container_name),
        phase: Some("Running".into()),
        message: Some(format!(
            "Sub-agent '{}' spawned as Docker container (model: {}, governance: {}). Use AGT mesh to communicate.",
            req.name, model, req.governance
        )),
    })
}

/// Get sub-agent status in dev mode (Docker container inspect).
async fn get_sandbox_status_docker(name: &str) -> Result<SpawnResponse, String> {
    let container_name = if name.starts_with("azureclaw-") {
        name.to_string()
    } else {
        format!("azureclaw-{}", name)
    };

    let resp = docker_api("GET", &format!("/containers/{}/json", container_name), None)
        .await
        .map_err(|e| format!("Container '{}' not found: {}", name, e))?;

    let info: serde_json::Value =
        serde_json::from_str(&resp).map_err(|e| format!("Parse error: {e}"))?;

    let state = info
        .get("State")
        .and_then(|s| s.get("Status"))
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");

    let phase = match state {
        "running" => "Running",
        "exited" => "Exited",
        "created" => "Created",
        _ => state,
    };

    Ok(SpawnResponse {
        status: "ok".into(),
        name: name.to_string(),
        namespace: Some(container_name),
        phase: Some(phase.to_string()),
        message: None,
    })
}

/// List sub-agents in dev mode (Docker containers with parent label).
pub async fn list_sandboxes_docker(parent_name: &str) -> Result<Vec<SubAgentEntry>, String> {
    let filter = format!(r#"{{"label":["azureclaw.parent={}"]}}"#, parent_name);
    // URL-encode the filter JSON (only special chars that appear in our filter)
    let encoded = filter
        .replace('{', "%7B")
        .replace('}', "%7D")
        .replace('[', "%5B")
        .replace(']', "%5D")
        .replace('"', "%22")
        .replace('=', "%3D");
    let resp = docker_api(
        "GET",
        &format!("/containers/json?all=true&filters={}", encoded),
        None,
    )
    .await?;

    let containers: Vec<serde_json::Value> =
        serde_json::from_str(&resp).map_err(|e| format!("Parse error: {e}"))?;

    let entries = containers
        .iter()
        .filter_map(|c| {
            let names = c.get("Names")?.as_array()?;
            let raw_name = names.first()?.as_str()?.trim_start_matches('/');
            let name = raw_name
                .strip_prefix("azureclaw-")
                .unwrap_or(raw_name)
                .to_string();
            let state = c.get("State")?.as_str().unwrap_or("unknown");
            let phase = if state == "running" {
                "Running"
            } else {
                "Stopped"
            };

            Some(SubAgentEntry {
                name,
                namespace: Some(raw_name.to_string()),
                phase: Some(phase.to_string()),
                model: None,
                governance: true,
            })
        })
        .collect();

    Ok(entries)
}

/// Delete a sub-agent Docker container (dev mode).
pub async fn delete_sandbox_docker(parent_name: &str, name: &str) -> Result<SpawnResponse, String> {
    let container_name = format!("azureclaw-{}", name);

    // Verify parent label via inspect
    let inspect = docker_api("GET", &format!("/containers/{}/json", container_name), None)
        .await
        .map_err(|_| format!("Container '{}' not found", name))?;

    let info: serde_json::Value =
        serde_json::from_str(&inspect).map_err(|e| format!("Parse error: {e}"))?;

    let labels = info.pointer("/Config/Labels");
    let actual_parent = labels
        .and_then(|l| l.get("azureclaw.parent"))
        .and_then(|v| v.as_str());

    if actual_parent != Some(parent_name) {
        return Err(format!(
            "Container '{}' was not spawned by '{}'",
            name, parent_name
        ));
    }

    docker_api(
        "DELETE",
        &format!("/containers/{}?force=true", container_name),
        None,
    )
    .await
    .map_err(|e| format!("Failed to delete: {e}"))?;

    tracing::info!(parent = %parent_name, child = %name, "Sub-agent container deleted (dev mode)");
    Ok(SpawnResponse {
        status: "deleted".into(),
        name: name.to_string(),
        namespace: None,
        phase: Some("Terminated".into()),
        message: Some(format!("Sub-agent '{}' container removed", name)),
    })
}
