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
    let parent_labels: BTreeMap<String, String> = match api.get(parent_name).await {
        Ok(parent_obj) => parent_obj.metadata.labels.unwrap_or_default(),
        Err(e) => {
            tracing::warn!(
                parent = %parent_name,
                child = %req.agent_id,
                "Could not fetch parent labels for inheritance (non-fatal): {e}"
            );
            BTreeMap::new()
        }
    };

    let crd = build_sub_agent_crd_with_labels(
        parent_name,
        &namespace,
        isolation,
        model,
        req,
        &parent_labels,
    );

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
                .get("metadata")
                .and_then(|m| m.get("annotations"))
                .and_then(|a| a.get("azureclaw.azure.com/model"))
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

        // Reconstruct SpawnRequest from CRD metadata + spec.
        // Model lives on the `azureclaw.azure.com/model` annotation since
        // S13 (delegated to InferencePolicy on-CR).
        let model = obj
            .data
            .get("metadata")
            .and_then(|m| m.get("annotations"))
            .and_then(|a| a.get("azureclaw.azure.com/model"))
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

// ---------------------------------------------------------------------------
// Pure CRD builder (kept testable; called from `create_sandbox`)
// ---------------------------------------------------------------------------

/// Build the ClawSandbox CRD payload for a spawned sub-agent or handoff
/// target. Pure function — no I/O, no env vars except `FOUNDRY_AGENT_TOOLS`
/// — so it round-trips through JSON-shape contract tests below, catching
/// schema regressions to the pre-S10/S13 shape (`spec.openclaw`,
/// `spec.inference`, `governance.toolPolicy: <string>`,
/// top-level `spec.handoff`/`spec.model` — all rejected by
/// `additionalProperties: false` at admission).
///
/// **No-inherit invariant (Slice 3a/3b)**: the parent's
/// `spec.memoryRef` is deliberately NOT propagated onto the spawned
/// sub-agent. ClawMemory bindings are scoped to the agent that
/// declared them and must not flow through `handoff` or `spawn`. The
/// contract test `sub_agent_crd_never_inherits_memory_ref` asserts
/// this by construction — if a future caller adds a `memory_ref`
/// field to `SpawnRequest`, the test fails before the CRD ships.
/// Slice 2 DoD #6 — parent-label inheritance.
///
/// Pure label-merge: filter the parent's `metadata.labels` to drop
/// azureclaw-controlled keys (anything starting with `azureclaw.`
/// and the `app.kubernetes.io/*` tracking labels), then start the
/// child's label map from that filtered set. Spawn-tracking labels
/// (`parent`, `spawned-by`, `predecessor`) are written last so they
/// always win if the parent happened to carry a colliding key.
///
/// Operators who tag a parent with e.g. `tier=prod` /
/// `team=payments` / `env=staging` get those same tags on every
/// sub-agent the parent spawns — so a single `kubectl get
/// clawsandbox -l tier=prod` returns the parent and every
/// descendant without the operator having to walk the
/// `azureclaw.azure.com/parent` graph by hand.
pub(crate) fn inherit_parent_labels(
    parent_labels: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in parent_labels {
        // Drop labels we control — they get re-stamped per spawn
        // based on the *child's* role (handoff vs. agent vs. mesh)
        // and inheriting them would lie about the child's lineage.
        if k.starts_with("azureclaw.azure.com/") || k.starts_with("app.kubernetes.io/") {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    out
}

pub(crate) fn build_sub_agent_crd_with_labels(
    parent_name: &str,
    namespace: &str,
    isolation: &str,
    model: &str,
    req: &SpawnRequest,
    parent_labels: &BTreeMap<String, String>,
) -> serde_json::Value {
    let mut spec = serde_json::json!({
        "runtime": {
            "kind": "OpenClaw",
            "openclaw": {}
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
            "approvalRequired": true,
            "egressMode": if req.learn_egress { "Learn" } else { "Strict" },
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

    let mut annotations = BTreeMap::new();
    annotations.insert("azureclaw.azure.com/model".to_string(), model.to_string());

    serde_json::json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawSandbox",
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

    fn minimal_req(agent_id: &str) -> SpawnRequest {
        SpawnRequest {
            agent_id: agent_id.into(),
            model: None,
            governance: true,
            trust_threshold: None,
            learn_egress: false,
            isolation: None,
            token_budget_daily: None,
            token_budget_per_request: None,
            trusted_peers: None,
            handoff: None,
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
        let crd = build_sub_agent_crd_with_labels(
            "azclaw2",
            "azureclaw-system",
            "enhanced",
            "gpt-5.4",
            &minimal_req("viz"),
            &BTreeMap::new(),
        );

        // 1. Top-level
        assert_eq!(crd["apiVersion"], "azureclaw.azure.com/v1alpha1");
        assert_eq!(crd["kind"], "ClawSandbox");
        assert_eq!(
            crd["metadata"]["annotations"]["azureclaw.azure.com/model"],
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
            "azureclaw-system",
            "enhanced",
            "gpt-5.4",
            &req,
            &BTreeMap::new(),
        );

        assert_eq!(crd["apiVersion"], "azureclaw.azure.com/v1alpha1");
        assert_eq!(
            crd["metadata"]["labels"]["azureclaw.azure.com/spawned-by"],
            "handoff"
        );
        assert_eq!(
            crd["metadata"]["labels"]["azureclaw.azure.com/predecessor"],
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
        // No-inherit invariant for ClawMemory (Slice 3a/3b):
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
                "azureclaw-parent",
                "default",
                "gpt-5.4",
                &req,
                &BTreeMap::new(),
            );
            assert!(
                crd["spec"].get("memoryRef").is_none(),
                "spec.memoryRef leaked into spawned sub-agent CRD (handoff={handoff:?}); \
                 ClawMemory bindings must not inherit (Slice 3a no-inherit rule)"
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
    fn inherit_parent_labels_drops_azureclaw_controlled_keys() {
        let mut parent = BTreeMap::new();
        parent.insert("tier".to_string(), "prod".to_string());
        parent.insert("team".to_string(), "payments".to_string());
        parent.insert(
            "azureclaw.azure.com/parent".to_string(),
            "grandparent".to_string(),
        );
        parent.insert(
            "azureclaw.azure.com/spawned-by".to_string(),
            "agent".to_string(),
        );
        parent.insert(
            "app.kubernetes.io/managed-by".to_string(),
            "controller".to_string(),
        );

        let inherited = inherit_parent_labels(&parent);

        assert_eq!(inherited.get("tier"), Some(&"prod".to_string()));
        assert_eq!(inherited.get("team"), Some(&"payments".to_string()));
        assert!(
            !inherited.contains_key("azureclaw.azure.com/parent"),
            "azureclaw-controlled label leaked: child must re-stamp its own parent ref"
        );
        assert!(
            !inherited.contains_key("azureclaw.azure.com/spawned-by"),
            "azureclaw-controlled spawned-by leaked: child role depends on the spawn call, not the parent's"
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
            "azureclaw-system",
            "enhanced",
            "gpt-5.4",
            &minimal_req("child"),
            &parent_labels,
        );

        let child_labels = &crd["metadata"]["labels"];
        assert_eq!(child_labels["tier"], "prod");
        assert_eq!(child_labels["env"], "staging");
        // Spawn-tracking labels must coexist with inherited ones.
        assert_eq!(child_labels["azureclaw.azure.com/parent"], "azclaw-parent");
        assert_eq!(child_labels["azureclaw.azure.com/spawned-by"], "agent");
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
            "azureclaw-system",
            "enhanced",
            "gpt-5.4",
            &req,
            &parent_labels,
        );

        let child_labels = &crd["metadata"]["labels"];
        assert_eq!(child_labels["tier"], "prod");
        assert_eq!(child_labels["azureclaw.azure.com/spawned-by"], "handoff");
        assert_eq!(
            child_labels["azureclaw.azure.com/predecessor"],
            "local-parent"
        );
        // The handoff path intentionally omits the `parent` label
        // (predecessor takes its place semantically) — make sure
        // inheritance does not accidentally restore it.
        assert!(
            child_labels.get("azureclaw.azure.com/parent").is_none()
                || child_labels["azureclaw.azure.com/parent"].is_null(),
            "handoff path must not stamp the `parent` label"
        );
    }

    #[test]
    fn spawn_tracking_labels_win_over_parent_labels_on_collision() {
        // Defence-in-depth: if a parent somehow carried
        // `azureclaw.azure.com/parent=evil` (shouldn't happen — we
        // filter it — but belt-and-suspenders), the child's
        // re-stamped value must win. This pins the ordering.
        let mut parent_labels = BTreeMap::new();
        parent_labels.insert("azureclaw.azure.com/parent".to_string(), "evil".to_string());
        parent_labels.insert("tier".to_string(), "prod".to_string());

        let crd = build_sub_agent_crd_with_labels(
            "real-parent",
            "azureclaw-system",
            "enhanced",
            "gpt-5.4",
            &minimal_req("child"),
            &parent_labels,
        );

        assert_eq!(
            crd["metadata"]["labels"]["azureclaw.azure.com/parent"], "real-parent",
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
            "azureclaw-system",
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
        assert!(labels.contains_key("azureclaw.azure.com/parent"));
        assert!(labels.contains_key("azureclaw.azure.com/spawned-by"));
    }
}
