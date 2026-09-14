// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional operator-owned GitHub App credential, independent of credentialsRef.
//! The single JSON key binds the full service identity and rotates atomically.

use serde_json::{Value, json};

pub(super) fn mount(pod: &mut Value, required: bool) {
    pod["volumes"]
        .as_array_mut()
        .expect("pod volumes")
        .push(json!({
            "name":"github-service",
            "secret":{"secretName":"router-github-app","optional":!required,
                "items":[{"key":"config.json","path":"config.json"}]}
        }));
    for container in pod["containers"].as_array_mut().expect("pod containers") {
        if container["name"] == "inference-router" {
            container["volumeMounts"]
                .as_array_mut()
                .expect("router mounts")
                .push(json!({
                    "name":"github-service","mountPath":"/etc/kars/github","readOnly":true
                }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_app_projection_is_optional_and_router_private() {
        let mut pod = json!({"volumes":[],"containers":[
            {"name":"openclaw","volumeMounts":[],"envFrom":[]},
            {"name":"agent","volumeMounts":[],"envFrom":[]},
            {"name":"inference-router","volumeMounts":[],"envFrom":[]}
        ],"initContainers":[{"name":"egress-guard","volumeMounts":[]}]});
        mount(&mut pod, false);
        assert_eq!(pod["containers"][0]["volumeMounts"], json!([]));
        assert_eq!(pod["containers"][1]["volumeMounts"], json!([]));
        assert_eq!(pod["initContainers"][0]["volumeMounts"], json!([]));
        assert_eq!(pod["containers"][2]["envFrom"], json!([]));
        assert_eq!(
            pod["containers"][2]["volumeMounts"][0]["mountPath"],
            "/etc/kars/github"
        );
        assert_eq!(
            pod["volumes"][0]["secret"]["secretName"],
            "router-github-app"
        );
        assert_eq!(pod["volumes"][0]["secret"]["optional"], true);
    }

    #[test]
    fn github_app_governed_projection_is_required_and_not_duplicated() {
        let mut pod = json!({"volumes":[],"containers":[
            {"name":"openclaw","volumeMounts":[]},
            {"name":"inference-router","volumeMounts":[]}
        ]});
        mount(&mut pod, true);
        assert_eq!(pod["volumes"].as_array().unwrap().len(), 1);
        assert_eq!(pod["volumes"][0]["secret"]["optional"], false);
        assert_eq!(pod["containers"][0]["volumeMounts"], json!([]));
        assert_eq!(
            pod["containers"][1]["volumeMounts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
