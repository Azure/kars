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

    fn runtime_policy() -> Value {
        let bundle: Value = serde_json::from_str(BUNDLE).unwrap();
        bundle["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|policy| policy["name"] == "kars-inference-budget-runtime-binding")
            .unwrap()["spec"]
            .clone()
    }

    fn variable(policy: &Value, name: &str) -> String {
        policy["variables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|variable| variable["name"] == name)
            .unwrap()["expression"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn runtime_lifecycle_projection_covers_every_generated_spec_field() {
        use kube::CustomResourceExt;
        let crd = serde_json::to_value(crate::crd::KarsSandbox::crd()).unwrap();
        let properties = crd
            .pointer("/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties")
            .unwrap()
            .as_object()
            .unwrap();
        let expression = variable(&runtime_policy(), "suspensionOnly");
        let expected = format!(
            "oldObject != null && {}",
            properties
                .keys()
                .filter(|field| *field != "suspended")
                .map(|field| format!("dyn(object.spec).?{field} == dyn(oldObject.spec).?{field}"))
                .collect::<Vec<_>>()
                .join(" && ")
        );
        let clauses = |expression: &str| {
            expression
                .split(" && ")
                .map(str::to_string)
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            clauses(&expression),
            clauses(&expected),
            "Every new spec field must be frozen before native schema expansion"
        );
    }

    #[test]
    fn runtime_lifecycle_exception_retains_controller_and_source_authority_fences() {
        let policy = runtime_policy();
        let expression = policy["validations"][0]["expression"].as_str().unwrap();
        assert_eq!(
            expression,
            "request.userInfo.username == 'system:serviceaccount:__ACCOUNTING_NAMESPACE__:kars-controller' || (!has(object.spec.inferenceBudgetRef) && (oldObject == null || !has(oldObject.spec.inferenceBudgetRef))) || (variables.builtinSource && variables.sourceMetadataUnchanged && variables.suspensionOnly)"
        );
        let source = variable(&policy, "builtinSource");
        for fence in [
            "request.operation == 'UPDATE'",
            "oldObject != null",
            "has(oldObject.spec.inferenceBudgetRef)",
            "oldObject.metadata.name == 'bridge-orchestrator'",
            "dyn(oldObject.spec.inferenceBudgetRef).scope == 'GovernedInference'",
            "dyn(oldObject.spec.inferenceBudgetRef).root.kind == 'KarsSandbox'",
            "dyn(oldObject.spec.inferenceBudgetRef).root.resource.name == oldObject.metadata.name",
            "dyn(oldObject.spec.inferenceBudgetRef).root.resource['namespace'] == dyn(oldObject.metadata)['namespace']",
            "dyn(oldObject.spec.inferenceBudgetRef).root.resource.uid == oldObject.metadata.uid",
            "dyn(oldObject.spec.inferenceBudgetRef).taskUid == oldObject.metadata.uid",
            "dyn(oldObject.spec.inferenceBudgetRef).rootTaskUid == oldObject.metadata.uid",
            "dyn(oldObject.spec.inferenceBudgetRef).?parentTaskUid.orValue(null) == null",
            "oldObject.metadata.?ownerReferences.orValue([]).size() == 0",
            "object.metadata.?ownerReferences.orValue([]).size() == 0",
        ] {
            assert!(
                source.split(" && ").any(|clause| clause == fence),
                "{fence}"
            );
        }
        assert!(!source.contains(" || "));
        let metadata = variable(&policy, "sourceMetadataUnchanged");
        for field in ["generateName", "deletionTimestamp", "labels", "finalizers"] {
            assert!(metadata.contains(&format!(
                "object.metadata.?{field} == oldObject.metadata.?{field}"
            )));
        }
        for field in ["name", "uid"] {
            assert!(metadata.contains(&format!(
                "object.metadata.{field} == oldObject.metadata.{field}"
            )));
        }
        assert!(
            metadata.contains(
                "dyn(object.metadata)['namespace'] == dyn(oldObject.metadata)['namespace']"
            )
        );
        assert_eq!(metadata.matches(" || ").count(), 2);
        assert_eq!(
            metadata
                .matches("k == 'kars.azure.com/orchestrator-model-revision' ||")
                .count(),
            2
        );
        assert!(metadata.contains("object.metadata.?annotations.orValue({}).all(k,"));
        assert!(metadata.contains("oldObject.metadata.?annotations.orValue({}).all(k,"));
        assert!(metadata.contains("object.metadata.?annotations.orValue({})[?k] == oldObject.metadata.?annotations.orValue({})[?k]"));
        assert!(metadata.contains("oldObject.metadata.?annotations.orValue({})[?k] == object.metadata.?annotations.orValue({})[?k]"));
    }

    const FUND_AUTHORIZATION: &str = "authorizer.group('kars.azure.com').resource('karsbudgetaccounts').namespace(request.namespace).check('fund').allowed()";
    const PRIVATE_MAINTENANCE_AUTHORIZATION: &str = "authorizer.group('kars.azure.com').resource('karscredentialgrants').namespace('__ACCOUNTING_NAMESPACE__').name('workspace').check('manage').allowed()";

    fn has_unreviewed_namespace_accessor(expression: &str) -> bool {
        // Native admission accepts these exact ResourceCheck calls. Keep the
        // conservative schema-field guard for every other namespace accessor.
        expression
            .replace(FUND_AUTHORIZATION, "")
            .replace(PRIVATE_MAINTENANCE_AUTHORIZATION, "")
            .contains(".namespace")
    }

    #[test]
    fn namespace_guard_only_exempts_reviewed_native_authorization() {
        assert!(!has_unreviewed_namespace_accessor(FUND_AUTHORIZATION));
        assert!(!has_unreviewed_namespace_accessor(
            PRIVATE_MAINTENANCE_AUTHORIZATION
        ));
        for (from, to) in [
            ("karscredentialgrants", "secrets"),
            ("__ACCOUNTING_NAMESPACE__", "other-namespace"),
            ("name('workspace')", "name('other-grant')"),
            ("check('manage')", "check('update')"),
            (".namespace('__ACCOUNTING_NAMESPACE__')", ".namespace()"),
        ] {
            assert!(has_unreviewed_namespace_accessor(
                &PRIVATE_MAINTENANCE_AUTHORIZATION.replace(from, to)
            ));
        }
        assert!(has_unreviewed_namespace_accessor(&format!(
            "{PRIVATE_MAINTENANCE_AUTHORIZATION} || object.spec.namespace == 'workspace'"
        )));
        assert!(!has_unreviewed_namespace_accessor(
            "object.spec.__namespace__ == 'workspace'"
        ));
        for expression in [
            "object.spec.namespace == 'workspace'".to_string(),
            FUND_AUTHORIZATION.replace(".namespace(request.namespace)", ".namespace"),
            FUND_AUTHORIZATION.replace(".namespace(request.namespace)", ".namespace()"),
            FUND_AUTHORIZATION.replace("request.namespace", "object.metadata.namespace"),
            FUND_AUTHORIZATION.replace("check('fund')", "check('update')"),
            format!("{FUND_AUTHORIZATION} || object.spec.namespace == 'workspace'"),
        ] {
            assert!(has_unreviewed_namespace_accessor(&expression));
        }
    }

    #[test]
    fn shared_bundle_has_no_duplicate_names_or_cel_namespace_accessors() {
        let bundle: Value = serde_json::from_str(BUNDLE).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for policy in bundle["items"].as_array().unwrap() {
            assert!(seen.insert(policy["name"].as_str().unwrap()));
            assert_eq!(policy["spec"]["failurePolicy"], "Fail");
            assert!(!has_unreviewed_namespace_accessor(&policy.to_string()));
        }
        assert_eq!(
            seen,
            std::collections::BTreeSet::from([
                "kars-inference-budget-token-top-ups",
                "kars-inference-budget-public-ca",
                "kars-inference-budget-store",
                "kars-inference-budget-status",
                "kars-inference-budget-runtime-binding",
                "kars-inference-budget-namespace",
                "kars-inference-budget-workloads",
                "kars-inference-budget-token",
                "kars-inference-budget-private-connect",
            ])
        );
    }
}
