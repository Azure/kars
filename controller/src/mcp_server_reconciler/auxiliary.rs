// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use kube::api::{DeleteParams, Preconditions};

pub(super) fn ensure_aux_owner(
    meta: &ObjectMeta,
    mcp: &McpServer,
    require_uid: bool,
) -> Result<(), ReconcileError> {
    let uid = meta
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(SOURCE_UID));
    if meta.uid.as_deref().is_none_or(str::is_empty)
        || meta.resource_version.as_deref().is_none_or(str::is_empty)
        || meta.deletion_timestamp.is_some()
        || meta.namespace != mcp.metadata.namespace
        || meta
            .owner_references
            .as_ref()
            .is_some_and(|owners| !owners.is_empty())
        || meta
            .labels
            .as_ref()
            .and_then(|labels| labels.get("app.kubernetes.io/managed-by"))
            .map(String::as_str)
            != Some("kars-controller")
        || meta
            .labels
            .as_ref()
            .and_then(|labels| labels.get("kars.azure.com/mcp-server"))
            .map(String::as_str)
            != Some(mcp.name_any().as_str())
        || uid.is_some_and(|uid| Some(uid) != mcp.metadata.uid.as_ref())
        || (require_uid && uid != mcp.metadata.uid.as_ref())
    {
        return Err(ReconcileError::Configuration(
            "MCP auxiliary resource ownership conflicts; no adoption permitted".into(),
        ));
    }
    Ok(())
}

fn exact_aux_owner(meta: &ObjectMeta, mcp: &McpServer) -> bool {
    let mut live = meta.clone();
    live.deletion_timestamp = None;
    meta.annotations
        .as_ref()
        .and_then(|annotations| annotations.get(SOURCE_UID))
        == mcp.metadata.uid.as_ref()
        && mcp.uid().is_some()
        && ensure_aux_owner(&live, mcp, true).is_ok()
}

pub(super) async fn finalize(
    api: &Api<McpServer>,
    secrets: &Api<Secret>,
    configmaps: &Api<ConfigMap>,
    mcp: &McpServer,
    name: &str,
) -> Result<Action, ReconcileError> {
    let secret_name = format!("mcp-{name}-signing");
    let cm_name = format!("mcp-{name}-jwks");
    if let Some(secret) = secrets.get_opt(&secret_name).await? {
        if exact_aux_owner(&secret.metadata, mcp) {
            if secret.metadata.deletion_timestamp.is_none() {
                secrets
                    .delete(
                        &secret_name,
                        &DeleteParams {
                            preconditions: Some(Preconditions {
                                uid: secret.metadata.uid,
                                resource_version: secret.metadata.resource_version,
                            }),
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            if secrets.get_opt(&secret_name).await?.is_some() {
                return Ok(Action::requeue(Duration::from_secs(5)));
            }
        } else {
            tracing::warn!(mcp = %name, "Preserving legacy/unowned MCP signing Secret during cleanup");
        }
    }
    if let Some(cm) = configmaps.get_opt(&cm_name).await? {
        if exact_aux_owner(&cm.metadata, mcp) {
            if cm.metadata.deletion_timestamp.is_none() {
                configmaps
                    .delete(
                        &cm_name,
                        &DeleteParams {
                            preconditions: Some(Preconditions {
                                uid: cm.metadata.uid,
                                resource_version: cm.metadata.resource_version,
                            }),
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            if configmaps.get_opt(&cm_name).await?.is_some() {
                return Ok(Action::requeue(Duration::from_secs(5)));
            }
        } else {
            tracing::warn!(mcp = %name, "Preserving legacy/unowned MCP metadata ConfigMap during cleanup");
        }
    }
    let finalizers: Vec<_> = mcp
        .metadata
        .finalizers
        .as_ref()
        .map(|values| {
            values
                .iter()
                .filter(|value| *value != FINALIZER)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    api.patch(name, &PatchParams::default(), &Patch::Merge(json!({
        "metadata":{"uid":mcp.metadata.uid,"resourceVersion":mcp.metadata.resource_version,"finalizers":finalizers},
    }))).await?;
    Ok(Action::await_change())
}
