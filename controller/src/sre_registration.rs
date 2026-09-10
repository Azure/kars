// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Cluster-scoped operator authorization for the canonical SRE identity.
//! Namespace ownership identifies an occupant; only this resource delegates
//! privileged SRE authority to that exact occupant.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const NAME: &str = "canonical";
pub const ROUTER_SA: &str = "sre-api-router";
pub const RUNTIME_NAMESPACE: &str = "kars-sre";
pub const PRIVATE_SECRET: &str = "sre-api-router-identity";
pub const AGENT_SECRET: &str = "sre-api-agent";
pub const OWNER: &str = "kars.azure.com/sre-registration-uid";
pub const EPOCH: &str = "kars.azure.com/sre-privacy-epoch";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NamedUid {
    pub name: String,
    pub uid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControllerIdentity {
    pub namespace: NamedUid,
    pub deployment: NamedUid,
    pub release: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BindingReview {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub name: String,
    pub uid: String,
    pub resource_version: String,
    pub role_ref: k8s_openapi::api::rbac::v1::RoleRef,
    pub subjects: Vec<k8s_openapi::api::rbac::v1::Subject>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerReview {
    pub namespace: String,
    pub name: String,
    pub uid: String,
    pub resource_version: String,
}

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsSRERegistration",
    plural = "karssreregistrations",
    status = "RegistrationStatus",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsSRERegistrationSpec {
    pub controller: ControllerIdentity,
    pub sandbox: Source,
    pub runtime_namespace: NamedUid,
    #[serde(default)]
    pub legacy_bindings: Vec<BindingReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_consumer: Option<ConsumerReview>,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationStatus {
    pub phase: String,
    pub observed_generation: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_epoch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_service_account_uid: Option<String>,
    #[serde(default)]
    pub legacy_secret_access_denied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl KarsSRERegistration {
    pub fn validate(&self) -> Result<(), String> {
        let label = |value: &str| {
            !value.is_empty()
                && value.len() <= 63
                && value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && value.as_bytes()[0].is_ascii_alphanumeric()
                && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
        };
        if self.metadata.name.as_deref() != Some(NAME)
            || self.spec.sandbox.name != "sre"
            || self.spec.runtime_namespace.name != RUNTIME_NAMESPACE
            || !label(&self.spec.sandbox.namespace)
            || self.spec.sandbox.namespace != self.spec.controller.namespace.name
            || self.spec.controller.deployment.name != "kars-controller"
            || self.spec.controller.release.is_empty()
        {
            return Err(
                "Registration must pin the canonical SRE and its controller/release namespace"
                    .into(),
            );
        }
        for uid in [
            self.metadata.uid.as_deref().unwrap_or_default(),
            &self.spec.controller.namespace.uid,
            &self.spec.controller.deployment.uid,
            &self.spec.sandbox.uid,
            &self.spec.runtime_namespace.uid,
        ] {
            if uid.trim().is_empty() {
                return Err("Registration identity omitted an exact UID".into());
            }
        }
        if self.metadata.deletion_timestamp.is_some() {
            return Err(
                "Registration is terminating; disable and retire it before deletion".into(),
            );
        }
        let mut bindings = std::collections::BTreeSet::new();
        for review in &self.spec.legacy_bindings {
            if !matches!(review.kind.as_str(), "ClusterRoleBinding" | "RoleBinding")
                || review.name.is_empty()
                || review.uid.is_empty()
                || review.resource_version.is_empty()
                || (review.kind == "RoleBinding")
                    != review.namespace.as_ref().is_some_and(|ns| label(ns))
                || !bindings.insert((
                    review.kind.clone(),
                    review.namespace.clone(),
                    review.name.clone(),
                ))
            {
                return Err(
                    "Legacy binding reviews require unique exact names, UIDs and resourceVersions"
                        .into(),
                );
            }
        }
        if let Some(consumer) = &self.spec.legacy_consumer
            && (consumer.namespace != RUNTIME_NAMESPACE
                || consumer.name != "sre"
                || consumer.uid.is_empty()
                || consumer.resource_version.is_empty())
        {
            return Err(
                "Legacy SRE consumer review is incomplete or targets another workload".into(),
            );
        }
        Ok(())
    }

    pub fn epoch(&self) -> String {
        let mut value = serde_json::json!({
            "domain":crate::sre_privacy::REVISION,
            "uid":self.metadata.uid, "generation":self.metadata.generation, "spec":self.spec,
        });
        value.sort_all_objects();
        crate::providers::signing::sha256_hex(
            &serde_json::to_vec(&value).expect("registration serializes"),
        )
    }
}
