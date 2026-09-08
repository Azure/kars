// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn consumer(meta: &ObjectMeta, sandbox: &KarsSandbox, ns: &Namespace) -> bool {
    annotation(meta, SANDBOX_UID) == sandbox.metadata.uid.as_deref()
        && annotation(meta, NAMESPACE_UID) == ns.metadata.uid.as_deref()
}

fn owned(meta: &ObjectMeta, sandbox: &KarsSandbox, ns: &Namespace) -> Result<(), Error> {
    identity(meta)?;
    let labels = meta.labels.as_ref().cloned().unwrap_or_default();
    let authored = meta.managed_fields.as_ref().is_some_and(|fields| {
        fields.iter().any(|entry| {
            entry.manager.as_deref() == Some(crate::field_managers::CLAWSANDBOX)
                && entry.operation.as_deref() == Some("Apply")
                && entry
                    .fields_v1
                    .as_ref()
                    .is_some_and(|fields| fields.0.get("f:spec").is_some())
        })
    });
    if meta.name != sandbox.metadata.name
        || meta.namespace.as_deref() != Some(ns.name_any().as_str())
        || meta.deletion_timestamp.is_some()
        || meta
            .owner_references
            .as_ref()
            .is_some_and(|refs| !refs.is_empty())
        || labels.get("kars.azure.com/sandbox") != sandbox.metadata.name.as_ref()
        || labels.get("kars.azure.com/component").map(String::as_str) != Some("sandbox")
        || labels
            .get("kars.azure.com/parent-namespace")
            .is_some_and(|value| Some(value) != sandbox.metadata.namespace.as_ref())
        || (!authored && !consumer(meta, sandbox, ns))
    {
        return Err(Error::Invalid(
            "runtime Deployment ownership is unproven; no workload takeover",
        ));
    }
    Ok(())
}

pub(super) async fn pause(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    only_consumer: bool,
) -> Result<(), Error> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), &ns.name_any());
    let deployment = match api.get(&sandbox.name_any()).await {
        Ok(value) => value,
        Err(kube::Error::Api(status)) if status.code == 404 => return Ok(()),
        Err(error) => return Err(api_error("read runtime Deployment", error)),
    };
    if only_consumer && !consumer(&deployment.metadata, sandbox, ns) {
        return Ok(());
    }
    owned(&deployment.metadata, sandbox, ns)?;
    let strategy = if sandbox.spec.credentials_ref.is_some() {
        "Recreate"
    } else {
        "RollingUpdate"
    };
    if deployment.spec.as_ref().and_then(|spec| spec.replicas) == Some(0)
        && deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.strategy.as_ref())
            .is_some_and(|value| {
                value.type_.as_deref() == Some(strategy)
                    && (strategy != "Recreate" || value.rolling_update.is_none())
            })
    {
        return Ok(());
    }
    namespace_current(client, sandbox, ns).await?;
    let (uid, rv) = identity(&deployment.metadata)?;
    api.patch(
        &sandbox.name_any(),
        &PatchParams::default(),
        &Patch::Merge(json!({
            "metadata": {"uid": uid, "resourceVersion": rv},
            "spec": {"replicas": 0, "strategy": {"type": strategy, "rollingUpdate": null}}
        })),
    )
    .await
    .map_err(|e| api_error("stop credential consumer", e))?;
    Ok(())
}

pub(super) async fn current(
    client: &Client,
    sandbox: &KarsSandbox,
    ns: &Namespace,
    mode: &Mode,
) -> Result<bool, Error> {
    let Mode::Source { uid, version, .. } = mode else {
        return Ok(false);
    };
    let api: Api<Deployment> = Api::namespaced(client.clone(), &ns.name_any());
    let deployment = match api.get(&sandbox.name_any()).await {
        Ok(value) => value,
        Err(kube::Error::Api(status)) if status.code == 404 => return Ok(false),
        Err(error) => return Err(api_error("read credential consumer", error)),
    };
    owned(&deployment.metadata, sandbox, ns)?;
    let expected = format!("{uid}:{version}");
    Ok(consumer(&deployment.metadata, sandbox, ns)
        && deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|meta| annotation(meta, POD_VERSION))
            == Some(expected.as_str()))
}
