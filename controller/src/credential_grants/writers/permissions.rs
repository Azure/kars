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
        for (group, resource) in [
            ("", "replicationcontrollers"),
            ("apps", "deployments"),
            ("apps", "replicasets"),
            ("apps", "statefulsets"),
            ("apps", "daemonsets"),
            ("batch", "jobs"),
            ("batch", "cronjobs"),
        ] {
            for verb in ["create", "patch", "update"] {
                checks.push((group, resource, verb, None));
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
    let mut principals = vec![(
        namespace.to_string(),
        name.to_string(),
        controller.1.to_string(),
    )];
    if let Some(activation) = &grant.spec.private_activation {
        principals.extend(
            activation
                .controller_uids
                .iter()
                .map(|(name, uid)| ("kube-system".into(), name.clone(), uid.clone())),
        );
    }
    for (namespace, name, uid) in principals {
        for attributes in [
            json!({"group":"","resource":"serviceaccounts","subresource":"token","verb":"create","namespace":namespace,"name":name}),
            json!({"group":"","resource":"serviceaccounts","verb":"impersonate","namespace":namespace,"name":name}),
            json!({"group":"","resource":"users","verb":"impersonate","name":format!("system:serviceaccount:{namespace}:{name}")}),
            json!({"group":"","resource":"uids","verb":"impersonate","name":uid}),
        ] {
            requests.push(
                json!({"apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                "spec":{"user":format!("system:serviceaccount:{}:{}",writer.namespace,writer.name),
                    "uid":writer.uid,"groups":["system:authenticated","system:serviceaccounts",
                        format!("system:serviceaccounts:{}",writer.namespace)],
                    "resourceAttributes":attributes}}),
            );
        }
    }
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
    use std::sync::{Arc, Mutex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCOPES: [Option<&str>; 6] = [
        None,
        Some("work"),
        Some("bridge"),
        Some("core"),
        Some("kars-agent"),
        Some("kars-second"),
    ];
    const TEMPLATES: [(&str, &str); 3] = [
        ("", "replicationcontrollers"),
        ("batch", "jobs"),
        ("batch", "cronjobs"),
    ];

    fn template_grant() -> KarsCredentialGrant {
        serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"work","uid":"grant"},
            "spec":{"workspaceUid":"workspace","writers":[{"namespace":"bridge","name":"bff","uid":"writer"}],
                "observationTargets":[
                    {"kind":"KarsSandbox","namespace":"work","name":"agent","uid":"agent-uid"},
                    {"kind":"KarsSandbox","namespace":"work","name":"second","uid":"second-uid"}
                ]}
        }))
        .unwrap()
    }

    fn attributes(namespace: Option<&str>, group: &str, resource: &str, verb: &str) -> Value {
        let mut value = json!({"group":group,"resource":resource,"verb":verb});
        if let Some(namespace) = namespace {
            value["namespace"] = namespace.into();
        }
        value
    }

    #[derive(Default)]
    struct Reviews {
        fault: Option<(Value, Value)>,
        calls: Vec<Value>,
    }

    async fn review_fixture() -> (MockServer, Client, Arc<Mutex<Reviews>>) {
        let server = MockServer::start().await;
        let reviews = Arc::new(Mutex::new(Reviews::default()));
        let captured = reviews.clone();
        Mock::given(|_: &wiremock::Request| true)
            .respond_with(move |request: &wiremock::Request| {
                if request.method == "POST" && request.url.path().ends_with("/selfsubjectreviews") {
                    return ResponseTemplate::new(201).set_body_json(json!({
                        "apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview",
                        "status":{"userInfo":{"username":"system:serviceaccount:core:kars-controller","uid":"controller"}}
                    }));
                }
                assert_eq!(request.method, "POST");
                assert!(request.url.path().ends_with("/subjectaccessreviews"));
                let body: Value = request.body_json().unwrap();
                let mut reviews = captured.lock().unwrap();
                let status = reviews
                    .fault
                    .as_ref()
                    .filter(|(attributes, _)| body["spec"]["resourceAttributes"] == *attributes)
                    .map_or_else(|| json!({"allowed":false}), |(_, status)| status.clone());
                reviews.calls.push(body.clone());
                ResponseTemplate::new(201).set_body_json(json!({
                    "apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                    "spec":body["spec"],"status":status
                }))
            })
            .mount(&server)
            .await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        (server, client, reviews)
    }

    #[tokio::test]
    async fn credential_writer_template_denials_cover_every_protected_namespace_and_effective_identity()
     {
        let (_server, client, reviews) = review_fixture().await;
        verify(&client, &template_grant()).await.unwrap();
        let reviews = reviews.lock().unwrap();
        for namespace in SCOPES {
            for (group, resource) in TEMPLATES {
                for verb in ["create", "update", "patch"] {
                    let expected = attributes(namespace, group, resource, verb);
                    assert_eq!(
                        reviews
                            .calls
                            .iter()
                            .filter(|request| request["spec"]["resourceAttributes"] == expected)
                            .count(),
                        1,
                        "Each protected scope requires exactly one unnamed template permission review"
                    );
                }
            }
        }
        assert!(reviews.calls.iter().all(|request| {
            request["spec"]["user"] == "system:serviceaccount:bridge:bff"
                && request["spec"]["uid"] == "writer"
                && request["spec"]["groups"]
                    == json!([
                        "system:authenticated",
                        "system:serviceaccounts",
                        "system:serviceaccounts:bridge"
                    ])
        }));
    }

    #[tokio::test]
    async fn credential_writer_each_template_permission_or_evaluation_error_fails_closed_in_each_scope()
     {
        let (_server, client, reviews) = review_fixture().await;
        let grant = template_grant();
        for status in [
            json!({"allowed":true}),
            json!({"allowed":false,"evaluationError":"PRIVATE_REVIEW_ERROR"}),
        ] {
            for namespace in SCOPES {
                for (group, resource) in TEMPLATES {
                    for verb in ["create", "update", "patch"] {
                        let expected = attributes(namespace, group, resource, verb);
                        {
                            let mut reviews = reviews.lock().unwrap();
                            reviews.fault = Some((expected.clone(), status.clone()));
                            reviews.calls.clear();
                        }
                        let error = verify(&client, &grant).await.unwrap_err();
                        assert!(error.contains("workload"));
                        assert!(!error.contains("PRIVATE_REVIEW_ERROR"));
                        let reviews = reviews.lock().unwrap();
                        assert_eq!(
                            reviews.calls.last().unwrap()["spec"]["resourceAttributes"],
                            expected
                        );
                    }
                }
            }
        }
        reviews.lock().unwrap().fault = None;
        verify(&client, &grant).await.unwrap();
    }

    #[tokio::test]
    async fn credential_writer_template_review_malformed_allowance_fails_closed() {
        let (_server, client, reviews) = review_fixture().await;
        reviews.lock().unwrap().fault = Some((
            attributes(Some("kars-agent"), "batch", "jobs", "create"),
            json!({"allowed":"PRIVATE_REVIEW_ERROR"}),
        ));
        let error = verify(&client, &template_grant()).await.unwrap_err();
        assert!(!error.contains("PRIVATE_REVIEW_ERROR"));
    }

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
