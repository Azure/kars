// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::sync::Arc;

use k8s_openapi::api::core::v1::ConfigMap;
use kube::{ResourceExt, runtime::reflector::ObjectRef};

use crate::crd::KarsSandbox;

pub(super) fn referencing_sandboxes(
    profile: &ConfigMap,
    sandboxes: &[Arc<KarsSandbox>],
) -> Vec<ObjectRef<KarsSandbox>> {
    if profile
        .labels()
        .get("kars.azure.com/artifact")
        .map(String::as_str)
        != Some("inference-policy-profile")
        || profile
            .labels()
            .contains_key(super::governance_mounts::MIRROR_OWNER_LABEL)
    {
        return Vec::new();
    }
    let (Some(namespace), Some(name)) = (&profile.metadata.namespace, &profile.metadata.name)
    else {
        return Vec::new();
    };
    sandboxes
        .iter()
        .filter(|sandbox| {
            sandbox.metadata.namespace.as_ref() == Some(namespace)
                && !sandbox.spec.inference_ref.name.is_empty()
                && format!(
                    "inferencepolicy-{}-profile",
                    sandbox.spec.inference_ref.name
                ) == *name
        })
        .map(|sandbox| ObjectRef::from_obj(sandbox.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::KarsSandboxSpec;

    fn sandbox(name: &str, namespace: &str, policy: &str) -> Arc<KarsSandbox> {
        let mut spec = KarsSandboxSpec::default();
        spec.inference_ref.name = policy.into();
        let mut sandbox = KarsSandbox::new(name, spec);
        sandbox.metadata.namespace = Some(namespace.into());
        Arc::new(sandbox)
    }

    fn profile(namespace: &str, policy: &str) -> ConfigMap {
        ConfigMap {
            metadata: kube::api::ObjectMeta {
                name: Some(format!("inferencepolicy-{policy}-profile")),
                namespace: Some(namespace.into()),
                labels: Some(std::collections::BTreeMap::from([(
                    "kars.azure.com/artifact".into(),
                    "inference-policy-profile".into(),
                )])),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn compiled_profile_requeues_every_referrer_in_its_namespace() {
        let sandboxes = vec![
            sandbox("bridge-orchestrator", "kars-system", "shared"),
            sandbox("memory-consumer", "kars-system", "shared"),
            sandbox("other-policy", "kars-system", "other"),
            sandbox("other-namespace", "different", "shared"),
        ];
        let refs = referencing_sandboxes(&profile("kars-system", "shared"), &sandboxes);
        assert_eq!(
            refs,
            sandboxes[..2]
                .iter()
                .map(|s| ObjectRef::from_obj(s.as_ref()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn matching_uses_full_resource_name_not_truncated_policy_label() {
        let policy = "shared-policy-with-a-name-longer-than-sixty-three-characters-for-testing";
        let mut cm = profile("kars-system", policy);
        cm.metadata.labels.as_mut().unwrap().insert(
            "kars.azure.com/inferencepolicy".into(),
            "truncated-hash".into(),
        );
        let sandboxes = vec![sandbox("consumer", "kars-system", policy)];
        assert_eq!(referencing_sandboxes(&cm, &sandboxes).len(), 1);
        cm.metadata.namespace = None;
        assert!(referencing_sandboxes(&cm, &sandboxes).is_empty());
        cm.metadata.namespace = Some("kars-system".into());
        cm.metadata.name = None;
        assert!(referencing_sandboxes(&cm, &sandboxes).is_empty());
    }

    #[test]
    fn mirrors_and_unrelated_configmaps_do_not_trigger_reconciliation() {
        let sandboxes = vec![sandbox("consumer", "kars-system", "shared")];
        let mut cm = profile("kars-system", "shared");
        cm.metadata.labels.as_mut().unwrap().insert(
            super::super::governance_mounts::MIRROR_OWNER_LABEL.into(),
            "inferencepolicy.shared".into(),
        );
        assert!(referencing_sandboxes(&cm, &sandboxes).is_empty());
        cm.metadata.labels = None;
        assert!(referencing_sandboxes(&cm, &sandboxes).is_empty());
        assert!(referencing_sandboxes(&ConfigMap::default(), &sandboxes).is_empty());
    }
}
