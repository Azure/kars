// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Controller-owned identity projection and current-workload mission bindings.

mod admission;
mod binding;
mod identity;
mod roots;
#[cfg(test)]
mod tests;
mod workload;

use crate::{
    crd::{KarsSandbox, RuntimeKind},
    kars_task::KarsTask,
    reconciler::namespace_ownership,
};
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{EnvVar, EnvVarSource, Namespace, ObjectFieldSelector, SecretKeySelector},
    },
    apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference},
};
use kube::{Api, Client, ResourceExt};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("Mission delivery: {0}")]
    Invalid(&'static str),
    #[error("Mission delivery pending: {0}")]
    NotReady(&'static str),
    #[error("Mission delivery: {stage} (Kubernetes status {status})")]
    Api { stage: &'static str, status: u16 },
}
type Result<T> = std::result::Result<T, Error>;
fn api_error(stage: &'static str, error: kube::Error) -> Error {
    Error::Api {
        stage,
        status: match error {
            kube::Error::Api(response) => response.code,
            _ => 0,
        },
    }
}
fn annotation<'a>(meta: &'a ObjectMeta, key: &str) -> Option<&'a str> {
    meta.annotations.as_ref()?.get(key).map(String::as_str)
}
fn identity(meta: &ObjectMeta) -> Result<(&str, &str)> {
    match (meta.uid.as_deref(), meta.resource_version.as_deref()) {
        (Some(uid), Some(rv))
            if !uid.is_empty() && !rv.is_empty() && meta.deletion_timestamp.is_none() =>
        {
            Ok((uid, rv))
        }
        _ => Err(Error::Invalid("missing or deleting API identity")),
    }
}
fn same_snapshot(prior: &ObjectMeta, current: &ObjectMeta) -> Result<()> {
    let (prior_uid, prior_rv) = identity(prior)?;
    let (current_uid, current_rv) = identity(current)?;
    if prior_uid != current_uid
        || prior.name != current.name
        || prior.namespace != current.namespace
    {
        return Err(Error::Invalid(
            "resource identity changed during mission preparation",
        ));
    }
    if prior_rv != current_rv {
        return Err(Error::NotReady(
            "resource version changed during mission preparation",
        ));
    }
    Ok(())
}
fn owner(meta: &ObjectMeta, api: &str, kind: &str, name: &str, uid: &str) -> bool {
    let owners: Vec<_> = meta
        .owner_references
        .iter()
        .flatten()
        .filter(|o| o.controller == Some(true))
        .collect();
    owners.len() == 1
        && owners[0].api_version == api
        && owners[0].kind == kind
        && owners[0].name == name
        && owners[0].uid == uid
}
fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}

pub(crate) struct Config {
    namespace: String,
    deployment: String,
    release: String,
}
impl Config {
    pub(crate) fn from_env() -> Result<Option<Self>> {
        Self::read(|key| std::env::var(key).ok())
    }
    fn read(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>> {
        match get("KARS_MISSION_DISPATCH_ENABLED").as_deref() {
            None | Some("false") => return Ok(None),
            Some("true") => {}
            _ => {
                return Err(Error::Invalid(
                    "mission dispatch gate must be true or false",
                ));
            }
        }
        let namespace =
            get("POD_NAMESPACE").ok_or(Error::Invalid("missing controller namespace"))?;
        let deployment = get("KARS_MISSION_DISPATCHER_DEPLOYMENT")
            .ok_or(Error::Invalid("missing dispatcher deployment"))?;
        let release = get("KARS_MISSION_DISPATCHER_RELEASE")
            .ok_or(Error::Invalid("missing dispatcher Helm release"))?;
        if ![&namespace, &deployment, &release]
            .into_iter()
            .all(|v| dns_label(v))
        {
            return Err(Error::Invalid("invalid dispatcher configuration"));
        }
        Ok(Some(Self {
            namespace,
            deployment,
            release,
        }))
    }
    fn validate_dispatcher(&self, deployment: &Deployment) -> Result<()> {
        identity(&deployment.metadata)?;
        let labels = deployment.metadata.labels.as_ref();
        let label = |key| labels.and_then(|l| l.get(key)).map(String::as_str);
        if deployment.namespace().as_deref() != Some(&self.namespace)
            || deployment.name_any() != self.deployment
            || deployment
                .metadata
                .owner_references
                .as_ref()
                .is_some_and(|o| !o.is_empty())
            || label("app.kubernetes.io/managed-by") != Some("Helm")
            || label("app.kubernetes.io/component") != Some("mission-dispatcher")
            || annotation(&deployment.metadata, "meta.helm.sh/release-name") != Some(&self.release)
            || annotation(&deployment.metadata, "meta.helm.sh/release-namespace")
                != Some(&self.namespace)
        {
            return Err(Error::Invalid(
                "dispatcher is outside configured Core release custody",
            ));
        }
        let spec = deployment
            .spec
            .as_ref()
            .ok_or(Error::Invalid("missing dispatcher specification"))?;
        let pod = spec
            .template
            .spec
            .as_ref()
            .ok_or(Error::Invalid("missing dispatcher pod specification"))?;
        let containers: Vec<_> = pod
            .containers
            .iter()
            .filter(|c| c.name == "mission-dispatcher")
            .collect();
        if spec.replicas != Some(1)
            || spec.paused == Some(true)
            || spec.strategy.as_ref().and_then(|s| s.type_.as_deref()) != Some("Recreate")
            || containers.len() != 1
            || pod.containers.len() != 1
        {
            return Err(Error::Invalid("dispatcher must have one Recreate replica"));
        }
        let env: Vec<_> = containers[0]
            .env
            .iter()
            .flatten()
            .filter(|e| e.name == "KARS_MISSION_IDENTITY_ROOT")
            .collect();
        let expected_source = EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: roots::DISPATCHER_SECRET.into(),
                key: roots::ROOT_KEY.into(),
                optional: Some(false),
            }),
            ..Default::default()
        };
        if env.len() != 1
            || env[0].value.is_some()
            || env[0].value_from.as_ref() != Some(&expected_source)
        {
            return Err(Error::Invalid(
                "dispatcher requires its dedicated identity Secret",
            ));
        }
        Ok(())
    }
}

pub(crate) struct Prepared {
    task: KarsTask,
    sandbox: KarsSandbox,
    namespace: Namespace,
    dispatcher: workload::Workload,
    dispatcher_namespace: Namespace,
    dispatcher_did: String,
    runtime_root: roots::Root,
}

async fn task_for(client: &Client, sandbox: &KarsSandbox) -> Result<Option<KarsTask>> {
    let controllers: Vec<_> = sandbox
        .metadata
        .owner_references
        .iter()
        .flatten()
        .filter(|o| o.controller == Some(true))
        .collect();
    if controllers.is_empty() {
        return Ok(None);
    }
    if controllers.len() != 1
        || controllers[0].api_version != "kars.azure.com/v1alpha1"
        || controllers[0].kind != "KarsTask"
    {
        return Err(Error::Invalid(
            "mission sandbox must have one exact Task owner",
        ));
    }
    let namespace = sandbox
        .namespace()
        .ok_or(Error::Invalid("missing sandbox namespace"))?;
    let task = Api::<KarsTask>::namespaced(client.clone(), &namespace)
        .get(&controllers[0].name)
        .await
        .map_err(|e| api_error("read mission Task", e))?;
    let (uid, _) = identity(&task.metadata)?;
    if !owner(
        &sandbox.metadata,
        "kars.azure.com/v1alpha1",
        "KarsTask",
        &task.name_any(),
        uid,
    ) {
        return Err(Error::Invalid("mission Task owner changed"));
    }
    Ok(Some(task))
}
fn executable(task: &KarsTask, sandbox: &KarsSandbox) -> bool {
    crate::kars_task_reconciler::task_is_ready(task)
        && task.spec.execution.as_ref().is_some_and(|e| e.launch)
        && sandbox.spec.suspended != Some(true)
        && annotation(
            &sandbox.metadata,
            "kars.azure.com/credential-rebind-task-uid",
        )
        .is_none()
}
fn task_owner(task: &KarsTask) -> Result<OwnerReference> {
    Ok(OwnerReference {
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsTask".into(),
        name: task.name_any(),
        uid: identity(&task.metadata)?.0.into(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    })
}

/// Runs inside the existing namespace lock and controller leader lifecycle.
/// No Secret API is touched until the fresh Task and namespace claims authorize it.
pub(crate) async fn prepare(
    client: &Client,
    sandbox: &KarsSandbox,
    desired: &mut Deployment,
) -> Result<Option<Prepared>> {
    let Some(config) = Config::from_env()? else {
        return Ok(None);
    };
    prepare_with(client, sandbox, desired, &config).await
}
async fn prepare_with(
    client: &Client,
    sandbox: &KarsSandbox,
    desired: &mut Deployment,
    config: &Config,
) -> Result<Option<Prepared>> {
    let workspace = sandbox
        .namespace()
        .ok_or(Error::Invalid("missing sandbox namespace"))?;
    let sandboxes = Api::<KarsSandbox>::namespaced(client.clone(), &workspace);
    let fresh = sandboxes
        .get(&sandbox.name_any())
        .await
        .map_err(|e| api_error("read mission sandbox", e))?;
    same_snapshot(&sandbox.metadata, &fresh.metadata)?;
    let Some(task) = task_for(client, &fresh).await? else {
        return Ok(None);
    };
    if !executable(&task, &fresh) || desired.spec.as_ref().and_then(|s| s.replicas) == Some(0) {
        return Ok(None);
    }
    if fresh.spec.runtime.kind != RuntimeKind::OpenClaw {
        return Err(Error::Invalid(
            "encrypted mission execution requires OpenClaw",
        ));
    }
    let namespace_name = desired
        .namespace()
        .ok_or(Error::Invalid("missing runtime namespace"))?;
    let namespace = Api::<Namespace>::all(client.clone())
        .get(&namespace_name)
        .await
        .map_err(|e| api_error("read mission namespace", e))?;
    let namespace = namespace_ownership::recheck(client, &fresh, &namespace)
        .await
        .map_err(|_| Error::Invalid("mission runtime namespace is not owned"))?;
    if annotation(&fresh.metadata, namespace_ownership::NAMESPACE_UID)
        != namespace.metadata.uid.as_deref()
    {
        return Err(Error::Invalid(
            "mission runtime namespace backlink is missing",
        ));
    }
    let helper_api = Api::<Deployment>::namespaced(client.clone(), &config.namespace);
    let helper = helper_api
        .get(&config.deployment)
        .await
        .map_err(|e| api_error("read dispatcher deployment", e))?;
    config.validate_dispatcher(&helper)?;
    let helper_namespace = Api::<Namespace>::all(client.clone())
        .get(&config.namespace)
        .await
        .map_err(|e| api_error("read dispatcher namespace", e))?;
    let helper_uid = helper.metadata.uid.clone();
    let helper_generation = helper.metadata.generation;
    let helper_root = roots::ensure(
        client,
        roots::Anchor::Dispatcher(&helper),
        &helper_namespace,
    )
    .await?;
    let helper = helper_api
        .get(&config.deployment)
        .await
        .map_err(|e| api_error("reread dispatcher deployment", e))?;
    config.validate_dispatcher(&helper)?;
    same_snapshot(&helper_root.anchor, &helper.metadata)?;
    if helper.metadata.uid != helper_uid || helper.metadata.generation != helper_generation {
        return Err(Error::Invalid("dispatcher identity changed"));
    }
    if annotation(&helper.metadata, roots::PIN) != Some(&helper_root.uid) {
        return Err(Error::Invalid("dispatcher root pin changed"));
    }
    let dispatcher = workload::read(client, helper).await?;
    let dispatcher_did = identity::did(
        &helper_root.value,
        identity::Role::Dispatcher,
        identity(&dispatcher.deployment.metadata)?.0,
        identity(&dispatcher.pod.metadata)?.0,
    )?;
    let current_task = Api::<KarsTask>::namespaced(client.clone(), &workspace)
        .get(&task.name_any())
        .await
        .map_err(|e| api_error("recheck mission Task", e))?;
    same_snapshot(&task.metadata, &current_task.metadata)?;
    let runtime_root = roots::ensure(client, roots::Anchor::Runtime(&fresh), &namespace).await?;
    let pinned = sandboxes
        .get(&fresh.name_any())
        .await
        .map_err(|e| api_error("reread pinned sandbox", e))?;
    same_snapshot(&runtime_root.anchor, &pinned.metadata)?;
    if pinned.metadata.uid != fresh.metadata.uid
        || pinned.metadata.generation != fresh.metadata.generation
        || annotation(&pinned.metadata, roots::PIN) != Some(&runtime_root.uid)
        || !executable(&task, &pinned)
    {
        return Err(Error::Invalid("sandbox changed while pinning mission root"));
    }
    project(desired, &task, &pinned, &runtime_root.uid, &dispatcher_did)?;
    Ok(Some(Prepared {
        task,
        sandbox: pinned,
        namespace,
        dispatcher,
        dispatcher_namespace: helper_namespace,
        dispatcher_did,
        runtime_root,
    }))
}

fn project(
    deployment: &mut Deployment,
    task: &KarsTask,
    sandbox: &KarsSandbox,
    root_uid: &str,
    dispatcher: &str,
) -> Result<()> {
    let spec = deployment
        .spec
        .as_mut()
        .ok_or(Error::Invalid("missing runtime deployment specification"))?;
    let pod = spec
        .template
        .spec
        .as_mut()
        .ok_or(Error::Invalid("missing runtime pod specification"))?;
    let containers: Vec<_> = pod
        .containers
        .iter_mut()
        .filter(|c| c.name == "openclaw")
        .collect();
    if containers.len() != 1 {
        return Err(Error::Invalid(
            "mission requires exactly one OpenClaw container",
        ));
    }
    let container = containers.into_iter().next().unwrap();
    let env = container.env.get_or_insert_with(Vec::new);
    if env.iter().any(|e| e.name.starts_with("KARS_MISSION_")) {
        return Err(Error::Invalid("mission environment is already configured"));
    }
    let admission = admission::for_task(task)?.to_string();
    for (key, value) in [
        ("KARS_MISSION_ADMISSION", admission.as_str()),
        ("KARS_MISSION_DISPATCH_ENABLED", "true"),
        ("KARS_MISSION_CONTRACT", r#"{"version":1}"#),
        (
            "KARS_MISSION_TASK_NAME",
            task.metadata.name.as_deref().unwrap_or_default(),
        ),
        ("KARS_MISSION_TASK_UID", identity(&task.metadata)?.0),
        ("KARS_MISSION_SANDBOX_UID", identity(&sandbox.metadata)?.0),
        ("KARS_MISSION_DISPATCHER_DID", dispatcher),
    ] {
        env.push(EnvVar {
            name: key.into(),
            value: Some(value.into()),
            ..Default::default()
        });
    }
    env.push(EnvVar {
        name: "KARS_MISSION_POD_UID".into(),
        value_from: Some(EnvVarSource {
            field_ref: Some(ObjectFieldSelector {
                api_version: Some("v1".into()),
                field_path: "metadata.uid".into(),
            }),
            ..Default::default()
        }),
        ..Default::default()
    });
    env.push(EnvVar {
        name: "KARS_MISSION_IDENTITY_ROOT".into(),
        value_from: Some(EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: roots::RUNTIME_SECRET.into(),
                key: "root".into(),
                optional: Some(false),
            }),
            ..Default::default()
        }),
        ..Default::default()
    });
    spec.template
        .metadata
        .get_or_insert_with(Default::default)
        .annotations
        .get_or_insert_with(Default::default)
        .insert(roots::PIN.into(), root_uid.into());
    // Identity/prekey state must never be shared by overlapping runtime replicas.
    spec.strategy = Some(k8s_openapi::api::apps::v1::DeploymentStrategy {
        type_: Some("Recreate".into()),
        rolling_update: None,
    });
    Ok(())
}

/// A transient dispatcher rollout must not stop an accepted, non-replayable run.
/// The caller still applies the ordinary credential/private-activation fence.
pub(crate) async fn defer_until_ready(
    client: &Client,
    sandbox: &KarsSandbox,
    desired: &mut Deployment,
) -> Result<()> {
    if desired.spec.is_none() {
        return Err(Error::Invalid("missing deferred runtime specification"));
    }
    let ns = desired
        .namespace()
        .ok_or(Error::Invalid("missing deferred runtime namespace"))?;
    let namespace = Api::<Namespace>::all(client.clone())
        .get(&ns)
        .await
        .map_err(|e| api_error("read deferred runtime namespace", e))?;
    let namespace = namespace_ownership::recheck(client, sandbox, &namespace)
        .await
        .map_err(|_| Error::Invalid("deferred runtime namespace is not owned"))?;
    if let Some(prior) = Api::<Deployment>::namespaced(client.clone(), &ns)
        .get_opt(&sandbox.name_any())
        .await
        .map_err(|e| api_error("read deferred runtime deployment", e))?
    {
        crate::reconciler::credential_sources::validate_owned_deployment(
            &prior, sandbox, &namespace,
        )
        .map_err(|_| Error::Invalid("deferred runtime deployment is outside sandbox custody"))?;
        desired.spec = Some(
            prior
                .spec
                .ok_or(Error::Invalid("missing deferred runtime specification"))?,
        );
        // Carry this read's preconditions through the ordinary apply fence.
        desired.metadata.uid = prior.metadata.uid;
        desired.metadata.resource_version = prior.metadata.resource_version;
    } else {
        return Err(Error::NotReady(
            "new runtime awaits mission dispatcher readiness",
        ));
    }
    Ok(())
}

pub(crate) async fn publish(client: &Client, prepared: &Prepared) -> Result<()> {
    binding::publish(client, prepared).await
}
