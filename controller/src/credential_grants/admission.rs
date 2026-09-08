// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::api_error;
use k8s_openapi::api::admissionregistration::v1::{
    ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding,
};
use kube::{Api, Client};

pub(super) async fn verify(client: &Client) -> Result<(), String> {
    for name in [
        "kars-credential-grant-authority",
        "kars-credential-source-boundary",
        "kars-credential-namespace-boundary",
        "kars-credential-source-writes",
        "kars-credential-enrolled-store-shape",
        "kars-credential-consumer-karssandboxes",
        "kars-credential-consumer-karstasks",
        "kars-credential-consumer-karsteams",
    ] {
        let policy = Api::<ValidatingAdmissionPolicy>::all(client.clone())
            .get(name)
            .await
            .map_err(|e| api_error("Read credential admission policy", e))?;
        let binding = Api::<ValidatingAdmissionPolicyBinding>::all(client.clone())
            .get(name)
            .await
            .map_err(|e| api_error("Read credential admission binding", e))?;
        if policy.metadata.deletion_timestamp.is_some()
            || policy
                .spec
                .as_ref()
                .and_then(|s| s.failure_policy.as_deref())
                != Some("Fail")
            || policy.status.as_ref().is_none_or(|status| {
                status.observed_generation != policy.metadata.generation
                    || status.type_checking.as_ref().is_none_or(|check| {
                        check
                            .expression_warnings
                            .as_ref()
                            .is_some_and(|w| !w.is_empty())
                    })
            })
            || binding.metadata.deletion_timestamp.is_some()
            || binding.spec.as_ref().is_none_or(|s| {
                s.policy_name.as_deref() != Some(name)
                    || s.validation_actions
                        .as_ref()
                        .is_none_or(|actions| !actions.iter().any(|a| a == "Deny"))
            })
        {
            return Err("Credential admission is not observed, type-checked and enforced".into());
        }
    }
    Ok(())
}
