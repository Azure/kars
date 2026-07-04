// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsTask` execution bridge (Bridge V0.1b) — materialize a governed
//! `KarsSandbox` from a launched task.
//!
//! This is the wire that turns a *governed* task (validated envelope + digest)
//! into a *running* one. It is gated by `spec.execution.launch` (plan §20:
//! review the package, then launch). On launch the controller materializes,
//! owned by the task for cascade cleanup:
//!
//! 1. a minimal `InferencePolicy` (`<task>-inference`) the sandbox references;
//! 2. a `KarsSandbox` (`<task>`) bounded by the task's envelope — the existing
//!    sandbox reconciler then spawns the real pod + OpenClaw agent through the
//!    secure inference router.
//!
//! **Honest limitation:** the sandbox needs a real AI Foundry inference
//! endpoint to perform inference. On a local kind cluster with no endpoint the
//! sandbox materializes but degrades at the inference step — the controller
//! surfaces that verbatim in `status.executionDetail` rather than hiding it.

use kube::api::{Api, DynamicObject, ObjectMeta, Patch, PatchParams};
use kube::core::ApiResource;
use kube::{Client, ResourceExt};
use serde_json::json;

use crate::kars_task::{KarsTask, TaskBlueprint, TaskEnvelope};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_TASK;

fn sandbox_api_resource() -> ApiResource {
    ApiResource {
        group: "kars.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsSandbox".into(),
        plural: "karssandboxes".into(),
    }
}

fn inference_policy_api_resource() -> ApiResource {
    ApiResource {
        group: "kars.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "InferencePolicy".into(),
        plural: "inferencepolicies".into(),
    }
}

/// Outcome of a launch reconcile, reflected into `KarsTask.status`.
pub struct ExecutionOutcome {
    /// `Launching` | `Running` | `Degraded`.
    pub phase: String,
    /// Name of the materialized sandbox.
    pub sandbox_name: String,
    /// Human-readable detail surfaced verbatim in the product.
    pub detail: String,
}

/// The owner reference making materialized resources cascade-delete with the
/// task and be server-side-apply-owned by this controller.
fn owner_ref(task: &KarsTask) -> serde_json::Value {
    json!([{
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "name": task.name_any(),
        "uid": task.uid().unwrap_or_default(),
        "controller": true,
        "blockOwnerDeletion": true,
    }])
}

/// Runtime variant key for the sandbox spec discriminator.
fn runtime_variant_key(kind: &str) -> &'static str {
    match kind {
        "Hermes" => "hermes",
        "OpenAIAgents" => "openaiAgents",
        "MAF" => "maf",
        _ => "openclaw",
    }
}

/// Resolve the default `(deployment, provider)` a task-materialized
/// InferencePolicy should request. The deployment is required by the sandbox
/// reconciler — without it the pod degrades — so we derive a sane default from
/// the controller's own configured inference model and let an operator override
/// it for the task lane specifically.
///
/// Resolution order for the deployment:
/// `KARS_TASK_DEFAULT_MODEL` → `AZURE_OPENAI_DEPLOYMENT` → `DEFAULT_MODEL` →
/// `gpt-4o-mini`. The provider tag is `KARS_TASK_DEFAULT_PROVIDER` →
/// `azure-openai` (the router routes by the configured endpoint URL, so this
/// tag only needs to be a valid non-empty value).
fn default_model() -> (String, String) {
    let deployment = std::env::var("KARS_TASK_DEFAULT_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("AZURE_OPENAI_DEPLOYMENT").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("DEFAULT_MODEL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    let provider = std::env::var("KARS_TASK_DEFAULT_PROVIDER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "azure-openai".to_string());
    (deployment, provider)
}

/// Build the agent's standing instructions (system prompt) from the task
/// objective plus any blueprint instructions. Pure + testable.
fn build_instructions(objective: &str, extra: Option<&str>) -> String {
    let mut out = format!("Your objective:\n{}", objective.trim());
    if let Some(extra) = extra.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("\n\nAdditional instructions:\n");
        out.push_str(extra);
    }
    out
}

/// Materialize (or re-apply) the InferencePolicy + KarsSandbox for a launched
/// task, then read back the sandbox phase. Idempotent via server-side apply.
pub async fn materialize(
    client: &Client,
    namespace: &str,
    task: &KarsTask,
) -> Result<ExecutionOutcome, kube::Error> {
    let task_name = task.name_any();
    let inference_name = format!("{task_name}-inference");
    let envelope = &task.spec.envelope;
    let blueprint = task.spec.blueprint.clone().unwrap_or_default();
    // Runtime: blueprint wins, then execution.runtime, then OpenClaw.
    let runtime_kind = blueprint
        .runtime
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            task.spec
                .execution
                .as_ref()
                .and_then(|e| e.runtime.clone())
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "OpenClaw".to_string());

    // 1. InferencePolicy scoped to this sandbox. Model: blueprint wins, else
    //    the controller default (required — without it the sandbox degrades).
    let (model_deployment, model_provider) = match &blueprint.model {
        Some(m) if !m.deployment.trim().is_empty() => {
            let provider = if m.provider.trim().is_empty() {
                "azure-openai".to_string()
            } else {
                m.provider.clone()
            };
            (m.deployment.clone(), provider)
        }
        _ => default_model(),
    };
    let mut inference_spec = json!({
        "appliesTo": { "sandboxName": task_name },
        "modelPreference": {
            "primary": { "provider": model_provider, "deployment": model_deployment },
        },
    });
    if let Some(tokens) = envelope.budget.as_ref().and_then(|b| b.tokens)
        && tokens > 0
    {
        inference_spec["tokenBudget"] = json!({ "dailyTokens": tokens });
    }
    apply_dynamic(
        client,
        namespace,
        &inference_policy_api_resource(),
        &inference_name,
        task,
        inference_spec,
        None,
    )
    .await?;

    // 2. KarsSandbox bounded by the envelope + shaped by the blueprint. Each
    //    blueprint field drives a real sandbox field; unset → safe default.
    let isolation = blueprint
        .isolation
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "standard".to_string());
    let mut sandbox_spec = json!({
        "runtime": {
            "kind": runtime_kind,
            runtime_variant_key(&runtime_kind): {},
        },
        "inferenceRef": { "name": inference_name },
        "sandbox": { "isolation": isolation },
        "networkPolicy": { "defaultDeny": true },
    });

    // Egress: when the blueprint names destinations, bound the sandbox to
    // exactly those hosts in strict mode (the substance of "what it can reach").
    if !blueprint.egress.is_empty() {
        let endpoints: Vec<serde_json::Value> = blueprint
            .egress
            .iter()
            .map(|e| match e.port {
                Some(p) => json!({ "host": e.host, "port": p }),
                None => json!({ "host": e.host }),
            })
            .collect();
        sandbox_spec["networkPolicy"] = json!({
            "defaultDeny": true,
            "egressMode": "Strict",
            "allowedEndpoints": endpoints,
        });
    }

    // Agent instructions (the system prompt) — combine the objective with any
    // standing instructions the blueprint carries, so the agent knows both
    // *what* to do and *how* to behave.
    let instructions = build_instructions(&task.spec.objective, blueprint.instructions.as_deref());
    sandbox_spec["agent"] = json!({ "instructions": instructions });

    // Governance: tools = an existing ToolPolicy (composed by reference), from
    // the blueprint or the envelope; MCP servers (connected services) ride on
    // top, bounded by that policy. See `governance_spec`.
    sandbox_spec["governance"] = governance_spec(&blueprint, envelope);

    // Shared team memory: reference an existing KarsMemory so the agent
    // reads/writes the team's shared knowledge (persistent teams share memory
    // across members and over time).
    if let Some(mem) = blueprint
        .memory
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        sandbox_spec["memoryRef"] = json!({ "name": mem });
    }
    // Task attribution for router metering: the task id and its lineage *root*
    // (the oldest ancestor, or the task itself when it is a root). The main
    // reconciler forwards these to the router as KARS_TASK_ID / KARS_TASK_ROOT
    // so token cost is attributable per task branch.
    let task_root = task
        .status
        .as_ref()
        .and_then(|s| s.lineage.first().cloned())
        .unwrap_or_else(|| task_name.clone());
    let attribution = std::collections::BTreeMap::from([
        ("kars.azure.com/task-id".to_string(), task_name.clone()),
        ("kars.azure.com/task-root".to_string(), task_root),
    ]);
    apply_dynamic(
        client,
        namespace,
        &sandbox_api_resource(),
        &task_name,
        task,
        sandbox_spec,
        Some(attribution),
    )
    .await?;

    // 3. Read back the sandbox phase to reflect honest execution status.
    let sb_api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &sandbox_api_resource());
    let (phase, detail) = match sb_api.get_opt(&task_name).await? {
        Some(sb) => {
            let sb_phase = sb
                .data
                .get("status")
                .and_then(|s| s.get("phase"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            map_sandbox_phase(&sb_phase)
        }
        None => (
            "Launching".to_string(),
            "Sandbox materialized; awaiting the controller to reconcile it.".to_string(),
        ),
    };

    Ok(ExecutionOutcome {
        phase,
        sandbox_name: task_name,
        detail,
    })
}

/// Tear down the materialized sandbox + inference policy when a task is
/// un-launched (`execution.launch` flipped back to false). Owner references
/// also cascade on task deletion; this handles the in-place un-launch.
pub async fn teardown(
    client: &Client,
    namespace: &str,
    task: &KarsTask,
) -> Result<(), kube::Error> {
    use kube::api::DeleteParams;
    let task_name = task.name_any();
    let sb_api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &sandbox_api_resource());
    let ip_api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &inference_policy_api_resource());
    // Ignore 404 (already gone) but PROPAGATE any other error so the caller can
    // requeue — silently swallowing a failed sandbox delete would leave the pod
    // running and the agent still reachable on the mesh (it could keep receiving
    // and answering delegated tasks as a "retired" agent).
    match sb_api.delete(&task_name, &DeleteParams::default()).await {
        Ok(_) => {}
        Err(kube::Error::Api(ae)) if ae.code == 404 => {}
        Err(e) => return Err(e),
    }
    match ip_api
        .delete(&format!("{task_name}-inference"), &DeleteParams::default())
        .await
    {
        Ok(_) => {}
        Err(kube::Error::Api(ae)) if ae.code == 404 => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Build the sandbox governance block by composing an existing `ToolPolicy`
/// (from the blueprint or the envelope) plus any MCP server refs. Tools are a
/// `ToolPolicy` reference rather than a duplicated allow-list, so the AGT
/// profile + `appliesTo` scope stay authoritative. MCP refs only attach when a
/// tool policy bounds them; without a policy governance stays `enabled: false`
/// (a valid, un-governed sandbox) instead of an invalid `enabled: true` with no
/// `toolPolicyRef`.
fn governance_spec(blueprint: &TaskBlueprint, envelope: &TaskEnvelope) -> serde_json::Value {
    let tool_policy = blueprint
        .tool_policy
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| envelope.tool_policy_ref.as_ref().map(|r| r.name.clone()));
    match tool_policy {
        Some(tp) => {
            let mut g = json!({ "enabled": true, "toolPolicyRef": { "name": tp } });
            if !blueprint.mcp_servers.is_empty() {
                let refs: Vec<serde_json::Value> = blueprint
                    .mcp_servers
                    .iter()
                    .map(|name| json!({ "name": name }))
                    .collect();
                g["mcpServerRefs"] = json!(refs);
            }
            g
        }
        None => json!({ "enabled": false }),
    }
}

/// Map a `KarsSandbox` phase to the task's execution phase + honest detail.
fn map_sandbox_phase(sb_phase: &str) -> (String, String) {
    match sb_phase {
        "Running" => (
            "Running".to_string(),
            "The governed agent is running in its sandbox.".to_string(),
        ),
        "Failed" | "Degraded" => (
            "Degraded".to_string(),
            "Sandbox degraded. On a local cluster this is expected at the inference \
             step — a real AI Foundry endpoint is required for the agent to run."
                .to_string(),
        ),
        "" | "Pending" | "Creating" => (
            "Launching".to_string(),
            "Sandbox materialized; the controller is bringing the agent up.".to_string(),
        ),
        other => ("Launching".to_string(), format!("Sandbox phase: {other}.")),
    }
}

/// Server-side-apply an owned dynamic object (spec only; status is the target
/// reconciler's). Idempotent — safe to call every reconcile.
async fn apply_dynamic(
    client: &Client,
    namespace: &str,
    ar: &ApiResource,
    name: &str,
    task: &KarsTask,
    spec: serde_json::Value,
    annotations: Option<std::collections::BTreeMap<String, String>>,
) -> Result<(), kube::Error> {
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), namespace, ar);
    let mut obj = DynamicObject::new(name, ar).within(namespace);
    obj.metadata = ObjectMeta {
        name: Some(name.to_string()),
        namespace: Some(namespace.to_string()),
        owner_references: serde_json::from_value(owner_ref(task)).ok(),
        labels: Some(std::collections::BTreeMap::from([
            (
                "app.kubernetes.io/managed-by".to_string(),
                "kars-controller".to_string(),
            ),
            ("kars.azure.com/karstask".to_string(), task.name_any()),
        ])),
        annotations,
        ..Default::default()
    };
    obj.data = json!({ "spec": spec });
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&obj),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_instructions_includes_objective_and_extra() {
        let only_obj = build_instructions("Summarize the doc", None);
        assert!(only_obj.contains("Summarize the doc"));
        assert!(only_obj.contains("Your objective"));
        assert!(!only_obj.contains("Additional instructions"));

        let with_extra = build_instructions("Summarize the doc", Some("Be concise. Cite sources."));
        assert!(with_extra.contains("Summarize the doc"));
        assert!(with_extra.contains("Additional instructions"));
        assert!(with_extra.contains("Be concise"));

        // Blank extra is ignored.
        let blank = build_instructions("X", Some("   "));
        assert!(!blank.contains("Additional instructions"));
    }

    #[test]
    fn default_model_resolution() {
        // Single test (env is process-global; avoid cross-test races).
        unsafe {
            std::env::remove_var("KARS_TASK_DEFAULT_MODEL");
            std::env::remove_var("AZURE_OPENAI_DEPLOYMENT");
            std::env::remove_var("DEFAULT_MODEL");
            std::env::remove_var("KARS_TASK_DEFAULT_PROVIDER");
        }
        // No knobs → safe builtin default + valid provider tag.
        let (deployment, provider) = default_model();
        assert!(!deployment.is_empty());
        assert_eq!(provider, "azure-openai");

        // Explicit task overrides win.
        unsafe {
            std::env::set_var("KARS_TASK_DEFAULT_MODEL", "openai/gpt-4o-mini");
            std::env::set_var("KARS_TASK_DEFAULT_PROVIDER", "github-models");
        }
        let (deployment, provider) = default_model();
        assert_eq!(deployment, "openai/gpt-4o-mini");
        assert_eq!(provider, "github-models");
        unsafe {
            std::env::remove_var("KARS_TASK_DEFAULT_MODEL");
            std::env::remove_var("KARS_TASK_DEFAULT_PROVIDER");
        }
    }

    #[test]
    fn runtime_variant_keys() {
        assert_eq!(runtime_variant_key("OpenClaw"), "openclaw");
        assert_eq!(runtime_variant_key("Hermes"), "hermes");
        assert_eq!(runtime_variant_key("OpenAIAgents"), "openaiAgents");
        assert_eq!(runtime_variant_key("anything-else"), "openclaw");
    }

    #[test]
    fn governance_disabled_without_tool_policy() {
        let e = TaskEnvelope {
            tier: 3,
            authority_ceiling: 2,
            delegation_depth: 1,
            budget: None,
            tool_policy_ref: None,
            egress_allowlist_ref: None,
        };
        let bp = TaskBlueprint::default();
        let g = governance_spec(&bp, &e);
        assert_eq!(g["enabled"], false);
        assert!(g.get("toolPolicyRef").is_none());
    }

    #[test]
    fn governance_uses_envelope_tool_policy() {
        let e = TaskEnvelope {
            tier: 3,
            authority_ceiling: 2,
            delegation_depth: 1,
            budget: None,
            tool_policy_ref: Some(crate::mcp_server::LocalObjectRef { name: "tp".into() }),
            egress_allowlist_ref: None,
        };
        let g = governance_spec(&TaskBlueprint::default(), &e);
        assert_eq!(g["enabled"], true);
        assert_eq!(g["toolPolicyRef"]["name"], "tp");
    }

    #[test]
    fn governance_blueprint_tool_policy_carries_mcp_refs() {
        let e = TaskEnvelope {
            tier: 3,
            authority_ceiling: 2,
            delegation_depth: 1,
            budget: None,
            tool_policy_ref: None,
            egress_allowlist_ref: None,
        };
        let bp = TaskBlueprint {
            tool_policy: Some("eng-tools".into()),
            mcp_servers: vec!["docs-index".into(), "jira".into()],
            ..Default::default()
        };
        let g = governance_spec(&bp, &e);
        assert_eq!(g["enabled"], true);
        assert_eq!(g["toolPolicyRef"]["name"], "eng-tools");
        assert_eq!(g["mcpServerRefs"][0]["name"], "docs-index");
        assert_eq!(g["mcpServerRefs"][1]["name"], "jira");
    }

    #[test]
    fn degraded_phase_explains_inference_caveat() {
        let (phase, detail) = map_sandbox_phase("Degraded");
        assert_eq!(phase, "Degraded");
        assert!(detail.contains("Foundry"));
    }

    #[test]
    fn running_phase_maps_through() {
        let (phase, _) = map_sandbox_phase("Running");
        assert_eq!(phase, "Running");
    }
}
