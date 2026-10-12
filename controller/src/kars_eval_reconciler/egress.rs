// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact-Pod, proxy-port-only grants for egress evaluations.

use anyhow::{Context, ensure};
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet},
    core::v1::{Namespace, Pod},
};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, ListParams, PostParams, Preconditions},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde::Serialize;
use serde_json::{Value, json};

use super::workloads;
use crate::{
    crd::KarsSandbox, kars_eval::KarsEval, providers::signing::content_digest,
    reconciler::namespace_ownership,
};

const LABEL: &str = "kars.azure.com/eval-proxy-owner";
const SOURCE: &str = "kars.azure.com/eval-proxy-source";
const POD_UID: &str = "kars.azure.com/eval-proxy-pod-uid";
const ADMISSION: &str = include_str!("../../../deploy/helm/kars/files/eval-proxy-admission.json");

#[derive(Clone, Serialize)]
pub(super) struct Target {
    namespace: String,
    namespace_uid: String,
    pod: String,
    uid: String,
    deployment_uid: String,
    service_account: String,
}

fn key(parts: &[&str]) -> String {
    let digest = content_digest(&serde_json::to_vec(parts).expect("serialize string slice"));
    digest[7..31].into()
}

fn api(client: &Client, version: &str, kind: &str, namespace: Option<&str>) -> Api<DynamicObject> {
    let (group, version) = version.split_once('/').unwrap_or(("", version));
    let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
    match namespace {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, &resource),
        None => Api::all_with(client.clone(), &resource),
    }
}

async fn require_admission(client: &Client) -> anyhow::Result<()> {
    let contract: Value = serde_json::from_str(ADMISSION)?;
    for entry in ["policy", "binding"] {
        let desired = &contract[entry];
        let current = api(
            client,
            "admissionregistration.k8s.io/v1",
            desired["kind"].as_str().context("admission kind")?,
            None,
        )
        .get("kars-eval-proxy-port")
        .await
        .context("install the maintained evaluation proxy admission policy and binding")?;
        ensure!(
            current.metadata.deletion_timestamp.is_none()
                && current.data["spec"] == desired["spec"],
            "evaluation proxy admission contract differs from the maintained policy"
        );
        if entry == "policy" {
            ensure!(
                current.data["status"]["observedGeneration"].as_i64()
                    == current.metadata.generation
                    && current.metadata.generation.is_some()
                    && current.data["status"]["typeChecking"].is_object()
                    && current.data["status"]["typeChecking"]["expressionWarnings"]
                        .as_array()
                        .is_none_or(Vec::is_empty),
                "evaluation proxy admission policy is not type-checked at its current generation"
            );
        }
    }
    Ok(())
}

fn ready(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod.status.as_ref().is_some_and(|status| {
            status.phase.as_deref() == Some("Running")
                && status
                    .container_statuses
                    .as_ref()
                    .is_some_and(|containers| {
                        containers
                            .iter()
                            .any(|c| c.name == "inference-router" && c.ready)
                    })
        })
}

impl Target {
    pub async fn resolve(client: &Client, eval: &KarsEval) -> anyhow::Result<Self> {
        let source_namespace = workloads::namespace(eval)?;
        let name = &eval.spec.target_sandbox_ref.name;
        let sandbox = Api::<KarsSandbox>::namespaced(client.clone(), source_namespace)
            .get(name)
            .await?;
        let namespace = format!("kars-{name}");
        let ns = Api::<Namespace>::all(client.clone())
            .get(&namespace)
            .await?;
        ensure!(
            sandbox.metadata.deletion_timestamp.is_none()
                && ns.metadata.deletion_timestamp.is_none()
                && namespace_ownership::claimed(&ns, &sandbox)?,
            "evaluation target has no current namespace claim"
        );
        let deployment = Api::<Deployment>::namespaced(client.clone(), &namespace)
            .get(name)
            .await?;
        ensure!(
            deployment.metadata.deletion_timestamp.is_none(),
            "target Deployment is terminating"
        );
        let deployment_uid = workloads::identity(&deployment.metadata)?.0.to_owned();
        let pods = Api::<Pod>::namespaced(client.clone(), &namespace)
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={name}")))
            .await?;
        let mut candidates = Vec::new();
        for pod in pods.items.into_iter().filter(ready) {
            let Some(owners) = pod.metadata.owner_references.as_ref() else {
                continue;
            };
            if owners.len() != 1
                || owners[0].kind != "ReplicaSet"
                || owners[0].api_version != "apps/v1"
                || owners[0].controller != Some(true)
            {
                continue;
            }
            let rs = Api::<ReplicaSet>::namespaced(client.clone(), &namespace)
                .get(&owners[0].name)
                .await?;
            if rs.metadata.uid.as_deref() == Some(&owners[0].uid)
                && rs.metadata.deletion_timestamp.is_none()
                && workloads::owned_by(
                    &rs.metadata,
                    &namespace,
                    &deployment_uid,
                    name,
                    "Deployment",
                    "apps/v1",
                )
            {
                candidates.push(pod);
            }
        }
        ensure!(
            candidates.len() == 1,
            "egress evaluation requires exactly one ready, Deployment-owned router Pod"
        );
        let pod = candidates.pop().context("target Pod unavailable")?;
        let eval_uid = workloads::identity(&eval.metadata)?.0;
        Ok(Self {
            namespace,
            namespace_uid: workloads::identity(&ns.metadata)?.0.into(),
            pod: pod.name_any(),
            uid: workloads::identity(&pod.metadata)?.0.into(),
            deployment_uid,
            service_account: format!("karseval-proxy-{}", key(&[source_namespace, eval_uid])),
        })
    }

    pub async fn validate(&self, client: &Client, eval: &KarsEval) -> anyhow::Result<()> {
        let current = Self::resolve(client, eval).await?;
        ensure!(
            serde_json::to_value(current)? == serde_json::to_value(self)?,
            "evaluation target Pod or namespace incarnation changed"
        );
        require_admission(client).await
    }

    pub fn configure(&self, spec: &mut Value) -> anyhow::Result<()> {
        spec["serviceAccountName"] = json!(self.service_account);
        spec["automountServiceAccountToken"] = json!(true);
        let args = spec["containers"][0]["args"]
            .as_array_mut()
            .context("runner arguments")?;
        args.extend([
            json!("--forward-proxy-pod"),
            json!(format!("{}/{}", self.namespace, self.pod)),
            json!("--forward-proxy-pod-uid"),
            json!(self.uid),
        ]);
        Ok(())
    }

    pub async fn grant(&self, client: &Client, eval: &KarsEval) -> anyhow::Result<()> {
        self.validate(client, eval).await?;
        let ns = workloads::namespace(eval)?;
        let eval_uid = workloads::identity(&eval.metadata)?.0;
        let label = key(&[ns, eval_uid]);
        let source = format!("{ns}/{}/{eval_uid}", eval.name_any());
        let grant = format!("karseval-proxy-{}", key(&[ns, eval_uid, &self.uid]));
        let metadata = json!({
            "name": grant, "namespace": self.namespace,
            "labels": { LABEL: label },
            "annotations": { SOURCE: source, POD_UID: self.uid },
            "ownerReferences": [{
                "apiVersion": "v1", "kind": "Pod", "name": self.pod,
                "uid": self.uid, "controller": true, "blockOwnerDeletion": false
            }]
        });
        ensure_resource(
            client,
            json!({
                "apiVersion": "v1", "kind": "ServiceAccount",
                "metadata": {
                    "name": self.service_account, "namespace": ns,
                    "ownerReferences": workloads::owner(eval)?
                },
                "automountServiceAccountToken": false
            }),
        )
        .await?;
        ensure_resource(client, json!({
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
            "metadata": metadata,
            "rules": [
                {"apiGroups": [""], "resources": ["pods"], "resourceNames": [self.pod], "verbs": ["get"]},
                {"apiGroups": [""], "resources": ["pods/portforward"], "resourceNames": [self.pod], "verbs": ["get", "create"]}
            ]
        })).await?;
        ensure_resource(client, json!({
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
            "metadata": metadata,
            "subjects": [{"kind": "ServiceAccount", "name": self.service_account, "namespace": ns}],
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": grant}
        })).await?;
        revoke(client, eval, Some(&grant)).await
    }
}

async fn ensure_resource(client: &Client, value: Value) -> anyhow::Result<()> {
    let desired: DynamicObject = serde_json::from_value(value)?;
    let types = desired.types.as_ref().context("grant type")?;
    let api = api(
        client,
        &types.api_version,
        &types.kind,
        desired.namespace().as_deref(),
    );
    if let Some(current) = api.get_opt(&desired.name_any()).await? {
        workloads::identity(&current.metadata)?;
        ensure!(
            current.metadata.deletion_timestamp.is_none()
                && current.metadata.owner_references == desired.metadata.owner_references
                && desired
                    .data
                    .as_object()
                    .context("grant fields")?
                    .iter()
                    .all(|(key, value)| current.data.get(key) == Some(value))
                && desired
                    .annotations()
                    .iter()
                    .all(|(key, value)| current.annotations().get(key) == Some(value))
                && desired
                    .labels()
                    .iter()
                    .all(|(key, value)| current.labels().get(key) == Some(value)),
            "existing evaluation proxy grant is foreign or changed"
        );
    } else {
        api.create(&PostParams::default(), &desired).await?;
    }
    Ok(())
}

pub(super) async fn revoke(
    client: &Client,
    eval: &KarsEval,
    keep: Option<&str>,
) -> anyhow::Result<()> {
    let ns = workloads::namespace(eval)?;
    let uid = workloads::identity(&eval.metadata)?.0;
    let source = format!("{ns}/{}/{uid}", eval.name_any());
    let selector = format!("{LABEL}={}", key(&[ns, uid]));
    for kind in ["RoleBinding", "Role"] {
        for grant in api(client, "rbac.authorization.k8s.io/v1", kind, None)
            .list(&ListParams::default().labels(&selector))
            .await?
            .items
        {
            if keep == Some(grant.name_any().as_str()) {
                continue;
            }
            let owners = grant
                .metadata
                .owner_references
                .as_ref()
                .context("grant Pod owner missing")?;
            ensure!(
                grant.annotations().get(SOURCE) == Some(&source)
                    && owners.len() == 1
                    && owners[0].kind == "Pod"
                    && owners[0].api_version == "v1"
                    && owners[0].controller == Some(true)
                    && grant.annotations().get(POD_UID) == Some(&owners[0].uid)
                    && grant.name_any()
                        == format!("karseval-proxy-{}", key(&[ns, uid, &owners[0].uid])),
                "refusing to delete a foreign evaluation proxy grant"
            );
            let (uid, rv) = workloads::identity(&grant.metadata)?;
            let result = api(
                client,
                "rbac.authorization.k8s.io/v1",
                kind,
                Some(
                    grant
                        .metadata
                        .namespace
                        .as_deref()
                        .context("grant namespace missing")?,
                ),
            )
            .delete(
                &grant.name_any(),
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: Some(uid.into()),
                        resource_version: Some(rv.into()),
                    }),
                    ..Default::default()
                },
            )
            .await;
            match result {
                Ok(_) => {}
                Err(kube::Error::Api(error)) if error.code == 404 => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[test]
    fn pod_binding_only_changes_the_proxy_transport_and_service_account() {
        let target = Target {
            namespace: "kars-demo".into(),
            namespace_uid: "namespace-uid".into(),
            pod: "demo-pod".into(),
            uid: "pod-uid".into(),
            deployment_uid: "deployment-uid".into(),
            service_account: format!("karseval-proxy-{}", key(&["tenant", "eval-uid"])),
        };
        let original = super::super::runner::runner_pod_spec_json(
            "eval",
            "corpus",
            "runner:latest",
            "http://demo:8443",
            "",
        );
        let mut bound = original.clone();
        target.configure(&mut bound).unwrap();
        assert_eq!(original["automountServiceAccountToken"], false);
        assert_eq!(bound["automountServiceAccountToken"], true);
        assert_eq!(bound["serviceAccountName"], target.service_account);
        assert_eq!(
            bound["containers"][0]["args"],
            json!([
                "--corpus",
                "/etc/kars/eval-corpus/corpus.json",
                "--router-base",
                "http://demo:8443",
                "--output",
                "/dev/stdout",
                "--forward-proxy-pod",
                "kars-demo/demo-pod",
                "--forward-proxy-pod-uid",
                "pod-uid",
            ])
        );
        assert_eq!(
            bound["containers"][0]["securityContext"],
            original["containers"][0]["securityContext"]
        );
        assert_eq!(bound["volumes"], original["volumes"]);
        assert_ne!(
            key(&["tenant", "eval-uid", "pod-uid"]),
            key(&["tenant", "eval-uid", "replacement"])
        );
    }

    #[tokio::test]
    async fn an_existing_grant_cannot_gain_additional_verbs_or_foreign_owners() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let desired = json!({
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
            "metadata": {
                "name": "grant", "namespace": "target",
                "ownerReferences": [{
                    "apiVersion": "v1", "kind": "Pod", "name": "router",
                    "uid": "pod-uid", "controller": true, "blockOwnerDeletion": false
                }]
            },
            "rules": [{"apiGroups": [""], "resources": ["pods"], "resourceNames": ["router"], "verbs": ["get"]}]
        });
        for mutation in ["none", "verbs", "owner"] {
            let server = MockServer::start().await;
            let mut current = desired.clone();
            current["metadata"]["uid"] = json!("grant-uid");
            current["metadata"]["resourceVersion"] = json!("1");
            match mutation {
                "verbs" => current["rules"][0]["verbs"] = json!(["get", "list"]),
                "owner" => current["metadata"]["ownerReferences"][0]["uid"] = json!("foreign"),
                _ => {}
            }
            Mock::given(method("GET"))
                .and(path(
                    "/apis/rbac.authorization.k8s.io/v1/namespaces/target/roles/grant",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(current))
                .mount(&server)
                .await;
            let client =
                Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
            assert_eq!(
                ensure_resource(&client, desired.clone()).await.is_ok(),
                mutation == "none"
            );
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn missing_or_relaxed_admission_never_authorizes_a_grant() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let contract: Value = serde_json::from_str(ADMISSION).unwrap();
        for mutation in ["none", "missing-binding", "relaxed", "unchecked"] {
            let server = MockServer::start().await;
            let mut policy = contract["policy"].clone();
            policy["metadata"]["generation"] = json!(1);
            policy["status"] = json!({"observedGeneration": 1, "typeChecking": {}});
            if mutation == "relaxed" {
                policy["spec"]["validations"][0]["expression"] = json!("true");
            }
            if mutation == "unchecked" {
                policy["status"]["observedGeneration"] = json!(0);
            }
            Mock::given(method("GET"))
                .and(path("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/kars-eval-proxy-port"))
                .respond_with(ResponseTemplate::new(200).set_body_json(policy))
                .mount(&server).await;
            if mutation != "missing-binding" {
                Mock::given(method("GET"))
                    .and(path("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings/kars-eval-proxy-port"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&contract["binding"]))
                    .mount(&server).await;
            }
            let client =
                Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
            assert_eq!(
                require_admission(&client).await.is_ok(),
                mutation == "none",
                "{mutation}"
            );
        }
    }
}
