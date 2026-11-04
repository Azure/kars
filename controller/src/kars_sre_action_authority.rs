// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact-target, per-attempt authority for the maintained SRE action executor.

use anyhow::{Result, anyhow, bail, ensure};
use k8s_openapi::{
    api::{
        authentication::v1::TokenRequest,
        core::v1::{Namespace, ServiceAccount},
        rbac::v1::{Role, RoleBinding},
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    Api, Client, Config, ResourceExt,
    api::{DeleteParams, PostParams, Preconditions},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::{Value, json};

use crate::kars_sre_action::{ActionSpec, KarsSREAction, SREActionExecution};

pub(super) const FINALIZER: &str = "kars.azure.com/sre-action-authority";
const SOURCE_UID: &str = "kars.azure.com/sre-action-uid";
const SOURCE_NAME: &str = "kars.azure.com/sre-action-name";
const SOURCE_NAMESPACE: &str = "kars.azure.com/sre-action-namespace";
const ATTEMPT: &str = "kars.azure.com/sre-action-started-at";
const NAMESPACE_UID: &str = "kars.azure.com/sre-target-namespace-uid";
const TOKEN_TTL_SECONDS: i64 = 600;

pub(super) struct Scope {
    pub group: &'static str,
    pub kind: &'static str,
    pub resource: &'static str,
    pub scale: bool,
    pub verb: &'static str,
}

pub(super) fn scope(action: &ActionSpec) -> Result<Scope> {
    let (group, kind, resource, scale, verb) = match action.kind.as_str() {
        "DeleteResourceQuota" => ("", "ResourceQuota", "resourcequotas", false, "delete"),
        "DeletePod" => ("", "Pod", "pods", false, "delete"),
        "PatchDeploymentImage" => ("apps", "Deployment", "deployments", false, "patch"),
        "ScaleDeployment" => ("apps", "Deployment", "deployments", true, "patch"),
        "RolloutRestart" => match action
            .params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("Deployment")
        {
            "Deployment" => ("apps", "Deployment", "deployments", false, "patch"),
            "StatefulSet" => ("apps", "StatefulSet", "statefulsets", false, "patch"),
            "DaemonSet" => ("apps", "DaemonSet", "daemonsets", false, "patch"),
            _ => bail!("unsupported rollout workload kind"),
        },
        _ => bail!("unsupported action kind"),
    };
    Ok(Scope {
        group,
        kind,
        resource,
        scale,
        verb,
    })
}

pub(super) fn target_api(
    client: &Client,
    action: &ActionSpec,
    namespace: &str,
) -> Result<Api<DynamicObject>> {
    let scope = scope(action)?;
    let mut resource = ApiResource::from_gvk(&GroupVersionKind::gvk(scope.group, "v1", scope.kind));
    resource.plural = scope.resource.into();
    Ok(Api::namespaced_with(client.clone(), namespace, &resource))
}

pub(super) async fn target_metadata(
    client: &Client,
    action: &ActionSpec,
    namespace: &str,
    name: &str,
) -> Result<ObjectMeta> {
    let api = target_api(client, action, namespace)?;
    let metadata = if scope(action)?.scale {
        api.get_scale(name).await?.metadata
    } else {
        api.get(name).await?.metadata
    };
    ensure!(
        metadata.deletion_timestamp.is_none(),
        "target is being deleted"
    );
    ensure!(
        metadata.name.as_deref() == Some(name),
        "target name mismatch"
    );
    uid(&metadata)?;
    ensure!(
        metadata
            .resource_version
            .as_deref()
            .is_some_and(|v| !v.is_empty()),
        "target resourceVersion missing"
    );
    Ok(metadata)
}

pub(super) fn uid(meta: &ObjectMeta) -> Result<&str> {
    meta.uid
        .as_deref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("object UID missing"))
}

pub(super) fn authority_name(cr: &KarsSREAction) -> Result<String> {
    let uid = uid(&cr.metadata)?;
    ensure!(
        uid.len() <= 48
            && uid
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "action UID is not a valid authority suffix"
    );
    Ok(format!("sre-action-{uid}"))
}

pub(super) fn connection_config(original: &Config) -> Config {
    let mut config = original.clone();
    // AuthInfo includes mTLS, basic auth, token files, exec/auth-provider and
    // impersonation. Arbitrary headers can also carry credentials: retain none.
    config.auth_info = Default::default();
    config.headers.clear();
    config
}

fn metadata(cr: &KarsSREAction, execution: &SREActionExecution) -> Value {
    json!({
        "name": execution.authority_name, "namespace": execution.namespace,
        "labels": {"app.kubernetes.io/managed-by":"kars-controller", SOURCE_UID: cr.metadata.uid},
        "annotations": {SOURCE_NAME:cr.name_any(), SOURCE_NAMESPACE:cr.namespace(),
            ATTEMPT:execution.started_at, NAMESPACE_UID:execution.namespace_uid}
    })
}

fn owned(meta: &ObjectMeta, cr: &KarsSREAction, execution: &SREActionExecution) -> bool {
    let expected: ObjectMeta =
        serde_json::from_value(metadata(cr, execution)).expect("authority metadata");
    meta.name == expected.name
        && meta.namespace == expected.namespace
        && expected.labels.as_ref().is_some_and(|labels| {
            labels
                .iter()
                .all(|(k, v)| meta.labels.as_ref().and_then(|m| m.get(k)) == Some(v))
        })
        && expected.annotations.as_ref().is_some_and(|annotations| {
            annotations
                .iter()
                .all(|(k, v)| meta.annotations.as_ref().and_then(|m| m.get(k)) == Some(v))
        })
}

pub(super) async fn namespace_uid(client: &Client, namespace: &str) -> Result<String> {
    let live = Api::<Namespace>::all(client.clone()).get(namespace).await?;
    ensure!(
        live.metadata.deletion_timestamp.is_none(),
        "target namespace is being deleted"
    );
    Ok(uid(&live.metadata)?.into())
}

async fn check_namespace(client: &Client, execution: &SREActionExecution) -> Result<()> {
    ensure!(
        namespace_uid(client, &execution.namespace).await? == execution.namespace_uid,
        "target namespace was replaced"
    );
    Ok(())
}

pub(super) fn role(cr: &KarsSREAction, execution: &SREActionExecution) -> Result<Role> {
    let scope = scope(&cr.spec.action)?;
    let resource = if scope.scale {
        format!("{}/scale", scope.resource)
    } else {
        scope.resource.into()
    };
    Ok(serde_json::from_value(json!({
        "apiVersion":"rbac.authorization.k8s.io/v1", "kind":"Role",
        "metadata":metadata(cr, execution),
        "rules":[{"apiGroups":[scope.group],"resources":[resource],
            "resourceNames":[cr.spec.action.params["name"]],"verbs":["get",scope.verb]}]
    }))?)
}

pub(super) async fn writer(
    client: &Client,
    connection: &Config,
    cr: &KarsSREAction,
    execution: &SREActionExecution,
) -> Result<Client> {
    check_namespace(client, execution).await?;
    ensure!(
        authority_name(cr)? == execution.authority_name,
        "authority name mismatch"
    );
    let accounts = Api::<ServiceAccount>::namespaced(client.clone(), &execution.namespace);
    let account: ServiceAccount = serde_json::from_value(json!({
        "apiVersion":"v1","kind":"ServiceAccount", "metadata":metadata(cr, execution),
        "automountServiceAccountToken":false
    }))?;
    // CREATE only: a conflict is never permission to overwrite/adopt an identity.
    let account = accounts.create(&PostParams::default(), &account).await?;
    let account_uid = uid(&account.metadata)?.to_owned();
    Api::<Role>::namespaced(client.clone(), &execution.namespace)
        .create(&PostParams::default(), &role(cr, execution)?)
        .await?;
    let binding: RoleBinding = serde_json::from_value(json!({
        "apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding",
        "metadata":metadata(cr, execution),
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":execution.authority_name},
        "subjects":[{"kind":"ServiceAccount","name":execution.authority_name,"namespace":execution.namespace}]
    }))?;
    Api::<RoleBinding>::namespaced(client.clone(), &execution.namespace)
        .create(&PostParams::default(), &binding)
        .await?;
    check_namespace(client, execution).await?;
    let live = accounts.get(&execution.authority_name).await?;
    ensure!(
        uid(&live.metadata)? == account_uid
            && owned(&live.metadata, cr, execution)
            && live.metadata.deletion_timestamp.is_none(),
        "writer identity changed"
    );
    let request: TokenRequest = serde_json::from_value(json!({
        "apiVersion":"authentication.k8s.io/v1","kind":"TokenRequest",
        "spec":{"audiences":[],"expirationSeconds":TOKEN_TTL_SECONDS}
    }))?;
    // Kubernetes intrinsically binds this token to the issuing SA's UID. It is
    // NOT bound to the unrelated, stable SRE Pod or permissionless sre-writer SA.
    let response = accounts
        .create_token_request(&execution.authority_name, &PostParams::default(), &request)
        .await
        .map_err(|_| anyhow!("writer TokenRequest failed"))?;
    let status = response
        .status
        .ok_or_else(|| anyhow!("writer TokenRequest omitted status"))?;
    let expires = status.expiration_timestamp.0.as_second();
    let now = chrono::Utc::now().timestamp();
    ensure!(
        !status.token.trim().is_empty()
            && expires > now + 30
            && expires <= now + TOKEN_TTL_SECONDS + 30,
        "writer TokenRequest returned unusable token or excessive expiry"
    );
    // Recheck the issuing UID after minting; never use a replacement SA's token.
    let live = accounts.get(&execution.authority_name).await?;
    ensure!(
        uid(&live.metadata)? == account_uid
            && owned(&live.metadata, cr, execution)
            && live.metadata.deletion_timestamp.is_none(),
        "writer identity changed during TokenRequest"
    );
    let mut config = connection_config(connection);
    config.auth_info.token = Some(status.token.into());
    Client::try_from(config).map_err(|_| anyhow!("could not construct isolated writer client"))
}

async fn delete_owned<K>(
    api: Api<K>,
    cr: &KarsSREAction,
    execution: &SREActionExecution,
) -> Result<()>
where
    K: Clone + std::fmt::Debug + serde::de::DeserializeOwned + kube::Resource<DynamicType = ()>,
{
    let Some(object) = api.get_opt(&execution.authority_name).await? else {
        return Ok(());
    };
    ensure!(
        owned(object.meta(), cr, execution),
        "foreign authority object {}; refusing deletion",
        execution.authority_name
    );
    let params = DeleteParams {
        preconditions: Some(Preconditions {
            uid: Some(uid(object.meta())?.into()),
            resource_version: object.meta().resource_version.clone(),
        }),
        ..Default::default()
    };
    match api.delete(&execution.authority_name, &params).await {
        Ok(_) => {}
        Err(kube::Error::Api(e)) if e.code == 404 => {}
        Err(e) => return Err(e.into()),
    }
    // An accepted DELETE is not proof of revocation (e.g. a foreign finalizer).
    ensure!(
        api.get_opt(&execution.authority_name).await?.is_none(),
        "authority deletion still pending: {}",
        execution.authority_name
    );
    Ok(())
}

pub(super) async fn cleanup(
    client: &Client,
    cr: &KarsSREAction,
    execution: &SREActionExecution,
) -> Result<()> {
    // A disappeared/recreated namespace has already destroyed our objects. Do
    // not touch any same-named objects in the new namespace incarnation.
    let namespace = Api::<Namespace>::all(client.clone())
        .get_opt(&execution.namespace)
        .await?;
    if namespace
        .as_ref()
        .is_none_or(|ns| ns.metadata.uid.as_deref() != Some(execution.namespace_uid.as_str()))
    {
        return Ok(());
    }
    ensure!(
        authority_name(cr)? == execution.authority_name,
        "authority name mismatch during cleanup"
    );
    // Attempt every revocation even if another fails; persist all failures.
    let binding = delete_owned(
        Api::<RoleBinding>::namespaced(client.clone(), &execution.namespace),
        cr,
        execution,
    )
    .await;
    let account = delete_owned(
        Api::<ServiceAccount>::namespaced(client.clone(), &execution.namespace),
        cr,
        execution,
    )
    .await;
    let role = delete_owned(
        Api::<Role>::namespaced(client.clone(), &execution.namespace),
        cr,
        execution,
    )
    .await;
    let failures: Vec<_> = [binding, account, role]
        .into_iter()
        .filter_map(Result::err)
        .map(|e| e.to_string())
        .collect();
    ensure!(
        failures.is_empty(),
        "authority cleanup failed: {}",
        failures.join("; ")
    );
    Ok(())
}
