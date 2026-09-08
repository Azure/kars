// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::store::StoreError;
use crate::inference_budget_contract::BudgetError;
use k8s_openapi::api::admissionregistration::v1::{
    ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding,
};
use kube::{Api, Client, ResourceExt};
use serde_json::{Value, json};

const BUNDLE: &str =
    include_str!("../../../deploy/helm/kars/files/inference-budget-admission.json");

/// Erase only semantically empty/default selectors. Nonempty match conditions,
/// exclusions, namespace selectors, param refs or bypasses are never ignored.
fn normalized(value: &Value) -> Value {
    let mut value = prune_empty(value);
    for section in ["matchConstraints", "matchResources"] {
        for field in ["resourceRules", "excludeResourceRules"] {
            if let Some(rules) = value
                .get_mut(section)
                .and_then(|section| section.get_mut(field))
                .and_then(Value::as_array_mut)
            {
                for rule in rules {
                    if let Some(rule) = rule.as_object_mut() {
                        rule.entry("scope").or_insert(json!("*"));
                    }
                }
            }
        }
    }
    value
}

fn prune_empty(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter_map(|(key, value)| {
                    let value = prune_empty(value);
                    if value.is_null()
                        || value.as_array().is_some_and(Vec::is_empty)
                        || value.as_object().is_some_and(serde_json::Map::is_empty)
                    {
                        None
                    } else {
                        Some((key.clone(), value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(prune_empty).collect()),
        value => value.clone(),
    }
}

fn denied() -> StoreError {
    BudgetError::Authorization.into()
}

fn api_error(error: kube::Error) -> StoreError {
    StoreError::Api {
        stage: "verify budget admission",
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

fn matches_policy(actual: &Value, expected: &Value) -> bool {
    normalized(actual) == normalized(expected)
}

/// A Helm flag or an object name is not an effective admission proof. Compare
/// the exact shared policy specification and an unrestricted Deny binding,
/// including current-generation CEL compilation status, on every issuance.
pub async fn verify(client: &Client, accounting_namespace: &str) -> Result<(), StoreError> {
    let policies: Api<ValidatingAdmissionPolicy> = Api::all(client.clone());
    let bindings: Api<ValidatingAdmissionPolicyBinding> = Api::all(client.clone());
    let bundle: Value =
        serde_json::from_str(&BUNDLE.replace("__ACCOUNTING_NAMESPACE__", accounting_namespace))
            .map_err(|_| denied())?;
    for expected in bundle
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(denied)?
    {
        let name = expected
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(denied)?;
        let policy = policies.get(name).await.map_err(api_error)?;
        let policy_value = serde_json::to_value(&policy).map_err(|_| denied())?;
        let spec = policy_value.get("spec").ok_or_else(denied)?;
        if policy.metadata.deletion_timestamp.is_some()
            || policy.uid().is_none()
            || policy.metadata.generation.is_none()
            || policy_value
                .pointer("/status/observedGeneration")
                .and_then(Value::as_i64)
                != policy.metadata.generation
            || !policy_value
                .pointer("/status/typeChecking/expressionWarnings")
                .is_none_or(|warnings| warnings.as_array().is_some_and(Vec::is_empty))
            || !matches_policy(spec, expected.get("spec").ok_or_else(denied)?)
        {
            return Err(denied());
        }
        let binding = bindings.get(name).await.map_err(api_error)?;
        let binding_value = serde_json::to_value(&binding).map_err(|_| denied())?;
        if binding.metadata.deletion_timestamp.is_some()
            || binding.uid().is_none()
            || !matches_policy(
                binding_value.get("spec").ok_or_else(denied)?,
                &json!({"policyName": name, "validationActions": ["Deny", "Audit"]}),
            )
        {
            return Err(denied());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_empty_selectors_do_not_change_a_policy() {
        let wanted =
            json!({"failurePolicy":"Fail","matchConstraints":{"matchPolicy":"Equivalent"}});
        let defaulted = json!({"failurePolicy":"Fail","matchConstraints":{
            "matchPolicy":"Equivalent", "objectSelector":{}, "namespaceSelector":null
        }, "matchConditions":[]});
        assert!(matches_policy(&wanted, &defaulted));
    }

    #[test]
    fn names_modes_and_restrictive_bindings_are_not_proof() {
        let wanted = json!({"policyName":"guard", "validationActions":["Deny","Audit"]});
        for modified in [
            json!({"policyName":"guard","validationActions":["Warn","Audit"]}),
            json!({"policyName":"guard","validationActions":["Deny","Audit"],
                "matchResources":{"namespaceSelector":{"matchLabels":{"never":"true"}}}}),
            json!({"policyName":"other","validationActions":["Deny","Audit"]}),
        ] {
            assert!(!matches_policy(&wanted, &modified));
        }
        let wanted = json!({"failurePolicy":"Fail","validations":[{"expression":"false"}]});
        let mut bypass = wanted.clone();
        bypass["matchConditions"] = json!([{"name":"skip","expression":"false"}]);
        assert!(!matches_policy(&wanted, &bypass));
        let mut selector_bypass = wanted.clone();
        selector_bypass["matchConstraints"] = json!({
            "namespaceSelector":{"matchLabels":{"scope":"*"}}
        });
        assert!(!matches_policy(&wanted, &selector_bypass));
    }

    #[test]
    fn shared_bundle_has_no_duplicate_names_or_cel_namespace_accessors() {
        let bundle: Value = serde_json::from_str(BUNDLE).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for policy in bundle["items"].as_array().unwrap() {
            assert!(seen.insert(policy["name"].as_str().unwrap()));
            assert_eq!(policy["spec"]["failurePolicy"], "Fail");
            assert!(!policy.to_string().contains(".namespace"));
        }
        assert_eq!(seen.len(), 8);
    }
}
