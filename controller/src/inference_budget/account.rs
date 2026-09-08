// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.ledger.phase"}"#
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KarsBudgetAccountSpec {
    pub scope: BudgetScope,
    /// Root Task UID, or lifetime Team UID. This is never a display-name key.
    pub root: RootIdentity,
    pub limits: Limits,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KarsBudgetAccountStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<Ledger>,
}

pub const BOOTSTRAP: &str = "kars.azure.com/inference-budget-bootstrap";
pub const MANAGED_BY: &str = "app.kubernetes.io/managed-by";
pub const OWNER: &str = "kars-inference-budget";

pub fn name_for_root(root: &RootIdentity) -> String {
    format!("inference-budget-{}", root.resource.uid)
}
