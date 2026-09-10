// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{crd::KarsSandbox, mcp_server::McpServer};
use k8s_openapi::api::apps::v1::Deployment;
use kube::{ResourceExt, runtime::reflector::ObjectRef};

pub(super) fn sandbox_references(sandbox: KarsSandbox) -> Vec<ObjectRef<McpServer>> {
    let Some(workspace) = sandbox.namespace() else {
        return Vec::new();
    };
    sandbox
        .spec
        .governance
        .unwrap_or_default()
        .effective_mcp_server_refs()
        .iter()
        .map(|reference| ObjectRef::new(&reference.name).within(&workspace))
        .collect()
}

pub(super) fn workload_reference(deployment: Deployment) -> Option<ObjectRef<McpServer>> {
    let annotations = deployment.metadata.annotations.as_ref()?;
    let namespace = annotations.get("kars.azure.com/mcp-source-namespace")?;
    let name = annotations.get("kars.azure.com/mcp-source-name")?;
    if annotations
        .get("kars.azure.com/mcp-source-uid")
        .is_none_or(String::is_empty)
        || namespace.is_empty()
        || name.is_empty()
    {
        return None;
    }
    // Watch mapping is a scheduling hint, never resource-mutation authority.
    Some(ObjectRef::new(name).within(namespace))
}
