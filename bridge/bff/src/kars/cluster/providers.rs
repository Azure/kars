// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::Cluster;
use k8s_openapi::api::core::v1::EnvVar;
use kube::api::Api;

pub(super) fn normalize_registry_host(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub(super) fn image_registry_host(image: &str) -> String {
    let first = image.trim().split('/').next().unwrap_or_default();
    if first.contains('.') || first.contains(':') || first == "localhost" {
        first.to_ascii_lowercase()
    } else {
        "docker.io".to_string()
    }
}

pub(super) fn public_registry(registry: &str) -> bool {
    matches!(
        registry,
        "docker.io" | "registry-1.docker.io" | "mcr.microsoft.com" | "public.ecr.aws"
    )
}

/// Classify the inherited inference provider from an optional `KARS_PROVIDER`
/// override plus the configured endpoint hosts. Mirrors the router's detection
/// (`inference-router/src/config.rs`): the three providers kars supports are
/// GitHub Copilot, GitHub Models, and Azure AI Foundry. Returns `(id, label,
/// note)`, or `None` when nothing identifiable is configured.
pub(super) fn classify_provider(
    override_val: Option<&str>,
    endpoints: &[String],
    token_hint: Option<&str>,
) -> Option<(String, String, String)> {
    let host_has = |needle: &str| endpoints.iter().any(|e| e.contains(needle));
    let copilot = (
        "github-copilot",
        "GitHub Copilot",
        "Models served through your GitHub Copilot subscription (GitHub-hosted inference).",
    );
    let gh_models = (
        "github-models",
        "GitHub Models",
        "Models served through GitHub Models (OpenAI-compatible, GitHub-hosted).",
    );
    let foundry = (
        "azure-foundry",
        "Azure AI Foundry",
        "Models served through your Azure AI Foundry project.",
    );
    // A local in-cluster model deployed via the "Local model" wizard —
    // its endpoint is always a Service DNS name inside the Bridge-owned
    // kars-local-inference namespace (see docs/local-inference.md). Checked
    // before the generic Foundry fallback so promoting one to the cluster
    // default doesn't display as a misleading "Azure AI Foundry" label.
    let local = (
        "local-inference",
        "Local model (in-cluster)",
        "Models served by an in-cluster deployment — no external API, no per-token billing.",
    );
    let is_local_host = host_has(".kars-local-inference.svc.cluster.local");
    // A GitHub OAuth/user token (`gho_`/`ghu_`) indicates a Copilot login; a
    // classic PAT (`ghp_`) indicates free GitHub Models.
    let is_oauth_token =
        matches!(token_hint, Some(t) if t.starts_with("gho_") || t.starts_with("ghu_"));
    let on_github = host_has("models.github.ai") || host_has("models.inference.ai.azure.com");
    let pick = match override_val {
        // Explicit operator declaration is authoritative.
        Some("github-copilot") | Some("copilot") => copilot,
        Some("github-models") => gh_models,
        Some("foundry") | Some("azure-openai") | Some("azure-foundry") => foundry,
        // Otherwise infer from endpoint + token kind.
        _ if host_has("api.githubcopilot.com") => copilot,
        _ if on_github && is_oauth_token => copilot,
        _ if on_github => gh_models,
        _ if is_local_host => local,
        _ if !endpoints.is_empty() => foundry,
        _ => return None,
    };
    Some((pick.0.to_string(), pick.1.to_string(), pick.2.to_string()))
}

pub(super) fn parse_model_catalog(value: &str) -> Vec<String> {
    let value = value.trim();
    let entries = if value.starts_with('[') {
        serde_json::from_str::<Vec<String>>(value).unwrap_or_default()
    } else {
        value.split(',').map(str::to_string).collect()
    };
    entries
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect()
}

fn configured_models(envs: &[EnvVar]) -> (Option<String>, Vec<String>) {
    let mut default = None;
    let mut legacy_default = None;
    let mut catalog = Vec::new();
    let mut legacy_catalog = Vec::new();
    for e in envs {
        let Some(val) = &e.value else { continue };
        match e.name.as_str() {
            "KARS_TASK_DEFAULT_MODEL" if !val.trim().is_empty() => {
                default = Some(val.clone());
            }
            "AZURE_OPENAI_DEPLOYMENT" if !val.trim().is_empty() => {
                legacy_default = Some(val.clone());
            }
            "KARS_MODEL_CATALOG" => {
                catalog.extend(parse_model_catalog(val));
            }
            "FOUNDRY_DEPLOYMENTS" => legacy_catalog.extend(parse_model_catalog(val)),
            _ => {}
        }
    }
    if !envs
        .iter()
        .any(|env| env.name == "KARS_MODEL_CATALOG" && env.value.is_some())
    {
        catalog = legacy_catalog;
    }
    (default.or(legacy_default), catalog)
}

fn incomplete_bootstrap_model() -> kube::Error {
    kube::Error::Api(kube::core::Status {
        status: Some(kube::core::response::StatusSummary::Failure),
        reason: "InferenceDefaultsUnavailable".into(),
        code: 503,
        message: "Core inference defaults are incomplete or indirect; configure the provider and default model before initializing the Bridge orchestrator".into(),
        ..Default::default()
    }.boxed())
}

pub(super) fn configured_bootstrap_model(
    envs: &[EnvVar],
) -> Result<Option<crate::kars::task::TaskModel>, kube::Error> {
    let (default, catalog) = configured_models(envs);
    if let Some(provider) = envs
        .iter()
        .find(|env| env.name == "KARS_TASK_DEFAULT_PROVIDER")
    {
        let provider = provider
            .value
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(incomplete_bootstrap_model)?;
        let deployment = envs
            .iter()
            .find(|env| env.name == "KARS_TASK_DEFAULT_MODEL")
            .filter(|env| env.value_from.is_none())
            .and_then(|env| env.value.as_deref())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(incomplete_bootstrap_model)?;
        return Ok(Some(crate::kars::task::TaskModel {
            provider: provider.into(),
            deployment: deployment.into(),
        }));
    }
    Ok(configured_provider(envs).and_then(|(provider, _, _)| {
        default
            .or_else(|| catalog.into_iter().next())
            .map(|deployment| crate::kars::task::TaskModel {
                provider,
                deployment,
            })
    }))
}

pub(super) fn inherited_provider_registered(
    envs: &[EnvVar],
    keys: &std::collections::BTreeMap<String, String>,
) -> bool {
    let Some((provider, _, _)) = configured_provider(envs) else {
        return true;
    };
    if provider == "github-copilot" {
        return keys
            .get("COPILOT_GITHUB_TOKEN")
            .is_some_and(|value| !value.trim().is_empty());
    }
    configured_endpoint(envs).is_some_and(|endpoint| {
        keys.iter().any(|(key, value)| {
            key.starts_with("KARS_PROVIDER_")
                && key.ends_with("_ENDPOINT")
                && value.trim_end_matches('/') == endpoint.trim_end_matches('/')
        })
    })
}

fn configured_endpoint(envs: &[EnvVar]) -> Option<&str> {
    envs.iter()
        .find_map(|env| {
            (env.name == "FOUNDRY_ENDPOINT")
                .then(|| {
                    env.value
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                })
                .flatten()
        })
        .or_else(|| {
            envs.iter().find_map(|env| {
                (env.name == "AZURE_OPENAI_ENDPOINT")
                    .then(|| {
                        env.value
                            .as_deref()
                            .filter(|value| !value.trim().is_empty())
                    })
                    .flatten()
            })
        })
}

fn configured_provider(envs: &[EnvVar]) -> Option<(String, String, String)> {
    let mut provider_override = None;
    let mut endpoints = Vec::new();
    let mut token_hint = None;
    for e in envs {
        let Some(val) = &e.value else { continue };
        match e.name.as_str() {
            "KARS_PROVIDER" | "KARS_INFERENCE_PROVIDER" if !val.is_empty() => {
                provider_override = Some(val.clone());
            }
            "FOUNDRY_ENDPOINT" | "FOUNDRY_PROJECT_ENDPOINT" | "AZURE_OPENAI_ENDPOINT"
                if !val.trim().is_empty() =>
            {
                endpoints.push(val.clone());
            }
            "AZURE_OPENAI_API_KEY" | "GITHUB_TOKEN" | "COPILOT_GITHUB_TOKEN"
                if token_hint.is_none() && !val.is_empty() =>
            {
                token_hint = Some(val.chars().take(4).collect::<String>());
            }
            _ => {}
        }
    }
    classify_provider(
        provider_override.as_deref(),
        &endpoints,
        token_hint.as_deref(),
    )
}

pub type FoundryConnection = (Option<String>, Option<String>, Option<String>, bool);

fn parse_foundry_connection(
    controller: k8s_openapi::api::core::v1::Container,
) -> Result<FoundryConnection, String> {
    let envs = controller.env.unwrap_or_default();
    if controller
        .env_from
        .as_ref()
        .is_some_and(|sources| !sources.is_empty())
        && !envs
            .iter()
            .any(|env| env.name == "FOUNDRY_PROJECT_ENDPOINT")
    {
        return Err("Foundry discovery cannot resolve imported project configuration; not selecting built-in memory".into());
    }
    let mut project = None;
    let mut inference = None;
    let mut store = None;
    let mut has_key = false;
    for env in envs {
        match env.name.as_str() {
            "FOUNDRY_PROJECT_ENDPOINT" => {
                if env.value_from.is_some() {
                    return Err("Foundry discovery cannot resolve an indirect project endpoint; not selecting built-in memory".into());
                }
                project = env.value.filter(|v| !v.trim().is_empty());
            }
            "FOUNDRY_ENDPOINT" => inference = env.value.filter(|v| !v.trim().is_empty()),
            "FOUNDRY_MEMORY_STORE_ID" => store = env.value.filter(|v| !v.trim().is_empty()),
            "FOUNDRY_API_KEY" => {
                has_key =
                    env.value_from.is_some() || env.value.as_ref().is_some_and(|v| !v.is_empty());
            }
            _ => {}
        }
    }
    Ok((project, inference, store, has_key))
}

impl Cluster {
    async fn require_registered_default_before_switch(
        &self,
        next_endpoint: Option<&str>,
    ) -> Result<(), kube::Error> {
        use k8s_openapi::api::apps::v1::Deployment;
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let current = api.get("kars-controller").await?;
        let envs: Vec<_> = current
            .spec
            .and_then(|spec| spec.template.spec)
            .into_iter()
            .flat_map(|spec| spec.containers)
            .flat_map(|container| container.env.unwrap_or_default())
            .collect();
        if next_endpoint.is_some_and(|next| {
            configured_endpoint(&envs)
                .is_some_and(|current| current.trim_end_matches('/') == next.trim_end_matches('/'))
        }) && configured_provider(&envs).is_some_and(|(id, _, _)| id != "github-copilot")
        {
            return Ok(());
        }
        let keys = self.additional_provider_keys().await?;
        if !inherited_provider_registered(&envs, &keys) {
            return Err(kube::Error::Api(kube::core::Status {
                status: Some(kube::core::response::StatusSummary::Failure),
                message: "Connect the current default as an additional provider before switching, so its endpoint, credentials and models remain available.".into(),
                reason: "ProviderRegistrationRequired".into(),
                code: 409,
                ..Default::default()
            }.boxed()));
        }
        Ok(())
    }

    /// Read the provider/model pair from one Core revision. An explicitly selected
    /// provider can differ from the bootstrap endpoint (an additional provider).
    pub async fn controller_bootstrap_model(
        &self,
    ) -> Result<Option<crate::kars::task::TaskModel>, kube::Error> {
        use k8s_openapi::api::apps::v1::Deployment;
        let deploys: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let deployment = deploys.get("kars-controller").await?;
        let controller = deployment
            .spec
            .and_then(|spec| spec.template.spec)
            .and_then(|spec| {
                spec.containers
                    .into_iter()
                    .find(|container| container.name == "controller")
            })
            .ok_or_else(incomplete_bootstrap_model)?;
        configured_bootstrap_model(controller.env.as_deref().unwrap_or_default())
    }

    /// The model deployments this cluster is configured to serve, read from the
    /// controller Deployment's environment (`KARS_TASK_DEFAULT_MODEL`,
    /// `AZURE_OPENAI_DEPLOYMENT`, JSON-array `FOUNDRY_DEPLOYMENTS`, or legacy CSV).
    /// This is the authoritative "what can actually run here" fact — the same
    /// values the controller stamps onto a task's InferencePolicy. Best-effort:
    /// an unreadable Deployment yields an empty list (honest, not an error), so
    /// the launch package degrades to the controller default rather than lying.
    pub async fn controller_models(&self) -> (Option<String>, Vec<String>) {
        use k8s_openapi::api::apps::v1::Deployment;
        let deploys: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let Ok(Some(d)) = deploys.get_opt("kars-controller").await else {
            return (None, Vec::new());
        };
        let envs: Vec<_> = d
            .spec
            .and_then(|s| s.template.spec)
            .map(|ps| ps.containers)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.env.unwrap_or_default())
            .collect();
        configured_models(&envs)
    }

    /// The GitHub token wired for GitHub Copilot — checked in BOTH places
    /// Copilot can be configured: the shared providers secret (an additional
    /// provider, or one signed-in via the wizard's device login) FIRST, then
    /// the controller's `COPILOT_GITHUB_TOKEN` env (the cluster default). Used
    /// to fetch the seat's LIVE model catalog so the Model catalogue +
    /// orchestrator reflect what Copilot actually serves. `None` when unset.
    pub async fn controller_copilot_token(&self) -> Option<String> {
        // Wizard sign-in / additional-provider path stores it here.
        if let Ok(keys) = self
            .read_secret_all("kars-system", "kars-inference-providers")
            .await
            && let Some(t) = keys
                .get("COPILOT_GITHUB_TOKEN")
                .filter(|v| !v.trim().is_empty())
        {
            return Some(t.clone());
        }
        use k8s_openapi::api::apps::v1::Deployment;
        let deploys: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let d = deploys.get_opt("kars-controller").await.ok().flatten()?;
        d.spec
            .and_then(|s| s.template.spec)
            .map(|ps| ps.containers)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.env.unwrap_or_default())
            .find(|e| e.name == "COPILOT_GITHUB_TOKEN")
            .and_then(|e| e.value)
            .filter(|v| !v.trim().is_empty())
    }
    /// fact chosen at cluster setup, NOT something the Bridge picks. kars
    /// supports exactly three: GitHub Copilot, GitHub Models, and Azure AI
    /// Foundry. The classification mirrors the inference-router's own endpoint
    /// detection (`inference-router/src/config.rs`): a `KARS_PROVIDER` override
    /// wins, otherwise the configured endpoint host decides. Returns
    /// `(id, label, note)` or `None` when the controller is unreadable.
    pub async fn controller_provider(&self) -> Option<(String, String, String)> {
        use k8s_openapi::api::apps::v1::Deployment;
        let deploys: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let d = deploys.get_opt("kars-controller").await.ok().flatten()?;
        let envs: Vec<_> = d
            .spec
            .and_then(|s| s.template.spec)
            .map(|ps| ps.containers)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.env.unwrap_or_default())
            .collect();
        configured_provider(&envs)
    }

    /// The orchestrator inference config this cluster already provides — read
    /// from the controller Deployment env the SAME way the runtime does, so the
    /// Bridge's intent→package orchestrator inherits the cluster's provider
    /// instead of needing its own credentials. Returns `(endpoint, token,
    /// model)` when an endpoint, a usable token, and a default model are all
    /// present. `None` when the cluster authenticates via workload identity
    /// (no static token the BFF can reuse) — the UI then falls back to manual
    /// composition honestly.
    pub async fn orchestrator_inference(&self) -> Option<(String, String, String)> {
        use k8s_openapi::api::apps::v1::Deployment;
        let deploys: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let d = deploys.get_opt("kars-controller").await.ok().flatten()?;
        let mut endpoint: Option<String> = None;
        let mut token: Option<String> = None;
        let mut model: Option<String> = None;
        let envs = d
            .spec
            .and_then(|s| s.template.spec)
            .map(|ps| ps.containers)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.env.unwrap_or_default());
        for e in envs {
            let Some(val) = e.value else { continue };
            if val.is_empty() {
                continue;
            }
            match e.name.as_str() {
                "FOUNDRY_ENDPOINT" if endpoint.is_none() => endpoint = Some(val),
                "AZURE_OPENAI_ENDPOINT" if endpoint.is_none() => endpoint = Some(val),
                "AZURE_OPENAI_API_KEY" | "GITHUB_TOKEN" | "COPILOT_GITHUB_TOKEN"
                    if token.is_none() =>
                {
                    token = Some(val)
                }
                "KARS_TASK_DEFAULT_MODEL" | "AZURE_OPENAI_DEPLOYMENT" if model.is_none() => {
                    model = Some(val)
                }
                _ => {}
            }
        }
        // Normalize a bare Foundry/AOAI endpoint to its OpenAI-compatible base so
        // `{endpoint}/chat/completions` resolves. GitHub Models already exposes
        // `/inference` as the base; leave it intact.
        let endpoint = endpoint?;
        Some((endpoint, token?, model?))
    }

    /// Harnesses with declared image/registry coverage, not execution qualification.
    /// Private registries need explicit coverage; access may use credentials,
    /// node identity or anonymous mirrors. No actual image pull is performed.
    pub async fn runnable_runtimes(&self) -> std::collections::BTreeSet<String> {
        use k8s_openapi::api::apps::v1::Deployment;
        let mut runnable: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        // BYO remains selectable because its image is supplied by the BYO contract.
        runnable.insert("BYO".into());
        let Ok(Some(d)) = (Api::<Deployment>::namespaced(self.client.clone(), "kars-system"))
            .get_opt("kars-controller")
            .await
        else {
            return runnable;
        };
        let Some(pod_spec) = d.spec.and_then(|s| s.template.spec) else {
            return runnable;
        };
        let configured: std::collections::BTreeMap<String, String> = pod_spec
            .containers
            .into_iter()
            .flat_map(|c| c.env.unwrap_or_default())
            .filter_map(|e| {
                e.value
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| (e.name, value))
            })
            .collect();
        // The BFF deliberately has no Secret RBAC. The controller exposes only
        // the non-sensitive registry hostnames covered by its declared access.
        let covered_registries = configured
            .get("IMAGE_PULL_REGISTRIES")
            .into_iter()
            .flat_map(|value| value.split(','))
            .map(normalize_registry_host)
            .filter(|registry| !registry.is_empty())
            .collect::<std::collections::BTreeSet<_>>();
        let image_is_pullable = |image: &str| {
            let registry = image_registry_host(image);
            public_registry(&registry) || covered_registries.contains(&registry)
        };
        if configured
            .get("SANDBOX_IMAGE")
            .is_some_and(|image| image_is_pullable(image))
        {
            runnable.insert("OpenClaw".into());
        }
        let mapping = [
            ("OPENAI_AGENTS_RUNTIME_IMAGE", "OpenAIAgents"),
            ("MAF_RUNTIME_IMAGE", "MicrosoftAgentFramework"),
            ("ANTHROPIC_RUNTIME_IMAGE", "Anthropic"),
            ("LANGGRAPH_RUNTIME_IMAGE", "LangGraph"),
            ("LANGGRAPH_TS_RUNTIME_IMAGE", "LangGraph"),
            ("PYDANTIC_AI_RUNTIME_IMAGE", "PydanticAi"),
            ("HERMES_RUNTIME_IMAGE", "Hermes"),
        ];
        for (env, kind) in mapping {
            if configured
                .get(env)
                .is_some_and(|image| image_is_pullable(image))
            {
                runnable.insert(kind.to_string());
            }
        }
        runnable
    }

    /// Patch the controller deployment env to set the model catalog (and
    /// optionally an endpoint) so an onboarded provider's models surface in the
    /// launch palette. Triggers a rolling restart. Operator-gated write.
    ///
    /// When `key_secret` is `Some((secret_name, secret_key))`, the provider's
    /// API key is wired via a `secretKeyRef` on `AZURE_OPENAI_API_KEY` — the env
    /// var the controller reads and then propagates to every sandbox pod it
    /// creates (see controller reconciler). This is what makes an `auth=api`
    /// provider actually usable end-to-end, not merely stored.
    pub async fn set_controller_catalog(
        &self,
        catalog: &str,
        endpoint: Option<&str>,
        key_secret: Option<(&str, &str)>,
    ) -> Result<(), kube::Error> {
        self.require_registered_default_before_switch(endpoint)
            .await?;
        // Strategic merge on `env` (merge-key `name`) upserts these entries and
        // preserves every other existing env var on the container.
        let mut env = vec![serde_json::json!({"name": "KARS_MODEL_CATALOG", "value": catalog})];
        // The FIRST catalog entry is the default model — pin it as
        // KARS_TASK_DEFAULT_MODEL + AZURE_OPENAI_DEPLOYMENT so switching the
        // default provider (or a specific default model) actually changes what
        // missions inherit, not just the offered catalog. Without this, the
        // controller kept serving a STALE default model after every switch.
        if let Some(default_model) = catalog.split(',').map(str::trim).find(|s| !s.is_empty()) {
            env.push(
                serde_json::json!({"name": "KARS_TASK_DEFAULT_MODEL", "value": default_model}),
            );
            env.push(
                serde_json::json!({"name": "AZURE_OPENAI_DEPLOYMENT", "value": default_model}),
            );
        }
        // Replace default-route aliases, preserving independently configured
        // Foundry project services and the registered previous inference route.
        for stale in [
            "KARS_PROVIDER",
            "KARS_INFERENCE_PROVIDER",
            "AZURE_OPENAI_ENDPOINT",
        ] {
            env.push(serde_json::json!({"name": stale, "$patch": "delete"}));
        }
        if let Some(e) = endpoint {
            env.push(serde_json::json!({"name": "FOUNDRY_ENDPOINT", "value": e}));
        } else {
            // No explicit endpoint (e.g. switching to GitHub Copilot/Models
            // default, which reach their well-known host without one) — clear
            // any previously-set FOUNDRY_ENDPOINT too, for the same reason.
            env.push(serde_json::json!({"name": "FOUNDRY_ENDPOINT", "$patch": "delete"}));
        }
        if let Some((secret, key)) = key_secret {
            // valueFrom.secretKeyRef replaces any prior static `value` for this
            // name under strategic merge, so the key is sourced from the Secret.
            env.push(serde_json::json!({
                "name": "AZURE_OPENAI_API_KEY",
                "valueFrom": { "secretKeyRef": { "name": secret, "key": key } },
            }));
        } else {
            // The new default has no key (e.g. an unauthenticated in-cluster
            // local model, or Workload Identity) — clear any key wired for a
            // PRIOR default so the router doesn't keep sending a stale
            // credential to an endpoint that never asked for one.
            env.push(serde_json::json!({"name": "AZURE_OPENAI_API_KEY", "$patch": "delete"}));
        }
        self.write_controller_environment(env).await
    }

    /// The current Foundry connection, read live from the `kars-controller`
    /// Deployment env: `(project_endpoint, inference_endpoint, memory_store_id,
    /// has_api_key)`. All `None`/false when Foundry has not been onboarded. The
    /// API key is NEVER returned — only whether one is wired.
    pub async fn get_foundry_connection(&self) -> Result<FoundryConnection, String> {
        use k8s_openapi::api::apps::v1::Deployment;
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let dep = api.get("kars-controller").await.map_err(|_| {
            "Foundry discovery is unavailable; memory backend cannot be inferred".to_string()
        })?;
        let spec = dep
            .spec
            .and_then(|s| s.template.spec)
            .ok_or("Foundry discovery controller template is unavailable")?;
        let controller = spec
            .containers
            .into_iter()
            .find(|c| c.name == "controller")
            .ok_or("Foundry discovery controller container is unavailable")?;
        parse_foundry_connection(controller)
    }

    /// The workload-identity client-id wired onto the sandbox/controller service
    /// account, if any — evidence the cluster can obtain managed-identity tokens
    /// (the same path Foundry data-plane access uses). `None` when not wired.
    pub async fn workload_identity_client_id(&self) -> Option<String> {
        use k8s_openapi::api::core::v1::ServiceAccount;
        let api: Api<ServiceAccount> = Api::namespaced(self.client.clone(), "kars-system");
        for sa in ["kars-controller", "default"] {
            if let Some(obj) = api.get_opt(sa).await.ok().flatten()
                && let Some(cid) = obj
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get("azure.workload.identity/client-id"))
                    .filter(|v| !v.is_empty())
            {
                return Some(cid.clone());
            }
        }
        None
    }

    /// The model every team run inherits when its blueprint pins none — the
    /// controller's `KARS_TASK_DEFAULT_MODEL` env (see
    /// `controller/src/kars_task_execution.rs::default_model`). Read live from
    /// the `kars-controller` Deployment so the Bridge shows the *effective*
    /// model, not a hardcoded guess. `None` when the controller isn't found or
    /// the env is unset (the caller then labels it generically).
    pub async fn controller_default_model(&self) -> Option<String> {
        use k8s_openapi::api::apps::v1::Deployment;
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let dep = api.get_opt("kars-controller").await.ok().flatten()?;
        let containers = dep.spec?.template.spec?.containers;
        for c in containers {
            for env in c.env.unwrap_or_default() {
                if env.name == "KARS_TASK_DEFAULT_MODEL"
                    && let Some(v) = env.value.filter(|v| !v.is_empty())
                {
                    return Some(v);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod memory_discovery_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn memory_discovery_does_not_infer_foundry_from_azure_inference() {
        let container = serde_json::from_value(json!({"name":"controller","env":[
            {"name":"KARS_PROVIDER","value":"azure-openai"},
            {"name":"FOUNDRY_ENDPOINT","value":"https://inference.openai.azure.com"}]}))
        .unwrap();
        assert!(parse_foundry_connection(container).unwrap().0.is_none());
        let container = serde_json::from_value(json!({"name":"controller","env":[
            {"name":"KARS_PROVIDER","value":"github-copilot"},
            {"name":"FOUNDRY_PROJECT_ENDPOINT","value":"https://account.services.ai.azure.com/api/projects/project"}]})).unwrap();
        assert!(parse_foundry_connection(container).unwrap().0.is_some());
    }

    #[test]
    fn unresolved_project_configuration_is_not_absence() {
        for container in [
            json!({"name":"controller","envFrom":[{"configMapRef":{"name":"settings"}}]}),
            json!({"name":"controller","env":[{"name":"FOUNDRY_PROJECT_ENDPOINT","valueFrom":{"configMapKeyRef":{"name":"settings","key":"project"}}}]}),
        ] {
            assert!(parse_foundry_connection(serde_json::from_value(container).unwrap()).is_err());
        }
        let container = serde_json::from_value(
            json!({"name":"controller","env":[{"name":"FOUNDRY_PROJECT_ENDPOINT","value":" "}]}),
        )
        .unwrap();
        assert!(parse_foundry_connection(container).unwrap().0.is_none());
    }
}
