// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use k8s_openapi::{
    api::core::v1::{Endpoints, Service},
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{Api, Client};
use serde_json::{Value, json};
use std::{collections::BTreeSet, net::IpAddr};

const MAX_ENDPOINTS: usize = 32;

fn read_error(resource: &'static str, error: kube::Error) -> String {
    match error {
        kube::Error::Api(response) => {
            format!(
                "Read canonical API {resource}: Kubernetes status {}",
                response.code
            )
        }
        _ => format!("Read canonical API {resource}: Kubernetes transport failure"),
    }
}

pub(super) async fn rules(
    client: &Client,
    service_host: &str,
    service_port: &str,
) -> Result<Vec<Value>, String> {
    let service = Api::<Service>::namespaced(client.clone(), "default")
        .get("kubernetes")
        .await
        .map_err(|error| read_error("Service", error))?;
    let endpoints = Api::<Endpoints>::namespaced(client.clone(), "default")
        .get("kubernetes")
        .await
        .map_err(|error| read_error("Endpoints", error))?;
    from_objects(&service, &endpoints, service_host, service_port)
}

fn canonical(metadata: &ObjectMeta) -> bool {
    metadata.name.as_deref() == Some("kubernetes")
        && metadata.namespace.as_deref() == Some("default")
        && metadata
            .uid
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && metadata
            .resource_version
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && metadata.deletion_timestamp.is_none()
}

fn address(value: &str) -> Result<IpAddr, String> {
    let ip: IpAddr = value
        .parse()
        .map_err(|_| "Invalid API endpoint IP address")?;
    if ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || matches!(ip, IpAddr::V4(ip) if ip.is_broadcast() || ip.is_link_local())
        || matches!(ip, IpAddr::V6(ip) if ip.is_unicast_link_local())
    {
        return Err("API endpoint must be a routable unicast IP".into());
    }
    Ok(ip)
}

fn port(value: i32) -> Result<u16, String> {
    u16::try_from(value)
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| "Invalid API HTTPS port".into())
}

fn rule(ip: IpAddr, port: u16) -> Value {
    let prefix = if ip.is_ipv4() { 32 } else { 128 };
    json!({
        "to": [{"ipBlock": {"cidr": format!("{ip}/{prefix}")}}],
        "ports": [{"protocol": "TCP", "port": port}],
    })
}

fn from_objects(
    service: &Service,
    endpoints: &Endpoints,
    service_host: &str,
    service_port: &str,
) -> Result<Vec<Value>, String> {
    if !canonical(&service.metadata) || !canonical(&endpoints.metadata) {
        return Err("Canonical API Service/Endpoints identity is missing or terminating".into());
    }
    let host = address(service_host)?;
    let configured_port = port(
        service_port
            .parse()
            .map_err(|_| "Invalid configured API HTTPS port")?,
    )?;
    let spec = service
        .spec
        .as_ref()
        .ok_or("Canonical API Service spec is missing")?;
    if spec.cluster_ip.as_deref().map(address).transpose()? != Some(host)
        || !spec
            .ports
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|entry| {
                entry.name.as_deref() == Some("https")
                    && entry.protocol.as_deref().unwrap_or("TCP") == "TCP"
                    && entry.port == i32::from(configured_port)
            })
    {
        return Err("Canonical API Service does not match the controller's HTTPS target".into());
    }
    let mut targets = BTreeSet::new();
    for subset in endpoints.subsets.as_deref().unwrap_or_default() {
        for entry in subset.ports.as_deref().unwrap_or_default() {
            if entry.name.as_deref() != Some("https") {
                continue;
            }
            if entry.protocol.as_deref().unwrap_or("TCP") != "TCP" {
                return Err("API HTTPS endpoint must use TCP".into());
            }
            let endpoint_port = port(entry.port)?;
            // Only ready addresses are eligible; never include notReadyAddresses.
            for entry in subset.addresses.as_deref().unwrap_or_default() {
                let ip = address(&entry.ip)?;
                if ip.is_ipv4() != host.is_ipv4() {
                    continue;
                }
                targets.insert((ip, endpoint_port));
                if targets.len() > MAX_ENDPOINTS {
                    return Err("Canonical API endpoint inventory exceeds its bounded limit".into());
                }
            }
        }
    }
    if targets.is_empty() {
        return Err("Canonical API Service has no ready HTTPS endpoints".into());
    }
    let mut rules = vec![rule(host, configured_port)];
    rules.extend(
        targets
            .into_iter()
            .filter(|target| *target != (host, configured_port))
            .map(|(ip, port)| rule(ip, port)),
    );
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> (Service, Endpoints) {
        let metadata =
            json!({"name":"kubernetes","namespace":"default","uid":"live","resourceVersion":"1"});
        (
            serde_json::from_value(json!({"metadata":metadata,"spec":{
                "clusterIP":"10.96.0.1","ports":[{"name":"https","port":443,"protocol":"TCP"}]}}))
            .unwrap(),
            serde_json::from_value(json!({"metadata":metadata,"subsets":[{
                "addresses":[{"ip":"172.18.0.2"},{"ip":"172.18.0.2"}],
                "notReadyAddresses":[{"ip":"172.18.0.99"}],
                "ports":[{"name":"https","port":6443,"protocol":"TCP"}]}]}))
            .unwrap(),
        )
    }

    #[test]
    fn exact_service_and_post_dnat_targets_are_paired_deduplicated_and_ready_only() {
        let (service, endpoints) = fixtures();
        assert_eq!(
            from_objects(&service, &endpoints, "10.96.0.1", "443").unwrap(),
            vec![
                rule("10.96.0.1".parse().unwrap(), 443),
                rule("172.18.0.2".parse().unwrap(), 6443)
            ]
        );
    }

    #[test]
    fn ipv6_uses_only_exact_ready_targets_of_the_service_family() {
        let (mut service, mut endpoints) = fixtures();
        service.spec.as_mut().unwrap().cluster_ip = Some("fd00::1".into());
        endpoints.subsets.as_mut().unwrap()[0]
            .addresses
            .as_mut()
            .unwrap()[0]
            .ip = "fd01::2".into();
        assert_eq!(
            from_objects(&service, &endpoints, "fd00::1", "443").unwrap(),
            vec![
                rule("fd00::1".parse().unwrap(), 443),
                rule("fd01::2".parse().unwrap(), 6443)
            ]
        );
    }

    #[test]
    fn missing_identity_mismatched_service_and_unready_inventory_fail_closed() {
        let (service, endpoints) = fixtures();
        assert!(from_objects(&service, &endpoints, "10.96.0.2", "443").is_err());
        assert!(from_objects(&service, &endpoints, "10.96.0.1", "444").is_err());
        assert!(from_objects(&service, &endpoints, "10.96.0.1", "invalid").is_err());
        let mut invalid = service.clone();
        invalid.metadata.namespace = Some("other".into());
        assert!(from_objects(&invalid, &endpoints, "10.96.0.1", "443").is_err());
        let mut invalid = endpoints.clone();
        invalid.metadata.uid = None;
        assert!(from_objects(&service, &invalid, "10.96.0.1", "443").is_err());
        let mut invalid = endpoints.clone();
        invalid.subsets.as_mut().unwrap()[0].addresses = None;
        assert!(from_objects(&service, &invalid, "10.96.0.1", "443").is_err());
    }

    #[test]
    fn malformed_addresses_ports_or_protocols_never_produce_partial_rules() {
        let (service, endpoints) = fixtures();
        for ip in [
            "invalid",
            "0.0.0.0",
            "127.0.0.1",
            "169.254.169.254",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            let mut invalid = endpoints.clone();
            invalid.subsets.as_mut().unwrap()[0]
                .addresses
                .as_mut()
                .unwrap()[0]
                .ip = ip.into();
            assert!(from_objects(&service, &invalid, "10.96.0.1", "443").is_err());
        }
        for value in [0, -1, 65536] {
            let mut invalid = endpoints.clone();
            invalid.subsets.as_mut().unwrap()[0].ports.as_mut().unwrap()[0].port = value;
            assert!(from_objects(&service, &invalid, "10.96.0.1", "443").is_err());
        }
        let mut invalid = endpoints;
        invalid.subsets.as_mut().unwrap()[0].ports.as_mut().unwrap()[0].protocol =
            Some("UDP".into());
        assert!(from_objects(&service, &invalid, "10.96.0.1", "443").is_err());
    }

    #[test]
    fn endpoint_expansion_is_bounded_and_read_errors_never_echo_api_bodies() {
        let (service, mut endpoints) = fixtures();
        endpoints.subsets.as_mut().unwrap()[0].addresses = Some(
            (1..=33)
                .map(|index| {
                    serde_json::from_value(json!({"ip":format!("172.18.1.{index}")})).unwrap()
                })
                .collect(),
        );
        assert!(from_objects(&service, &endpoints, "10.96.0.1", "443").is_err());
        let error = kube::Error::Api(serde_json::from_value(json!({
            "status":"Failure","message":"private-response-must-not-log","reason":"Forbidden","code":403
        })).unwrap());
        assert_eq!(
            read_error("Endpoints", error),
            "Read canonical API Endpoints: Kubernetes status 403"
        );
    }
}
