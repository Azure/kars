// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use k8s_openapi::api::apps::v1::Deployment;
use kube::runtime::reflector::ObjectRef;

use crate::crd::KarsSandbox;

/// Phase G P1 #5 — Deployment-to-KarsSandbox parent mapper.
///
/// Triggers a reconcile on the parent KarsSandbox whenever a child
/// Deployment is created, updated, or deleted. Combined with
/// `Controller::watches`, this closes the gap that previously let
/// manual `kubectl edit deploy` / `kubectl scale` mutations linger
/// for up to 5 minutes (the periodic requeue interval) before the
/// reconciler restored the desired state.
///
/// Cross-namespace constraint: K8s does not allow ownerReferences
/// across namespaces, and the Deployment lives in `kars-{name}`
/// while the parent KarsSandbox can live in any namespace. We
/// therefore identify the parent by the labels we stamp at
/// Deployment-creation time:
///
/// * `kars.azure.com/component=sandbox` — required (filters
///   out unrelated Deployments).
/// * `kars.azure.com/sandbox=<name>` — parent CR name.
/// * `kars.azure.com/parent-namespace=<ns>` — parent CR
///   namespace.
///
/// Deployments that pre-date this PR carry the first two labels but
/// not the third. They are silently skipped by the mapper; the next
/// periodic 5-minute requeue re-applies the Deployment with the new
/// label, after which subsequent edits trigger this watch.
pub(super) fn deployment_to_sandbox_ref(d: Deployment) -> Option<ObjectRef<KarsSandbox>> {
    let labels = d.metadata.labels.as_ref()?;
    if labels.get("kars.azure.com/component").map(|s| s.as_str()) != Some("sandbox") {
        return None;
    }
    let sandbox_name = labels.get("kars.azure.com/sandbox")?;
    let parent_ns = labels.get("kars.azure.com/parent-namespace")?;
    if sandbox_name.is_empty() || parent_ns.is_empty() {
        return None;
    }
    Some(ObjectRef::<KarsSandbox>::new(sandbox_name).within(parent_ns))
}

// Annotation-only run requests must refresh the installed admission without waiting for a spec change.
pub(super) fn task_to_sandbox_ref(
    task: crate::kars_task::KarsTask,
) -> Option<ObjectRef<KarsSandbox>> {
    let name = task.metadata.name.as_deref().filter(|v| !v.is_empty())?;
    let namespace = task
        .metadata
        .namespace
        .as_deref()
        .filter(|v| !v.is_empty())?;
    Some(ObjectRef::<KarsSandbox>::new(name).within(namespace))
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn task_mapper_requires_nonempty_name_and_namespace() {
        let mut task = crate::kars_task::KarsTask::new("writer", Default::default());
        task.metadata.namespace = Some("team-work".into());
        let reference = task_to_sandbox_ref(task.clone()).unwrap();
        assert_eq!(reference.name, "writer");
        assert_eq!(reference.namespace.as_deref(), Some("team-work"));
        for value in [None, Some(String::new())] {
            let mut missing_name = task.clone();
            missing_name.metadata.name = value.clone();
            assert!(task_to_sandbox_ref(missing_name).is_none());
            let mut missing_namespace = task.clone();
            missing_namespace.metadata.namespace = value;
            assert!(task_to_sandbox_ref(missing_namespace).is_none());
        }
    }

    fn deploy_with_labels(labels: BTreeMap<String, String>) -> Deployment {
        Deployment {
            metadata: kube::api::ObjectMeta {
                labels: Some(labels),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn mapper_returns_ref_for_well_labeled_deployment() {
        let mut labels = BTreeMap::new();
        labels.insert(
            "kars.azure.com/component".to_string(),
            "sandbox".to_string(),
        );
        labels.insert("kars.azure.com/sandbox".to_string(), "my-agent".to_string());
        labels.insert(
            "kars.azure.com/parent-namespace".to_string(),
            "kars-system".to_string(),
        );
        let r = deployment_to_sandbox_ref(deploy_with_labels(labels)).expect("ref");
        assert_eq!(r.name, "my-agent");
        assert_eq!(r.namespace.as_deref(), Some("kars-system"));
    }

    #[test]
    fn mapper_skips_unlabeled_deployment() {
        assert!(deployment_to_sandbox_ref(deploy_with_labels(BTreeMap::new())).is_none());
    }

    #[test]
    fn mapper_skips_wrong_component() {
        let mut labels = BTreeMap::new();
        labels.insert("kars.azure.com/component".to_string(), "router".to_string());
        labels.insert("kars.azure.com/sandbox".to_string(), "my-agent".to_string());
        labels.insert(
            "kars.azure.com/parent-namespace".to_string(),
            "kars-system".to_string(),
        );
        assert!(deployment_to_sandbox_ref(deploy_with_labels(labels)).is_none());
    }

    #[test]
    fn mapper_skips_pre_pr_deployment_without_parent_namespace() {
        // Deployments created before this PR have component+sandbox
        // labels but lack parent-namespace. They MUST be skipped (the
        // periodic requeue re-applies them with the new label, then
        // subsequent edits start triggering this watch).
        let mut labels = BTreeMap::new();
        labels.insert(
            "kars.azure.com/component".to_string(),
            "sandbox".to_string(),
        );
        labels.insert("kars.azure.com/sandbox".to_string(), "my-agent".to_string());
        assert!(deployment_to_sandbox_ref(deploy_with_labels(labels)).is_none());
    }

    #[test]
    fn mapper_skips_empty_label_values() {
        let mut labels = BTreeMap::new();
        labels.insert(
            "kars.azure.com/component".to_string(),
            "sandbox".to_string(),
        );
        labels.insert("kars.azure.com/sandbox".to_string(), "".to_string());
        labels.insert(
            "kars.azure.com/parent-namespace".to_string(),
            "kars-system".to_string(),
        );
        assert!(deployment_to_sandbox_ref(deploy_with_labels(labels)).is_none());
    }
}
