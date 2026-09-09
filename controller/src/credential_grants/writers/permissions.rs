// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use k8s_openapi::api::authorization::v1::SubjectAccessReview;
use serde_json::Value;
use std::collections::BTreeSet;

fn requests(
    grant: &KarsCredentialGrant,
    writer: &CredentialWriter,
    controller: (&str, &str),
) -> Result<Vec<Value>, String> {
    let workspace = grant.namespace().ok_or("Credential workspace missing")?;
    let mut scopes = BTreeSet::from([None, Some(workspace), Some(writer.namespace.clone())]);
    if let Some((namespace, _)) = controller
        .0
        .strip_prefix("system:serviceaccount:")
        .and_then(|identity| identity.split_once(':'))
    {
        scopes.insert(Some(namespace.into()));
    }
    scopes.extend(
        grant
            .spec
            .observation_targets
            .iter()
            .map(|target| Some(format!("kars-{}", target.name))),
    );
    let mut requests = Vec::new();
    for namespace in scopes {
        let mut checks = vec![
            ("", "secrets", "get", None),
            ("", "secrets", "list", None),
            ("", "secrets", "watch", None),
            ("", "secrets", "get", Some("router-services-admin")),
            (
                "",
                "secrets",
                "get",
                Some(crate::service_observer::TLS_SECRET),
            ),
            ("", "secrets", "get", Some("router-github-app")),
            (
                "",
                "secrets",
                "get",
                Some(crate::observation_privacy::SECRET),
            ),
            ("", "serviceaccounts/token", "create", None),
            ("", "pods", "create", None),
            ("", "pods/exec", "create", None),
            ("", "pods/attach", "create", None),
            ("", "pods/ephemeralcontainers", "patch", None),
            ("rbac.authorization.k8s.io", "roles", "bind", None),
            ("rbac.authorization.k8s.io", "clusterroles", "bind", None),
            ("rbac.authorization.k8s.io", "roles", "escalate", None),
            (
                "rbac.authorization.k8s.io",
                "clusterroles",
                "escalate",
                None,
            ),
            (
                "kars.azure.com",
                "karscredentialgrants",
                "manage",
                Some(NAME),
            ),
            (
                "kars.azure.com",
                "karscredentialgrants",
                "project-credentials",
                Some(NAME),
            ),
        ];
        for resource in ["deployments", "replicasets", "statefulsets", "daemonsets"] {
            for verb in ["create", "patch", "update"] {
                checks.push(("apps", resource, verb, None));
            }
        }
        for resource in [
            "roles",
            "rolebindings",
            "clusterroles",
            "clusterrolebindings",
        ] {
            for verb in ["create", "patch", "update"] {
                checks.push(("rbac.authorization.k8s.io", resource, verb, None));
            }
        }
        for (group, resource, verb, name) in checks {
            let (resource, subresource) = resource
                .split_once('/')
                .map_or((resource, None), |(r, s)| (r, Some(s)));
            let mut attributes = json!({"group":group,"resource":resource,"verb":verb});
            if let Some(subresource) = subresource {
                attributes["subresource"] = subresource.into();
            }
            if let Some(namespace) = &namespace {
                attributes["namespace"] = namespace.clone().into();
            }
            if let Some(name) = name {
                attributes["name"] = name.into();
            }
            requests.push(
                json!({"apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                "spec":{"user":format!("system:serviceaccount:{}:{}",writer.namespace,writer.name),
                    "uid":writer.uid,"groups":["system:authenticated","system:serviceaccounts",
                        format!("system:serviceaccounts:{}",writer.namespace)],
                    "resourceAttributes":attributes}}),
            );
        }
    }
    for (resource, name) in [
        ("groups", "system:masters"),
        ("groups", "system:authenticated"),
        ("groups", "system:serviceaccounts"),
        ("users", "system:kube-controller-manager"),
        ("uids", writer.uid.as_str()),
        ("users", controller.0),
        ("uids", controller.1),
    ] {
        requests.push(json!({"apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
            "spec":{"user":format!("system:serviceaccount:{}:{}",writer.namespace,writer.name),
                "uid":writer.uid,"groups":["system:authenticated","system:serviceaccounts",
                    format!("system:serviceaccounts:{}",writer.namespace)],
                "resourceAttributes":{"group":"","resource":resource,"verb":"impersonate","name":name}}}));
    }
    let (namespace, name) = controller
        .0
        .strip_prefix("system:serviceaccount:")
        .and_then(|identity| identity.split_once(':'))
        .ok_or("Controller subject is invalid")?;
    requests.push(json!({"apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
        "spec":{"user":format!("system:serviceaccount:{}:{}",writer.namespace,writer.name),
            "uid":writer.uid,"groups":["system:authenticated","system:serviceaccounts",
                format!("system:serviceaccounts:{}",writer.namespace)],
            "resourceAttributes":{"group":"","resource":"serviceaccounts","verb":"impersonate","namespace":namespace,"name":name}}}));
    Ok(requests)
}

pub(super) async fn verify(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    let reviews = Api::<SubjectAccessReview>::all(client.clone());
    if grant.spec.writers.is_empty() {
        return Ok(());
    }
    let controller = super::controller_subject(client).await?;
    for writer in &grant.spec.writers {
        for request in requests(grant, writer, (&controller.0, &controller.1))? {
            let request: SubjectAccessReview = serde_json::from_value(request)
                .map_err(|_| "Writer isolation authorization request is invalid")?;
            let response = reviews
                .create(&PostParams::default(), &request)
                .await
                .map_err(|e| api_error("Verify effective writer permission boundary", e))?;
            let response = serde_json::to_value(response)
                .map_err(|_| "Writer isolation authorization response is invalid")?;
            if response["status"]["allowed"] != false
                || response["status"]
                    .get("evaluationError")
                    .is_some_and(|error| !error.is_null() && error.as_str() != Some(""))
            {
                return Err("Writer has broad credential, workload, RBAC or impersonation authority (including inherited authentication groups); remove it before enrollment".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_writer_reviews_include_effective_groups_and_no_name_only_identity_assumption() {
        let grant: KarsCredentialGrant = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"work","uid":"grant"},
            "spec":{"workspaceUid":"workspace","writers":[{"namespace":"bridge","name":"bff","uid":"writer"}]}
        })).unwrap();
        let requests = requests(
            &grant,
            &grant.spec.writers[0],
            ("system:serviceaccount:core:kars-controller", "controller"),
        )
        .unwrap();
        assert!(
            requests
                .iter()
                .all(|request| request["spec"]["uid"] == "writer"
                    && request["spec"]["groups"]
                        == json!([
                            "system:authenticated",
                            "system:serviceaccounts",
                            "system:serviceaccounts:bridge"
                        ]))
        );
        for verb in ["get", "list", "watch"] {
            for namespace in [None, Some("work"), Some("bridge")] {
                assert!(requests.iter().any(|request| {
                    let attributes = &request["spec"]["resourceAttributes"];
                    attributes["resource"] == "secrets"
                        && attributes["verb"] == verb
                        && attributes["namespace"].as_str() == namespace
                        && attributes["name"].is_null()
                }));
            }
        }
    }
}
