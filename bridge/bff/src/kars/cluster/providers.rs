use super::Cluster;
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

impl Cluster {
    /// The model deployments this cluster is configured to serve, read from the
    /// controller Deployment's environment (`KARS_TASK_DEFAULT_MODEL`,
    /// `AZURE_OPENAI_DEPLOYMENT`, and the comma-separated `FOUNDRY_DEPLOYMENTS`).
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
        let mut default: Option<String> = None;
        let mut catalog: Vec<String> = Vec::new();
        let envs = d
            .spec
            .and_then(|s| s.template.spec)
            .map(|ps| ps.containers)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.env.unwrap_or_default());
        for e in envs {
            let Some(val) = e.value else { continue };
            match e.name.as_str() {
                "KARS_TASK_DEFAULT_MODEL" | "AZURE_OPENAI_DEPLOYMENT" if default.is_none() => {
                    default = Some(val);
                }
                "FOUNDRY_DEPLOYMENTS" | "KARS_MODEL_CATALOG" => {
                    catalog.extend(
                        val.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()),
                    );
                }
                _ => {}
            }
        }
        (default, catalog)
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
        let mut provider_override: Option<String> = None;
        let mut endpoints: Vec<String> = Vec::new();
        let mut token_hint: Option<String> = None;
        let envs = d
            .spec
            .and_then(|s| s.template.spec)
            .map(|ps| ps.containers)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.env.unwrap_or_default());
        for e in envs {
            let Some(val) = e.value else { continue };
            match e.name.as_str() {
                // Explicit operator declaration — the authoritative brand signal.
                "KARS_PROVIDER" | "KARS_INFERENCE_PROVIDER" if !val.is_empty() => {
                    provider_override = Some(val)
                }
                "FOUNDRY_ENDPOINT" | "FOUNDRY_PROJECT_ENDPOINT" | "AZURE_OPENAI_ENDPOINT" => {
                    endpoints.push(val)
                }
                // Auth token KIND disambiguates the GitHub endpoint: a GitHub
                // OAuth/user token (`gho_`/`ghu_`) is a Copilot login; a classic
                // PAT (`ghp_`) is free GitHub Models. We only inspect the prefix,
                // never the secret, and only when provided inline (dev profile).
                "AZURE_OPENAI_API_KEY" | "GITHUB_TOKEN" | "COPILOT_GITHUB_TOKEN"
                    if token_hint.is_none() && !val.is_empty() =>
                {
                    token_hint = Some(val.chars().take(4).collect());
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

    /// Which agent harnesses are actually runnable on this cluster. A configured
    /// image is insufficient: private images also need a controller pull secret
    /// whose Docker auth covers that image registry. This keeps the composer and
    /// preflight from advertising a runtime that will immediately ImagePullBackOff.
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
        // the non-sensitive registry hostnames covered by its pull credentials.
        let authenticated_registries = configured
            .get("IMAGE_PULL_REGISTRIES")
            .into_iter()
            .flat_map(|value| value.split(','))
            .map(normalize_registry_host)
            .filter(|registry| !registry.is_empty())
            .collect::<std::collections::BTreeSet<_>>();
        let image_is_pullable = |image: &str| {
            let registry = image_registry_host(image);
            public_registry(&registry) || authenticated_registries.contains(&registry)
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
        // `controller_provider()` (the "what's the current default provider"
        // read used by the Configuration page's status card) checks THREE
        // things it treats as stale-able: an explicit `KARS_PROVIDER` /
        // `KARS_INFERENCE_PROVIDER` override (checked FIRST, absolute
        // priority over everything else — typically set once at cluster
        // bootstrap, e.g. `KARS_PROVIDER=github-copilot`), then
        // FOUNDRY_ENDPOINT / FOUNDRY_PROJECT_ENDPOINT / AZURE_OPENAI_ENDPOINT
        // as interchangeable endpoint aliases (picks whichever it finds
        // FIRST). Every caller of this function is switching the cluster
        // default to a NEW provider, so ALL of these must be cleared here —
        // confirmed live this was a real, pre-existing bug affecting the
        // ORIGINAL "Add or switch a provider" flow too, not just the new
        // local-inference promote action: switching the default endpoint
        // correctly patched FOUNDRY_ENDPOINT, but the Configuration page
        // kept showing "GitHub Copilot" forever after, because the
        // bootstrap-time `KARS_PROVIDER=github-copilot` override (checked
        // before any endpoint) was never cleared by anything. Neither
        // caller of this function ever wants to declare copilot/models as
        // default (both explicitly reject that combination before calling
        // in), so unconditionally clearing the override is correct here.
        // `$patch: delete` is the standard strategic-merge-patch mechanism
        // for removing one named entry from a mergeKey'd list without
        // touching the rest — a no-op if the name was never present.
        for stale in [
            "KARS_PROVIDER",
            "KARS_INFERENCE_PROVIDER",
            "AZURE_OPENAI_ENDPOINT",
            "FOUNDRY_PROJECT_ENDPOINT",
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

    /// Make GitHub Copilot the cluster's DEFAULT provider. Copilot doesn't use
    /// the endpoint+key shape `set_controller_catalog` wires — it authenticates
    /// via a GitHub token exchanged for a short-lived Copilot JWT by the router
    /// (`copilot_auth`). This wires exactly what Copilot-as-default needs on the
    /// controller (which propagates it to every sandbox): `KARS_PROVIDER=
    /// github-copilot`, `COPILOT_GITHUB_TOKEN` (the token signed in via the
    /// wizard, read from the shared providers secret), the Copilot API host as
    /// the `AZURE_OPENAI_ENDPOINT` sentinel (the controller refuses to
    /// provision a sandbox without SOME inference endpoint), the model catalog,
    /// and the default model — clearing any stale Azure/Foundry endpoint+key
    /// from a prior default. Returns an error if no Copilot token is stored yet
    /// (the operator must sign in first).
    pub async fn set_copilot_as_default(&self, models: &str) -> Result<(), kube::Error> {
        let token = self
            .read_secret_all("kars-system", "kars-inference-providers")
            .await?
            .get("COPILOT_GITHUB_TOKEN")
            .filter(|v| !v.trim().is_empty())
            .cloned();
        let Some(_token) = token else {
            return Err(kube::Error::Api(kube::error::ErrorResponse {
                status: "Failure".into(),
                message: "no Copilot token is stored — sign in to GitHub Copilot first".into(),
                reason: "BadRequest".into(),
                code: 400,
            }));
        };
        let default_model = models
            .split(',')
            .next()
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let mut env = vec![
            serde_json::json!({"name": "KARS_PROVIDER", "value": "github-copilot"}),
            serde_json::json!({"name": "COPILOT_GITHUB_TOKEN", "valueFrom":{"secretKeyRef":{
                "name":"kars-inference-providers","key":"COPILOT_GITHUB_TOKEN"}}}),
            serde_json::json!({"name": "AZURE_OPENAI_ENDPOINT", "value": "https://api.githubcopilot.com"}),
            serde_json::json!({"name": "KARS_MODEL_CATALOG", "value": models}),
        ];
        if !default_model.is_empty() {
            env.push(serde_json::json!({"name": "KARS_TASK_DEFAULT_MODEL", "value": default_model.clone()}));
            env.push(
                serde_json::json!({"name": "AZURE_OPENAI_DEPLOYMENT", "value": default_model}),
            );
        }
        // Clear anything a prior (Azure/Foundry) default left behind so the
        // router doesn't keep a stale endpoint/key alongside Copilot.
        for stale in [
            "FOUNDRY_ENDPOINT",
            "FOUNDRY_PROJECT_ENDPOINT",
            "AZURE_OPENAI_API_KEY",
            "KARS_INFERENCE_PROVIDER",
        ] {
            env.push(serde_json::json!({"name": stale, "$patch": "delete"}));
        }
        self.write_controller_environment(env).await
    }

    pub async fn controller_env_value(&self, name: &str) -> Option<String> {
        use k8s_openapi::api::apps::v1::Deployment;
        let deployment = Api::<Deployment>::namespaced(self.client.clone(), "kars-system")
            .get_opt("kars-controller")
            .await
            .ok()
            .flatten()?;
        deployment
            .spec?
            .template
            .spec?
            .containers
            .first()?
            .env
            .as_ref()?
            .iter()
            .find(|entry| entry.name == name)
            .and_then(|entry| entry.value.clone())
    }

    /// The current Foundry connection, read live from the `kars-controller`
    /// Deployment env: `(project_endpoint, inference_endpoint, memory_store_id,
    /// has_api_key)`. All `None`/false when Foundry has not been onboarded. The
    /// API key is NEVER returned — only whether one is wired.
    pub async fn get_foundry_connection(
        &self,
    ) -> (Option<String>, Option<String>, Option<String>, bool) {
        use k8s_openapi::api::apps::v1::Deployment;
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), &self.core_namespace());
        let Some(dep) = api.get_opt("kars-controller").await.ok().flatten() else {
            return (None, None, None, false);
        };
        let mut project = None;
        let mut inference = None;
        let mut store = None;
        let mut has_key = false;
        if let Some(spec) = dep.spec.and_then(|s| s.template.spec) {
            for c in spec.containers {
                for env in c.env.unwrap_or_default() {
                    match env.name.as_str() {
                        "FOUNDRY_PROJECT_ENDPOINT" => project = env.value.filter(|v| !v.is_empty()),
                        "FOUNDRY_ENDPOINT" => inference = env.value.filter(|v| !v.is_empty()),
                        "FOUNDRY_MEMORY_STORE_ID" => store = env.value.filter(|v| !v.is_empty()),
                        "FOUNDRY_API_KEY" => {
                            has_key = env.value_from.is_some()
                                || env.value.as_ref().is_some_and(|v| !v.is_empty());
                        }
                        _ => {}
                    }
                }
            }
        }
        (project, inference, store, has_key)
    }

    /// Onboard a Foundry connection by patching the `kars-controller` Deployment
    /// env (strategic merge on `env` by name, preserving all other vars). Sets
    /// `FOUNDRY_PROJECT_ENDPOINT` (+ optional inference endpoint / memory store),
    /// and for API-key auth wires `FOUNDRY_API_KEY` from a Secret via
    /// `secretKeyRef`. The controller then propagates these to sandbox routers.
    /// Managed-identity auth stores no key — the router uses the cluster's
    /// workload identity (audience `https://ai.azure.com`).
    pub async fn set_foundry_connection(
        &self,
        project_endpoint: &str,
        inference_endpoint: Option<&str>,
        memory_store_id: Option<&str>,
        key_secret: Option<(&str, &str)>,
    ) -> Result<(), kube::Error> {
        let mut env = vec![
            serde_json::json!({"name": "FOUNDRY_PROJECT_ENDPOINT", "value": project_endpoint}),
        ];
        if let Some(e) = inference_endpoint.filter(|e| !e.is_empty()) {
            env.push(serde_json::json!({"name": "FOUNDRY_ENDPOINT", "value": e}));
        }
        if let Some(s) = memory_store_id.filter(|s| !s.is_empty()) {
            env.push(serde_json::json!({"name": "FOUNDRY_MEMORY_STORE_ID", "value": s}));
        }
        if let Some((secret, key)) = key_secret {
            env.push(serde_json::json!({
                "name": "FOUNDRY_API_KEY",
                "valueFrom": { "secretKeyRef": { "name": secret, "key": key } },
            }));
        }
        self.write_controller_environment(env).await
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
