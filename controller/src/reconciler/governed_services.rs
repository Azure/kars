// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Narrow router service identity/credential projection; no approvals or grants.

use crate::{crd::KarsSandbox, kars_task::KarsTask};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Namespace};
use kube::{Api, Client, ResourceExt};
use serde_json::{Value, json};

#[cfg(test)]
mod continuity_tests;
#[cfg(test)]
mod credential_tests;
pub(crate) mod credentials;

const SECRET: &str = "router-services-admin";
const SOURCE_UID: &str = "kars.azure.com/sandbox-uid";
const NAMESPACE_UID: &str = "kars.azure.com/namespace-uid";
pub(super) use credentials::quarantine_on_privacy_loss;

pub struct Projection {
    pub identity: Value,
    credential: credentials::Projection,
    github: crate::credential_grants::github::Projection,
}

impl Projection {
    pub fn decorate(&self, deployment: &mut Deployment) {
        self.credential.decorate(deployment);
        self.github.decorate(deployment);
    }

    pub fn mount(&self, pod: &mut Value) {
        mount(pod);
        if let Some(required) = self.github.required_mount() {
            super::github_services::mount(pod, required);
        }
    }

    pub async fn consumers_current(
        &self,
        client: &Client,
        namespace: &str,
        name: &str,
    ) -> Result<bool, String> {
        if !self
            .github
            .consumers_current(client, namespace, name)
            .await?
        {
            return Ok(false);
        }
        self.credential
            .consumers_current(client, namespace, name)
            .await
    }
}

fn api_error(error: kube::Error) -> String {
    match error {
        kube::Error::Api(status) => {
            format!("Governed service credential API status {}", status.code)
        }
        _ => "Governed service credential API/transport failure".into(),
    }
}

fn authorized_task(
    task: &KarsTask,
    workspace: &str,
    sandbox: &str,
    name: &str,
    uid: &str,
) -> Option<String> {
    let status = task.status.as_ref()?;
    let authorization = task.spec.authorization_digest();
    (!crate::kars_task_reconciler::rebind::pending(task)
        && task.metadata.namespace.as_deref() == Some(workspace)
        && task.metadata.name.as_deref() == Some(name)
        && task.metadata.uid.as_deref() == Some(uid)
        && task
            .metadata
            .generation
            .is_some_and(|generation| generation > 0)
        && task.metadata.deletion_timestamp.is_none()
        && status.observed_generation == task.metadata.generation
        && status.phase.as_deref() == Some("Ready")
        && status
            .conditions
            .iter()
            .flatten()
            .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        && status.envelope_digest.as_deref() == Some(authorization.as_str())
        && status
            .sandbox_ref
            .as_ref()
            .map(|reference| reference.name.as_str())
            == Some(sandbox)
        && task
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch)
        && crate::kars_task::validate_execution_contract(&task.spec).is_ok())
    .then_some(authorization)
}

pub(crate) async fn identity(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Value, String> {
    // Reuse the authoritative namespace claim path, not labels or a caller's
    // requested namespace. Recreated CRs/namespaces cannot inherit this token.
    let (live, owned) = super::namespace_ownership::ensure(client, sandbox)
        .await
        .map_err(|_| "Governed service namespace authority could not be verified")?;
    let owned = owned.ok_or("Governed service namespace is absent")?;
    if live.metadata.uid != sandbox.metadata.uid || owned.metadata.uid != namespace.metadata.uid {
        return Err("Governed service namespace or Sandbox incarnation changed".into());
    }
    identity_from_live(client, &live, &owned).await
}

pub(crate) async fn identity_read_only(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Value, String> {
    super::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| "Governed service namespace identity changed")?;
    let workspace = sandbox.namespace().ok_or("Sandbox workspace missing")?;
    let live = Api::<KarsSandbox>::namespaced(client.clone(), &workspace)
        .get(&sandbox.name_any())
        .await
        .map_err(api_error)?;
    if live.metadata.uid != sandbox.metadata.uid
        || live.metadata.generation != sandbox.metadata.generation
        || live.metadata.deletion_timestamp.is_some()
    {
        return Err("Governed service source changed".into());
    }
    identity_from_live(client, &live, namespace).await
}

async fn identity_from_live(
    client: &Client,
    live: &KarsSandbox,
    owned: &Namespace,
) -> Result<Value, String> {
    let sandbox_uid = live
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .ok_or("Sandbox UID missing")?;
    let namespace_uid = owned
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .ok_or("Namespace UID missing")?;
    let workspace = live.namespace().ok_or("Sandbox workspace missing")?;
    let name = live.name_any();
    let mut task_identity = Value::Null;
    let mut task_authorization = None;
    let mut task_generation = None;
    if let Some(owner) = live.metadata.owner_references.as_ref().and_then(|owners| {
        owners.iter().find(|owner| {
            owner.kind == "KarsTask"
                && owner.api_version == "kars.azure.com/v1alpha1"
                && owner.controller == Some(true)
        })
    }) {
        let tasks: Api<KarsTask> = Api::namespaced(client.clone(), &workspace);
        let task = tasks.get(&owner.name).await.map_err(api_error)?;
        let authorization = authorized_task(&task, &workspace, &name, &owner.name, &owner.uid)
            .ok_or("Task UID/effective authorization does not bind this Sandbox")?;
        task_identity = json!({"namespace":workspace,"name":owner.name,"uid":owner.uid});
        task_authorization = Some(authorization);
        task_generation = task.metadata.generation;
    }
    Ok(
        json!({"sandbox":{"namespace":workspace,"name":name,"uid":sandbox_uid},
        "namespace_uid":namespace_uid,"task":task_identity,"task_authorization":task_authorization,
        "task_generation":task_generation,"managed":true}),
    )
}

pub async fn ensure(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Projection, String> {
    let identity = identity(client, sandbox, namespace).await?;
    let credential = credentials::ensure(client, sandbox, namespace).await?;
    let github =
        crate::credential_grants::github::ensure(client, sandbox, namespace, &identity).await?;
    Ok(Projection {
        identity,
        credential,
        github,
    })
}

pub fn mount(pod: &mut Value) {
    pod["volumes"]
        .as_array_mut()
        .expect("pod volumes array")
        .push(json!({
        "name":"governed-services-control","secret":{"secretName":SECRET,
            "items":[{"key":"control-token","path":"control-token"}]}}));
    for container in pod["containers"]
        .as_array_mut()
        .expect("pod containers array")
    {
        if container["name"] == "inference-router" {
            container["volumeMounts"].as_array_mut().expect("router mounts array").push(json!({
                "name":"governed-services-control","mountPath":"/etc/kars/services","readOnly":true}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task() -> KarsTask {
        let mut task:KarsTask=serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
            "metadata":{"name":"task","namespace":"workspace","uid":"task-uid","generation":1},
            "spec":{"objective":"inspect","envelope":{"tier":1,"authorityCeiling":1,"delegationDepth":0},"execution":{"launch":true}},
            "status":{"phase":"Ready","observedGeneration":1,"sandboxRef":{"name":"sandbox"},
                "conditions":[{"type":"Ready","status":"True","reason":"Valid","message":"validated","lastTransitionTime":"2026-09-08T00:00:00Z"}]},
        })).unwrap();
        task.status.as_mut().unwrap().envelope_digest = Some(task.spec.authorization_digest());
        task
    }
    #[test]
    fn service_task_attribution_requires_full_authorization_and_current_uid() {
        let mut task = task();
        assert!(authorized_task(&task, "workspace", "sandbox", "task", "task-uid").is_some());
        assert!(authorized_task(&task, "other", "sandbox", "task", "task-uid").is_none());
        assert!(authorized_task(&task, "workspace", "sandbox", "task", "old-task-uid").is_none());
        task.spec.blueprint = Some(crate::kars_task::TaskBlueprint {
            instructions: Some("changed after approval".into()),
            ..Default::default()
        });
        assert!(authorized_task(&task, "workspace", "sandbox", "task", "task-uid").is_none());
    }
    #[test]
    fn envelope_only_hash_and_unsupported_launch_budgets_never_authorize_projection() {
        let mut task = task();
        task.status.as_mut().unwrap().envelope_digest = Some(task.spec.envelope.digest());
        assert!(authorized_task(&task, "workspace", "sandbox", "task", "task-uid").is_none());
        task.spec.envelope.budget = Some(crate::kars_task::TaskBudget {
            tokens: Some(10),
            ..Default::default()
        });
        task.status.as_mut().unwrap().envelope_digest = Some(task.spec.authorization_digest());
        assert!(authorized_task(&task, "workspace", "sandbox", "task", "task-uid").is_none());
    }
    #[test]
    fn control_credential_is_never_mounted_in_agent_containers() {
        let mut pod = json!({"volumes":[],"containers":[
            {"name":"openclaw","volumeMounts":[]},{"name":"agent","volumeMounts":[]},
            {"name":"inference-router","volumeMounts":[]}]});
        mount(&mut pod);
        assert_eq!(pod["containers"][0]["volumeMounts"], json!([]));
        assert_eq!(pod["containers"][1]["volumeMounts"], json!([]));
        assert_eq!(
            pod["containers"][2]["volumeMounts"][0]["mountPath"],
            "/etc/kars/services"
        );
    }
    #[test]
    fn standard_control_tokens_are_bounded_and_distinct() {
        let first = crate::providers::signing::generate_service_token();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert_ne!(first, crate::providers::signing::generate_service_token());
    }
}
