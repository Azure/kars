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

pub(crate) fn isolated(
    policies: &[NetworkPolicy],
    labels: &BTreeMap<String, String>,
    direction: &str,
) -> bool {
    policies
        .iter()
        .filter(|policy| {
            !policy
                .metadata
                .labels
                .as_ref()
                .is_some_and(|labels| labels.contains_key("kars.azure.com/observer-metadata-grant"))
        })
        .filter_map(|policy| policy.spec.as_ref())
        .any(|spec| {
            spec.pod_selector
                .as_ref()
                .is_none_or(|selector| matches(selector, labels))
                && spec.policy_types.as_ref().map_or_else(
                    || direction == "Ingress" || spec.egress.is_some(),
                    |types| types.iter().any(|kind| kind == direction),
                )
        })
}

pub(super) async fn rpc_baseline(
    client: &Client,
    sandbox: &crate::crd::KarsSandbox,
    runtime: &Namespace,
    endpoint: &crate::observation_privacy::Endpoint,
) -> Result<(), String> {
    let controller = Api::<Namespace>::all(client.clone())
        .get(&endpoint.namespace)
        .await
        .map_err(|e| api_error("Read verifier network namespace", e))?;
    if controller.uid().as_deref() != Some(endpoint.namespace_uid.as_str())
        || controller.metadata.deletion_timestamp.is_some()
    {
        return Err("Verifier network namespace changed".into());
    }
    for (namespace, labels) in [
        (
            runtime.name_any(),
            crate::reconciler::build_pod_labels(&sandbox.name_any()),
        ),
        (
            endpoint.namespace.clone(),
            BTreeMap::from([
                ("app.kubernetes.io/name".into(), "kars".into()),
                ("app.kubernetes.io/component".into(), "controller".into()),
            ]),
        ),
    ] {
        let policies = Api::<NetworkPolicy>::namespaced(client.clone(), &namespace)
            .list(&ListParams::default())
            .await
            .map_err(|e| api_error("Read approved verifier network baseline", e))?;
        for direction in ["Ingress", "Egress"] {
            if !isolated(&policies.items, &labels, direction) {
                return Err("Observation verifier requires approved existing controller/runtime network isolation; no new global isolation was created".into());
            }
        }
    }
    Ok(())
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
    let target = crate::reconciler::build_pod_labels(&sandbox.name_any());
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
    fn observation_baseline_uses_the_generated_runtime_labels_without_accepting_other_selectors() {
        let labels = crate::reconciler::build_pod_labels("agent");
        assert_eq!(
            labels,
            BTreeMap::from([
                ("kars.azure.com/sandbox".into(), "agent".into()),
                ("kars.azure.com/component".into(), "sandbox".into()),
                ("azure.workload.identity/use".into(), "true".into()),
            ])
        );
        let policy: NetworkPolicy = serde_json::from_value(json!({
            "metadata":{"name":"sandbox-policy","namespace":"kars-agent"},
            "spec":{"podSelector":{"matchLabels":{"kars.azure.com/component":"sandbox"}},
                "policyTypes":["Ingress","Egress"],"ingress":[],"egress":[]}
        }))
        .unwrap();
        for direction in ["Ingress", "Egress"] {
            assert!(isolated(std::slice::from_ref(&policy), &labels, direction));
            let incomplete = BTreeMap::from([("kars.azure.com/sandbox".into(), "agent".into())]);
            assert!(!isolated(
                std::slice::from_ref(&policy),
                &incomplete,
                direction
            ));
        }
        let mut foreign = policy.clone();
        foreign
            .spec
            .as_mut()
            .unwrap()
            .pod_selector
            .as_mut()
            .unwrap()
            .match_labels = Some(BTreeMap::from([(
            "kars.azure.com/sandbox".into(),
            "other".into(),
        )]));
        assert!(!isolated(&[foreign], &labels, "Ingress"));
        let mut observer_only = policy;
        observer_only.metadata.labels = Some(BTreeMap::from([(
            "kars.azure.com/observer-metadata-grant".into(),
            "grant".into(),
        )]));
        assert!(!isolated(&[observer_only], &labels, "Egress"));
    }

    #[test]
    fn observation_sender_egress_can_select_the_actual_runtime_component_and_name() {
        let runtime: Namespace = serde_json::from_value(json!({"metadata":{"name":"kars-agent",
            "labels":{"kubernetes.io/metadata.name":"kars-agent"}}}))
        .unwrap();
        let sender = BTreeMap::from([("app".into(), "bff".into())]);
        let policy: NetworkPolicy = serde_json::from_value(json!({"metadata":{},"spec":{
            "podSelector":{"matchLabels":{"app":"bff"}},"policyTypes":["Egress"],"egress":[{
                "to":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":"kars-agent"}},
                    "podSelector":{"matchLabels":{"kars.azure.com/component":"sandbox",
                        "kars.azure.com/sandbox":"agent"}}}],
                "ports":[{"port":9447,"protocol":"TCP"}]
            }]
        }})).unwrap();
        assert!(approved(
            std::slice::from_ref(&policy),
            "bridge",
            &sender,
            &runtime,
            &crate::reconciler::build_pod_labels("agent")
        ));
        assert!(!approved(
            &[policy],
            "bridge",
            &sender,
            &runtime,
            &crate::reconciler::build_pod_labels("other")
        ));
    }

    #[test]
    fn observation_egress_preflight_does_not_require_or_create_isolation() {
        let runtime: Namespace = serde_json::from_value(json!({"metadata":{"name":"kars-agent",
            "labels":{"kubernetes.io/metadata.name":"kars-agent"}}}))
        .unwrap();
        let target = crate::reconciler::build_pod_labels("agent");
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
