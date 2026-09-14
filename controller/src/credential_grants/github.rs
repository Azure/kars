// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact operator App-store projection. No installation token or App key reaches agents.

use super::*;
use crate::{
    crd::KarsSandbox, credential_grant::github as contract, reconciler::governed_services,
};
use governed_services::credentials::{self, GITHUB};
use k8s_openapi::api::core::v1::ConfigMap;
use serde_json::Value;

const ENROLLED: &str = "kars.azure.com/github-grant-uid";

pub(crate) enum Projection {
    Legacy,
    Issued(credentials::Projection),
    Retired(credentials::Projection),
}

impl Projection {
    pub(crate) fn required_mount(&self) -> Option<bool> {
        match self {
            Self::Legacy => Some(false),
            Self::Issued(_) => Some(true),
            Self::Retired(_) => None,
        }
    }

    pub(crate) fn decorate(&self, deployment: &mut k8s_openapi::api::apps::v1::Deployment) {
        if let Self::Issued(projection) | Self::Retired(projection) = self {
            projection.decorate(deployment);
        }
    }

    pub(crate) async fn consumers_current(
        &self,
        client: &Client,
        namespace: &str,
        name: &str,
    ) -> Result<bool, String> {
        match self {
            Self::Legacy => Ok(true),
            Self::Issued(projection) | Self::Retired(projection) => {
                projection.consumers_current(client, namespace, name).await
            }
        }
    }
}

#[cfg(test)]
mod tests;

fn string(secret: &Secret, key: &str) -> Result<String, String> {
    secret
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|data| std::str::from_utf8(&data.0).ok())
        .map(str::to_string)
        .ok_or_else(|| "Operator App store has missing or invalid material".into())
}

fn validated_material<'grant>(
    selection: &GitHubBinding,
    grant: &'grant KarsCredentialGrant,
    connection: &ConfigMap,
    store: &Secret,
) -> Result<(&'grant GitHubConnectionGrant, String, String), String> {
    contract::validate(selection)?;
    let approved = grant
        .spec
        .github_connections
        .iter()
        .find(|candidate| candidate.connection == selection.connection)
        .ok_or("GitHub connection UID has no explicit operator grant")?;
    let expected_name = format!(
        "kars-github-connection-{}",
        &crate::providers::signing::sha256_hex(approved.owner_subject.as_bytes())[..16]
    );
    if approved.owner_subject.is_empty()
        || expected_name != connection.name_any()
        || identity(&connection.metadata)?.0 != approved.connection.uid
        || identity(&store.metadata)?.0 != approved.app_secret.uid
        || connection.namespace() != grant.namespace()
        || store.namespace() != grant.namespace()
        || store.name_any() != approved.app_secret.name
        || store.type_.as_deref() != Some("Opaque")
        || !grant
            .spec
            .integration_stores
            .iter()
            .any(|entry| entry.purpose == "github-app" && entry.secret == approved.app_secret)
        || approved.installation_id == 0
        || approved.repositories.is_empty()
        || approved.repositories.len() > 32
        || approved
            .repositories
            .iter()
            .any(|repo| !contract::repository(repo))
        || selection
            .repositories
            .iter()
            .any(|repo| !approved.repositories.contains(repo))
        || (selection.write && !approved.write)
    {
        return Err("GitHub App, connection, owner or repository authority differs from its operator enrollment".into());
    }
    let data = connection
        .data
        .as_ref()
        .ok_or("GitHub connection metadata is unavailable")?;
    let installation = data
        .get("installation_id")
        .and_then(|id| id.parse::<u64>().ok());
    let repositories: Vec<String> = serde_json::from_str(
        data.get("repos")
            .ok_or("GitHub connection repositories missing")?,
    )
    .map_err(|_| "GitHub connection repositories are invalid")?;
    if installation != Some(approved.installation_id)
        || selection.repositories.iter().any(|repo| {
            !repositories
                .iter()
                .any(|actual| actual.to_ascii_lowercase() == *repo)
        })
    {
        return Err("Stored GitHub connection changed after operator review".into());
    }
    let app = string(store, "GITHUB_APP_ID")?;
    let key = string(store, "GITHUB_APP_PRIVATE_KEY")?;
    if app != approved.app_id
        || app.is_empty()
        || app.len() > 20
        || !app.bytes().all(|byte| byte.is_ascii_digit())
        || app.parse::<u64>().ok().is_none_or(|id| id == 0)
        || jsonwebtoken::EncodingKey::from_rsa_pem(key.as_bytes()).is_err()
    {
        return Err("Operator App ID or RSA key is invalid or changed".into());
    }
    let app = app
        .parse::<u64>()
        .map_err(|_| "Operator App ID is invalid")?
        .to_string();
    Ok((approved, app, key))
}

fn configuration(
    selection: &GitHubBinding,
    grant: &KarsCredentialGrant,
    connection: &ConfigMap,
    store: &Secret,
    managed_identity: &Value,
) -> Result<String, String> {
    if managed_identity["managed"] != true
        || managed_identity["sandbox"]["namespace"] != json!(grant.namespace())
    {
        return Err(
            "GitHub private projection requires the verified managed workspace identity".into(),
        );
    }
    let (approved, app, key) = validated_material(selection, grant, connection, store)?;
    let value = json!({"identity":managed_identity,"app_id":app,"installation_id":approved.installation_id,
        "private_key_pem":key,"repositories":selection.repositories,"write":selection.write});
    let serialized = serde_json::to_string(&value)
        .map_err(|_| "GitHub private configuration serialization failed")?;
    if serialized.len() > 65536 {
        return Err("GitHub private configuration exceeds the consumer limit".into());
    }
    Ok(serialized)
}

async fn prepare(
    client: &Client,
    sandbox: &KarsSandbox,
    managed_identity: &Value,
) -> Result<(KarsCredentialGrant, ConfigMap, Secret, String), String> {
    let selection = sandbox
        .spec
        .github_binding
        .as_ref()
        .ok_or("GitHub selection missing")?;
    contract::agent_sources(sandbox.spec.credential_bindings.as_ref())?;
    if sandbox.spec.credentials_ref.is_some()
        || sandbox.spec.network_policy.as_ref().is_none_or(|policy| {
            !policy.default_deny
                || policy.egress_mode != crate::crd::EgressMode::Strict
                || policy.allowlist_ref.is_some()
                || policy
                    .allowed_endpoints
                    .iter()
                    .flatten()
                    .any(|endpoint| contract::opaque_github_egress(&endpoint.host))
        })
    {
        return Err("Keyless GitHub requires explicit Strict inline egress without direct credentials, external allowlist authority or opaque GitHub access".into());
    }
    let workspace = sandbox.namespace().ok_or("GitHub workspace missing")?;
    if let Some(task_uid) = managed_identity["task"]["uid"].as_str() {
        let name = managed_identity["task"]["name"]
            .as_str()
            .ok_or("GitHub Task identity missing")?;
        let task = Api::<crate::kars_task::KarsTask>::namespaced(client.clone(), &workspace)
            .get(name)
            .await
            .map_err(|e| api_error("Read GitHub Task authorization", e))?;
        if task.uid().as_deref() != Some(task_uid)
            || !crate::kars_task_reconciler::task_is_ready(&task)
            || task
                .spec
                .blueprint
                .as_ref()
                .and_then(|blueprint| blueprint.github_binding.as_ref())
                != Some(selection)
            || managed_identity["task_authorization"] != task.spec.authorization_digest()
        {
            return Err(
                "GitHub selection differs from the live UID-bound Task authorization".into(),
            );
        }
    }
    let (grant, connection, store) = read_connection(client, &workspace, selection).await?;
    let configuration = configuration(selection, &grant, &connection, &store, managed_identity)?;
    Ok((grant, connection, store, configuration))
}

async fn read_connection(
    client: &Client,
    workspace: &str,
    selection: &GitHubBinding,
) -> Result<(KarsCredentialGrant, ConfigMap, Secret), String> {
    let grant = current(client, workspace, &selection.grant).await?;
    let approved = grant
        .spec
        .github_connections
        .iter()
        .find(|candidate| candidate.connection == selection.connection)
        .ok_or("GitHub connection requires explicit operator enrollment")?;
    let connection = Api::<ConfigMap>::namespaced(client.clone(), workspace)
        .get(&approved.connection.name)
        .await
        .map_err(|e| api_error("Read reviewed GitHub connection", e))?;
    let store = Api::<Secret>::namespaced(client.clone(), workspace)
        .get(&approved.app_secret.name)
        .await
        .map_err(|e| api_error("Read enrolled GitHub App store", e))?;
    Ok((grant, connection, store))
}

pub(super) async fn preflight_binding(
    client: &Client,
    workspace: &str,
    selection: &GitHubBinding,
) -> Result<(), String> {
    let (grant, connection, store) = read_connection(client, workspace, selection).await?;
    validated_material(selection, &grant, &connection, &store)?;
    Ok(())
}

pub(crate) async fn ensure(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    managed_identity: &Value,
) -> Result<Projection, String> {
    let previous = sandbox
        .metadata
        .annotations
        .as_ref()
        .and_then(|values| values.get(ENROLLED));
    let previously_enrolled = previous.is_some();
    if sandbox.spec.github_binding.is_none() {
        if previous.is_some_and(|value| value != "retired") {
            credentials::retire_for(client, sandbox, namespace, GITHUB).await?;
            let workspace = sandbox.namespace().ok_or("GitHub workspace missing")?;
            Api::<KarsSandbox>::namespaced(client.clone(),&workspace).patch(&sandbox.name_any(),&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":sandbox.metadata.uid,"resourceVersion":sandbox.metadata.resource_version,
                    "annotations":{ENROLLED:"retired"}}
            }))).await.map_err(|e|api_error("Record private GitHub revocation",e))?;
        }
        return if previously_enrolled {
            credentials::Projection::retired(GITHUB, sandbox).map(Projection::Retired)
        } else {
            Ok(Projection::Legacy)
        };
    }
    let result = issue(client, sandbox, namespace, managed_identity).await;
    if matches!(&result, Err(credentials::IssuanceError::Rejected(_))) && previously_enrolled {
        credentials::retire_for(client, sandbox, namespace, GITHUB).await?;
    }
    result
        .map(Projection::Issued)
        .map_err(|error| error.to_string())
}

pub(super) async fn revoke(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    let workspace = grant.namespace().ok_or("GitHub grant workspace missing")?;
    for sandbox in Api::<KarsSandbox>::namespaced(client.clone(), &workspace)
        .list(&ListParams::default())
        .await
        .map_err(|e| api_error("Read enrolled GitHub consumers", e))?
    {
        if sandbox
            .metadata
            .annotations
            .as_ref()
            .and_then(|values| values.get(ENROLLED))
            == grant.metadata.uid.as_ref()
        {
            let namespace = Api::<Namespace>::all(client.clone())
                .get(&format!("kars-{}", sandbox.name_any()))
                .await
                .map_err(|e| api_error("Read private GitHub namespace for revocation", e))?;
            credentials::retire_for(client, &sandbox, &namespace, GITHUB).await?;
        }
    }
    Ok(())
}

async fn issue(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    managed_identity: &Value,
) -> Result<credentials::Projection, credentials::IssuanceError> {
    let (grant, connection, store, configuration) =
        prepare(client, sandbox, managed_identity).await?;
    let workspace = sandbox.namespace().ok_or("GitHub workspace missing")?;
    let sandboxes: Api<KarsSandbox> = Api::namespaced(client.clone(), &workspace);
    let current_sandbox = sandboxes
        .get(&sandbox.name_any())
        .await
        .map_err(|e| api_error("Refresh GitHub target", e))?;
    if current_sandbox.uid() != sandbox.uid()
        || current_sandbox.spec.github_binding != sandbox.spec.github_binding
        || current_sandbox.metadata.generation != sandbox.metadata.generation
        || current_sandbox.metadata.deletion_timestamp.is_some()
    {
        return Err("GitHub target changed before private issuance".into());
    }
    if current_sandbox
        .metadata
        .annotations
        .as_ref()
        .and_then(|values| values.get(ENROLLED))
        != grant.metadata.uid.as_ref()
    {
        sandboxes.patch(&sandbox.name_any(),&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":current_sandbox.metadata.uid,"resourceVersion":current_sandbox.metadata.resource_version,
                "annotations":{ENROLLED:grant.metadata.uid}}
        }))).await.map_err(|e|api_error("Record exact GitHub credential enrollment",e))?;
    }
    verify(client, &grant).await?;
    let live_connection = Api::<ConfigMap>::namespaced(client.clone(), &workspace)
        .get_metadata(&connection.name_any())
        .await
        .map_err(|e| api_error("Recheck GitHub connection identity", e))?;
    let live_store = Api::<Secret>::namespaced(client.clone(), &workspace)
        .get_metadata(&store.name_any())
        .await
        .map_err(|e| api_error("Recheck GitHub App identity", e))?;
    if identity(&live_connection.metadata)? != identity(&connection.metadata)?
        || identity(&live_store.metadata)? != identity(&store.metadata)?
    {
        return Err("GitHub source UID/resourceVersion changed before issuance".into());
    }
    let fresh_identity = governed_services::identity(client, sandbox, namespace).await?;
    if fresh_identity != *managed_identity {
        return Err("GitHub managed authority changed before issuance".into());
    }
    let revision=serde_json::to_string(&json!({
        "grant":{"namespace":workspace,"uid":grant.metadata.uid,"generation":grant.metadata.generation,
            "workspaceUid":grant.spec.workspace_uid},
        "appSecret":{"name":store.metadata.name,"uid":store.metadata.uid,"resourceVersion":store.metadata.resource_version},
        "connection":{"name":connection.metadata.name,"uid":connection.metadata.uid,"resourceVersion":connection.metadata.resource_version},
        "sandbox":{"uid":sandbox.metadata.uid,"generation":sandbox.metadata.generation},
        "runtimeNamespaceUid":namespace.metadata.uid,"identity":fresh_identity,
    })).map_err(|_|"GitHub source revision serialization failed")?;
    credentials::ensure_bound(
        client,
        sandbox,
        namespace,
        GITHUB,
        Some(&configuration),
        Some(&revision),
    )
    .await
}
