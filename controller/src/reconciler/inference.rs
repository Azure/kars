// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use k8s_openapi::api::core::v1::Secret;
use kube::{
    Client,
    api::{Api, DeleteParams},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PROVIDERS_SECRET: &str = "kars-inference-providers";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalTarget {
    namespace: String,
    match_labels: BTreeMap<String, String>,
    ports: Vec<u16>,
}

fn valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.len() <= 63
        && namespace
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && namespace
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && namespace
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

pub(super) fn local_egress_rules(targets: &str, namespaces: &str) -> Result<Vec<Value>, String> {
    let targets: Vec<LocalTarget> = if targets.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(targets)
            .map_err(|e| format!("LOCAL_INFERENCE_TARGETS_JSON is invalid: {e}"))?
    };
    if !targets.is_empty() {
        return targets.into_iter().map(|target| {
            if !valid_namespace(&target.namespace) || target.match_labels.is_empty()
                || target.match_labels.keys().any(|key| key.trim().is_empty())
                || target.ports.is_empty() || target.ports.contains(&0)
            {
                return Err("local inference targets require a namespace, pod labels and TCP ports in 1..65535".into());
            }
            Ok(json!({
                "to": [{
                    "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": target.namespace}},
                    "podSelector": {"matchLabels": target.match_labels}
                }],
                "ports": target.ports.iter().map(|port| json!({"protocol": "TCP", "port": port})).collect::<Vec<_>>()
            }))
        }).collect();
    }
    namespaces.split(',').map(str::trim).filter(|ns| !ns.is_empty()).map(|namespace| {
        if !valid_namespace(namespace) {
            return Err(format!("invalid local inference namespace: {namespace}"));
        }
        Ok(json!({
            "to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": namespace}}}],
            "ports": [{"protocol": "TCP"}]
        }))
    }).collect()
}

pub(super) fn configured_local_egress_rules() -> Result<Vec<Value>, String> {
    local_egress_rules(
        &std::env::var("LOCAL_INFERENCE_TARGETS_JSON").unwrap_or_default(),
        &std::env::var("LOCAL_INFERENCE_NAMESPACES").unwrap_or_default(),
    )
}

pub(super) fn provider_env_from() -> Value {
    json!([{"secretRef": {"name": PROVIDERS_SECRET, "optional": true}}])
}

/// A resource-version annotation restarts router pods when environment credentials change.
pub(super) async fn mirror_providers(
    client: &Client,
    source_ns: &str,
    target_ns: &str,
    sandbox: &str,
) -> Result<Option<String>, kube::Error> {
    let source: Api<Secret> = Api::namespaced(client.clone(), source_ns);
    let Some(secret) = source.get_opt(PROVIDERS_SECRET).await? else {
        let target: Api<Secret> = Api::namespaced(client.clone(), target_ns);
        if let Some(existing) = target.get_opt(PROVIDERS_SECRET).await?
            && existing
                .metadata
                .annotations
                .as_ref()
                .is_some_and(|annotations| {
                    annotations
                        .get(super::governance_mounts::MIRROR_SOURCE_KIND_ANNOTATION)
                        .map(String::as_str)
                        == Some("InferenceProviders")
                        && annotations
                            .get(super::governance_mounts::MIRROR_SOURCE_NS_ANNOTATION)
                            .map(String::as_str)
                            == Some(source_ns)
                })
        {
            match target
                .delete(PROVIDERS_SECRET, &DeleteParams::default())
                .await
            {
                Ok(_) => {}
                Err(kube::Error::Api(error)) if error.code == 404 => {}
                Err(error) => return Err(error),
            }
        }
        return Ok(None);
    };
    super::governance_mounts::mirror_secret(
        client,
        PROVIDERS_SECRET,
        source_ns,
        target_ns,
        sandbox,
        "InferenceProviders",
    )
    .await?;
    Ok(secret.metadata.resource_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_inference_is_opt_in_and_precise_targets_replace_namespace_allowance() {
        assert!(local_egress_rules("", "").unwrap().is_empty());
        let rules = local_egress_rules(
            r#"[{"namespace":"models","matchLabels":{"app":"model"},"ports":[5000]}]"#,
            "kars-local-inference",
        )
        .unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0]["to"][0]["namespaceSelector"]["matchLabels"]["kubernetes.io/metadata.name"],
            "models"
        );
        assert_eq!(
            rules[0]["to"][0]["podSelector"]["matchLabels"]["app"],
            "model"
        );
        assert_eq!(rules[0]["ports"][0]["port"], 5000);
    }

    #[test]
    fn malformed_targets_do_not_fall_back_to_broad_allowances() {
        for target in [
            "invalid",
            "{}",
            r#"[{"namespace":"models","ports":[80]}]"#,
            r#"[{"namespace":"models","matchLabels":{},"ports":[80]}]"#,
            r#"[{"namespace":"models","matchLabels":{"app":"x"},"ports":[0]}]"#,
            r#"[{"namespace":"models","matchLabels":{"app":"x"},"ports":[65536]}]"#,
        ] {
            assert!(local_egress_rules(target, "models").is_err(), "{target}");
        }
        assert!(local_egress_rules("[]", "models").is_ok());
        assert!(local_egress_rules("", "INVALID").is_err());
    }

    #[test]
    fn provider_credentials_are_optional_router_inputs() {
        assert_eq!(
            provider_env_from(),
            json!([{"secretRef":{"name":"kars-inference-providers","optional":true}}])
        );
    }
}
