// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::Cluster;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{ConfigMap, Pod},
};
use kube::api::{Api, DynamicObject, ListParams};

#[cfg(test)]
#[path = "orchestrator_startup_tests.rs"]
mod tests;

const ORCHESTRATOR_POLICY_READY_ATTEMPTS: usize = 180;
const ORCHESTRATOR_POLICY_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(500);

fn funding_declaration(catalog: &ConfigMap) -> &str {
    catalog
        .data
        .as_ref()
        .and_then(|data| data.get("built-in-funding.json"))
        .map(String::as_str)
        .unwrap_or("[]")
}

fn validate_funding_catalog(catalog: &ConfigMap) -> anyhow::Result<()> {
    anyhow::ensure!(catalog.metadata.uid.as_deref().is_some_and(|uid| !uid.is_empty())
        && catalog.metadata.resource_version.as_deref().is_some_and(|version| !version.is_empty())
        && catalog.metadata.deletion_timestamp.is_none()
        && catalog.metadata.annotations.as_ref().and_then(|annotations| annotations.get("kars.azure.com/inference-budget-contracts"))
            .map(String::as_str) == Some("v1"),
        "Orchestrator funding catalog identity is invalid");
    Ok(())
}

fn invalid_orchestrator_source() -> kube::Error {
    kube::Error::Api(kube::core::Status {
        status: Some(kube::core::response::StatusSummary::Failure),
        message: "Existing bridge-orchestrator source identity or inference reference is invalid; startup will not overwrite it".into(),
        reason: "Conflict".into(),
        code: 409,
        ..Default::default()
    }.boxed())
}

fn verify_orchestrator_source(source: &DynamicObject) -> Result<(), kube::Error> {
    use crate::inference_budget_contract::{RootKind, TaskBudgetBinding};

    let uid = source
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .ok_or_else(invalid_orchestrator_source)?;
    if source.metadata.name.as_deref() != Some("bridge-orchestrator")
        || source.metadata.namespace.as_deref() != Some("kars-system")
        || source
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || source.metadata.deletion_timestamp.is_some()
        || source
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|owners| !owners.is_empty())
        || source
            .data
            .pointer("/spec/inferenceRef/name")
            .and_then(serde_json::Value::as_str)
            != Some("bridge-orchestrator-inference")
    {
        return Err(invalid_orchestrator_source());
    }
    if let Some(binding) = source
        .data
        .pointer("/spec/inferenceBudgetRef")
        .filter(|value| !value.is_null())
    {
        let binding: TaskBudgetBinding =
            serde_json::from_value(binding.clone()).map_err(|_| invalid_orchestrator_source())?;
        if binding.root.validate().is_err()
            || binding.root.kind != RootKind::KarsSandbox
            || binding.root.resource.uid != uid
            || binding.root.resource.name != "bridge-orchestrator"
            || binding.root.resource.namespace != "kars-system"
            || binding.task_uid != uid
            || binding.root_task_uid != uid
            || binding.parent_task_uid.is_some()
        {
            return Err(invalid_orchestrator_source());
        }
    }
    Ok(())
}

impl Cluster {
    /// Resolve only the selected actor's governed router. None permits legacy
    /// routing only after checking that no lifetime account pin survives.
    pub async fn funded_orchestrator_route(&self) -> anyhow::Result<Option<(String, String)>> {
        const DENIED: &str = "Orchestrator account pin remains without a funding selection; no direct endpoint or fallback may bypass the same account";
        let controllers: Api<Deployment> =
            Api::namespaced(self.client.clone(), &self.core_namespace());
        let controller = controllers.get_opt("kars-controller").await.map_err(|_| {
            anyhow::anyhow!(
                "Orchestrator funding selection is unavailable; no model call was authorized"
            )
        })?;
        let mut selected = None;
        let mut actors = std::collections::BTreeSet::new();
        if let Some(controller) = &controller {
            let spec = controller.spec.as_ref().and_then(|spec| spec.template.spec.as_ref())
                .ok_or_else(|| anyhow::anyhow!("Controller funding configuration is incomplete; no model call was authorized"))?;
            if spec.containers.is_empty()
                || spec.containers.iter().any(|container| {
                    container
                        .env_from
                        .as_ref()
                        .is_some_and(|sources| !sources.is_empty())
                })
            {
                anyhow::bail!(
                    "Controller funding selection cannot be resolved safely from indirect environment sources"
                );
            }
            if spec
                .containers
                .iter()
                .flat_map(|container| container.env.iter().flatten())
                .any(|env| env.name == "KARS_BUILTIN_INFERENCE_FUNDING")
            {
                anyhow::bail!(
                    "Controller funding uses a retired environment declaration; configure the operator budget catalog"
                );
            }
            let mut controls = spec
                .containers
                .iter()
                .filter(|container| container.name == "controller");
            let control = controls
                .next()
                .ok_or_else(|| anyhow::anyhow!("Funding controller container is missing"))?;
            anyhow::ensure!(
                controls.next().is_none(),
                "Funding controller container is duplicated"
            );
            let literal = |name: &str| -> anyhow::Result<Option<&str>> {
                let mut values = control.env.iter().flatten().filter(|env| env.name == name);
                let Some(value) = values.next() else {
                    return Ok(None);
                };
                anyhow::ensure!(
                    values.next().is_none(),
                    "Controller funding setting is duplicated"
                );
                anyhow::ensure!(
                    value.value_from.is_none(),
                    "Controller funding setting is indirect"
                );
                Ok(Some(value.value.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("Controller funding setting is missing")
                })?))
            };
            match literal("KARS_INFERENCE_BUDGET_ENABLED")? {
                None | Some("" | "false") => {}
                Some("true") => {
                    let catalog_name = literal("KARS_INFERENCE_BUDGET_CATALOG")?
                        .unwrap_or("kars-inference-budget-contracts");
                    anyhow::ensure!(
                        crate::inference_budget_contract::valid_name(catalog_name),
                        "Controller funding catalog name is invalid"
                    );
                    let maps: Api<ConfigMap> =
                        Api::namespaced(self.client.clone(), &self.core_namespace());
                    let catalog = maps.get(catalog_name).await.map_err(|_| anyhow::anyhow!("Orchestrator funding catalog is unavailable; no model call was authorized"))?;
                    validate_funding_catalog(&catalog)?;
                    let encoded = funding_declaration(&catalog);
                    anyhow::ensure!(
                        encoded.len() <= 32_768,
                        "Controller funding selection exceeds its size bound"
                    );
                    let grants: Vec<crate::inference_budget_contract::BuiltinFunding> =
                        serde_json::from_str(encoded).map_err(|_| {
                            anyhow::anyhow!("Controller funding selection is malformed")
                        })?;
                    for grant in grants {
                        // Match controller parsing: an expired unrelated actor must not block this one.
                        grant.validate(i64::MIN).map_err(|_| {
                            anyhow::anyhow!("Controller funding selection is invalid")
                        })?;
                        anyhow::ensure!(
                            actors.insert(grant.root.resource.name.clone()),
                            "Controller funding selection is duplicated"
                        );
                        if grant.root.resource.name == "bridge-orchestrator" {
                            selected = Some(serde_json::to_value(grant)?);
                        }
                    }
                    if let Some(grant) = selected {
                        let route =
                            super::orchestrator_funding::resolve(self, &grant, controller).await?;
                        let current = maps.get(catalog_name).await?;
                        validate_funding_catalog(&current)?;
                        anyhow::ensure!(
                            current.metadata.uid == catalog.metadata.uid
                                && funding_declaration(&current) == encoded,
                            "Orchestrator funding declaration changed during route verification"
                        );
                        let current_controller = controllers.get("kars-controller").await?;
                        anyhow::ensure!(
                            current_controller.metadata.uid == controller.metadata.uid
                                && current_controller.metadata.deletion_timestamp.is_none()
                                && current_controller.spec == controller.spec,
                            "Controller funding configuration changed during route verification"
                        );
                        return Ok(Some(route));
                    }
                }
                Some(_) => anyhow::bail!("Controller funding enablement is invalid"),
            }
        }
        // Inspect source pins as well as current desired controller settings:
        // removing the declaration cannot reopen the unfunded legacy routes.
        for sandbox in self.list_kind_all("KarsSandbox").await.map_err(|_| {
            anyhow::anyhow!(
                "Orchestrator account binding is unavailable; no model call was authorized"
            )
        })? {
            if sandbox.metadata.name.as_deref() != Some("bridge-orchestrator") {
                continue;
            }
            let spec = sandbox
                .data
                .get("spec")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("Orchestrator source binding is malformed"))?;
            if spec
                .get("inferenceBudgetRef")
                .is_some_and(|binding| !binding.is_null())
            {
                anyhow::bail!(DENIED);
            }
        }
        Ok(None)
    }

    /// Ready routers belonging to the standing composer, newest first.
    /// Resolve its runtime namespace from the reconciled source rather than
    /// charging composition to an unrelated customer's Mission or Team account.
    pub async fn running_sandbox_candidates(&self) -> Result<Vec<(String, String)>, kube::Error> {
        let Some(source) = self
            .get_kind(&self.core_namespace(), "KarsSandbox", "bridge-orchestrator")
            .await?
        else {
            return Ok(Vec::new());
        };
        verify_orchestrator_source(&source)?;
        if source
            .data
            .pointer("/spec/suspended")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            return Ok(Vec::new());
        }
        let Some(namespace) = source
            .data
            .pointer("/status/namespace")
            .and_then(serde_json::Value::as_str)
            .filter(|name| crate::inference_budget_contract::valid_name(name))
        else {
            return Ok(Vec::new());
        };
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        let list = pods
            .list(&ListParams::default().labels("kars.azure.com/sandbox=bridge-orchestrator"))
            .await?;
        let mut candidates: Vec<&Pod> = list
            .items
            .iter()
            .filter(|p| {
                if p.metadata.namespace.as_deref() != Some(namespace)
                    || p.metadata
                        .labels
                        .as_ref()
                        .and_then(|labels| labels.get("kars.azure.com/sandbox"))
                        .map(String::as_str)
                        != Some("bridge-orchestrator")
                {
                    return false;
                }
                let phase = p.status.as_ref().and_then(|s| s.phase.as_deref());
                if phase != Some("Running") || p.metadata.deletion_timestamp.is_some() {
                    return false;
                }
                // Registrar-private routers are not general composer candidates.
                if p.metadata.annotations.as_ref().is_some_and(|annotations| {
                    annotations.contains_key("kars.azure.com/sre-registration-uid")
                        || annotations.contains_key("kars.azure.com/sre-private-credential-uid")
                }) {
                    return false;
                }
                p.status
                    .as_ref()
                    .and_then(|s| s.container_statuses.as_ref())
                    .map(|cs| cs.iter().any(|c| c.name == "inference-router" && c.ready))
                    .unwrap_or(false)
            })
            .collect();
        candidates.sort_by(|a, b| {
            let ta = a.metadata.creation_timestamp.as_ref().map(|t| t.0);
            let tb = b.metadata.creation_timestamp.as_ref().map(|t| t.0);
            tb.cmp(&ta)
        });
        Ok(candidates
            .into_iter()
            .filter_map(|p| Some((p.metadata.namespace.clone()?, p.metadata.name.clone()?)))
            .collect())
    }

    /// Drive a real model call through a sandbox's secure inference router,
    /// using the Kubernetes **pods/proxy subresource** — hard-scoped to one
    /// pod, port 8443, and the exact `/v1/chat/completions` path. This is the
    /// only proxy the BFF performs and it is NOT a generic tunnel: it cannot
    /// reach any other port or path. The router still enforces content-safety,
    /// token budgets, and governance on the call — the agent never sees a key.
    ///
    /// `ns` is the sandbox namespace (`kars-<sandbox>`), `pod` the Running pod.
    /// Returns the raw response JSON text from the router.
    pub async fn router_chat(
        &self,
        ns: &str,
        pod: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<String> {
        let path = format!("/api/v1/namespaces/{ns}/pods/{pod}:8443/proxy/v1/chat/completions");
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri(path)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(body)?)?;
        let text = self.client.request_text(req).await?;
        Ok(text)
    }

    /// Drive a model call through a sandbox's inference router using the NATIVE
    /// Anthropic Messages endpoint (`/v1/messages`) via pods/proxy. Claude on
    /// the OpenAI-compatible `/chat/completions` path returns empty content for
    /// the Bridge's composer; the native path returns proper text/`tool_use`
    /// content (the same reason agents use `/v1/messages`). Body is Anthropic-
    /// shaped (`{model, system, messages, max_tokens}`). Returns raw response.
    pub async fn router_messages(
        &self,
        ns: &str,
        pod: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<String> {
        let path = format!("/api/v1/namespaces/{ns}/pods/{pod}:8443/proxy/v1/messages");
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri(path)
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .body(serde_json::to_vec(body)?)?;
        let text = self.client.request_text(req).await?;
        Ok(text)
    }

    /// Ensure the standing **orchestrator sandbox** exists — a persistent,
    /// non-ephemeral sandbox whose inference router the Bridge orchestrator
    /// (compose) always routes through. Without it, compose can only borrow a
    /// running agent's router, so on a teams-only cluster (all ephemeral runs)
    /// it has no cold-start inference path. Both objects are create-only: an
    /// existing source is verified and reused without resetting suspension,
    /// operator configuration or a controller-authored lifetime account binding.
    pub async fn ensure_orchestrator_sandbox(&self) -> Result<(), kube::Error> {
        const NAME: &str = "bridge-orchestrator";
        const NS: &str = "kars-system";
        if let Some(source) = self.get_kind(NS, "KarsSandbox", NAME).await? {
            verify_orchestrator_source(&source)?;
            // Never recreate a bound source's missing policy from current defaults.
            if self
                .get_kind(NS, "InferencePolicy", &format!("{NAME}-inference"))
                .await?
                .is_none()
            {
                return Err(invalid_orchestrator_source());
            }
            return Ok(());
        }
        if self
            .get_kind(NS, "InferencePolicy", &format!("{NAME}-inference"))
            .await?
            .is_none()
        {
            let model = self.controller_bootstrap_model().await?;
            let mut spec = serde_json::json!({"appliesTo": {"sandboxName": NAME}});
            if let Some(model) = model {
                spec["modelPreference"] = serde_json::json!({"primary": model});
            }
            let inference = serde_json::json!({
                "apiVersion": "kars.azure.com/v1alpha1",
                "kind": "InferencePolicy",
                "metadata": { "name": format!("{NAME}-inference"), "namespace": NS,
                    "labels": { "kars.azure.com/managed-by": "kars-bridge" } },
                "spec": spec,
            });
            // Bootstrap once; restarting Bridge must not reset the selected route.
            match self.create_kind(NS, "InferencePolicy", inference).await {
                Ok(_) => {}
                Err(kube::Error::Api(error)) if error.code == 409 => {}
                Err(error) => return Err(error),
            }
        }
        let sandbox = serde_json::json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsSandbox",
            "metadata": { "name": NAME, "namespace": NS,
                "labels": { "kars.azure.com/managed-by": "kars-bridge", "kars.azure.com/orchestrator": "true" } },
            "spec": {
                "runtime": { "kind": "OpenClaw", "openclaw": {} },
                "inferenceRef": { "name": format!("{NAME}-inference") },
                "sandbox": { "isolation": "standard" },
                "networkPolicy": { "defaultDeny": true },
                "governance": { "enabled": true, "toolPolicyRef": { "name": "kars-default" }, "trustThreshold": 0 },
                "agent": { "instructions": "Standing orchestrator inference host for the kars Bridge composer. Stay idle; your router serves compose requests." },
            },
        });
        match self.create_kind(NS, "KarsSandbox", sandbox).await {
            Ok(source) => verify_orchestrator_source(&source),
            Err(kube::Error::Api(error)) if error.code == 409 => {
                let source = self
                    .get_kind(NS, "KarsSandbox", NAME)
                    .await?
                    .ok_or_else(invalid_orchestrator_source)?;
                verify_orchestrator_source(&source)
            }
            Err(error) => Err(error),
        }
    }

    /// Model pinned to the persistent Bridge composer sandbox. Composition calls
    /// must use this route rather than an unrelated cluster-default deployment.
    pub async fn bridge_orchestrator_model(&self) -> Option<String> {
        self.get_kind(
            "kars-system",
            "InferencePolicy",
            "bridge-orchestrator-inference",
        )
        .await
        .ok()
        .flatten()?
        .data
        .pointer("/spec/modelPreference/primary/deployment")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|model| !model.trim().is_empty())
    }

    pub async fn bridge_orchestrator_model_route(
        &self,
    ) -> Result<Option<crate::kars::task::TaskModel>, kube::Error> {
        let Some(policy) = self
            .get_kind(
                "kars-system",
                "InferencePolicy",
                "bridge-orchestrator-inference",
            )
            .await?
        else {
            return Ok(None);
        };
        if policy
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get("kars.azure.com/model-default-source"))
            .map(String::as_str)
            == Some("cluster")
        {
            return self.model_default().await?.map(Some).ok_or_else(|| kube::Error::Api(kube::core::Status {
                code: 422, status: Some(kube::core::response::StatusSummary::Failure), reason: "ModelDefaultUnavailable".into(),
                message: "The Orchestrator inherits the cluster default, but no default is saved.".into(),
                ..Default::default()
            }.boxed()));
        }
        policy
            .data
            .pointer("/spec/modelPreference/primary")
            .map(|value| serde_json::from_value(value.clone()).map_err(kube::Error::SerdeError))
            .transpose()
    }

    pub async fn wait_for_bridge_orchestrator_model(
        &self,
        provider: &str,
        deployment: &str,
    ) -> Result<(), String> {
        let policy_ready = |policy: &DynamicObject| {
            let generation_matches = policy
                .data
                .pointer("/status/observedGeneration")
                .and_then(serde_json::Value::as_i64)
                == policy.metadata.generation;
            generation_matches
                && policy.metadata.generation.is_some()
                && policy
                    .data
                    .pointer("/status/compiledDigest")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|digest| {
                        !digest.is_empty()
                            && policy
                                .data
                                .pointer("/status/loadedDigest")
                                .and_then(serde_json::Value::as_str)
                                == Some(digest)
                    })
                && policy
                    .data
                    .pointer("/spec/modelPreference/primary/provider")
                    .and_then(serde_json::Value::as_str)
                    == Some(provider)
                && policy
                    .data
                    .pointer("/spec/modelPreference/primary/deployment")
                    .and_then(serde_json::Value::as_str)
                    == Some(deployment)
                && policy
                    .data
                    .pointer("/status/conditions")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|conditions| {
                        conditions.iter().any(|condition| {
                            condition.get("type").and_then(serde_json::Value::as_str)
                                == Some("Ready")
                                && condition.get("status").and_then(serde_json::Value::as_str)
                                    == Some("True")
                        })
                    })
        };
        if self
            .get_kind(
                "kars-system",
                "InferencePolicy",
                "bridge-orchestrator-inference",
            )
            .await
            .map_err(|error| error.to_string())?
            .as_ref()
            .is_some_and(policy_ready)
        {
            return Ok(());
        }
        // Composition consumes operator-selected policy; it must never rewrite
        // an override, clear fallbacks, or restart a standing actor.
        // Wait for the exact generation + router-echo confirmation from every
        // referencing sandbox, not merely the policy write or a pod-start delay.
        for _ in 0..ORCHESTRATOR_POLICY_READY_ATTEMPTS {
            let ready = self
                .get_kind(
                    "kars-system",
                    "InferencePolicy",
                    "bridge-orchestrator-inference",
                )
                .await
                .map_err(|error| error.to_string())?
                .as_ref()
                .is_some_and(policy_ready);
            if ready {
                return Ok(());
            }
            tokio::time::sleep(ORCHESTRATOR_POLICY_POLL_INTERVAL).await;
        }
        Err(format!(
            "timed out waiting for the bridge orchestrator router to enforce {provider}/{deployment}"
        ))
    }
}
