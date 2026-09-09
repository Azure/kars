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

use kube::api::{Api, DeleteParams, DynamicObject, ObjectMeta, PostParams, Preconditions};
use kube::core::ApiResource;
use kube::{Client, ResourceExt};
use serde_json::{Value, json};

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

/// Controller owner reference binding materialized resources to the task UID.
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

fn runtime_spec(task: &KarsTask) -> Result<crate::crd::RuntimeSpec, kube::Error> {
    use crate::crd::{RuntimeKind, RuntimeSpec};
    let kind = crate::kars_task::task_runtime(&task.spec).map_err(contract_error)?;
    let mut runtime = RuntimeSpec {
        kind: kind.clone(),
        openclaw: None,
        ..RuntimeSpec::default()
    };
    match kind {
        RuntimeKind::OpenClaw => runtime.openclaw = Some(Default::default()),
        RuntimeKind::OpenAIAgents => runtime.openai_agents = Some(Default::default()),
        RuntimeKind::MicrosoftAgentFramework => {
            runtime.microsoft_agent_framework = Some(Default::default());
        }
        RuntimeKind::Hermes => runtime.hermes = Some(Default::default()),
        _ => return Err(contract_error("unsupported task runtime".into())),
    }
    Ok(runtime)
}

fn contract_error(message: String) -> kube::Error {
    kube::Error::Api(Box::new(kube::core::Status {
        message,
        reason: "Conflict".into(),
        code: 409,
        ..Default::default()
    }))
}

fn network_policy(blueprint: &TaskBlueprint) -> serde_json::Value {
    json!({
        "defaultDeny": true,
        "egressMode": "Strict",
        "allowedEndpoints": blueprint.egress,
    })
}

/// Build primary and fallback routes from the shared normalized blueprint.
fn inference_spec(task_name: &str, blueprint: &TaskBlueprint) -> Result<Value, kube::Error> {
    let primary = blueprint
        .model
        .as_ref()
        .ok_or_else(|| contract_error("effective task model is missing".into()))?;
    Ok(json!({
        "appliesTo": { "sandboxName": task_name },
        "modelPreference": {
            "primary": primary,
            "fallback": crate::task_models::fallback_routes(
                &blueprint.model_fallbacks, &primary.provider, &primary.deployment,
            ),
        },
    }))
}

/// Materialize the InferencePolicy + KarsSandbox for a launched task using
/// atomic creation or version-checked owned updates, then read sandbox status.
pub async fn materialize(
    client: &Client,
    namespace: &str,
    task: &KarsTask,
) -> Result<ExecutionOutcome, kube::Error> {
    if crate::kars_task_reconciler::rebind::pending(task) {
        return Err(contract_error(
            "Credential rebind is awaiting owned runtime quiescence".into(),
        ));
    }
    crate::kars_task::validate_execution_contract(&task.spec).map_err(contract_error)?;
    let task_name = task.name_any();
    let inference_name = format!("{task_name}-inference");
    let envelope = &task.spec.envelope;
    let blueprint = crate::kars_task::blueprint::effective_blueprint(&task.spec);
    let runtime = runtime_spec(task)?;

    // 1. InferencePolicy scoped to this sandbox. Model: blueprint wins, else
    //    the controller default (required — without it the sandbox degrades).
    let inference_spec = inference_spec(&task_name, &blueprint)?;
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
    let mut sandbox_spec = json!({
        "runtime": runtime,
        "inferenceRef": { "name": inference_name },
        "sandbox": { "isolation": blueprint.isolation },
        "networkPolicy": network_policy(&blueprint),
        "credentialBindings": blueprint.credential_bindings,
        "githubBinding": blueprint.github_binding,
    });

    // Agent instructions (the system prompt) — combine the objective with any
    // standing instructions the blueprint carries, so the agent knows both
    // *what* to do and *how* to behave.
    sandbox_spec["agent"] = json!({ "instructions": blueprint.instructions });

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
            if !owned_by_task(&sb, task) {
                return Err(contract_error(
                    "sandbox was replaced after materialization".into(),
                ));
            }
            if sb
                .annotations()
                .contains_key(crate::kars_task_reconciler::rebind::HOLD)
            {
                return Ok(ExecutionOutcome {
                    phase:"Launching".into(),sandbox_name:task_name,
                    detail:"Credential runtime remains held until current authorization and attestation are durable".into(),
                });
            }
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
/// Returns true only once no owned execution resources remain.
pub async fn teardown(
    client: &Client,
    namespace: &str,
    task: &KarsTask,
) -> Result<bool, kube::Error> {
    let task_name = task.name_any();
    let sb_api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &sandbox_api_resource());
    let ip_api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &inference_policy_api_resource());
    let sandbox_gone = delete_owned(&sb_api, &task_name, task).await?;
    let policy_gone = delete_owned(&ip_api, &format!("{task_name}-inference"), task).await?;
    Ok(sandbox_gone && policy_gone)
}

pub(crate) async fn pause_credentials(client: &Client, task: &KarsTask) -> Result<bool, String> {
    let namespace = task
        .namespace()
        .ok_or("Credential Task workspace missing")?;
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &namespace, &sandbox_api_resource());
    let Some(object) = api.get_opt(&task.name_any()).await.map_err(|error| {
        crate::credential_grants::api_error("Read credential Task execution", error)
    })?
    else {
        return Ok(false);
    };
    if !owned_by_task(&object, task) || object.metadata.deletion_timestamp.is_some() {
        return Err("Credential Task cannot pause a foreign or terminating Sandbox".into());
    }
    let sandbox: crate::crd::KarsSandbox = serde_json::from_value(
        serde_json::to_value(object).map_err(|_| "Credential Sandbox serialization failed")?,
    )
    .map_err(|_| "Credential Sandbox is malformed")?;
    if let Some(runtime) = Api::<k8s_openapi::api::core::v1::Namespace>::all(client.clone())
        .get_opt(&format!("kars-{}", sandbox.name_any()))
        .await
        .map_err(|error| {
            crate::credential_grants::api_error("Read credential runtime namespace", error)
        })?
    {
        crate::reconciler::credential_sources::pause_owned(client, &sandbox, &runtime)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(true)
}

pub(crate) async fn credentials_quiescent(
    client: &Client,
    task: &KarsTask,
) -> Result<bool, String> {
    let workspace = task
        .namespace()
        .ok_or("Credential Task workspace missing")?;
    let sandbox = Api::<crate::crd::KarsSandbox>::namespaced(client.clone(), &workspace)
        .get_opt(&task.name_any())
        .await
        .map_err(|e| crate::credential_grants::api_error("Read paused credential Sandbox", e))?;
    let namespace = Api::<k8s_openapi::api::core::v1::Namespace>::all(client.clone())
        .get_opt(&format!("kars-{}", task.name_any()))
        .await
        .map_err(|e| crate::credential_grants::api_error("Read paused credential namespace", e))?;
    let Some(sandbox) = sandbox else {
        return if namespace.is_none() {
            Ok(true)
        } else {
            Err("Credential namespace exists without its current owned Sandbox".into())
        };
    };
    let dynamic: DynamicObject = serde_json::from_value(
        serde_json::to_value(&sandbox)
            .map_err(|_| "Credential Sandbox identity encoding failed")?,
    )
    .map_err(|_| "Credential Sandbox identity invalid")?;
    if !owned_by_task(&dynamic, task) || sandbox.metadata.deletion_timestamp.is_some() {
        return Err("Credential pause cannot adopt a foreign or terminating Sandbox".into());
    }

    pub(crate) async fn hold_credential_runtime(
        client: &Client,
        task: &KarsTask,
    ) -> Result<(), String> {
        let namespace = task
            .namespace()
            .ok_or("Credential Task workspace missing")?;
        let api = Api::<DynamicObject>::namespaced_with(
            client.clone(),
            &namespace,
            &sandbox_api_resource(),
        );
        let Some(sandbox) = api
            .get_opt(&task.name_any())
            .await
            .map_err(|e| crate::credential_grants::api_error("Read credential hold target", e))?
        else {
            return Ok(());
        };
        if !owned_by_task(&sandbox, task) || sandbox.metadata.deletion_timestamp.is_some() {
            return Err("Credential hold target is foreign or terminating".into());
        }
        let marker = crate::kars_task_reconciler::rebind::HOLD;
        if sandbox.annotations().get(marker) == task.metadata.uid.as_ref() {
            return Ok(());
        }
        if sandbox.annotations().contains_key(marker) {
            return Err("Credential runtime is held by another Task UID".into());
        }
        api.patch_metadata(&task.name_any(),&kube::api::PatchParams::default(),&kube::api::Patch::Merge(json!({
            "metadata":{"uid":sandbox.metadata.uid,"resourceVersion":sandbox.metadata.resource_version,
                "annotations":{marker:task.metadata.uid}}
        }))).await.map_err(|e|crate::credential_grants::api_error("Hold owned credential runtime",e))?;
        Ok(())
    }
    match namespace {
        None => Ok(true),
        Some(namespace) => {
            crate::reconciler::credential_sources::quiescent_owned(client, &sandbox, &namespace)
                .await
                .map_err(|e| e.to_string())
        }
    }
}

fn owned_by_task(object: &DynamicObject, task: &KarsTask) -> bool {
    task.metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .is_some_and(|uid| {
            object
                .metadata
                .owner_references
                .as_ref()
                .is_some_and(|owners| {
                    owners
                        .iter()
                        .filter(|owner| owner.controller == Some(true))
                        .count()
                        == 1
                        && owners.iter().any(|owner| {
                            owner.controller == Some(true)
                                && owner.uid == uid
                                && owner.name == task.name_any()
                                && owner.kind == "KarsTask"
                                && owner.api_version == "kars.azure.com/v1alpha1"
                        })
                })
        })
}

fn object_preconditions(object: &DynamicObject) -> Result<Preconditions, kube::Error> {
    let uid = object
        .uid()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| contract_error("resource UID missing".into()))?;
    let resource_version = object
        .resource_version()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| contract_error("resourceVersion missing".into()))?;
    Ok(Preconditions {
        uid: Some(uid),
        resource_version: Some(resource_version),
    })
}

async fn delete_owned(
    api: &Api<DynamicObject>,
    name: &str,
    task: &KarsTask,
) -> Result<bool, kube::Error> {
    let Some(object) = api.get_opt(name).await? else {
        return Ok(true);
    };
    if !owned_by_task(&object, task) {
        return Ok(true);
    }
    if object.metadata.deletion_timestamp.is_none() {
        let params = DeleteParams {
            preconditions: Some(object_preconditions(&object)?),
            ..Default::default()
        };
        match api.delete(name, &params).await {
            Ok(_) => {}
            Err(kube::Error::Api(error)) if error.code == 404 => return Ok(true),
            Err(error) => return Err(error),
        }
    }
    Ok(api
        .get_opt(name)
        .await?
        .is_none_or(|object| !owned_by_task(&object, task)))
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

/// Create atomically or replace an already-owned object using its UID and
/// resourceVersion. Never adopt a same-name customer object or force ownership.
async fn apply_dynamic(
    client: &Client,
    namespace: &str,
    ar: &ApiResource,
    name: &str,
    task: &KarsTask,
    spec: serde_json::Value,
    annotations: Option<std::collections::BTreeMap<String, String>>,
) -> Result<(), kube::Error> {
    if task.uid().is_none_or(|uid| uid.is_empty()) {
        return Err(contract_error("task UID missing".into()));
    }
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
    let params = PostParams {
        field_manager: Some(FIELD_MANAGER.into()),
        ..Default::default()
    };
    match api.get_opt(name).await? {
        None => {
            api.create(&params, &obj).await?;
        }
        Some(mut current) => {
            if !owned_by_task(&current, task) || current.metadata.deletion_timestamp.is_some() {
                return Err(contract_error(format!(
                    "refusing to replace {name}: not owned by this task UID or terminating"
                )));
            }
            object_preconditions(&current)?;
            if ar.kind == "KarsSandbox" {
                if current.data["spec"]["suspended"] == true {
                    obj.data["spec"]["suspended"] = true.into();
                }
                if let Some(reference) = current.data["spec"]
                    .get("credentialsRef")
                    .filter(|value| !value.is_null())
                    .cloned()
                {
                    if obj.data["spec"]["credentialBindings"].is_object()
                        && !reference["name"]
                            .as_str()
                            .is_some_and(|name| name.starts_with("kars-credential-bundle-"))
                    {
                        return Err(contract_error("Existing v1 runtime credentials require explicit migration before a governed rebind".into()));
                    }
                    obj.data["spec"]["credentialsRef"] = reference;
                }
            }
            current.data["spec"] = obj.data["spec"].clone();
            current
                .metadata
                .labels
                .get_or_insert_default()
                .extend(obj.metadata.labels.unwrap_or_default());
            current
                .metadata
                .annotations
                .get_or_insert_default()
                .extend(obj.metadata.annotations.unwrap_or_default());
            api.replace(name, &params, &current).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "kars_task_execution_tests.rs"]
mod api_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_task::blueprint::build_instructions;

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
    fn runtime_variants_follow_the_sandbox_contract() {
        for (input, canonical, key) in [
            ("OpenClaw", "OpenClaw", "openclaw"),
            ("OpenAIAgents", "OpenAIAgents", "openaiAgents"),
            ("MAF", "MicrosoftAgentFramework", "microsoftAgentFramework"),
            (
                "MicrosoftAgentFramework",
                "MicrosoftAgentFramework",
                "microsoftAgentFramework",
            ),
            ("Hermes", "Hermes", "hermes"),
        ] {
            let task = KarsTask::new(
                "t",
                crate::kars_task::KarsTaskSpec {
                    blueprint: Some(TaskBlueprint {
                        runtime: Some(input.into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            );
            let runtime = runtime_spec(&task).unwrap();
            crate::reconciler::runtime::validate_runtime_shape(&runtime).unwrap();
            let value = serde_json::to_value(runtime).unwrap();
            assert_eq!(value["kind"], canonical);
            assert!(value.get(key).is_some());
        }
    }

    #[test]
    fn empty_task_egress_is_strict_without_changing_standalone_default() {
        let policy = network_policy(&TaskBlueprint::default());
        assert_eq!(policy["egressMode"], "Strict");
        assert_eq!(policy["allowedEndpoints"], json!([]));
        let standalone: crate::crd::NetworkPolicyConfig =
            serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            serde_json::to_value(standalone).unwrap()["egressMode"],
            "Learn"
        );
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
