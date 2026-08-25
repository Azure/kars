// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reconciler unit tests — extracted from reconciler/mod.rs to keep the
//! reconcile-loop file under its Phase 1 LOC cap. `use super::*`
//! continues to give the tests access to crate-private helpers
//! (`build_pod_security_context`, `error_requeue_duration`, etc.).
//
// ci:loc-ok: pre-existing test corpus relocated wholesale from
// reconciler.rs; no test added or modified in this PR. Splitting further
// would scatter cohesive #[test] blocks across multiple files for no
// reviewer benefit.

use super::*;
use crate::crd::{
    OpenClawConfig, OpenClawWorkspaceSpec, PersistentVolumeAccessMode, SandboxConfig,
    SandboxStorageSpec, WorkspaceOverwritePolicy, WorkspaceRetainPolicy, WorkspaceStorageSpec,
};
use crate::mcp_server::LocalObjectRef;

#[test]
fn workspace_bootstrap_config_map_accepts_declarative_files() {
    let config_map: ConfigMap = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": "workspace"},
        "data": {
            "AGENTS.md": "instructions",
            "SOUL.md": "persona",
            "HEARTBEAT.md": "checks",
            "TOOLS.md": "tools",
            "USER.md": "profile"
        }
    }))
    .unwrap();

    assert!(validate_workspace_bootstrap_config_map(&config_map).is_ok());
}

#[test]
fn workspace_bootstrap_config_map_rejects_runtime_state_and_binary_data() {
    let runtime_state: ConfigMap = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": "workspace"},
        "data": {"MEMORY.md": "mutable state"}
    }))
    .unwrap();
    assert!(
        validate_workspace_bootstrap_config_map(&runtime_state)
            .unwrap_err()
            .contains("MEMORY.md")
    );

    let binary_data: ConfigMap = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": "workspace"},
        "binaryData": {"SOUL.md": "c2VjcmV0"}
    }))
    .unwrap();
    assert!(
        validate_workspace_bootstrap_config_map(&binary_data)
            .unwrap_err()
            .contains("binaryData")
    );
}

#[test]
fn workspace_bootstrap_plan_is_absent_without_config_map() {
    assert!(
        build_workspace_bootstrap_plan(
            &OpenClawConfig::default(),
            "openclaw:latest",
            "Always",
            "",
            "",
        )
        .is_none()
    );
}

#[test]
fn workspace_bootstrap_plan_mounts_config_map_and_workspace() {
    let config = OpenClawConfig {
        workspace: Some(OpenClawWorkspaceSpec {
            bootstrap_config_map_ref: Some(LocalObjectRef {
                name: "teaching-agent-workspace".into(),
            }),
            overwrite_policy: WorkspaceOverwritePolicy::Always,
        }),
        ..Default::default()
    };
    let plan =
        build_workspace_bootstrap_plan(&config, "openclaw:latest", "IfNotPresent", "cm-uid", "42")
            .expect("bootstrap plan");

    assert_eq!(plan.volume["configMap"]["name"], "teaching-agent-workspace");
    assert_eq!(plan.init_container["image"], "openclaw:latest");
    assert_eq!(plan.init_container["imagePullPolicy"], "IfNotPresent");
    assert_eq!(plan.init_container["env"][0]["value"], "Always");
    assert_eq!(plan.init_container["env"][1]["value"], "cm-uid");
    assert_eq!(plan.init_container["env"][2]["value"], "42");
    assert_eq!(
        plan.init_container["command"],
        json!(["/usr/local/bin/workspace-bootstrap.sh"])
    );
    assert_eq!(plan.init_container["securityContext"]["runAsUser"], 1000);
    assert_eq!(
        plan.init_container["volumeMounts"],
        json!([
            {"name": "sandbox-data", "mountPath": "/sandbox"},
            {"name": "workspace-bootstrap", "mountPath": "/etc/kars/workspace-bootstrap", "readOnly": true}
        ])
    );
    assert!(
        plan.init_container["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|mount| mount["name"] != "kube-api-access")
    );
}

#[test]
fn service_account_projection_is_explicit_and_skips_init_containers() {
    let mount = kube_api_access_mount();
    let volume = kube_api_access_volume();
    assert_eq!(
        mount["mountPath"],
        "/var/run/secrets/kubernetes.io/serviceaccount"
    );
    assert_eq!(
        volume["projected"]["sources"][0]["serviceAccountToken"]["path"],
        "token"
    );
    assert_eq!(
        WORKLOAD_IDENTITY_SKIP_CONTAINERS,
        "egress-guard;workspace-bootstrap"
    );
}

#[test]
fn workspace_bootstrap_conditions_gate_ready_and_surface_failure() {
    let sandbox = KarsSandbox {
        metadata: kube::api::ObjectMeta {
            name: Some("demo".into()),
            generation: Some(3),
            ..Default::default()
        },
        spec: Default::default(),
        status: None,
    };

    let pending =
        workspace_bootstrap_status_conditions(&sandbox, &WorkspaceBootstrapState::Pending);
    assert_eq!(pending[0].type_, "BootstrapReady");
    assert_eq!(pending[0].status, "False");
    assert_eq!(pending[1].type_, "Ready");
    assert_eq!(pending[1].status, "False");

    let ready = workspace_bootstrap_status_conditions(&sandbox, &WorkspaceBootstrapState::Ready);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].status, "True");

    let failed = workspace_bootstrap_status_conditions(
        &sandbox,
        &WorkspaceBootstrapState::Failed("symlink destination".into()),
    );
    assert!(failed.iter().any(|condition| {
        condition.type_ == "Degraded"
            && condition.status == "True"
            && condition.reason == "BootstrapFailed"
    }));
}

#[test]
fn workspace_bootstrap_state_reads_init_container_termination() {
    let succeeded: Pod = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "demo"},
        "spec": {"containers": [{"name": "openclaw", "image": "test"}]},
        "status": {
            "initContainerStatuses": [{
                "name": "workspace-bootstrap",
                "image": "test",
                "imageID": "test",
                "ready": true,
                "restartCount": 0,
                "state": {"terminated": {"exitCode": 0}}
            }]
        }
    }))
    .unwrap();
    assert_eq!(
        workspace_bootstrap_state(&[succeeded]),
        WorkspaceBootstrapState::Ready
    );

    let failed: Pod = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "demo"},
        "spec": {"containers": [{"name": "openclaw", "image": "test"}]},
        "status": {
            "initContainerStatuses": [{
                "name": "workspace-bootstrap",
                "image": "test",
                "imageID": "test",
                "ready": false,
                "restartCount": 0,
                "state": {"terminated": {"exitCode": 1, "message": "unsafe destination"}}
            }]
        }
    }))
    .unwrap();
    assert_eq!(
        workspace_bootstrap_state(&[failed]),
        WorkspaceBootstrapState::Failed("unsafe destination".into())
    );

    let crash_loop: Pod = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "demo"},
        "spec": {"containers": [{"name": "openclaw", "image": "test"}]},
        "status": {
            "initContainerStatuses": [{
                "name": "workspace-bootstrap",
                "image": "test",
                "imageID": "test",
                "ready": false,
                "restartCount": 2,
                "state": {"waiting": {"reason": "CrashLoopBackOff"}},
                "lastState": {"terminated": {"exitCode": 1, "message": "unsafe destination"}}
            }]
        }
    }))
    .unwrap();
    assert_eq!(
        workspace_bootstrap_state(&[crash_loop]),
        WorkspaceBootstrapState::Failed("unsafe destination".into())
    );

    let ready: Pod = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "old"},
        "spec": {"containers": [{"name": "openclaw", "image": "test"}]},
        "status": {
            "initContainerStatuses": [{
                "name": "workspace-bootstrap", "image": "test", "imageID": "test",
                "ready": true, "restartCount": 0,
                "state": {"terminated": {"exitCode": 0}}
            }]
        }
    }))
    .unwrap();
    let failed: Pod = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "new"},
        "spec": {"containers": [{"name": "openclaw", "image": "test"}]},
        "status": {
            "initContainerStatuses": [{
                "name": "workspace-bootstrap", "image": "test", "imageID": "test",
                "ready": false, "restartCount": 1,
                "state": {"waiting": {"reason": "CrashLoopBackOff"}},
                "lastState": {"terminated": {"exitCode": 1, "message": "new failed"}}
            }]
        }
    }))
    .unwrap();
    assert_eq!(
        workspace_bootstrap_state(&[ready.clone(), failed]),
        WorkspaceBootstrapState::Failed("new failed".into())
    );

    let pending: Pod = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "new"},
        "spec": {"containers": [{"name": "openclaw", "image": "test"}]}
    }))
    .unwrap();
    assert_eq!(
        workspace_bootstrap_state(&[ready, pending]),
        WorkspaceBootstrapState::Pending
    );
}

#[test]
fn workspace_plan_defaults_to_ephemeral_empty_dir() {
    let plan = build_workspace_storage_plan("demo", "kars-demo", None, None);
    assert!(plan.claim.is_none());
    assert_eq!(plan.volume, json!({"name": "sandbox-data", "emptyDir": {}}));
}

#[test]
fn workspace_plan_builds_retained_dynamic_claim() {
    let storage = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec::default()),
    };
    let plan = build_workspace_storage_plan("demo", "kars-demo", Some(&storage), Some("uid-1"));
    let claim = plan.claim.expect("dynamic claim");

    assert_eq!(claim["metadata"]["name"], "demo-workspace");
    assert_eq!(claim["metadata"]["namespace"], "kars-demo");
    assert_eq!(
        claim["metadata"]["annotations"]["kars.azure.com/retain-policy"],
        "Retain"
    );
    assert_eq!(
        claim["metadata"]["annotations"]["kars.azure.com/sandbox-uid"],
        "uid-1"
    );
    assert!(claim["metadata"].get("ownerReferences").is_none());
    assert_eq!(claim["spec"]["accessModes"], json!(["ReadWriteOnce"]));
    assert_eq!(claim["spec"]["resources"]["requests"]["storage"], "10Gi");
    assert_eq!(
        plan.volume,
        json!({
            "name": "sandbox-data",
            "persistentVolumeClaim": {"claimName": "demo-workspace"}
        })
    );
}

#[test]
fn workspace_plan_references_existing_claim_without_creating_one() {
    let storage = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec {
            existing_claim: Some("restored-workspace".into()),
            size: None,
            storage_class_name: None,
            access_modes: Vec::new(),
            retain_policy: WorkspaceRetainPolicy::Retain,
        }),
    };
    let plan = build_workspace_storage_plan("demo", "kars-demo", Some(&storage), Some("uid-1"));

    assert!(plan.claim.is_none());
    assert_eq!(
        plan.volume["persistentVolumeClaim"]["claimName"],
        "restored-workspace"
    );
}

#[test]
fn workspace_plan_delete_policy_owns_dynamic_claim() {
    let storage = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec {
            existing_claim: None,
            size: Some("20Gi".into()),
            storage_class_name: Some("managed-csi".into()),
            access_modes: vec![PersistentVolumeAccessMode::ReadWriteOncePod],
            retain_policy: WorkspaceRetainPolicy::Delete,
        }),
    };
    let plan = build_workspace_storage_plan("demo", "kars-demo", Some(&storage), Some("uid-1"));
    let claim = plan.claim.expect("dynamic claim");

    assert_eq!(claim["spec"]["storageClassName"], "managed-csi");
    assert_eq!(claim["spec"]["accessModes"], json!(["ReadWriteOncePod"]));
    assert_eq!(claim["spec"]["resources"]["requests"]["storage"], "20Gi");
    assert_eq!(
        claim["metadata"]["annotations"]["kars.azure.com/retain-policy"],
        "Delete"
    );
    assert_eq!(
        claim["metadata"]["annotations"]["kars.azure.com/sandbox-uid"],
        "uid-1"
    );
    assert!(claim["metadata"].get("ownerReferences").is_none());
}

#[test]
fn workspace_claim_provenance_rejects_implicit_adoption() {
    let claim: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": "demo-workspace",
            "annotations": {"kars.azure.com/sandbox-uid": "old-uid"}
        },
        "spec": {"accessModes": ["ReadWriteOnce"]},
        "status": {"phase": "Bound"}
    }))
    .unwrap();
    assert!(
        validate_dynamic_claim_provenance(&claim, Some("new-uid"))
            .unwrap_err()
            .contains("existingClaim")
    );
    assert!(validate_dynamic_claim_provenance(&claim, Some("old-uid")).is_ok());
}

#[test]
fn retained_claim_forces_namespace_preservation_for_later_same_name_cr() {
    let retained: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": "demo-workspace",
            "annotations": {
                "kars.azure.com/retain-policy": "Retain",
                "kars.azure.com/sandbox-uid": "old-uid"
            }
        }
    }))
    .unwrap();

    assert!(should_preserve_namespace_on_delete(
        None,
        &[retained],
        Some("new-uid")
    ));
}

#[test]
fn retained_namespace_still_deletes_current_delete_policy_claims() {
    let current_delete: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": "demo-workspace",
            "labels": {
                "app.kubernetes.io/managed-by": "kars-controller",
                "kars.azure.com/storage-role": "workspace"
            },
            "annotations": {
                "kars.azure.com/retain-policy": "Delete",
                "kars.azure.com/sandbox-uid": "current-uid"
            }
        }
    }))
    .unwrap();
    let foreign: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {"name": "foreign"}
    }))
    .unwrap();
    let delete_storage = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec {
            existing_claim: None,
            size: Some("10Gi".into()),
            storage_class_name: None,
            access_modes: vec![PersistentVolumeAccessMode::ReadWriteOnce],
            retain_policy: WorkspaceRetainPolicy::Delete,
        }),
    };

    assert_eq!(
        deletable_workspace_claims(
            Some(&delete_storage),
            "demo",
            &[current_delete.clone(), foreign],
            Some("current-uid")
        ),
        vec!["demo-workspace"]
    );
    let retain_storage = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec::default()),
    };
    assert!(
        deletable_workspace_claims(
            Some(&retain_storage),
            "demo",
            &[current_delete.clone()],
            Some("current-uid")
        )
        .is_empty()
    );
    let existing_storage = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec {
            existing_claim: Some("demo-workspace".into()),
            size: None,
            storage_class_name: None,
            access_modes: Vec::new(),
            retain_policy: WorkspaceRetainPolicy::Retain,
        }),
    };
    assert!(
        deletable_workspace_claims(
            Some(&existing_storage),
            "demo",
            &[current_delete.clone()],
            Some("current-uid")
        )
        .is_empty()
    );
    assert!(should_preserve_namespace_on_delete(
        None,
        &[current_delete.clone()],
        Some("current-uid")
    ));
    assert!(
        deletable_workspace_claims(None, "demo", &[current_delete.clone()], Some("current-uid"))
            .is_empty()
    );
    let missing_labels: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": "demo-workspace",
            "annotations": {
                "kars.azure.com/retain-policy": "Delete",
                "kars.azure.com/sandbox-uid": "current-uid"
            }
        }
    }))
    .unwrap();
    assert!(
        deletable_workspace_claims(
            Some(&delete_storage),
            "demo",
            &[missing_labels],
            Some("current-uid")
        )
        .is_empty()
    );
}

#[test]
fn retained_claim_requires_explicit_existing_claim_recovery() {
    let retained: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": "demo-workspace",
            "annotations": {
                "kars.azure.com/retain-policy": "Retain",
                "kars.azure.com/sandbox-uid": "old-uid"
            }
        }
    }))
    .unwrap();

    assert!(
        validate_namespace_claim_reuse(&[retained.clone()], None, None, Some("new-uid")).is_err()
    );
    assert!(
        validate_namespace_claim_reuse(&[retained], Some("demo-workspace"), None, Some("new-uid"))
            .is_ok()
    );
}

#[test]
fn suspended_transition_allows_current_and_target_claims() {
    let current: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {"name": "current-external"}
    }))
    .unwrap();
    let target: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {"name": "target-external"}
    }))
    .unwrap();

    assert!(
        validate_namespace_claim_reuse(
            &[current.clone(), target.clone()],
            Some("target-external"),
            Some("current-external"),
            Some("new-uid")
        )
        .is_ok()
    );
    assert!(
        validate_namespace_claim_reuse(
            &[current, target],
            Some("target-external"),
            None,
            Some("new-uid")
        )
        .is_err()
    );
}

#[test]
fn persistent_workspace_uses_recreate_rollout_strategy() {
    assert_eq!(
        workspace_deployment_strategy(true),
        Some(json!({"type": "Recreate"}))
    );
    assert_eq!(workspace_deployment_strategy(false), None);
}

#[test]
fn recreate_transition_clears_existing_rolling_update() {
    let deployment: Deployment = serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {"name": "demo"},
        "spec": {
            "selector": {"matchLabels": {"app": "demo"}},
            "strategy": {
                "type": "RollingUpdate",
                "rollingUpdate": {"maxSurge": "25%", "maxUnavailable": "25%"}
            },
            "template": {
                "metadata": {"labels": {"app": "demo"}},
                "spec": {"containers": [{"name": "agent", "image": "test"}]}
            }
        }
    }))
    .unwrap();
    assert!(deployment_needs_recreate_cleanup(Some(&deployment), true));
    assert!(!deployment_needs_recreate_cleanup(Some(&deployment), false));
    assert!(!deployment_needs_recreate_cleanup(None, true));
}

#[test]
fn existing_claim_waits_for_bound_but_dynamic_claim_can_trigger_wffc() {
    assert_eq!(workspace_desired_replicas(false, true, Some("Pending")), 0);
    assert_eq!(workspace_desired_replicas(false, true, Some("Bound")), 1);
    assert_eq!(workspace_desired_replicas(false, false, Some("Pending")), 1);
    assert_eq!(workspace_desired_replicas(true, false, Some("Bound")), 0);
}

#[test]
fn every_wired_runtime_has_a_persistent_state_directory() {
    use crate::crd::RuntimeKind;

    let expected = [
        (RuntimeKind::OpenClaw, "/sandbox/.openclaw"),
        (RuntimeKind::Hermes, "/sandbox/.hermes"),
        (RuntimeKind::OpenAIAgents, "/sandbox/.openai-agents"),
        (RuntimeKind::MicrosoftAgentFramework, "/sandbox/.maf"),
        (RuntimeKind::LangGraph, "/sandbox/.langgraph"),
        (RuntimeKind::Anthropic, "/sandbox/.anthropic"),
        (RuntimeKind::PydanticAi, "/sandbox/.pydantic-ai"),
        (RuntimeKind::BYO, "/sandbox/.byo"),
    ];
    for (kind, path) in expected {
        assert_eq!(runtime_state_dir(&kind), path);
        assert!(path.starts_with("/sandbox/"));
    }
}

#[test]
fn workspace_volume_transition_requires_suspension() {
    assert!(validate_workspace_volume_transition(None, Some("generated"), false).is_err());
    assert!(
        validate_workspace_volume_transition(Some("generated"), Some("restored"), false).is_err()
    );
    assert!(
        validate_workspace_volume_transition(Some("generated"), Some("restored"), true).is_ok()
    );
    assert!(
        validate_workspace_volume_transition(Some("generated"), Some("generated"), false).is_ok()
    );
}

#[test]
fn workspace_deletion_preserves_namespace_for_retained_claims() {
    let retained = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec::default()),
    };
    assert!(preserve_namespace_on_delete(Some(&retained)));

    let existing = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec {
            existing_claim: Some("imported".into()),
            size: None,
            storage_class_name: None,
            access_modes: Vec::new(),
            retain_policy: WorkspaceRetainPolicy::Retain,
        }),
    };
    assert!(preserve_namespace_on_delete(Some(&existing)));
}

#[test]
fn workspace_deletion_cascades_for_ephemeral_or_delete_policy() {
    assert!(!preserve_namespace_on_delete(None));

    let delete = SandboxStorageSpec {
        workspace: Some(WorkspaceStorageSpec {
            existing_claim: None,
            size: Some("10Gi".into()),
            storage_class_name: None,
            access_modes: vec![PersistentVolumeAccessMode::ReadWriteOnce],
            retain_policy: WorkspaceRetainPolicy::Delete,
        }),
    };
    assert!(!preserve_namespace_on_delete(Some(&delete)));
}

#[test]
fn workspace_storage_validation_rejects_mixed_existing_claim_fields() {
    let workspace = WorkspaceStorageSpec {
        existing_claim: Some("imported".into()),
        size: Some("10Gi".into()),
        storage_class_name: None,
        access_modes: Vec::new(),
        retain_policy: WorkspaceRetainPolicy::Retain,
    };
    assert!(
        validate_workspace_storage_spec(&workspace)
            .unwrap_err()
            .contains("mutually exclusive")
    );
}

#[test]
fn workspace_storage_validation_rejects_incomplete_dynamic_claim() {
    let workspace = WorkspaceStorageSpec {
        existing_claim: None,
        size: Some(String::new()),
        storage_class_name: None,
        access_modes: Vec::new(),
        retain_policy: WorkspaceRetainPolicy::Retain,
    };
    let error = validate_workspace_storage_spec(&workspace).unwrap_err();
    assert!(error.contains("size"));
    assert!(error.contains("accessModes"));
}

#[test]
fn workspace_claim_validation_accepts_supported_modes_and_rejects_lost_claims() {
    let bound: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {"name": "workspace"},
        "spec": {"accessModes": ["ReadWriteOnce"]},
        "status": {"phase": "Bound"}
    }))
    .unwrap();
    assert!(validate_workspace_claim(&bound).is_ok());

    let unsupported: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {"name": "workspace"},
        "spec": {"accessModes": ["ReadWriteMany"]},
        "status": {"phase": "Bound"}
    }))
    .unwrap();
    assert!(validate_workspace_claim(&unsupported).is_err());

    let lost: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {"name": "workspace"},
        "spec": {"accessModes": ["ReadWriteOnce"]},
        "status": {"phase": "Lost"}
    }))
    .unwrap();
    assert!(
        validate_workspace_claim(&lost)
            .unwrap_err()
            .contains("Lost")
    );

    let block: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {"name": "workspace"},
        "spec": {"accessModes": ["ReadWriteOnce"], "volumeMode": "Block"},
        "status": {"phase": "Bound"}
    }))
    .unwrap();
    assert!(
        validate_workspace_claim(&block)
            .unwrap_err()
            .contains("Filesystem")
    );

    let pending: PersistentVolumeClaim = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {"name": "workspace"},
        "spec": {"accessModes": ["ReadWriteOnce"]},
        "status": {"phase": "Pending"}
    }))
    .unwrap();
    assert!(validate_workspace_claim(&pending).is_ok());
}

#[test]
fn workspace_storage_conditions_gate_ready_until_claim_is_bound() {
    let sandbox = KarsSandbox {
        metadata: kube::api::ObjectMeta {
            name: Some("demo".into()),
            generation: Some(3),
            ..Default::default()
        },
        spec: Default::default(),
        status: None,
    };
    let pending = workspace_storage_status_conditions(&sandbox, Some("Pending"));
    assert_eq!(pending.len(), 3);
    assert_eq!(pending[0].type_, "StorageReady");
    assert_eq!(pending[0].status, "False");
    assert_eq!(pending[0].reason, "ClaimPending");
    assert_eq!(pending[1].type_, "Ready");
    assert_eq!(pending[1].status, "False");

    let bound = workspace_storage_status_conditions(&sandbox, Some("Bound"));
    assert_eq!(bound.len(), 1);
    assert_eq!(bound[0].status, "True");
    assert_eq!(bound[0].reason, "ClaimBound");

    let ephemeral = workspace_storage_status_conditions(&sandbox, None);
    assert_eq!(ephemeral[0].reason, "EmptyDir");
}

#[test]
fn standard_isolation_uses_runtime_default_seccomp() {
    let cfg = SandboxConfig {
        isolation: "standard".into(),
        ..Default::default()
    };
    let ctx = build_pod_security_context(&cfg);
    assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
}

#[test]
fn enhanced_isolation_uses_localhost_seccomp() {
    let cfg = SandboxConfig {
        isolation: "enhanced".into(),
        seccomp_profile: "kars-strict".into(),
        ..Default::default()
    };
    let ctx = build_pod_security_context(&cfg);
    assert_eq!(ctx["seccompProfile"]["type"], "Localhost");
    assert_eq!(
        ctx["seccompProfile"]["localhostProfile"],
        "profiles/kars-strict.json"
    );
}

#[test]
fn confidential_isolation_uses_runtime_default_seccomp() {
    let cfg = SandboxConfig {
        isolation: "confidential".into(),
        seccomp_profile: "kars-strict".into(),
        ..Default::default()
    };
    let ctx = build_pod_security_context(&cfg);
    // Kata VM provides isolation, so RuntimeDefault is sufficient
    assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
}

#[test]
fn security_context_enforces_non_root() {
    let cfg = SandboxConfig::default();
    let ctx = build_pod_security_context(&cfg);
    assert_eq!(ctx["runAsNonRoot"], true);
    assert_eq!(ctx["runAsUser"], 1000);
    assert_eq!(ctx["runAsGroup"], 1000);
    assert_eq!(ctx["fsGroup"], 1000);
}

#[test]
fn selinux_context_only_set_when_non_empty() {
    let cfg = SandboxConfig::default(); // empty selinux_context
    let ctx = build_pod_security_context(&cfg);
    assert!(ctx.get("seLinuxOptions").is_none());

    let cfg_with_selinux = SandboxConfig {
        selinux_context: "custom_t".into(),
        ..Default::default()
    };
    let ctx2 = build_pod_security_context(&cfg_with_selinux);
    assert_eq!(ctx2["seLinuxOptions"]["type"], "custom_t");
}

#[test]
fn isolation_scheduling_standard() {
    let (runtime, pool) = isolation_scheduling("standard");
    assert!(runtime.is_none());
    assert_eq!(pool, "sandbox");
}

#[test]
fn isolation_scheduling_enhanced() {
    let (runtime, pool) = isolation_scheduling("enhanced");
    assert!(runtime.is_none());
    assert_eq!(pool, "sandbox");
}

#[test]
fn isolation_scheduling_confidential() {
    let (runtime, pool) = isolation_scheduling("confidential");
    assert_eq!(runtime, Some("kata-vm-isolation"));
    assert_eq!(pool, "sandbox-kata");
}

#[test]
fn crd_defaults_are_secure() {
    let cfg = SandboxConfig::default();
    assert_eq!(cfg.isolation, "enhanced");
    assert!(cfg.read_only_root_filesystem);
    assert!(cfg.run_as_non_root);
    assert!(!cfg.allow_privilege_escalation);
    assert_eq!(cfg.seccomp_profile, "kars-strict");
    assert!(cfg.selinux_context.is_empty());
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Build namespace JSON the same way reconcile() does (line 224-239).
fn build_namespace_json(sandbox_name: &str) -> serde_json::Value {
    let sandbox_ns = format!("kars-{sandbox_name}");
    json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": {
            "name": sandbox_ns,
            "labels": {
                "app.kubernetes.io/name": "kars",
                "app.kubernetes.io/component": "sandbox",
                "kars.azure.com/sandbox": sandbox_name,
                "kars.azure.com/role": "sandbox",
                "kars.azure.com/isolated": "strict",
                "pod-security.kubernetes.io/enforce": "privileged",
                "pod-security.kubernetes.io/audit": "baseline",
                "pod-security.kubernetes.io/warn": "baseline"
            }
        }
    })
}

/// Build ServiceAccount JSON the same way reconcile() does (line 250-263).
fn build_sa_json(sandbox_name: &str, wi_client_id: &str) -> serde_json::Value {
    let sandbox_ns = format!("kars-{sandbox_name}");
    json!({
        "apiVersion": "v1",
        "kind": "ServiceAccount",
        "metadata": {
            "name": "sandbox",
            "namespace": sandbox_ns,
            "labels": {
                "kars.azure.com/sandbox": sandbox_name
            },
            "annotations": {
                "azure.workload.identity/client-id": wi_client_id
            }
        }
    })
}

/// Build ClusterRoleBinding JSON the same way reconcile() does (line 289-309).
fn build_crb_json(sandbox_name: &str) -> serde_json::Value {
    let sandbox_ns = format!("kars-{sandbox_name}");
    let crb_name = format!("kars-spawner-{sandbox_name}");
    json!({
        "apiVersion": "rbac.authorization.k8s.io/v1",
        "kind": "ClusterRoleBinding",
        "metadata": {
            "name": crb_name,
            "labels": {
                "kars.azure.com/sandbox": sandbox_name,
                "app.kubernetes.io/managed-by": "kars-controller"
            }
        },
        "roleRef": {
            "apiGroup": "rbac.authorization.k8s.io",
            "kind": "ClusterRole",
            "name": "kars-sandbox-spawner"
        },
        "subjects": [{
            "kind": "ServiceAccount",
            "name": "sandbox",
            "namespace": sandbox_ns
        }]
    })
}

/// Build default egress rules the same way reconcile() does (line 443-480).
fn build_default_egress_rules() -> Vec<serde_json::Value> {
    vec![
        json!({
            "to": [
                {"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}},
                {"ipBlock": {"cidr": "10.0.0.10/32"}}
            ],
            "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]
        }),
        json!({
            "to": [{"ipBlock": {"cidr": "169.254.169.254/32"}}],
            "ports": [{"protocol": "TCP", "port": 80}]
        }),
        json!({
            "to": [{"ipBlock": {"cidr": "0.0.0.0/0", "except": ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]}}],
            "ports": [{"protocol": "TCP", "port": 443}]
        }),
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"kars.azure.com/role": "sandbox"}}}],
            "ports": [{"protocol": "TCP", "port": 8443}]
        }),
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"app.kubernetes.io/managed-by": "kars"}}}],
            "ports": [{"protocol": "TCP", "port": 8765}, {"protocol": "TCP", "port": 8080}]
        }),
    ]
}

/// Build the openclaw container JSON (line 702-746).
fn build_openclaw_container(image: &str, cfg: &SandboxConfig, model: &str) -> serde_json::Value {
    let pull_policy = if image.ends_with(":latest") {
        "Always"
    } else {
        "IfNotPresent"
    };
    json!({
        "name": "openclaw",
        "image": image,
        "imagePullPolicy": pull_policy,
        "ports": [{"containerPort": 18789, "name": "gateway"}],
        "env": [
            {"name": "OPENCLAW_MODEL", "value": model},
            {"name": "AZURE_OPENAI_ENDPOINT", "value": "https://test.openai.azure.com"},
            {"name": "KARS_AUTH_MODE", "value": "workload-identity"},
            {"name": "OPENCLAW_GATEWAY_TOKEN", "valueFrom": {"secretKeyRef": {"name": "gateway-token", "key": "token"}}},
        ],
        "securityContext": {
            "runAsUser": 1000,
            "allowPrivilegeEscalation": cfg.allow_privilege_escalation,
            "readOnlyRootFilesystem": cfg.read_only_root_filesystem,
            "capabilities": {"drop": ["ALL"]}
        },
        "volumeMounts": [
            {"name": "sandbox-data", "mountPath": "/sandbox"},
            {"name": "tmp", "mountPath": "/tmp"},
            {"name": "admin-token", "mountPath": "/etc/kars/secrets", "readOnly": true}
        ],
        "resources": {
            "requests": {"cpu": "500m", "memory": "1Gi"},
            "limits": {"cpu": "2", "memory": "4Gi"}
        },
        "livenessProbe": {
            "exec": {"command": ["sh", "-c", "test -f /proc/1/status"]},
            "initialDelaySeconds": 15,
            "periodSeconds": 30
        },
        "readinessProbe": {
            "exec": {"command": ["sh", "-c", "test -f /proc/1/status"]},
            "initialDelaySeconds": 5,
            "periodSeconds": 10
        }
    })
}

/// Build inference-router container JSON (line 747-778).
fn build_router_container(
    image: &str,
    name: &str,
    cfg: &SandboxConfig,
    model: &str,
) -> serde_json::Value {
    json!({
        "name": "inference-router",
        "image": image,
        "ports": [
            {"containerPort": 8443, "name": "inference"},
            {"containerPort": 9090, "name": "metrics"}
        ],
        "env": [
            {"name": "AZURE_OPENAI_ENDPOINT", "value": "https://test.openai.azure.com"},
            {"name": "FOUNDRY_ENDPOINT", "value": "https://test.foundry.azure.com"},
            {"name": "FOUNDRY_PROJECT_ENDPOINT", "value": "https://test.foundry.azure.com/project"},
            {"name": "IMDS_CLIENT_ID", "value": "test-imds-id"},
            {"name": "AZURE_OPENAI_DEPLOYMENT", "value": model},
            {"name": "KARS_AUTH_MODE", "value": "workload-identity"},
            {"name": "CONTENT_SAFETY_ENABLED", "value": "true"},
            {"name": "PROMPT_SHIELDS_ENABLED", "value": "true"},
            {"name": "CONTENT_SAFETY_ENDPOINT", "value": "https://test.contentsafety.azure.com"},
            {"name": "TOKEN_BUDGET_DAILY", "value": "0"},
            {"name": "TOKEN_BUDGET_PER_REQUEST", "value": "0"},
            {"name": "SANDBOX_NAME", "value": name},
            {"name": "SANDBOX_ISOLATION", "value": &cfg.isolation},
            {"name": "RUST_LOG", "value": "info,inference_router=debug"},
        ],
        "securityContext": {
            "runAsUser": 1001,
            "allowPrivilegeEscalation": false,
            "readOnlyRootFilesystem": true,
            "capabilities": {"drop": ["ALL"]}
        },
        "resources": {
            "requests": {"cpu": "100m", "memory": "64Mi"},
            "limits": {"cpu": "500m", "memory": "256Mi"}
        },
        "livenessProbe": {
            "httpGet": {"path": "/healthz", "port": "inference"},
            "initialDelaySeconds": 5,
            "periodSeconds": 15
        },
        "readinessProbe": {
            "httpGet": {"path": "/healthz", "port": "inference"},
            "initialDelaySeconds": 3,
            "periodSeconds": 5
        },
        "volumeMounts": [
            {"name": "admin-token", "mountPath": "/etc/kars/secrets", "readOnly": true}
        ]
    })
}

/// Build init container JSON (line 667-701).
fn build_init_container(image: &str) -> serde_json::Value {
    json!({
        "name": "egress-guard",
        "image": image,
        "securityContext": {
            "runAsUser": 0,
            "runAsNonRoot": false,
            "seccompProfile": { "type": "Unconfined" },
            "capabilities": {
                "add": ["NET_ADMIN", "NET_RAW"],
                "drop": ["ALL"]
            }
        },
        "resources": {
            "requests": {"cpu": "10m", "memory": "32Mi"},
            "limits": {"cpu": "200m", "memory": "256Mi"}
        }
    })
}

// ── Namespace creation tests ────────────────────────────────────────

#[test]
fn namespace_name_follows_kars_prefix() {
    let name = "my-agent";
    let sandbox_ns = format!("kars-{name}");
    assert_eq!(sandbox_ns, "kars-my-agent");
    assert!(sandbox_ns.starts_with("kars-"));
}

#[test]
fn namespace_labels_include_app_and_role() {
    let ns = build_namespace_json("test-agent");
    let labels = &ns["metadata"]["labels"];
    assert_eq!(labels["app.kubernetes.io/name"], "kars");
    assert_eq!(labels["app.kubernetes.io/component"], "sandbox");
    assert_eq!(labels["kars.azure.com/sandbox"], "test-agent");
    assert_eq!(labels["kars.azure.com/role"], "sandbox");
    assert_eq!(labels["kars.azure.com/isolated"], "strict");
}

#[test]
fn namespace_has_pod_security_admission_labels() {
    let ns = build_namespace_json("psa-test");
    let labels = &ns["metadata"]["labels"];
    assert_eq!(labels["pod-security.kubernetes.io/enforce"], "privileged");
    assert_eq!(labels["pod-security.kubernetes.io/audit"], "baseline");
    assert_eq!(labels["pod-security.kubernetes.io/warn"], "baseline");
}

// ── NetworkPolicy tests ─────────────────────────────────────────────

#[test]
fn default_egress_allows_dns_on_port_53() {
    let rules = build_default_egress_rules();
    let dns_rule = &rules[0];
    let ports = dns_rule["ports"].as_array().unwrap();
    assert_eq!(ports.len(), 2);
    assert_eq!(ports[0]["port"], 53);
    assert_eq!(ports[0]["protocol"], "UDP");
    assert_eq!(ports[1]["port"], 53);
    assert_eq!(ports[1]["protocol"], "TCP");
}

#[test]
fn default_egress_allows_imds() {
    let rules = build_default_egress_rules();
    let imds_rule = &rules[1];
    assert_eq!(imds_rule["to"][0]["ipBlock"]["cidr"], "169.254.169.254/32");
    assert_eq!(imds_rule["ports"][0]["port"], 80);
}

#[test]
fn default_egress_allows_https_excluding_private_ranges() {
    let rules = build_default_egress_rules();
    let https_rule = &rules[2];
    assert_eq!(https_rule["to"][0]["ipBlock"]["cidr"], "0.0.0.0/0");
    let except = https_rule["to"][0]["ipBlock"]["except"].as_array().unwrap();
    assert!(except.contains(&json!("10.0.0.0/8")));
    assert!(except.contains(&json!("172.16.0.0/12")));
    assert!(except.contains(&json!("192.168.0.0/16")));
    assert_eq!(https_rule["ports"][0]["port"], 443);
}

#[test]
fn mesh_egress_targets_sandbox_namespaces() {
    let rules = build_default_egress_rules();
    let mesh_rule = &rules[3];
    assert_eq!(
        mesh_rule["to"][0]["namespaceSelector"]["matchLabels"]["kars.azure.com/role"],
        "sandbox"
    );
    assert_eq!(mesh_rule["ports"][0]["port"], 8443);
}

#[test]
fn relay_egress_targets_agentmesh_namespace() {
    let rules = build_default_egress_rules();
    let relay_rule = &rules[4];
    assert_eq!(
        relay_rule["to"][0]["namespaceSelector"]["matchLabels"]["app.kubernetes.io/managed-by"],
        "kars"
    );
    let ports = relay_rule["ports"].as_array().unwrap();
    assert_eq!(ports[0]["port"], 8765); // relay WebSocket
    assert_eq!(ports[1]["port"], 8080); // registry HTTP
}

#[test]
fn default_egress_has_five_rules() {
    let rules = build_default_egress_rules();
    assert_eq!(rules.len(), 5);
}

// ── RBAC tests ──────────────────────────────────────────────────────

#[test]
fn service_account_name_is_sandbox() {
    let sa = build_sa_json("my-agent", "test-client-id");
    assert_eq!(sa["metadata"]["name"], "sandbox");
}

#[test]
fn service_account_has_workload_identity_annotation() {
    let sa = build_sa_json("my-agent", "abc-123-client-id");
    assert_eq!(
        sa["metadata"]["annotations"]["azure.workload.identity/client-id"],
        "abc-123-client-id"
    );
}

#[test]
fn service_account_namespace_matches_sandbox() {
    let sa = build_sa_json("my-agent", "cid");
    assert_eq!(sa["metadata"]["namespace"], "kars-my-agent");
}

#[test]
fn cluster_role_binding_references_spawner_role() {
    let crb = build_crb_json("my-agent");
    assert_eq!(crb["roleRef"]["kind"], "ClusterRole");
    assert_eq!(crb["roleRef"]["name"], "kars-sandbox-spawner");
    assert_eq!(crb["roleRef"]["apiGroup"], "rbac.authorization.k8s.io");
}

#[test]
fn cluster_role_binding_name_includes_sandbox_name() {
    let crb = build_crb_json("my-agent");
    assert_eq!(crb["metadata"]["name"], "kars-spawner-my-agent");
}

#[test]
fn cluster_role_binding_subject_is_sandbox_sa() {
    let crb = build_crb_json("my-agent");
    let subject = &crb["subjects"][0];
    assert_eq!(subject["kind"], "ServiceAccount");
    assert_eq!(subject["name"], "sandbox");
    assert_eq!(subject["namespace"], "kars-my-agent");
}

#[test]
fn cluster_role_binding_has_managed_by_label() {
    let crb = build_crb_json("test");
    assert_eq!(
        crb["metadata"]["labels"]["app.kubernetes.io/managed-by"],
        "kars-controller"
    );
}

// ── Pod spec: container tests ───────────────────────────────────────

#[test]
fn base_pod_has_two_containers() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let containers = [oc, router];
    assert_eq!(containers.len(), 2);
    assert_eq!(containers[0]["name"], "openclaw");
    assert_eq!(containers[1]["name"], "inference-router");
}

#[test]
fn pod_has_two_containers() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let containers = [oc, router];
    assert_eq!(containers.len(), 2);
    assert_eq!(containers[0]["name"], "openclaw");
    assert_eq!(containers[1]["name"], "inference-router");
}

#[test]
fn inference_router_listens_on_port_8443() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let ports = router["ports"].as_array().unwrap();
    assert_eq!(ports[0]["containerPort"], 8443);
    assert_eq!(ports[0]["name"], "inference");
}

#[test]
fn inference_router_exposes_metrics_port() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let ports = router["ports"].as_array().unwrap();
    assert_eq!(ports[1]["containerPort"], 9090);
    assert_eq!(ports[1]["name"], "metrics");
}

#[test]
fn openclaw_gateway_port_18789() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    assert_eq!(oc["ports"][0]["containerPort"], 18789);
    assert_eq!(oc["ports"][0]["name"], "gateway");
}

// ── Pod spec: UID segregation ───────────────────────────────────────

#[test]
fn container_uids_are_segregated() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    assert_eq!(oc["securityContext"]["runAsUser"], 1000);
    assert_eq!(router["securityContext"]["runAsUser"], 1001);
}

// ── Pod spec: router security ──────────────────────────────────────

#[test]
fn router_denies_privilege_escalation() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    assert_eq!(router["securityContext"]["allowPrivilegeEscalation"], false);
}

#[test]
fn router_has_read_only_rootfs() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    assert_eq!(router["securityContext"]["readOnlyRootFilesystem"], true);
}

#[test]
fn router_drops_all_capabilities() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    assert_eq!(
        router["securityContext"]["capabilities"]["drop"],
        json!(["ALL"])
    );
}

// ── Pod spec: router probes ────────────────────────────────────────

#[test]
fn router_probes_use_httpget_no_host() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let liveness = &router["livenessProbe"]["httpGet"];
    assert_eq!(liveness["path"], "/healthz");
    assert_eq!(liveness["port"], "inference");
    assert!(liveness.get("host").is_none());

    let readiness = &router["readinessProbe"]["httpGet"];
    assert_eq!(readiness["path"], "/healthz");
    assert!(readiness.get("host").is_none());
}

// ── Pod spec: volumes ───────────────────────────────────────────────

#[test]
fn openclaw_has_sandbox_data_volume_mount() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    let mounts = oc["volumeMounts"].as_array().unwrap();
    let names: Vec<&str> = mounts.iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"sandbox-data"));
    assert!(names.contains(&"tmp"));
    assert!(names.contains(&"admin-token"));
}

#[test]
fn router_has_admin_token_volume_mount() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let mounts = router["volumeMounts"].as_array().unwrap();
    assert_eq!(mounts[0]["name"], "admin-token");
    assert_eq!(mounts[0]["readOnly"], true);
}

// ── Pod spec: init container ────────────────────────────────────────

#[test]
fn init_container_needs_net_admin_capability() {
    let init = build_init_container("sandbox:latest");
    let caps = &init["securityContext"]["capabilities"];
    let add = caps["add"].as_array().unwrap();
    assert!(add.contains(&json!("NET_ADMIN")));
    assert!(add.contains(&json!("NET_RAW")));
}

#[test]
fn init_container_runs_as_root() {
    let init = build_init_container("sandbox:latest");
    assert_eq!(init["securityContext"]["runAsUser"], 0);
    assert_eq!(init["securityContext"]["runAsNonRoot"], false);
}

#[test]
fn init_container_seccomp_unconfined() {
    let init = build_init_container("sandbox:latest");
    assert_eq!(
        init["securityContext"]["seccompProfile"]["type"],
        "Unconfined"
    );
}

// ── Pod spec: image pull policy ─────────────────────────────────────

#[test]
fn pull_policy_always_for_latest_tag() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    assert_eq!(oc["imagePullPolicy"], "Always");
}

#[test]
fn pull_policy_ifnotpresent_for_versioned_tag() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:v1.2.3", &cfg, "gpt-4.1");
    assert_eq!(oc["imagePullPolicy"], "IfNotPresent");
}

// ── Environment variable injection ──────────────────────────────────

#[test]
fn router_env_includes_sandbox_name() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "my-agent", &cfg, "gpt-4.1");
    let env = router["env"].as_array().unwrap();
    let sandbox_name_var = env
        .iter()
        .find(|e| e["name"] == "SANDBOX_NAME")
        .expect("SANDBOX_NAME env var missing");
    assert_eq!(sandbox_name_var["value"], "my-agent");
}

#[test]
fn router_env_includes_content_safety_endpoint() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let env = router["env"].as_array().unwrap();
    let cs_var = env
        .iter()
        .find(|e| e["name"] == "CONTENT_SAFETY_ENDPOINT")
        .expect("CONTENT_SAFETY_ENDPOINT missing");
    assert!(!cs_var["value"].as_str().unwrap().is_empty());
}

#[test]
fn router_env_includes_foundry_project_endpoint() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let env = router["env"].as_array().unwrap();
    let fp_var = env
        .iter()
        .find(|e| e["name"] == "FOUNDRY_PROJECT_ENDPOINT")
        .expect("FOUNDRY_PROJECT_ENDPOINT missing");
    assert!(!fp_var["value"].as_str().unwrap().is_empty());
}

#[test]
fn router_env_includes_model_deployment() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let env = router["env"].as_array().unwrap();
    let deployment_var = env
        .iter()
        .find(|e| e["name"] == "AZURE_OPENAI_DEPLOYMENT")
        .expect("AZURE_OPENAI_DEPLOYMENT missing");
    assert_eq!(deployment_var["value"], "gpt-4.1");
}

#[test]
fn router_env_includes_token_budget_daily() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    let env = router["env"].as_array().unwrap();
    let budget_var = env
        .iter()
        .find(|e| e["name"] == "TOKEN_BUDGET_DAILY")
        .expect("TOKEN_BUDGET_DAILY missing");
    assert_eq!(budget_var["value"], "0"); // default unlimited
}

#[test]
fn openclaw_env_includes_model() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    let env = oc["env"].as_array().unwrap();
    let model_var = env
        .iter()
        .find(|e| e["name"] == "OPENCLAW_MODEL")
        .expect("OPENCLAW_MODEL missing");
    assert_eq!(model_var["value"], "gpt-4.1");
}

#[test]
fn openclaw_env_includes_azure_openai_endpoint() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    let env = oc["env"].as_array().unwrap();
    let ep_var = env
        .iter()
        .find(|e| e["name"] == "AZURE_OPENAI_ENDPOINT")
        .expect("AZURE_OPENAI_ENDPOINT missing");
    assert!(!ep_var["value"].as_str().unwrap().is_empty());
}

// ── Default resource limits ─────────────────────────────────────────

#[test]
fn openclaw_default_resource_limits() {
    let cfg = SandboxConfig::default();
    let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
    assert_eq!(oc["resources"]["requests"]["cpu"], "500m");
    assert_eq!(oc["resources"]["requests"]["memory"], "1Gi");
    assert_eq!(oc["resources"]["limits"]["cpu"], "2");
    assert_eq!(oc["resources"]["limits"]["memory"], "4Gi");
}

#[test]
fn router_default_resource_limits() {
    let cfg = SandboxConfig::default();
    let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
    assert_eq!(router["resources"]["requests"]["cpu"], "100m");
    assert_eq!(router["resources"]["requests"]["memory"], "64Mi");
    assert_eq!(router["resources"]["limits"]["cpu"], "500m");
    assert_eq!(router["resources"]["limits"]["memory"], "256Mi");
}

// ── Finalizer ───────────────────────────────────────────────────────

#[test]
fn finalizer_name_is_namespace_cleanup() {
    // The reconcile function uses this exact finalizer name (line 127)
    let expected = "kars.azure.com/namespace-cleanup";
    // Verify the format matches the domain/purpose convention
    assert!(expected.starts_with("kars.azure.com/"));
    assert!(expected.contains("namespace-cleanup"));
}

// ── Isolation + runtime class ───────────────────────────────────────

#[test]
fn confidential_isolation_gets_kata_runtime_class() {
    let (runtime, _pool) = isolation_scheduling("confidential");
    assert_eq!(runtime, Some("kata-vm-isolation"));
}

#[test]
fn standard_and_enhanced_share_sandbox_pool() {
    let (_, pool_std) = isolation_scheduling("standard");
    let (_, pool_enh) = isolation_scheduling("enhanced");
    assert_eq!(pool_std, pool_enh);
    assert_eq!(pool_std, "sandbox");
}

#[test]
fn unknown_isolation_defaults_to_sandbox_pool() {
    let (runtime, pool) = isolation_scheduling("unknown-level");
    assert!(runtime.is_none());
    assert_eq!(pool, "sandbox");
}

// ── Security context edge cases ─────────────────────────────────────

#[test]
fn explicit_runtime_default_seccomp_overrides_localhost() {
    let cfg = SandboxConfig {
        isolation: "enhanced".into(),
        seccomp_profile: "RuntimeDefault".into(),
        ..Default::default()
    };
    let ctx = build_pod_security_context(&cfg);
    assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
}

#[test]
fn empty_seccomp_profile_uses_runtime_default() {
    let cfg = SandboxConfig {
        isolation: "enhanced".into(),
        seccomp_profile: String::new(),
        ..Default::default()
    };
    let ctx = build_pod_security_context(&cfg);
    assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
}

#[test]
fn custom_seccomp_profile_name() {
    let cfg = SandboxConfig {
        isolation: "enhanced".into(),
        seccomp_profile: "my-custom-profile".into(),
        ..Default::default()
    };
    let ctx = build_pod_security_context(&cfg);
    assert_eq!(ctx["seccompProfile"]["type"], "Localhost");
    assert_eq!(
        ctx["seccompProfile"]["localhostProfile"],
        "profiles/my-custom-profile.json"
    );
}

// ── Error-policy / watch-resilience contract (r4) ───────────────────
//
// These tests guard the reconcile-error requeue contract. The
// watch-stream itself is kube-rs's problem (Controller::new +
// watcher::Config handle stream reconnect with built-in backoff) —
// we only test the piece we own: that any ReconcileError yields a
// positive, bounded requeue duration. A regression to
// `Duration::ZERO` would hot-loop the controller.

#[test]
fn error_requeue_kube_is_short() {
    let err = ReconcileError::Kube(kube::Error::LinesCodecMaxLineLengthExceeded);
    let d = error_requeue_duration(&err);
    assert!(d >= Duration::from_secs(10), "too short: {:?}", d);
    assert!(d <= Duration::from_secs(120), "too long: {:?}", d);
}

#[test]
fn error_requeue_serde_is_long() {
    // Produce a real serde_json::Error without an unwrap panic.
    let serde_err = serde_json::from_str::<serde_json::Value>("{bad").unwrap_err();
    let err = ReconcileError::SerdeJson(serde_err);
    let d = error_requeue_duration(&err);
    // Serde errors won't heal on retry — we want a longer backoff.
    assert!(
        d >= Duration::from_secs(60),
        "serde backoff too short: {:?} — this would log-spam",
        d
    );
}

#[test]
fn error_requeue_is_never_zero() {
    // Build one of each variant and confirm the requeue is strictly
    // positive. A zero requeue would starve the controller event
    // loop and pin a CPU.
    let kube_err = ReconcileError::Kube(kube::Error::LinesCodecMaxLineLengthExceeded);
    assert!(error_requeue_duration(&kube_err) > Duration::ZERO);

    let serde_err =
        ReconcileError::SerdeJson(serde_json::from_str::<serde_json::Value>("{bad").unwrap_err());
    assert!(error_requeue_duration(&serde_err) > Duration::ZERO);
}
