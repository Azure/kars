// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::api_error;
use k8s_openapi::api::admissionregistration::v1::{
    ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding,
};
use kube::{Api, Client};

pub(super) const POLICIES: &[&str] = &[
    "kars-sre-source-authority",
    "kars-sre-registration-authority",
    "kars-sre-private-identity",
    "kars-sre-binding-authority",
    "kars-sre-private-material",
    "kars-sre-no-legacy-tokens",
    "kars-sre-source-retirement",
    "kars-sre-pending-proposals",
    "kars-sre-consumer-authority",
    "kars-sre-private-mounts",
    "kars-sre-private-workloads",
    "kars-sre-private-cronjobs",
    "kars-sre-private-connect",
    "kars-sre-role-authority",
];

pub(super) async fn verify(client: &Client) -> Result<(), String> {
    let policies: Api<ValidatingAdmissionPolicy> = Api::all(client.clone());
    let bindings: Api<ValidatingAdmissionPolicyBinding> = Api::all(client.clone());
    for name in POLICIES {
        let policy = policies
            .get(name)
            .await
            .map_err(|e| api_error("Verify SRE admission policy", e))?;
        let binding = bindings
            .get(name)
            .await
            .map_err(|e| api_error("Verify SRE admission binding", e))?;
        let checked = policy.status.as_ref().is_some_and(|status| {
            status.observed_generation == policy.metadata.generation
                && status.type_checking.as_ref().is_some_and(|checking| {
                    checking
                        .expression_warnings
                        .as_ref()
                        .is_none_or(Vec::is_empty)
                })
        });
        if policy.metadata.deletion_timestamp.is_some()
            || binding.metadata.deletion_timestamp.is_some()
            || policy
                .spec
                .as_ref()
                .and_then(|spec| spec.failure_policy.as_deref())
                != Some("Fail")
            || !checked
            || binding.spec.as_ref().is_none_or(|spec| {
                spec.policy_name.as_deref() != Some(*name)
                    || spec
                        .validation_actions
                        .as_ref()
                        .is_none_or(|actions| !actions.iter().any(|action| action == "Deny"))
            })
        {
            return Err(format!(
                "SRE admission policy {name} is not observed, type-checked, and enforced; no privilege may be issued"
            ));
        }
    }
    Ok(())
}
