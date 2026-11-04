// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only route selection, not budget authorization. The selected router's
//! private Pod identity still obtains every reservation from the existing broker.

use super::Cluster;
use crate::inference_budget_contract::{
    BuiltinFunding, RouterBinding, TaskBudgetBinding, ledger::Ledger,
};
use anyhow::{Result, ensure};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, ReplicaSet},
        core::v1::{Container, Namespace, Pod},
    },
    apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference},
};
use kube::{Api, ResourceExt, api::ListParams};
use serde_json::{Value, json};

const ACTOR: &str = "bridge-orchestrator";
const RUNTIME: &str = "kars-bridge-orchestrator";
const READY: &str = "governed inference authority and model contracts available";

fn text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Funded orchestrator identity is incomplete at {pointer}"))
}

fn name(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty()
            && value.len() <= 253
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.'),
        "Funded orchestrator resource identity is invalid"
    );
    Ok(value)
}

fn live(meta: &ObjectMeta) -> Result<&str> {
    ensure!(
        meta.deletion_timestamp.is_none()
            && meta
                .resource_version
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
        "Funded orchestrator resource is stale or terminating"
    );
    meta.uid
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Funded orchestrator resource has no UID"))
}

fn env<'a>(container: &'a Container, key: &str) -> Result<&'a str> {
    let values: Vec<_> = container
        .env
        .iter()
        .flatten()
        .filter(|env| env.name == key)
        .collect();
    ensure!(
        values.len() == 1 && values[0].value_from.is_none(),
        "Funded orchestrator {key} must be explicit and unique"
    );
    values[0]
        .value
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Funded orchestrator {key} is missing"))
}

fn router(containers: &[Container]) -> Result<&Container> {
    let matches: Vec<_> = containers
        .iter()
        .filter(|c| c.name == "inference-router")
        .collect();
    ensure!(
        matches.len() == 1,
        "Funded orchestrator router is ambiguous"
    );
    Ok(matches[0])
}

fn limits(value: &Value) -> Result<Value> {
    let tokens = value
        .get("tokens")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && *value <= i64::MAX as u64)
        .ok_or_else(|| anyhow::anyhow!("Funded orchestrator token allocation is invalid"))?;
    let money = match value.get("usdMicros") {
        None | Some(Value::Null) => 0,
        Some(value) => value
            .as_u64()
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| anyhow::anyhow!("Funded orchestrator currency allocation is invalid"))?,
    };
    let mut normalized = json!({"tokens":tokens});
    if money > 0 {
        normalized["usdMicros"] = json!(money);
    }
    Ok(normalized)
}

fn grant_digest(grant: &Value) -> Result<String> {
    let funding: BuiltinFunding = serde_json::from_value(grant.clone())?;
    // Original authority stays immutable; the account's renewal chain supplies expiry.
    funding.validate(i64::MIN)?;
    ensure!(
        funding.root.resource.name == ACTOR,
        "Funding source is not the orchestrator"
    );
    let mut canonical = grant.clone();
    canonical.sort_all_objects();
    Ok(format!(
        "sha256:{}",
        crate::providers::signing::sha256_hex(serde_json::to_vec(&canonical)?)
    ))
}

fn verify_account(account: &Value, grant: &Value, binding: &Value, digest: &str) -> Result<()> {
    let uid = text(grant, "/root/resource/uid")?;
    let generation = account
        .pointer("/metadata/generation")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow::anyhow!("Funding account generation is missing"))?;
    ensure!(
        account["metadata"]["uid"] == binding["account"]["uid"]
            && account["metadata"]["name"] == binding["account"]["name"]
            && account["metadata"]["namespace"] == binding["account"]["namespace"]
            && account["metadata"]["annotations"]["kars.azure.com/inference-budget-bootstrap"]
                == "sealed"
            && account["metadata"]["labels"]["app.kubernetes.io/managed-by"]
                == "kars-inference-budget"
            && account["spec"]["scope"] == "GovernedInference"
            && account["spec"]["root"] == grant["root"]
            && limits(&account["spec"]["limits"])? == limits(&grant["limits"])?
            && account["status"]["phase"] == "Active"
            && account["status"]["observedGeneration"] == generation,
        "Funded orchestrator account is replaced, unsealed, inactive or stale"
    );
    let conditions = account["status"]["conditions"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Funding account readiness is missing"))?;
    for kind in ["Ready", "LedgerValid"] {
        let matches: Vec<_> = conditions
            .iter()
            .filter(|condition| condition["type"] == kind)
            .collect();
        ensure!(
            matches.len() == 1
                && matches[0]["status"] == "True"
                && matches[0]["observedGeneration"] == generation,
            "Funded orchestrator account is not currently {kind}"
        );
    }
    let ledger = &account["status"]["ledger"];
    let recorded: Ledger = serde_json::from_value(ledger.clone())?;
    recorded.validate()?;
    recorded.validate_funding_expiry(
        &serde_json::from_value::<BuiltinFunding>(grant.clone())?,
        chrono::Utc::now().timestamp(),
    )?;
    let node = &ledger["nodes"][uid];
    ensure!(
        ledger["version"] == "governed-inference/v1"
            && ledger["scope"] == "GovernedInference"
            && ledger["phase"] == "Active"
            && ledger["accountUid"] == binding["account"]["uid"]
            && ledger["root"] == grant["root"]
            && limits(&ledger["limits"])? == limits(&grant["limits"])?
            && node["active"] == true
            && node["authority"]["task"] == grant["root"]["resource"]
            && node["authority"]["parentUid"].is_null()
            && node["authority"]["rootTaskUid"] == uid
            && node["authority"]["authorizationDigest"] == digest
            && node["authority"]["effectiveAuthorization"] == *grant
            && limits(&node["authority"]["limits"])? == limits(&grant["limits"])?,
        "Funded orchestrator ledger or source authority differs from its pin"
    );
    Ok(())
}

fn owner<'a>(meta: &'a ObjectMeta, kind: &str) -> Result<&'a OwnerReference> {
    let owners: Vec<_> = meta
        .owner_references
        .iter()
        .flatten()
        .filter(|o| o.controller == Some(true))
        .collect();
    ensure!(
        owners.len() == 1 && owners[0].api_version == "apps/v1" && owners[0].kind == kind,
        "Funded orchestrator workload ownership is unverified"
    );
    Ok(owners[0])
}

fn ready_pod(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod.status.as_ref().is_some_and(|status| {
            status.phase.as_deref() == Some("Running")
                && status
                    .conditions
                    .iter()
                    .flatten()
                    .any(|c| c.type_ == "Ready" && c.status == "True")
                && status
                    .container_statuses
                    .iter()
                    .flatten()
                    .any(|c| c.name == "inference-router" && c.ready)
        })
}

pub(super) async fn resolve(
    cluster: &Cluster,
    grant: &Value,
    controller: &Deployment,
) -> Result<(String, String)> {
    live(&controller.metadata)?;
    let controller_spec = controller
        .spec
        .as_ref()
        .and_then(|spec| spec.template.spec.as_ref())
        .ok_or_else(|| anyhow::anyhow!("Funding controller is incomplete"))?;
    let control = controller_spec
        .containers
        .iter()
        .find(|c| c.name == "controller")
        .ok_or_else(|| anyhow::anyhow!("Funding controller container is missing"))?;
    ensure!(
        env(control, "KARS_INFERENCE_BUDGET_ENABLED")? == "true",
        "Funding broker is not enabled"
    );
    let image_digest = env(control, "KARS_INFERENCE_BUDGET_ROUTER_DIGEST")?;
    ensure!(
        image_digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|c| c.is_ascii_hexdigit())),
        "Funding router image digest is invalid"
    );
    // Use the maintained wire type so optional fields canonicalize exactly as
    // the controller's persisted effective authorization and digest do.
    let funding: BuiltinFunding = serde_json::from_value(grant.clone())?;
    let grant = &serde_json::to_value(funding)?;
    let digest = grant_digest(grant)?;
    let workspace = text(grant, "/root/resource/namespace")?;
    let source = cluster
        .get_kind(workspace, "KarsSandbox", ACTOR)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Funded orchestrator source is missing"))?;
    ensure!(
        live(&source.metadata)? == text(grant, "/root/resource/uid")?
            && source
                .metadata
                .owner_references
                .as_ref()
                .is_none_or(Vec::is_empty),
        "Funded orchestrator Sandbox UID/ownership changed"
    );
    let binding = &source.data["spec"]["inferenceBudgetRef"];
    ensure!(
        binding["scope"] == "GovernedInference"
            && binding["root"] == grant["root"]
            && binding["taskUid"] == grant["root"]["resource"]["uid"]
            && binding["rootTaskUid"] == grant["root"]["resource"]["uid"]
            && binding["parentTaskUid"].is_null()
            && binding["authorizationDigest"] == digest
            && binding["account"]["namespace"] == cluster.core_namespace()
            && binding["account"]["name"]
                == format!("inference-budget-{}", live(&source.metadata)?),
        "Funded orchestrator source/account pin is missing or changed"
    );
    name(text(binding, "/account/uid")?)?;
    let account = cluster
        .get_kind(
            text(binding, "/account/namespace")?,
            "KarsBudgetAccount",
            name(text(binding, "/account/name")?)?,
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Funding account read unavailable; exact account GET permission is required"
            )
        })?
        .ok_or_else(|| anyhow::anyhow!("Funding account is missing; no zero or fallback"))?;
    live(&account.metadata)?;
    verify_account(&serde_json::to_value(&account)?, grant, binding, &digest)?;

    let namespaces: Api<Namespace> = Api::all(cluster.client.clone());
    for (namespace, pointer) in [
        (workspace, "/root/workspaceUid"),
        ("kube-system", "/root/clusterUid"),
    ] {
        let value = namespaces.get(namespace).await?;
        ensure!(
            live(&value.metadata)? == text(grant, pointer)?,
            "Funding namespace incarnation changed"
        );
    }
    let namespace = namespaces.get(RUNTIME).await?;
    let runtime_uid = live(&namespace.metadata)?;
    let annotations = namespace.annotations();
    ensure!(
        namespace
            .labels()
            .get("kars.azure.com/inference-budget")
            .map(String::as_str)
            == Some("v1")
            && annotations
                .get("kars.azure.com/namespace-claim-version")
                .map(String::as_str)
                == Some("v1")
            && annotations
                .get("kars.azure.com/sandbox-namespace")
                .map(String::as_str)
                == Some(workspace)
            && annotations
                .get("kars.azure.com/sandbox-name")
                .map(String::as_str)
                == Some(ACTOR)
            && annotations
                .get("kars.azure.com/sandbox-uid")
                .map(String::as_str)
                == Some(live(&source.metadata)?),
        "Funded orchestrator runtime namespace claim is stale"
    );
    let pods: Api<Pod> = Api::namespaced(cluster.client.clone(), RUNTIME);
    let candidates = pods
        .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={ACTOR}")))
        .await?;
    let ready: Vec<_> = candidates
        .items
        .iter()
        .filter(|pod| ready_pod(pod))
        .collect();
    ensure!(
        ready.len() == 1,
        "Funded orchestrator needs exactly one Ready Pod; no arbitrary fallback"
    );
    let pod = ready[0];
    let pod_uid = live(&pod.metadata)?;
    let pod_spec = pod
        .spec
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Funding Pod spec is absent"))?;
    ensure!(
        pod_spec.service_account_name.as_deref() == Some("sandbox"),
        "Funding Pod service account changed"
    );
    let actual = router(&pod_spec.containers)?;
    let replica = owner(&pod.metadata, "ReplicaSet")?;
    let replicas: Api<ReplicaSet> = Api::namespaced(cluster.client.clone(), RUNTIME);
    let rs = replicas.get(name(&replica.name)?).await?;
    ensure!(
        live(&rs.metadata)? == replica.uid,
        "Funding ReplicaSet was replaced"
    );
    let owner = owner(&rs.metadata, "Deployment")?;
    ensure!(owner.name == ACTOR, "Funding Deployment source changed");
    let deployments: Api<Deployment> = Api::namespaced(cluster.client.clone(), RUNTIME);
    let deployment = deployments.get(ACTOR).await?;
    ensure!(
        live(&deployment.metadata)? == owner.uid,
        "Funding Deployment was replaced"
    );
    let spec = deployment
        .spec
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Funding Deployment spec is absent"))?;
    ensure!(
        spec.replicas.is_none_or(|count| count > 0),
        "Funded orchestrator is inactive"
    );
    let template = router(
        &spec
            .template
            .spec
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Funding router template is absent"))?
            .containers,
    )?;
    ensure!(
        actual.image == template.image
            && actual
                .image
                .as_deref()
                .is_some_and(|image| image.ends_with(&format!("@{image_digest}")))
            && actual.command == template.command
            && actual.args == template.args,
        "Funded router image or entrypoint is not the current qualified template"
    );
    ensure!(
        env(actual, "KARS_INFERENCE_BUDGET_REQUIRED")? == "true"
            && env(template, "KARS_INFERENCE_BUDGET_REQUIRED")? == "true",
        "Funding enforcement cannot be disabled"
    );
    let parsed: RouterBinding =
        serde_json::from_str(env(actual, "KARS_INFERENCE_BUDGET_BINDING")?)?;
    ensure!(
        parsed
            .privacy_epoch
            .as_deref()
            .is_some_and(|epoch| !epoch.is_empty()),
        "Funding router privacy epoch is missing"
    );
    let router_binding = serde_json::to_value(parsed)?;
    let template_binding = serde_json::to_value(serde_json::from_str::<RouterBinding>(env(
        template,
        "KARS_INFERENCE_BUDGET_BINDING",
    )?)?)?;
    // Kubernetes prunes nullable CRD fields; compare both sides using the wire type.
    let source_binding = serde_json::to_value(serde_json::from_value::<TaskBudgetBinding>(
        binding.clone(),
    )?)?;
    ensure!(
        router_binding == template_binding
            && router_binding["task"] == source_binding
            && router_binding["sandbox"] == grant["root"]["resource"]
            && router_binding["runtimeNamespace"] == RUNTIME
            && router_binding["runtimeNamespaceUid"] == runtime_uid,
        "Router account, source identity or privacy epoch does not match its current template"
    );

    // This exact governed-only response traverses the private broker catalog:
    // account UID, live Pod token/owner, current privacy epoch and contracts.
    // Neither a Pod Ready bit nor legacy /readyz "ok" establishes custody.
    let pod_name = name(pod.metadata.name.as_deref().unwrap_or_default())?;
    let request = http::Request::builder()
        .method(http::Method::GET)
        .uri(format!(
            "/api/v1/namespaces/{RUNTIME}/pods/{pod_name}:8443/proxy/readyz"
        ))
        .body(Vec::new())?;
    ensure!(
        cluster.client.request_text(request).await?.trim() == READY,
        "Router did not prove current governed account custody"
    );
    let current = pods.get(pod_name).await?;
    ensure!(
        live(&current.metadata)? == pod_uid && current.spec == pod.spec && ready_pod(&current),
        "Funding Pod changed during route verification"
    );
    let current_source = cluster
        .get_kind(workspace, "KarsSandbox", ACTOR)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Funding source disappeared during route verification"))?;
    ensure!(
        live(&current_source.metadata)? == live(&source.metadata)?
            && current_source.data["spec"]["inferenceBudgetRef"] == *binding,
        "Funding account pin changed during route verification"
    );
    let current_account = cluster
        .get_kind(
            text(binding, "/account/namespace")?,
            "KarsBudgetAccount",
            text(binding, "/account/name")?,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("Funding account disappeared during route verification"))?;
    live(&current_account.metadata)?;
    verify_account(
        &serde_json::to_value(current_account)?,
        grant,
        binding,
        &digest,
    )?;
    grant_digest(grant)?;
    Ok((RUNTIME.into(), pod_name.into()))
}
