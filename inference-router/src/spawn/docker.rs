// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Docker dev-mode sandbox spawn — `azureclaw dev` path.
//!
//! Extracted from `spawn.rs` per §4.2 hotspot decomposition. The
//! controller-managed (K8s) path stays in `spawn::mod`; this module
//! holds every Docker Engine API call used when the router runs
//! against a local Docker socket instead of an in-cluster
//! ServiceAccount. Behaviour change: **none** — same HTTP shapes,
//! same network creation, same container labelling, same response
//! structures.

use super::{SpawnRequest, SpawnResponse, SubAgentEntry};

/// Docker dev-mode: collect sub-agent snapshots from Docker containers.
pub(super) async fn collect_sub_agent_snapshots_docker(
    parent_name: &str,
) -> Result<Vec<crate::handoff::SubAgentSnapshot>, String> {
    // List containers with the parent label — URL-encode the filter
    // (raw JSON braces/brackets cause curl globbing and Docker parse errors)
    let filter = format!(
        r#"{{"label":["azureclaw.parent={}"],"status":["running"]}}"#,
        parent_name
    );
    let encoded = filter
        .replace('{', "%7B")
        .replace('}', "%7D")
        .replace('[', "%5B")
        .replace(']', "%5D")
        .replace('"', "%22")
        .replace('=', "%3D")
        .replace(',', "%2C");

    let resp = docker_api("GET", &format!("/containers/json?filters={encoded}"), None).await?;
    let containers: Vec<serde_json::Value> = serde_json::from_str(&resp).unwrap_or_default();

    let mut snapshots = Vec::new();

    for container in &containers {
        let names = container
            .get("Names")
            .and_then(|n| n.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|n| n.as_str())
                    .map(|n| n.trim_start_matches('/').to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let container_name = names.first().cloned().unwrap_or_default();
        if container_name.is_empty() {
            continue;
        }

        // Strip the "azureclaw-" prefix that create_sandbox_docker adds,
        // so respawn doesn't double-prefix to "azureclaw-azureclaw-{name}".
        let name = container_name
            .strip_prefix("azureclaw-")
            .unwrap_or(&container_name)
            .to_string();

        // Extract model from container labels
        let labels = container.get("Labels").and_then(|l| l.as_object());
        let model = labels
            .and_then(|l| l.get("azureclaw.model"))
            .and_then(|m| m.as_str())
            .map(String::from);

        let spawn_config = SpawnRequest {
            agent_id: name.clone(),
            model,
            governance: true,
            trust_threshold: None,
            learn_egress: false,
            isolation: None,
            token_budget_daily: None,
            token_budget_per_request: None,
            trusted_peers: None,
            handoff: None,
        };

        snapshots.push(crate::handoff::SubAgentSnapshot {
            agent_id: name.clone(),
            original_amid: String::new(),
            spawn_config,
            task_context: format!("Sub-agent '{name}' (Docker)"),
            status: "paused_at_checkpoint".to_string(),
            checkpoint: None,
            workspace_tar: Vec::new(),
        });
    }

    Ok(snapshots)
}

// ── Docker dev-mode spawning ────────────────────────────────────────────────
//
// In dev mode the router runs inside the sandbox container which has no docker
// CLI. We talk to the Docker Engine API via the mounted Unix socket using curl.

/// Build a Docker Engine API JSON body for container creation.
pub(super) fn docker_create_body(
    container_name: &str,
    req: &SpawnRequest,
    parent_name: &str,
) -> serde_json::Value {
    // Inherit parent's model when sub-agent spawn request doesn't specify one.
    // Falls back to gpt-4.1 only if neither side has a model configured.
    let parent_model = std::env::var("OPENCLAW_MODEL")
        .or_else(|_| std::env::var("AZURE_OPENAI_DEPLOYMENT"))
        .unwrap_or_else(|_| "gpt-4.1".into());
    let model = req.model.as_deref().unwrap_or(&parent_model);
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
        format!("SANDBOX_NAME={}", req.agent_id),
        "AZURECLAW_DEV_MODE=true".to_string(),
        format!("DOCKER_NETWORK={}", network),
        "EGRESS_LEARN_MODE=true".to_string(),
    ];

    // Propagate the model provider to the sub-agent so its entrypoint +
    // plugin can pick the right tool catalog. Without this, sub-agents
    // spawned from a GH-Models parent would register the full Foundry
    // tool catalog (~25k tokens) and 413 on every chat against the 16k
    // GH-Models input cap. The parent router process inherits this env
    // from its own container, which the CLI sets in dev.ts.
    if let Ok(provider) = std::env::var("AZURECLAW_PROVIDER")
        && !provider.is_empty()
    {
        env.push(format!("AZURECLAW_PROVIDER={}", provider));
    }

    if !foundry_endpoint.is_empty() {
        env.push(format!("FOUNDRY_PROJECT_ENDPOINT={}", foundry_endpoint));
    }

    // Always propagate AGT relay/registry URLs to sub-agents (governance is native)
    if !relay_url.is_empty() {
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

    // Propagate mesh provider selection so spawned sub-agents use the same
    // SDK / wire shape as the parent. Without this, an AGT-mode parent
    // spawns vendored-mode children that connect to the AGT relay's wrong
    // path (root vs `/ws`) and get 403, and call vendored registry URLs
    // (`/registry/search`) on the AGT registry which 404s.
    if let Ok(mesh_provider) = std::env::var("AZURECLAW_MESH_PROVIDER")
        && !mesh_provider.is_empty()
    {
        env.push(format!("AZURECLAW_MESH_PROVIDER={}", mesh_provider));
    }

    let mut labels = serde_json::Map::new();
    labels.insert("azureclaw.parent".into(), serde_json::json!(parent_name));
    labels.insert("azureclaw.spawned-by".into(), serde_json::json!("agent"));

    serde_json::json!({
        "Image": image,
        "Hostname": req.agent_id,
        "Env": env,
        "Labels": labels,
        "HostConfig": {
            "ReadonlyRootfs": true,
            "CapAdd": ["NET_ADMIN"],
            "Tmpfs": { "/tmp": "rw,noexec,nosuid,size=4g" },
            "Binds": [
                "/var/run/docker.sock:/var/run/docker.sock",
                format!("{}-data:/sandbox", container_name),
            ],
            "NetworkMode": network,
        },
    })
}

/// Call Docker Engine API via curl --unix-socket.
pub(super) async fn docker_api(
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let mut args = vec![
        "--unix-socket".to_string(),
        "/var/run/docker.sock".into(),
        "-s".into(),
        "-S".into(),
        // Write HTTP status code after the response body
        "-w".into(),
        "\n__HTTP_STATUS__:%{http_code}".into(),
        "-X".into(),
        method.into(),
    ];
    if body.is_some() {
        args.extend(["-H".into(), "Content-Type: application/json".into()]);
        args.extend(["-d".into(), body.expect("body presence checked").into()]);
    }
    // The hostname is ignored when using --unix-socket; "docker" is a synthetic value
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

    let raw = String::from_utf8_lossy(&output.stdout).to_string();

    // Extract HTTP status code appended by -w flag
    let (response_body, http_status) = if let Some(idx) = raw.rfind("\n__HTTP_STATUS__:") {
        let status_str = &raw[idx + "\n__HTTP_STATUS__:".len()..];
        let status: u16 = status_str.trim().parse().unwrap_or(0);
        (raw[..idx].to_string(), status)
    } else {
        (raw, 0)
    };

    // Treat 4xx/5xx as errors (2xx and 3xx are success)
    if http_status >= 400 {
        // Try to extract Docker's error message from JSON response
        let msg = serde_json::from_str::<serde_json::Value>(&response_body)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or_else(|| response_body.clone());
        return Err(format!(
            "Docker API {method} {path} returned HTTP {http_status}: {msg}"
        ));
    }

    Ok(response_body)
}

/// Spawn a sub-agent as a sibling Docker container (dev mode only).
pub(super) async fn create_sandbox_docker(
    parent_name: &str,
    req: &SpawnRequest,
) -> Result<SpawnResponse, String> {
    let container_name = format!("azureclaw-{}", req.agent_id);
    let parent_model = std::env::var("OPENCLAW_MODEL")
        .or_else(|_| std::env::var("AZURE_OPENAI_DEPLOYMENT"))
        .unwrap_or_else(|_| "gpt-4.1".into());
    let model = req.model.as_deref().unwrap_or(&parent_model);

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
                tracing::info!(parent = %parent_name, child = %req.agent_id, "Sub-agent container already running — reusing");
                return Ok(SpawnResponse {
                    status: "created".into(),
                    agent_id: req.agent_id.clone(),
                    namespace: Some(container_name),
                    phase: Some("Running".into()),
                    message: Some(format!(
                        "Sub-agent '{}' already running (model: {}, governance: {}). Use AGT mesh to communicate.",
                        req.agent_id, model, req.governance
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

    // Ensure the Docker network exists (it may not if --agt was not used)
    let network = std::env::var("DOCKER_NETWORK").unwrap_or_else(|_| "azureclaw-dev".into());
    let net_check = docker_api("GET", &format!("/networks/{}", network), None).await;
    if net_check.is_err()
        || net_check
            .as_ref()
            .ok()
            .and_then(|r| serde_json::from_str::<serde_json::Value>(r).ok())
            .and_then(|v| v.get("message").map(|_| ()))
            .is_some()
    {
        let net_body = serde_json::json!({ "Name": network, "CheckDuplicate": true });
        let _ = docker_api("POST", "/networks/create", Some(&net_body.to_string())).await;
        tracing::info!(network = %network, "Created Docker network for sub-agent");
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

    tracing::info!(parent = %parent_name, child = %req.agent_id, "Sub-agent container spawned (dev mode)");
    Ok(SpawnResponse {
        status: "created".into(),
        agent_id: req.agent_id.clone(),
        namespace: Some(container_name),
        phase: Some("Running".into()),
        message: Some(format!(
            "Sub-agent '{}' spawned as Docker container (model: {}, governance: {}). Use AGT mesh to communicate.",
            req.agent_id, model, req.governance
        )),
    })
}

/// Get sub-agent status in dev mode (Docker container inspect).
pub(super) async fn get_sandbox_status_docker(name: &str) -> Result<SpawnResponse, String> {
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
        agent_id: name.to_string(),
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
                agent_id: name,
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
        agent_id: name.to_string(),
        namespace: None,
        phase: Some("Terminated".into()),
        message: Some(format!("Sub-agent '{}' container removed", name)),
    })
}
