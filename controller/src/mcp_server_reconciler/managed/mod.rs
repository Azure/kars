// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod ownership;
mod plan;
mod probe;
#[cfg(test)]
mod probe_tests;
#[cfg(test)]
mod tests;

use crate::{crd::KarsSandbox, mcp_server::McpServer};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Secret, Service},
    networking::v1::NetworkPolicy,
};
use kube::{Api, Client, ResourceExt, api::ListParams};
use ownership::api_error;
use plan::{Config, Owner, Plan};
use serde_json::{Value, json};

pub(super) struct Outcome {
    pub endpoint: Option<String>,
    pub workload_ref: String,
    pub namespace_uid: String,
    pub tools: Option<Vec<String>>,
    pub schema_digest: Option<String>,
    pub revision: String,
    pub pending: Option<String>,
    pub workload_generation: Option<i64>,
    pub workload_image: Option<String>,
}

pub(super) fn pending_conditions(
    prior: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
    generation: Option<i64>,
    message: &str,
) -> Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> {
    use crate::status::conditions::{find, preserve_transition_time};
    [
        ("Ready", "False"),
        ("Progressing", "True"),
        ("Degraded", "False"),
    ]
    .into_iter()
    .map(|(kind, status)| {
        preserve_transition_time(
            find(prior, kind),
            kind,
            status,
            "ManagedMcpPending",
            message,
            generation,
        )
    })
    .collect()
}

pub(crate) fn selected_for(mcp: &McpServer, sandbox: &KarsSandbox) -> bool {
    mcp.namespace() == sandbox.namespace()
        && mcp.metadata.deletion_timestamp.is_none()
        && mcp.spec.allowed_sandboxes.as_ref().is_none_or(|selector| {
            selector.match_labels.iter().all(|(key, expected)| {
                sandbox
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(key))
                    == Some(expected)
            })
        })
}

pub(crate) async fn qualified_endpoint(
    client: &Client,
    source: &McpServer,
) -> Result<Option<String>, String> {
    let Some(status) = source.status.as_ref().filter(|status| {
        status.phase.as_deref() == Some("Ready")
            && status.observed_generation == source.metadata.generation
    }) else {
        return Ok(None);
    };
    if source.spec.managed.is_none() {
        return Ok(source.spec.url.clone().or_else(|| {
            source
                .spec
                .bundle_ref
                .as_ref()
                .filter(|bundle| {
                    status.bundle_ref_digest.as_deref() == Some(bundle.digest.as_str())
                })
                .and_then(|_| status.endpoint.clone())
        }));
    }
    let config = Config::from_env()?;
    let plan = Plan::new(source, &config)?;
    if status.workload_ref.as_deref() != Some(plan.owner.workload_ref().as_str())
        || status.endpoint.as_deref() != Some(plan.endpoint().as_str())
        || status
            .managed_namespace_uid
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Ok(None);
    }
    let Some(namespace) = ownership::namespace(client, &config, source, false).await? else {
        return Ok(None);
    };
    let uid = namespace.uid().ok_or("Managed MCP namespace UID missing")?;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &config.namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), &config.namespace);
    let Some(deployment) = ownership::preflight(&deployments, &plan.owner, &uid).await? else {
        return Ok(None);
    };
    if ownership::preflight(&services, &plan.owner, &uid)
        .await?
        .is_none()
        || status.workload_generation != deployment.metadata.generation
        || status.workload_image.as_deref() != Some(plan.image.as_str())
        || deployment.status.as_ref().is_none_or(|status| {
            status.observed_generation != deployment.metadata.generation
                || status.updated_replicas != Some(1)
                || status.available_replicas != Some(1)
        })
        || deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.template.spec.as_ref())
            .is_none_or(|spec| {
                spec.containers
                    .iter()
                    .find(|container| container.name == "mcp")
                    .and_then(|container| container.image.as_deref())
                    != Some(plan.image.as_str())
            })
    {
        return Ok(None);
    }
    Ok(Some(plan.endpoint()))
}

async fn peers(client: &Client, source: &McpServer, config: &Config) -> Result<Vec<Value>, String> {
    let mut peers = vec![json!({
        "namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":config.controller_namespace}},
        "podSelector":{"matchLabels":{"app.kubernetes.io/component":"controller"}},
    })];
    let sandboxes = Api::<KarsSandbox>::namespaced(
        client.clone(),
        &source.namespace().ok_or("McpServer workspace missing")?,
    )
    .list(&ListParams::default())
    .await
    .map_err(|error| api_error("List MCP referring sandboxes", error))?;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    for sandbox in sandboxes {
        if !selected_for(source, &sandbox) || sandbox.metadata.deletion_timestamp.is_some() {
            continue;
        }
        let governance = sandbox.spec.governance.clone().unwrap_or_default();
        if !governance
            .effective_mcp_server_refs()
            .iter()
            .any(|reference| reference.name == source.name_any())
        {
            continue;
        }
        let namespace_name = format!("kars-{}", sandbox.name_any());
        let Some(namespace) = namespaces
            .get_opt(&namespace_name)
            .await
            .map_err(|error| api_error("Read MCP caller namespace", error))?
        else {
            continue;
        };
        if namespace.metadata.deletion_timestamp.is_some()
            || !crate::reconciler::namespace_ownership::claimed(&namespace, &sandbox)
                .map_err(|_| "MCP caller namespace ownership conflicts")?
        {
            continue;
        }
        peers.push(json!({
            "namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":namespace_name}},
            "podSelector":{"matchLabels":{"kars.azure.com/sandbox":sandbox.name_any()}},
        }));
    }
    Ok(peers)
}

pub(super) fn validate_source(source: &McpServer) -> Result<(), String> {
    let spec = &source.spec;
    if spec.managed.is_none()
        || spec.url.is_some()
        || spec.bundle_ref.is_some()
        || spec.oauth.is_some()
        || spec.production_mode.is_some()
        || spec.scopes.is_some()
        || spec.bearer_from_env.is_some()
    {
        return Err(
            "Managed MCP cannot combine workload presets with URL/bundle/auth source fields".into(),
        );
    }
    Ok(())
}

pub(super) async fn reconcile(
    client: &Client,
    source: &McpServer,
    http: &reqwest::Client,
) -> Result<Outcome, String> {
    validate_source(source)?;
    ownership::source_current(client, source, false).await?;
    let config = Config::from_env()?;
    let plan = Plan::new(source, &config)?;
    if source
        .status
        .as_ref()
        .and_then(|status| status.workload_ref.as_deref())
        .is_some_and(|reference| reference != plan.owner.workload_ref())
    {
        return Err("Managed MCP namespace/name changed; retire the recorded workload before reconfiguration".into());
    }
    let namespace = ownership::namespace(client, &config, source, true)
        .await?
        .ok_or("Managed MCP namespace is absent")?;
    let namespace_uid = namespace.uid().ok_or("Managed MCP namespace UID missing")?;
    let mut outcome = Outcome {
        endpoint: None,
        workload_ref: plan.owner.workload_ref(),
        namespace_uid: namespace_uid.clone(),
        tools: None,
        schema_digest: None,
        revision: String::new(),
        pending: None,
        workload_generation: None,
        workload_image: None,
    };
    if source
        .status
        .as_ref()
        .and_then(|status| status.managed_namespace_uid.as_ref())
        .is_none()
    {
        outcome.pending = Some(
            "Recording the exact managed namespace incarnation before creating workloads".into(),
        );
        return Ok(outcome);
    }
    if let Some(name) = config.pull_secret.as_ref() {
        let secret = Api::<Secret>::namespaced(client.clone(), &config.namespace)
            .get_opt(name)
            .await
            .map_err(|error| {
                api_error("Inspect explicitly configured local MCP pull Secret", error)
            })?;
        let Some(secret) = secret else {
            outcome.pending = Some(
                "The explicitly configured pull Secret is absent from the managed MCP namespace"
                    .into(),
            );
            return Ok(outcome);
        };
        if secret.type_.as_deref() != Some("kubernetes.io/dockerconfigjson")
            || secret.metadata.deletion_timestamp.is_some()
            || secret
                .data
                .as_ref()
                .is_none_or(|data| !data.contains_key(".dockerconfigjson"))
        {
            return Err(
                "Managed MCP image pull Secret is not a live dockerconfigjson Secret".into(),
            );
        }
    }
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &config.namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), &config.namespace);
    let policies: Api<NetworkPolicy> = Api::namespaced(client.clone(), &config.namespace);
    // Complete ownership/selector preflight before modifying any resource.
    ownership::preflight(&deployments, &plan.owner, &namespace_uid).await?;
    ownership::preflight(&services, &plan.owner, &namespace_uid).await?;
    ownership::preflight(&policies, &plan.owner, &namespace_uid).await?;
    let peers = peers(client, source, &config).await?;
    let policy = ownership::upsert(
        client,
        source,
        &policies,
        &plan.owner,
        &namespace_uid,
        plan.network_policy(&namespace_uid, peers),
    )
    .await?;
    let service = ownership::upsert(
        client,
        source,
        &services,
        &plan.owner,
        &namespace_uid,
        plan.service(&namespace_uid),
    )
    .await?;
    let deployment = ownership::upsert(
        client,
        source,
        &deployments,
        &plan.owner,
        &namespace_uid,
        plan.deployment(&namespace_uid, config.pull_secret.as_deref()),
    )
    .await?;
    let current = deployments
        .get(&plan.owner.name)
        .await
        .map_err(|error| api_error("Read managed MCP rollout", error))?;
    if current.uid() != deployment.uid()
        || !ownership::owned(&current.metadata, &plan.owner, &namespace_uid)
    {
        return Err("Managed MCP Deployment changed during readiness verification".into());
    }
    let ready = current.status.as_ref().is_some_and(|status| {
        status.observed_generation == current.metadata.generation
            && status.updated_replicas == Some(1)
            && status.available_replicas == Some(1)
            && status.replicas == Some(1)
    }) && current
        .spec
        .as_ref()
        .and_then(|spec| spec.template.spec.as_ref())
        .is_some_and(|spec| {
            spec.automount_service_account_token == Some(false)
                && spec
                    .containers
                    .iter()
                    .find(|container| container.name == "mcp")
                    .and_then(|container| container.image.as_deref())
                    == Some(plan.image.as_str())
        });
    if !ready {
        outcome.pending = Some(
            "Waiting for the exact managed MCP Deployment generation to become available".into(),
        );
        return Ok(outcome);
    }
    let probe = probe::probe(
        http,
        &plan.endpoint(),
        source.spec.allowed_tools.as_deref().unwrap_or_default(),
    )
    .await?;
    ownership::source_current(client, source, false).await?;
    let revision = json!({"uid":source.uid(),"generation":source.metadata.generation,
        "namespace":namespace_uid,"deployment":current.uid(),"deploymentGeneration":current.metadata.generation,
        "service":service.uid(),"networkPolicyVersion":policy.metadata.resource_version,"schema":probe.digest});
    outcome.revision = crate::providers::signing::content_digest(
        &serde_json::to_vec(&revision).map_err(|_| "Managed MCP revision serialization failed")?,
    );
    outcome.endpoint = Some(plan.endpoint());
    outcome.tools = Some(probe.names);
    outcome.schema_digest = Some(probe.digest);
    outcome.workload_generation = current.metadata.generation;
    outcome.workload_image = Some(plan.image.clone());
    Ok(outcome)
}

pub(super) async fn cleanup(client: &Client, source: &McpServer) -> Result<bool, String> {
    let mut config = Config::from_env()?;
    if let Some(reference) = source
        .status
        .as_ref()
        .and_then(|status| status.workload_ref.as_deref())
    {
        let (namespace, name) = reference
            .split_once('/')
            .ok_or("Invalid managed MCP workload display reference")?;
        config.namespace = namespace.into();
        config.validate()?;
        let owner = Owner::new(source, namespace)?;
        if name != owner.name {
            return Err(
                "Managed MCP cleanup reference does not match the source UID-derived resource name"
                    .into(),
            );
        }
    } else if source.spec.managed.is_none() {
        return Ok(true);
    }
    ownership::source_current(client, source, source.metadata.deletion_timestamp.is_some()).await?;
    let Some(namespace) = ownership::namespace(client, &config, source, false).await? else {
        return Ok(true);
    };
    let owner = Owner::new(source, &config.namespace)?;
    let uid = namespace
        .uid()
        .ok_or("Managed MCP cleanup namespace UID is missing")?;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &config.namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), &config.namespace);
    let policies: Api<NetworkPolicy> = Api::namespaced(client.clone(), &config.namespace);
    if !ownership::delete(&deployments, &owner, &uid).await? {
        return Ok(false);
    }
    if !ownership::delete(&services, &owner, &uid).await? {
        return Ok(false);
    }
    ownership::delete(&policies, &owner, &uid).await
}
