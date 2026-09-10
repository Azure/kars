// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewedObject {
    pub name: String,
    pub uid: String,
    pub resource_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewedConsumer {
    pub kind: String,
    pub object: ReviewedObject,
    pub template_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootReview {
    pub namespace: ReviewedObject,
    pub account: ReviewedObject,
    pub deployment: ReviewedObject,
    pub template_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tls: Option<BudgetTlsReview>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BudgetTlsReview {
    pub namespace: ReviewedObject,
    pub secret: ReviewedObject,
    pub key_digest: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ControllerProfile {
    ServiceAccounts,
    KcmCertificate,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamespaceReview {
    pub namespace: ReviewedObject,
    pub consumers: Vec<ReviewedConsumer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateActivation {
    pub contract: String,
    pub phase: String,
    pub bundle_revision: String,
    pub root: RootReview,
    pub profile: ControllerProfile,
    pub controller_uids: BTreeMap<String, String>,
    pub namespaces: Vec<NamespaceReview>,
}
