// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Live qualification of the generic private capability, not core bootstrap.

mod consumers;
mod runtime;
mod verification;

#[cfg(test)]
pub(crate) use consumers::private_material;
pub(crate) use consumers::{inspect_namespace, retired_material_consumers};
pub(crate) use runtime::{
    apply_deployment, approved_deployment, different_rsa_keys, for_sandbox, protect_pending,
    required_in_namespace, stamp_matches,
};
pub(crate) use verification::{bundle_revision, namespace_epoch, verify};

use k8s_openapi::api::core::v1::Namespace;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const PREFIX: &str = "kars.azure.com/private-";
pub(crate) const EPOCH: &str = "kars.azure.com/private-epoch";
pub(crate) const CONTRACT: &str = "kars.azure.com/private-consumption/v1";
const ERROR: &str =
    "Private capability is unqualified; regenerate and apply the reviewed grant activation";

pub(crate) fn bundle() -> Value {
    serde_json::from_str(include_str!(
        "../../deploy/helm/kars/files/private-consumption.json"
    ))
    .expect("embedded private admission bundle is valid JSON")
}

fn live(meta: &kube::api::ObjectMeta) -> Result<(&str, &str), String> {
    crate::credential_grants::identity(meta)
}

fn field(namespace: &Namespace, key: &str) -> Result<String, String> {
    namespace
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(&format!("{PREFIX}{key}")))
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| ERROR.into())
}

fn hash(value: &Value) -> String {
    fn ordered(value: &Value) -> Value {
        match value {
            Value::Object(fields) => serde_json::to_value(
                fields
                    .iter()
                    .map(|(key, value)| (key, ordered(value)))
                    .collect::<BTreeMap<_, _>>(),
            )
            .expect("JSON object serializes"),
            Value::Array(values) => Value::Array(values.iter().map(ordered).collect()),
            _ => value.clone(),
        }
    }
    crate::providers::signing::sha256_hex(
        &serde_json::to_vec(&ordered(value)).expect("JSON serializes"),
    )
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
