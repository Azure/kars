// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only sender preflight. Core never introduces sender egress isolation.

use super::*;
use k8s_openapi::{
    api::{
        core::v1::Pod,
        networking::v1::{NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyPeer},
    },
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use std::collections::BTreeMap;

fn matches(selector: &LabelSelector, labels: &BTreeMap<String, String>) -> bool {
    selector.match_labels.as_ref().is_none_or(|expected| {
        expected
            .iter()
            .all(|(key, value)| labels.get(key) == Some(value))
    }) && selector
        .match_expressions
        .as_ref()
        .is_none_or(|requirements| {
            requirements.iter().all(|requirement| {
                let value = labels.get(&requirement.key);
                let listed = value.is_some_and(|value| {
                    requirement
                        .values
                        .as_ref()
                        .is_some_and(|values| values.contains(value))
                });
                match requirement.operator.as_str() {
                    "In" => listed,
                    "NotIn" => !listed,
                    "Exists" => value.is_some(),
                    "DoesNotExist" => value.is_none(),
                    _ => false,
                }
            })
        })
}

fn peer_allows(
    peer: &NetworkPolicyPeer,
    sender_namespace: &str,
    runtime: &Namespace,
    target: &BTreeMap<String, String>,
) -> bool {
    if peer.ip_block.is_some() {
        return false;
    }
    let runtime_labels = runtime.metadata.labels.clone().unwrap_or_default();
    peer.namespace_selector.as_ref().map_or_else(
        || peer.pod_selector.is_none() || sender_namespace == runtime.name_any(),
        |selector| matches(selector, &runtime_labels),
    ) && peer
        .pod_selector
        .as_ref()
        .is_none_or(|selector| matches(selector, target))
}

fn rule_allows(
    rule: &NetworkPolicyEgressRule,
    sender_namespace: &str,
    runtime: &Namespace,
    target: &BTreeMap<String, String>,
) -> bool {
    let ports = rule.ports.as_ref().is_none_or(|ports| {
        ports.is_empty()
            || ports.iter().any(|port| {
                if port.protocol.as_deref().unwrap_or("TCP") != "TCP" {
                    return false;
                }
                match &port.port {
                    None => true,
                    Some(IntOrString::Int(start)) => (*start..=port.end_port.unwrap_or(*start))
                        .contains(&i32::from(crate::service_observer::PORT)),
                    Some(IntOrString::String(_)) => false,
                }
            })
    });
    ports
        && rule.to.as_ref().is_none_or(|peers| {
            peers.is_empty()
                || peers
                    .iter()
                    .any(|peer| peer_allows(peer, sender_namespace, runtime, target))
        })
}

fn approved(
    policies: &[NetworkPolicy],
    sender_namespace: &str,
    labels: &BTreeMap<String, String>,
    runtime: &Namespace,
    target: &BTreeMap<String, String>,
) -> bool {
    let selected: Vec<_> = policies
        .iter()
        .filter_map(|policy| policy.spec.as_ref())
        .filter(|spec| {
            spec.pod_selector
                .as_ref()
                .is_none_or(|selector| matches(selector, labels))
                && (spec
                    .policy_types
                    .as_ref()
                    .is_some_and(|types| types.iter().any(|kind| kind == "Egress"))
                    || (spec.policy_types.is_none() && spec.egress.is_some()))
        })
        .collect();
    selected.is_empty()
        || selected.iter().any(|spec| {
            spec.egress.as_ref().is_some_and(|rules| {
                rules
                    .iter()
                    .any(|rule| rule_allows(rule, sender_namespace, runtime, target))
            })
        })
}

pub(super) async fn verify(
    client: &Client,
    grant: &KarsCredentialGrant,
    sandbox: &crate::crd::KarsSandbox,
    runtime: &Namespace,
) -> Result<(), String> {
    let target = BTreeMap::from([("kars.azure.com/sandbox".into(), sandbox.name_any())]);
    for writer in &grant.spec.writers {
        let pods = Api::<Pod>::namespaced(client.clone(), &writer.namespace)
            .list(
                &ListParams::default()
                    .labels("app.kubernetes.io/name=kars-bridge,app.kubernetes.io/component=bff"),
            )
            .await
            .map_err(|e| api_error("Inspect observation sender workloads", e))?;
        let policies = Api::<NetworkPolicy>::namespaced(client.clone(), &writer.namespace)
            .list(&ListParams::default())
            .await
            .map_err(|e| api_error("Inspect approved observation sender egress", e))?;
        let senders: Vec<_> = pods
            .iter()
            .filter(|pod| {
                pod.metadata.deletion_timestamp.is_none()
                    && pod
                        .spec
                        .as_ref()
                        .and_then(|spec| spec.service_account_name.as_deref())
                        == Some(writer.name.as_str())
            })
            .collect();
        if senders.is_empty()
            || senders.iter().any(|pod| {
                !approved(
                    &policies.items,
                    &writer.namespace,
                    &pod.metadata.labels.clone().unwrap_or_default(),
                    runtime,
                    &target,
                )
            })
        {
            return Err("Private observations unavailable: no approved live BFF egress path to runtime TCP 9447; preserve the existing API/provider/OIDC policy and explicitly add that path".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_egress_preflight_does_not_require_or_create_isolation() {
        let runtime: Namespace = serde_json::from_value(json!({"metadata":{"name":"kars-agent",
            "labels":{"kubernetes.io/metadata.name":"kars-agent"}}}))
        .unwrap();
        let target = BTreeMap::from([("kars.azure.com/sandbox".into(), "agent".into())]);
        let labels = BTreeMap::from([("app".into(), "bff".into())]);
        assert!(approved(&[], "bridge", &labels, &runtime, &target));
        let mut policy: NetworkPolicy = serde_json::from_value(json!({"metadata":{},"spec":{
            "podSelector":{"matchLabels":{"app":"bff"}},"policyTypes":["Egress"],"egress":[]
        }}))
        .unwrap();
        assert!(!approved(
            &[policy.clone()],
            "bridge",
            &labels,
            &runtime,
            &target
        ));
        policy.spec.as_mut().unwrap().egress = Some(serde_json::from_value(json!([{
            "to":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":"kars-agent"}},
                "podSelector":{"matchExpressions":[{"key":"kars.azure.com/sandbox","operator":"Exists"}]}}],
            "ports":[{"port":9447,"protocol":"TCP"}]
        }])).unwrap());
        assert!(approved(
            &[policy.clone()],
            "bridge",
            &labels,
            &runtime,
            &target
        ));
        for (port, protocol) in [(8443, "TCP"), (9447, "UDP")] {
            let mut denied = policy.clone();
            let entry = &mut denied.spec.as_mut().unwrap().egress.as_mut().unwrap()[0]
                .ports
                .as_mut()
                .unwrap()[0];
            entry.port = Some(IntOrString::Int(port));
            entry.protocol = Some(protocol.into());
            assert!(!approved(&[denied], "bridge", &labels, &runtime, &target));
        }
        let mut foreign = runtime.clone();
        foreign
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .insert("kubernetes.io/metadata.name".into(), "other".into());
        assert!(!approved(&[policy], "bridge", &labels, &foreign, &target));
    }
}
