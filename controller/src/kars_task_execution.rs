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

use crate::kars_task::{KarsTask, TaskEnvelope};

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
    let runtime_kind = task
        .spec
        .execution
        .as_ref()
        .and_then(|e| e.runtime.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "OpenClaw".to_string());

    // 1. Minimal InferencePolicy scoped to this sandbox. Token budget mirrors
    //    the envelope when present (the router's TokenBudgetTracker enforces).
    let mut inference_spec = json!({
        "appliesTo": { "sandboxName": task_name },
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

    // 2. KarsSandbox bounded by the envelope. Tool policy from the envelope is
    //    wired into governance; egress allow-list (when present) rides the
    //    existing per-sandbox egress machinery via the same-named ref.
    let mut sandbox_spec = json!({
        "runtime": {
            "kind": runtime_kind,
            runtime_variant_key(&runtime_kind): {},
        },
        "inferenceRef": { "name": inference_name },
        "sandbox": { "isolation": "standard" },
        "networkPolicy": { "defaultDeny": true },
    });
    governance_block(envelope).inspect(|g| {
        sandbox_spec["governance"] = g.clone();
    });
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
    // Best-effort: ignore 404s.
    let _ = sb_api.delete(&task_name, &DeleteParams::default()).await;
    let _ = ip_api
        .delete(&format!("{task_name}-inference"), &DeleteParams::default())
        .await;
    Ok(())
}

/// Build the governance block from the envelope's tool-policy ref, if any.
fn governance_block(envelope: &TaskEnvelope) -> Option<serde_json::Value> {
    envelope.tool_policy_ref.as_ref().map(|r| {
        json!({
            "enabled": true,
            "toolPolicyRef": { "name": r.name },
        })
    })
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
    use crate::kars_task::TaskBudget;

    #[test]
    fn runtime_variant_keys() {
        assert_eq!(runtime_variant_key("OpenClaw"), "openclaw");
        assert_eq!(runtime_variant_key("Hermes"), "hermes");
        assert_eq!(runtime_variant_key("OpenAIAgents"), "openaiAgents");
        assert_eq!(runtime_variant_key("anything-else"), "openclaw");
    }

    #[test]
    fn governance_block_present_only_with_tool_policy() {
        let mut e = TaskEnvelope {
            tier: 3,
            authority_ceiling: 2,
            delegation_depth: 1,
            budget: Some(TaskBudget {
                tokens: Some(1000),
                usd_micros: None,
            }),
            tool_policy_ref: None,
            egress_allowlist_ref: None,
        };
        assert!(governance_block(&e).is_none());
        e.tool_policy_ref = Some(crate::mcp_server::LocalObjectRef { name: "tp".into() });
        let g = governance_block(&e).expect("present");
        assert_eq!(g["toolPolicyRef"]["name"], "tp");
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
