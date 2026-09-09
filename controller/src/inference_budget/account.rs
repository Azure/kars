// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::inference_budget_contract::{BudgetScope, Limits, RootIdentity, ledger::Ledger};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsBudgetAccount",
    namespaced,
    status = "KarsBudgetAccountStatus",
    shortname = "kbudget",
    printcolumn = r#"{"name":"Scope","type":"string","jsonPath":".spec.scope"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KarsBudgetAccountSpec {
    pub scope: BudgetScope,
    /// Root Task UID, or lifetime Team UID. This is never a display-name key.
    pub root: RootIdentity,
    pub limits: Limits,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum AccountStatusPhase {
    Bootstrap,
    Active,
    Blocked,
    Closing,
    Closed,
    Frozen,
    Corrupt,
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KarsBudgetAccountStatus {
    /// Observational ledger admission state, never spend authority or router readiness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<AccountStatusPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "conditions_schema")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<Ledger>,
}

fn conditions_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = Vec::<Condition>::json_schema(generator);
    schema.insert("x-kubernetes-list-type".into(), serde_json::json!("map"));
    schema.insert(
        "x-kubernetes-list-map-keys".into(),
        serde_json::json!(["type"]),
    );
    schema
}

pub const BOOTSTRAP: &str = "kars.azure.com/inference-budget-bootstrap";
pub const MANAGED_BY: &str = "app.kubernetes.io/managed-by";
pub const OWNER: &str = "kars-inference-budget";

pub fn name_for_root(root: &RootIdentity) -> String {
    format!("inference-budget-{}", root.resource.uid)
}
