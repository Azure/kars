// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bridge-owned private memory, with a router-only, exact-object Kubernetes grant.

use crate::{
    builtin_memory::{Identity, Store, digest},
    crd::KarsSandbox,
    kars_task::KarsTask,
    kars_team::KarsTeam,
};
use anyhow::{Context, Result, ensure};
use k8s_openapi::api::{
    authentication::v1::TokenRequest,
    core::v1::{Namespace, Secret, ServiceAccount},
};
use kube::{
    Api, Client, ResourceExt,
    api::{
        ApiResource, DeleteParams, DynamicObject, GroupVersionKind, Patch, PatchParams, PostParams,
        Preconditions,
    },
};
use serde_json::{Value, json};

const ADMIN: &str = "router-services-admin";
const CONFIG: &str = "builtin-memory.json";
const SYSTEM: &str = "kars-memory-system";
const SOURCE: &str = "kars.azure.com/memory-sandbox-uid";

#[derive(Debug, thiserror::Error)]
#[error("private memory credential epoch changed; old token revoked, retry issuance")]
pub(super) struct CredentialEpochChanged;

fn account_name(sandbox: &KarsSandbox) -> Result<String> {
    Ok(format!(
        "kars-memory-{}",
        &digest(sandbox.uid().context("sandbox UID missing")?)[..40]
    ))
}

async fn account_guard(client: &Client) -> Result<()> {
    let namespace =
        tokio::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
            .await?;
    for (kind, expected) in [
        (
            "ValidatingAdmissionPolicy",
            crate::builtin_memory::account_policy(namespace.trim()),
        ),
        (
            "ValidatingAdmissionPolicyBinding",
            json!({"policyName":"kars-memory-accounts","validationActions":["Deny","Audit"]}),
        ),
    ] {
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "admissionregistration.k8s.io",
            "v1",
            kind,
        ));
        let api: Api<DynamicObject> = Api::all_with(client.clone(), &resource);
        let actual = api.get("kars-memory-accounts").await?;
        ensure!(
            actual.metadata.deletion_timestamp.is_none() && actual.data["spec"] == expected,
            "private memory account admission is missing or changed"
        );
        if kind == "ValidatingAdmissionPolicy" {
            ensure!(
                actual.metadata.generation.is_some()
                    && actual.data["status"]["observedGeneration"].as_i64()
                        == actual.metadata.generation
                    && actual.data["status"]["typeChecking"].is_object()
                    && actual.data["status"]["typeChecking"]["expressionWarnings"]
                        .as_array()
                        .is_none_or(|w| w.is_empty()),
                "private memory admission is not ready or has type errors"
            );
        }
    }
    Ok(())
}

/// Revoke the account and its bound-token anchor, not the owner's retained memory.
pub(super) async fn cleanup(client: &Client, sandbox: &KarsSandbox) -> Result<()> {
    let api = Api::<ServiceAccount>::namespaced(client.clone(), SYSTEM);
    let Some(account) = api.get_opt(&account_name(sandbox)?).await? else {
        return Ok(());
    };
    let namespace = Api::<Namespace>::all(client.clone()).get(SYSTEM).await?;
    ensure!(
        account.annotations().get(SOURCE) == sandbox.metadata.uid.as_ref()
            && account.namespace().as_deref() == Some(SYSTEM)
            && account.automount_service_account_token == Some(false)
            && serde_json::to_value(&account.metadata.owner_references)?
                == owner(
                    "Namespace",
                    SYSTEM,
                    &namespace.uid().context("memory namespace UID missing")?,
                    "v1"
                ),
        "foreign memory account preserved during cleanup"
    );
    api.delete(
        &account.name_any(),
        &DeleteParams {
            preconditions: Some(Preconditions {
                uid: account.uid(),
                resource_version: account.resource_version(),
            }),
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

fn owner(kind: &str, name: &str, uid: &str, api: &str) -> Value {
    json!([{"apiVersion":api,"kind":kind,"name":name,"uid":uid,"controller":true,"blockOwnerDeletion":false}])
}

async fn object(
    client: &Client,
    namespace: &str,
    group: &str,
    version: &str,
    kind: &str,
    desired: Value,
) -> Result<DynamicObject> {
    let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), namespace, &resource);
    let name = desired["metadata"]["name"]
        .as_str()
        .context("memory object name missing")?;
    let actual = match api.get_opt(name).await? {
        Some(value) => value,
        None => {
            api.create(
                &PostParams::default(),
                &serde_json::from_value(desired.clone())?,
            )
            .await?
        }
    };
    ensure!(
        actual.namespace().as_deref() == Some(namespace)
            && actual.name_any() == name
            && actual.uid().is_some_and(|s| !s.is_empty())
            && actual.metadata.deletion_timestamp.is_none(),
        "memory object identity changed"
    );
    ensure!(
        serde_json::to_value(&actual.metadata.owner_references)?
            == desired["metadata"]["ownerReferences"],
        "foreign memory object preserved"
    );
    for field in [
        "rules",
        "roleRef",
        "subjects",
        "automountServiceAccountToken",
    ] {
        if let Some(expected) = desired.get(field) {
            ensure!(
                actual.data.get(field) == Some(expected),
                "memory object authority differs; preserved existing object"
            );
        }
    }
    Ok(actual)
}

async fn identity(
    client: &Client,
    sandbox: &KarsSandbox,
    service_identity: &Value,
) -> Result<Identity> {
    let workspace = sandbox.namespace().context("sandbox workspace missing")?;
    let namespace = Api::<Namespace>::all(client.clone())
        .get(&workspace)
        .await?;
    ensure!(
        namespace.metadata.deletion_timestamp.is_none(),
        "memory workspace terminating"
    );
    let mut result = Identity {
        namespace: workspace.clone(),
        namespace_uid: namespace.uid().context("workspace UID missing")?,
        owner_kind: "KarsSandbox".into(),
        owner_name: sandbox.name_any(),
        owner_uid: sandbox.uid().context("sandbox UID missing")?,
        principal: "principal".into(),
    };
    if let Some(name) = service_identity["task"]["name"].as_str() {
        let tasks = Api::<KarsTask>::namespaced(client.clone(), &workspace);
        let task = tasks.get(name).await?;
        ensure!(
            task.metadata.uid.as_deref() == service_identity["task"]["uid"].as_str()
                && task.metadata.deletion_timestamp.is_none(),
            "memory Task identity changed"
        );
        let mut root = task.clone();
        let grant = crate::reviewed_execution::grant(client, &task).await?;
        let reviewed = grant.is_some();
        if let Some(grant) = grant {
            result.principal = "synthesis".into();
            root = tasks
                .get(
                    grant.authority["rootName"]
                        .as_str()
                        .context("reviewed root missing")?,
                )
                .await?;
            ensure!(
                root.metadata.uid.as_deref() == grant.authority["rootUid"].as_str(),
                "memory reviewed root changed"
            );
            let slot = grant.authority["slot"]
                .as_str()
                .context("reviewed slot missing")?;
            if slot != "synthesis" {
                let index: usize = slot
                    .strip_prefix('r')
                    .and_then(|s| s.split_once("-p"))
                    .context("invalid reviewed memory slot")?
                    .0
                    .parse()?;
                result.principal = format!(
                    "role:{}",
                    root.spec
                        .blueprint
                        .as_ref()
                        .and_then(|b| b.execution_plan.as_ref())
                        .and_then(|p| p.roles.get(index))
                        .context("reviewed memory role missing")?
                        .name
                );
            }
        }
        result.owner_kind = "KarsTask".into();
        result.owner_name = root.name_any();
        result.owner_uid = root.uid().context("root Task UID missing")?;
        let owners = root
            .metadata
            .owner_references
            .as_deref()
            .unwrap_or_default();
        if reviewed && owners.iter().any(|o| o.kind == "KarsTeam") {
            ensure!(
                owners.len() == 1
                    && owners[0].kind == "KarsTeam"
                    && owners[0].api_version == "kars.azure.com/v1alpha1"
                    && owners[0].controller == Some(true),
                "invalid Team memory ownership"
            );
            let team = Api::<KarsTeam>::namespaced(client.clone(), &workspace)
                .get(&owners[0].name)
                .await?;
            ensure!(
                team.metadata.uid.as_deref() == Some(owners[0].uid.as_str())
                    && team.metadata.deletion_timestamp.is_none(),
                "Team memory owner changed"
            );
            result.owner_kind = "KarsTeam".into();
            result.owner_name = team.name_any();
            result.owner_uid = team.uid().context("Team UID missing")?;
        }
    }
    result.validate()?;
    Ok(result)
}

/// Called only after the existing private-consumption/credential checks succeed.
pub(super) async fn ensure(
    client: &Client,
    sandbox: &KarsSandbox,
    runtime_namespace: &Namespace,
    service_identity: &Value,
) -> Result<String> {
    super::namespace_ownership::recheck(client, sandbox, runtime_namespace).await?;
    let id = identity(client, sandbox, service_identity).await?;
    let map_name = id.map_name();
    let map = object(client, &id.namespace, "", "v1", "ConfigMap", json!({
        "apiVersion":"v1","kind":"ConfigMap","metadata":{"name":map_name,"namespace":id.namespace,
            "ownerReferences":owner(&id.owner_kind,&id.owner_name,&id.owner_uid,"kars.azure.com/v1alpha1")},
        "data":{"memory.json":serde_json::to_string(&Store::empty(id.clone()))?}
    })).await?;
    let stored: Store = serde_json::from_str(
        map.data["data"]["memory.json"]
            .as_str()
            .context("private memory payload missing")?,
    )?;
    stored.validate(&id)?;

    // No Pod may impersonate these accounts; only the core can request tokens.
    account_guard(client).await?;
    let core = SYSTEM.to_string();
    ensure!(
        core != runtime_namespace.name_any() && core != id.namespace,
        "private memory account requires a separate system namespace"
    );
    let namespaces = Api::<Namespace>::all(client.clone());
    let core_namespace = namespaces.get(&core).await.context(
        "install the protected kars-memory-system namespace before enabling built-in memory",
    )?;
    ensure!(
        core_namespace.metadata.deletion_timestamp.is_none()
            && core_namespace
                .metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("kars.azure.com/component"))
                .map(String::as_str)
                == Some("private-memory"),
        "private memory namespace is not managed"
    );
    let controller_owner = owner(
        "Namespace",
        &core,
        &core_namespace
            .uid()
            .context("memory namespace UID missing")?,
        "v1",
    );
    let account = account_name(sandbox)?;
    let sa = object(client, &core, "", "v1", "ServiceAccount", json!({"apiVersion":"v1","kind":"ServiceAccount",
        "metadata":{"name":account,"namespace":core,"ownerReferences":controller_owner,"annotations":{SOURCE:sandbox.uid()}},"automountServiceAccountToken":false})).await?;
    ensure!(
        sa.annotations().get(SOURCE) == sandbox.metadata.uid.as_ref(),
        "memory account source changed"
    );
    let role_owner = owner(
        &id.owner_kind,
        &id.owner_name,
        &id.owner_uid,
        "kars.azure.com/v1alpha1",
    );
    object(client, &id.namespace, "rbac.authorization.k8s.io", "v1", "Role", json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role",
        "metadata":{"name":map_name,"namespace":id.namespace,"ownerReferences":role_owner},
        "rules":[{"apiGroups":[""],"resources":["configmaps"],"resourceNames":[map_name],"verbs":["get","update"]}]})).await?;
    object(client, &id.namespace, "rbac.authorization.k8s.io", "v1", "RoleBinding", json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding",
        "metadata":{"name":account,"namespace":id.namespace,"ownerReferences":owner("KarsSandbox",&sandbox.name_any(),&sandbox.uid().context("sandbox UID missing")?,"kars.azure.com/v1alpha1")},
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":map_name},
        "subjects":[{"kind":"ServiceAccount","name":account,"namespace":core}]})).await?;

    let secrets = Api::<Secret>::namespaced(client.clone(), &runtime_namespace.name_any());
    let admin = super::governed_services::credentials::existing_secret(
        client,
        sandbox,
        runtime_namespace,
        super::governed_services::credentials::ADMIN,
    )
    .await
    .map_err(anyhow::Error::msg)?
    .context("private memory control credential is not current")?;
    let control = admin
        .data
        .as_ref()
        .and_then(|d| d.get("control-token"))
        .context("private control credential missing")?;
    let epoch = digest(&control.0);
    let anchor = object(client, &core, "", "v1", "Secret", json!({"apiVersion":"v1","kind":"Secret",
        "metadata":{"name":account,"namespace":core,"ownerReferences":owner("ServiceAccount",&account,&sa.uid().context("memory account UID missing")?,"v1")},
        "type":"Opaque","stringData":{"epoch":epoch}})).await?;
    let anchor_api = Api::<Secret>::namespaced(client.clone(), &core);
    let anchor_secret = anchor_api.get(&account).await?;
    ensure_anchor_epoch(&anchor_api, &anchor_secret, &epoch).await?;
    let current: Option<Value> = admin
        .data
        .as_ref()
        .and_then(|d| d.get(CONFIG))
        .and_then(|b| serde_json::from_slice(&b.0).ok());
    if current.as_ref().is_some_and(|v| {
        v["identity"] == json!(id)
            && v["map_uid"] == json!(map.uid())
            && v["anchor_uid"] == json!(anchor.uid())
            && v["epoch"] == epoch
            && v["expires_at"]
                .as_i64()
                .is_some_and(|t| t > chrono::Utc::now().timestamp() + 600)
    }) {
        return crate::sre_registration::control_credential_version(&admin)
            .map_err(anyhow::Error::msg);
    }
    let request: TokenRequest = serde_json::from_value(
        json!({"apiVersion":"authentication.k8s.io/v1","kind":"TokenRequest",
        "spec":{"audiences":[],"expirationSeconds":3600,"boundObjectRef":{"apiVersion":"v1","kind":"Secret","name":account,"uid":anchor.uid()}}}),
    )?;
    let issued = Api::<ServiceAccount>::namespaced(client.clone(), &core)
        .create_token_request(&account, &PostParams::default(), &request)
        .await?
        .status
        .context("memory TokenRequest omitted status")?;
    let token = issued.token.context("memory TokenRequest omitted token")?;
    let expires = issued
        .expiration_timestamp
        .context("memory TokenRequest omitted expiry")?
        .0
        .as_second();
    ensure!(
        !token.is_empty() && expires > chrono::Utc::now().timestamp() + 600,
        "unusable memory credential"
    );
    let host: std::net::IpAddr = std::env::var("KUBERNETES_SERVICE_HOST")?.parse()?;
    let port: u16 = std::env::var("KUBERNETES_SERVICE_PORT_HTTPS")
        .unwrap_or_else(|_| "443".into())
        .parse()?;
    let address = std::net::SocketAddr::new(host, port);
    let ca =
        tokio::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/ca.crt").await?;
    let config = json!({"identity":id,"map_uid":map.uid(),"anchor_uid":anchor.uid(),"epoch":epoch,"api_url":format!("https://{address}"),"ca":ca,"token":token,"expires_at":expires,"sandbox_uid":sandbox.uid()});
    publish_configuration(&secrets, &admin, &config).await
}

async fn ensure_anchor_epoch(anchors: &Api<Secret>, anchor: &Secret, epoch: &str) -> Result<()> {
    if anchor
        .data
        .as_ref()
        .and_then(|d| d.get("epoch"))
        .map(|b| b.0.as_slice())
        == Some(epoch.as_bytes())
    {
        return Ok(());
    }
    anchors
        .delete(
            &anchor.name_any(),
            &DeleteParams {
                preconditions: Some(Preconditions {
                    uid: anchor.uid(),
                    resource_version: anchor.resource_version(),
                }),
                ..Default::default()
            },
        )
        .await?;
    Err(CredentialEpochChanged.into())
}

async fn publish_configuration(
    secrets: &Api<Secret>,
    admin: &Secret,
    config: &Value,
) -> Result<String> {
    // First enrollment changes the control-version format. Stamp the initial Pod
    // with the post-write receipt so SRE does not drain a newly launched runtime.
    let updated = secrets.patch(ADMIN,&PatchParams::default(),&Patch::Merge(json!({"metadata":{"uid":admin.uid(),"resourceVersion":admin.resource_version()},"stringData":{CONFIG:serde_json::to_string(config)?}}))).await?;
    crate::sre_registration::control_credential_version(&updated).map_err(anyhow::Error::msg)
}

pub(super) async fn api_egress(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    rules: &[Value],
) -> Result<()> {
    super::namespace_ownership::recheck(client, sandbox, namespace).await?;
    if super::sre_egress::cilium_installed(client)
        .await
        .map_err(anyhow::Error::msg)?
    {
        let spec =
            super::sre_egress::cilium_spec(&sandbox.name_any(), &namespace.name_any(), rules)
                .map_err(anyhow::Error::msg)?;
        let actual = object(client, &namespace.name_any(), "cilium.io", "v2", "CiliumNetworkPolicy", json!({"apiVersion":"cilium.io/v2","kind":"CiliumNetworkPolicy",
            "metadata":{"name":"kars-memory-api","namespace":namespace.name_any(),"ownerReferences":owner("Namespace",&namespace.name_any(),&namespace.uid().context("runtime namespace UID missing")?,"v1")},"spec":spec})).await?;
        if actual.data["spec"] != spec {
            let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
                "cilium.io",
                "v2",
                "CiliumNetworkPolicy",
            ));
            let api: Api<DynamicObject> =
                Api::namespaced_with(client.clone(), &namespace.name_any(), &resource);
            let updated = api.patch("kars-memory-api", &PatchParams::default(), &Patch::Merge(json!({
                "metadata":{"uid":actual.uid(),"resourceVersion":actual.resource_version()},"spec":spec
            }))).await?;
            ensure!(
                updated.data["spec"] == spec,
                "private memory API policy update was not acknowledged"
            );
        }
    }
    Ok(())
}

pub(super) fn mount(pod: &mut Value) {
    pod["volumes"].as_array_mut().expect("pod volumes").push(json!({"name":"builtin-memory","secret":{"secretName":ADMIN,"items":[{"key":CONFIG,"path":CONFIG}]}}));
    for container in pod["containers"].as_array_mut().expect("containers") {
        if container["name"] == "inference-router" {
            container["volumeMounts"].as_array_mut().expect("router mounts").push(json!({"name":"builtin-memory","mountPath":"/etc/kars/builtin-memory","readOnly":true}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_anchor_current_epoch_needs_no_revocation() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use wiremock::MockServer;

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = MockServer::start().await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let anchors = Api::<Secret>::namespaced(client, SYSTEM);
        let anchor = serde_json::from_value(json!({"metadata":{
            "name":"memory-account","namespace":SYSTEM,"uid":"anchor-uid","resourceVersion":"4"},
            "data":{"epoch":STANDARD.encode("current-epoch")}}))
        .unwrap();
        ensure_anchor_epoch(&anchors, &anchor, "current-epoch")
            .await
            .unwrap();
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn memory_anchor_rotation_retries_only_after_fenced_revocation() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{body_json, method, path},
        };

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        for code in [200, 403, 409] {
            let server = MockServer::start().await;
            let client =
                Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
            let anchors = Api::<Secret>::namespaced(client, SYSTEM);
            let anchor = serde_json::from_value(json!({"metadata":{
                "name":"memory-account","namespace":SYSTEM,"uid":"old-anchor-uid","resourceVersion":"4"},
                "data":{"epoch":STANDARD.encode("old-epoch")}})).unwrap();
            Mock::given(method("DELETE"))
                .and(path("/api/v1/namespaces/kars-memory-system/secrets/memory-account"))
                .and(body_json(json!({"preconditions":{"uid":"old-anchor-uid","resourceVersion":"4"}})))
                .respond_with(ResponseTemplate::new(code).set_body_json(json!({
                    "apiVersion":"v1","kind":"Status","code":code,
                    "status":if code == 200 {"Success"} else {"Failure"},
                    "reason":if code == 200 {"Success"} else if code == 403 {"Forbidden"} else {"Conflict"},
                    "message":"anchor revocation result"})))
                .expect(1)
                .mount(&server)
                .await;
            let error = ensure_anchor_epoch(&anchors, &anchor, "new-epoch")
                .await
                .unwrap_err()
                .context("memory provisioning");
            assert_eq!(error.is::<CredentialEpochChanged>(), code == 200);
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn memory_publication_returns_post_write_version_for_enrollment_and_renewal() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{body_json, method, path},
        };

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        for renewal in [false, true] {
            let server = MockServer::start().await;
            let client =
                Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
            let secrets = Api::<Secret>::namespaced(client, "kars-test");
            let mut before = json!({"apiVersion":"v1","kind":"Secret",
                "metadata":{"name":ADMIN,"namespace":"kars-test","uid":"admin-uid","resourceVersion":"7"},
                "data":{"control-token":STANDARD.encode("private-control"),"config.json":STANDARD.encode("private-tls")}});
            if renewal {
                before["data"][CONFIG] = STANDARD.encode("prior-memory-config").into();
            }
            let admin: Secret = serde_json::from_value(before.clone()).unwrap();
            let config = json!({"token":"new-memory-token"});
            let mut after = before;
            after["metadata"]["resourceVersion"] = "8".into();
            after["data"][CONFIG] = STANDARD.encode(serde_json::to_vec(&config).unwrap()).into();
            let updated: Secret = serde_json::from_value(after.clone()).unwrap();
            Mock::given(method("PATCH"))
                .and(path(
                    "/api/v1/namespaces/kars-test/secrets/router-services-admin",
                ))
                .and(body_json(
                    json!({"metadata":{"uid":"admin-uid","resourceVersion":"7"},
                    "stringData":{CONFIG:serde_json::to_string(&config).unwrap()}}),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(after))
                .expect(1)
                .mount(&server)
                .await;

            let version = publish_configuration(&secrets, &admin, &config)
                .await
                .unwrap();
            assert_eq!(
                version,
                crate::sre_registration::control_credential_version(&updated).unwrap()
            );
            assert!(version.starts_with("admin-uid:control-v1:"));
            assert_eq!(
                version == crate::sre_registration::control_credential_version(&admin).unwrap(),
                renewal
            );
        }
    }

    #[tokio::test]
    async fn memory_publication_conflict_returns_no_control_receipt() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = MockServer::start().await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let secrets = Api::<Secret>::namespaced(client, "kars-test");
        let admin: Secret = serde_json::from_value(json!({"metadata":{
            "name":ADMIN,"namespace":"kars-test","uid":"admin-uid","resourceVersion":"7"}}))
        .unwrap();
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "apiVersion":"v1","kind":"Status","code":409,"status":"Failure",
                "reason":"Conflict","message":"Concurrent credential change"})))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            publish_configuration(&secrets, &admin, &json!({"token":"memory-token"}))
                .await
                .is_err()
        );
    }

    #[test]
    fn builtin_memory_credential_is_mounted_only_in_router() {
        let mut pod = json!({"volumes":[],"containers":[
            {"name":"openclaw","volumeMounts":[]},
            {"name":"hermes","volumeMounts":[]},
            {"name":"inference-router","volumeMounts":[]}]});
        mount(&mut pod);
        assert_eq!(pod["volumes"][0]["secret"]["secretName"], ADMIN);
        assert_eq!(
            pod["volumes"][0]["secret"]["items"],
            json!([{"key":CONFIG,"path":CONFIG}])
        );
        assert_eq!(pod["containers"][0]["volumeMounts"], json!([]));
        assert_eq!(pod["containers"][1]["volumeMounts"], json!([]));
        assert_eq!(pod["containers"][2]["volumeMounts"][0]["readOnly"], true);
    }

    #[test]
    fn builtin_memory_admission_template_matches_fail_closed_contract() {
        let template = include_str!("../../../deploy/helm/kars/templates/memory-namespace.yaml")
            .replace("{{ .Release.Namespace }}", "controller-system");
        let docs: Vec<Value> = serde_saphyr::from_multiple(&template).unwrap();
        let policy = docs
            .iter()
            .find(|d| d["kind"] == "ValidatingAdmissionPolicy")
            .unwrap();
        assert_eq!(
            policy["spec"],
            crate::builtin_memory::account_policy("controller-system")
        );
        let binding = docs
            .iter()
            .find(|d| d["kind"] == "ValidatingAdmissionPolicyBinding")
            .unwrap();
        assert_eq!(
            binding["spec"],
            json!({"policyName":"kars-memory-accounts","validationActions":["Deny","Audit"]})
        );
    }
}
