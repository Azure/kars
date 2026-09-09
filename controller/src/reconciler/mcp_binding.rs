// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    crd::{GovernanceConfig, KarsSandbox},
    mcp_server::McpServer,
};
use k8s_openapi::api::apps::v1::Deployment;
use kube::{Api, Client, ResourceExt};

pub(super) struct Binding {
    pub endpoint: Option<String>,
    pub signing: bool,
    pub revision: String,
}

pub(super) async fn resolve_all<'a>(
    client: &Client,
    sandbox: &KarsSandbox,
    governance: &'a GovernanceConfig,
) -> Result<Vec<(&'a str, Binding)>, String> {
    let name = sandbox.name_any();
    if governance.uses_singular_mcp_server_ref() {
        tracing::warn!(
            sandbox = %name,
            reason = crate::status::conditions::reason::MCP_SINGULAR_DEPRECATED,
            "spec.governance.mcpServerRef is deprecated; migrate to \
             spec.governance.mcpServerRefs (Slice 4d.1)",
        );
    }
    let mut bindings = Vec::new();
    for reference in governance.effective_mcp_server_refs() {
        let mcp_name = reference.name.trim();
        if mcp_name.is_empty() {
            continue;
        }
        match resolve(client, sandbox, mcp_name).await? {
            Some(binding) => bindings.push((mcp_name, binding)),
            None => tracing::warn!(sandbox = %name, mcp = %mcp_name,
                "MCP reference is not currently authorized and qualified"),
        }
    }
    Ok(bindings)
}

pub(super) fn decorate(
    deployment: &mut Deployment,
    revisions: &[String],
) -> Result<(), serde_json::Error> {
    if !revisions.is_empty() {
        deployment
            .spec
            .as_mut()
            .unwrap()
            .template
            .metadata
            .as_mut()
            .unwrap()
            .annotations
            .get_or_insert_with(Default::default)
            .insert(
                "kars.azure.com/mcp-bindings-version".into(),
                crate::providers::signing::content_digest(&serde_json::to_vec(revisions)?),
            );
    }
    Ok(())
}

pub(super) fn api_error(error: kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!("MCP binding API status {}", status.code),
        _ => "MCP binding API/transport failure".into(),
    }
}

pub(super) async fn resolve(
    client: &Client,
    sandbox: &KarsSandbox,
    name: &str,
) -> Result<Option<Binding>, String> {
    let workspace = sandbox
        .namespace()
        .ok_or("MCP caller workspace is absent")?;
    let source = Api::<McpServer>::namespaced(client.clone(), &workspace)
        .get_opt(name)
        .await
        .map_err(api_error)?;
    let Some(source) = source
        .filter(|source| crate::mcp_server_reconciler::managed::selected_for(source, sandbox))
    else {
        return Ok(None);
    };
    if source.uid().is_none()
        || source
            .metadata
            .generation
            .is_none_or(|generation| generation <= 0)
        || source.status.as_ref().is_none_or(|status| {
            status.phase.as_deref() != Some("Ready")
                || status.observed_generation != source.metadata.generation
        })
    {
        return Ok(None);
    }
    let endpoint =
        crate::mcp_server_reconciler::managed::qualified_endpoint(client, &source).await?;
    if source.spec.managed.is_some() && endpoint.is_none() {
        return Ok(None);
    }
    let cm_name = format!("mcp-{name}-jwks");
    let Some(cm) =
        Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(client.clone(), &workspace)
            .get_opt(&cm_name)
            .await
            .map_err(api_error)?
    else {
        return Ok(None);
    };
    let owner = cm
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kars.azure.com/mcp-source-uid"));
    if owner.is_some_and(|uid| source.metadata.uid.as_ref() != Some(uid))
        || (source.spec.managed.is_some() && owner != source.metadata.uid.as_ref())
        || cm.metadata.deletion_timestamp.is_some()
        || cm.metadata.uid.as_deref().is_none_or(str::is_empty)
        || cm
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || cm
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("app.kubernetes.io/managed-by"))
            .map(String::as_str)
            != Some("kars-controller")
        || cm
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("kars.azure.com/mcp-server"))
            .map(String::as_str)
            != Some(name)
    {
        return Ok(None);
    }
    let revision = crate::providers::signing::content_digest(
        &serde_json::to_vec(&(
            &source.metadata.uid,
            source.metadata.generation,
            &cm.metadata.uid,
            &cm.metadata.resource_version,
        ))
        .map_err(|_| "MCP binding revision serialization failed")?,
    );
    Ok(Some(Binding {
        endpoint,
        signing: source.spec.managed.is_none(),
        revision,
    }))
}
