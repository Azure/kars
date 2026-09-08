// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bind declared and effective launch configuration without treating the
//! receipt digest as the separate approval-binding digest or runtime evidence.

use serde::Serialize;

use crate::kars_task::blueprint::effective_blueprint;
use crate::kars_task::{KarsTask, KarsTaskSpec, TaskBlueprint};
use crate::providers::signing::content_digest;

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateEnvelope {
    pub tier: i32,
    pub authority_ceiling: i32,
    pub delegation_depth: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_policy_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress_allowlist_ref: Option<String>,
    pub digest: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateExecution {
    pub launched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_ref: Option<String>,
}

/// Legacy display summaries plus the complete input to task materialization.
/// Policy references bind names, not the contents of separately mutable CRs.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateLaunchPackage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_policy: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// Full typed specification, including egress, budgets, policy references,
    /// execution/runtime fallback, objective, and delegation parent. This is
    /// the raw declaration, not a round-trip of the normalized blueprint.
    pub configuration: KarsTaskSpec,
    /// The same resolved settings used by authorization and materialization.
    /// Instructions already contain the objective; never write this snapshot
    /// back into `spec.blueprint` as if it were the original declaration.
    pub effective_blueprint: TaskBlueprint,
    /// `sha256:` over canonical JSON of both configuration snapshots.
    pub digest: String,
}

pub(super) fn build_launch_package(task: &KarsTask) -> Option<PredicateLaunchPackage> {
    launch_package_with_blueprint(task, effective_blueprint(&task.spec))
}

fn configuration_digest(configuration: &KarsTaskSpec, effective: &TaskBlueprint) -> String {
    let mut canonical = serde_json::json!({
        "configuration": configuration,
        "effectiveBlueprint": effective,
    });
    canonical.sort_all_objects();
    content_digest(&serde_json::to_vec(&canonical).expect("Launch configuration always serializes"))
}

fn launch_package_with_blueprint(
    task: &KarsTask,
    effective: TaskBlueprint,
) -> Option<PredicateLaunchPackage> {
    if task.spec.blueprint.is_none() && task.spec.execution.is_none() {
        return None;
    }
    let configuration = task.spec.clone();
    let digest = configuration_digest(&configuration, &effective);
    Some(PredicateLaunchPackage {
        runtime: effective.runtime.clone(),
        model: effective
            .model
            .as_ref()
            .map(|m| format!("{}/{}", m.provider, m.deployment)),
        tool_policy: effective.tool_policy.clone(),
        mcp_servers: effective.mcp_servers.clone(),
        isolation: effective.isolation.clone(),
        memory: effective.memory.clone(),
        configuration,
        effective_blueprint: effective,
        digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_task::blueprint::effective_blueprint_with_model;
    use crate::kars_task::{
        KarsTaskStatus, TaskBlueprint, TaskBudget, TaskEgress, TaskEnvelope, TaskExecution,
        TaskModel,
    };
    use crate::mcp_server::LocalObjectRef;

    fn package(task: &KarsTask) -> Option<PredicateLaunchPackage> {
        let model = TaskModel {
            provider: "azure-openai".into(),
            deployment: "reviewed-model".into(),
        };
        launch_package_with_blueprint(task, effective_blueprint_with_model(&task.spec, &model))
    }

    fn task() -> KarsTask {
        KarsTask::new(
            "mission",
            KarsTaskSpec {
                objective: "Review changes".into(),
                envelope: TaskEnvelope {
                    tier: 4,
                    authority_ceiling: 3,
                    delegation_depth: 2,
                    budget: Some(TaskBudget {
                        tokens: Some(1000),
                        usd_micros: Some(2000),
                    }),
                    tool_policy_ref: Some(LocalObjectRef {
                        name: "bounded-tools".into(),
                    }),
                    egress_allowlist_ref: Some(LocalObjectRef {
                        name: "bounded-egress".into(),
                    }),
                },
                parent_ref: Some(LocalObjectRef {
                    name: "parent".into(),
                }),
                execution: Some(TaskExecution {
                    launch: true,
                    runtime: Some("Hermes".into()),
                }),
                blueprint: Some(TaskBlueprint {
                    runtime: Some("OpenClaw".into()),
                    model: Some(TaskModel {
                        provider: "azure-openai".into(),
                        deployment: "model".into(),
                    }),
                    instructions: Some("Follow the review policy".into()),
                    tool_policy: Some("review-tools".into()),
                    mcp_servers: vec!["review-mcp".into()],
                    egress: vec![TaskEgress {
                        host: "api.example.test".into(),
                        port: Some(443),
                    }],
                    isolation: Some("enhanced".into()),
                    memory: Some("review-memory".into()),
                    model_fallbacks: Vec::new(),
                }),
                display_name: Some("Review".into()),
            },
        )
    }

    #[test]
    fn complete_configuration_is_present_and_independently_redigestible() {
        let task = task();
        let package = package(&task).unwrap();
        assert_eq!(
            serde_json::to_value(&package.configuration).unwrap(),
            serde_json::to_value(&task.spec).unwrap()
        );
        assert_eq!(
            package.digest,
            configuration_digest(&package.configuration, &package.effective_blueprint)
        );
        let encoded = serde_json::to_value(package).unwrap();
        assert_eq!(
            encoded["configuration"]["blueprint"]["egress"][0]["host"],
            "api.example.test"
        );
        assert_eq!(
            encoded["configuration"]["blueprint"]["egress"][0]["port"],
            443
        );
        assert_eq!(encoded["model"], "azure-openai/model");
        assert_eq!(
            encoded["effectiveBlueprint"]["instructions"],
            "Your objective:\nReview changes\n\nAdditional instructions:\nFollow the review policy"
        );
        assert_eq!(
            encoded["configuration"]["blueprint"]["instructions"],
            "Follow the review policy"
        );
    }

    #[test]
    fn complete_configuration_digest_has_a_stable_golden_encoding() {
        let task = KarsTask::new(
            "example",
            KarsTaskSpec {
                objective: "example".into(),
                blueprint: Some(TaskBlueprint::default()),
                ..Default::default()
            },
        );
        let package = package(&task).unwrap();
        let canonical = r#"{"configuration":{"blueprint":{},"envelope":{"authorityCeiling":1,"delegationDepth":0,"tier":1},"objective":"example"},"effectiveBlueprint":{"instructions":"Your objective:\nexample","isolation":"standard","model":{"deployment":"reviewed-model","provider":"azure-openai"},"runtime":"OpenClaw"}}"#;
        let mut snapshots = serde_json::json!({
            "configuration": package.configuration,
            "effectiveBlueprint": package.effective_blueprint,
        });
        snapshots.sort_all_objects();
        assert_eq!(serde_json::to_string(&snapshots).unwrap(), canonical);
        assert_eq!(package.digest, content_digest(canonical.as_bytes()));
        assert_eq!(package.digest, "sha256:927490aa60df429ecb4ac9627b4989bd");
    }

    #[test]
    fn every_current_authority_input_changes_the_launch_digest() {
        let task = task();
        let original = serde_json::to_value(&task.spec).unwrap();
        let digest = package(&task).unwrap().digest;
        for (pointer, replacement) in [
            ("/objective", serde_json::json!("Different objective")),
            ("/parentRef/name", serde_json::json!("different-parent")),
            ("/envelope/tier", serde_json::json!(3)),
            ("/envelope/authorityCeiling", serde_json::json!(2)),
            ("/envelope/delegationDepth", serde_json::json!(1)),
            ("/envelope/budget/tokens", serde_json::json!(999)),
            ("/envelope/budget/usdMicros", serde_json::json!(1999)),
            (
                "/envelope/toolPolicyRef/name",
                serde_json::json!("other-tools"),
            ),
            (
                "/envelope/egressAllowlistRef/name",
                serde_json::json!("other-egress"),
            ),
            ("/envelope/budget", serde_json::Value::Null),
            ("/envelope/toolPolicyRef", serde_json::Value::Null),
            ("/envelope/egressAllowlistRef", serde_json::Value::Null),
            ("/execution/launch", serde_json::json!(false)),
            ("/execution/runtime", serde_json::json!("OpenAIAgents")),
            ("/blueprint/runtime", serde_json::json!("Hermes")),
            (
                "/blueprint/model/provider",
                serde_json::json!("different-provider"),
            ),
            (
                "/blueprint/model/deployment",
                serde_json::json!("different-deployment"),
            ),
            (
                "/blueprint/instructions",
                serde_json::json!("Different instructions"),
            ),
            (
                "/blueprint/toolPolicy",
                serde_json::json!("different-tools"),
            ),
            (
                "/blueprint/mcpServers",
                serde_json::json!(["different-mcp"]),
            ),
            (
                "/blueprint/egress/0/host",
                serde_json::json!("different.example.test"),
            ),
            ("/blueprint/egress/0/port", serde_json::json!(8443)),
            ("/blueprint/egress/0/port", serde_json::Value::Null),
            ("/blueprint/egress", serde_json::json!([])),
            ("/blueprint/isolation", serde_json::json!("standard")),
            ("/blueprint/memory", serde_json::json!("different-memory")),
        ] {
            let mut candidate = task.clone();
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            candidate.spec = serde_json::from_value(changed).unwrap();
            assert_ne!(digest, package(&candidate).unwrap().digest, "{pointer}");
        }
    }

    #[test]
    fn legacy_execution_runtime_is_bound_without_a_blueprint() {
        let mut task = task();
        task.spec.blueprint = None;
        let first = package(&task).unwrap();
        assert_eq!(first.runtime.as_deref(), Some("Hermes"));
        task.spec.execution.as_mut().unwrap().runtime = Some("OpenClaw".into());
        assert_ne!(first.digest, package(&task).unwrap().digest);
        task.spec.execution = None;
        assert!(package(&task).is_none());
    }

    #[test]
    fn egress_changes_the_signed_statement_and_requires_fresh_authorization() {
        let task = task();
        let status = KarsTaskStatus {
            phase: Some("Ready".into()),
            envelope_digest: Some(task.envelope_digest()),
            ..Default::default()
        };
        let statement = |task: &KarsTask, status: &KarsTaskStatus| {
            super::super::canonical_json(
                &super::super::build_statement(
                    task,
                    status,
                    "key-id",
                    &[],
                    super::super::PredicateCompleteness::default(),
                )
                .unwrap(),
            )
        };
        let mut changed = task.clone();
        changed.spec.blueprint.as_mut().unwrap().egress[0].host = "other.example.test".into();
        assert_eq!(task.spec.envelope.digest(), changed.spec.envelope.digest());
        assert_ne!(task.envelope_digest(), changed.envelope_digest());
        assert!(
            super::super::build_statement(
                &changed,
                &status,
                "key-id",
                &[],
                super::super::PredicateCompleteness::default(),
            )
            .is_none()
        );
        let refreshed = KarsTaskStatus {
            envelope_digest: Some(changed.envelope_digest()),
            ..status.clone()
        };
        assert_ne!(statement(&task, &status), statement(&changed, &refreshed));
    }

    #[test]
    fn kubernetes_metadata_does_not_change_launch_configuration_digest() {
        let task = task();
        let mut updated = task.clone();
        updated.metadata.resource_version = Some("2".into());
        updated.metadata.generation = Some(2);
        updated.metadata.namespace = Some("tenant-b".into());
        assert_eq!(
            package(&task).unwrap().digest,
            package(&updated).unwrap().digest
        );
    }

    #[test]
    fn changed_controller_defaults_change_evidence_without_rewriting_the_declaration() {
        let mut task = task();
        task.spec.blueprint.as_mut().unwrap().model = None;
        let first = package(&task).unwrap();
        let changed_model = TaskModel {
            provider: "different-provider".into(),
            deployment: "different-model".into(),
        };
        let second = launch_package_with_blueprint(
            &task,
            effective_blueprint_with_model(&task.spec, &changed_model),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&first.configuration).unwrap(),
            serde_json::to_value(&second.configuration).unwrap()
        );
        assert_ne!(first.digest, second.digest);
        assert_eq!(
            second.model.as_deref(),
            Some("different-provider/different-model")
        );
    }
}
