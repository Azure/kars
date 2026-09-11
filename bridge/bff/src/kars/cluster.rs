// kars Bridge BFF — cluster access.
//
// The BFF is the only process that holds a cluster client. The browser never
// receives kube credentials. For local dev the client is built from the
// ambient kubeconfig; in-cluster it uses the mounted ServiceAccount.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use k8s_openapi::api::core::v1::{ConfigMap, Node, Pod};
use kube::api::{Api, DynamicObject, GroupVersionKind, ListParams};
use kube::core::ApiResource;
use kube::{Client, ResourceExt};
use sha2::{Digest, Sha256};

const ORCHESTRATOR_POLICY_READY_ATTEMPTS: usize = 180;
const ORCHESTRATOR_POLICY_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(500);

/// True when a sandbox name denotes an ephemeral standing-run sandbox
/// (`<team>-run-<timestamp>`), which is short-lived and often mid-execution —
/// not a stable target for routing the Bridge's orchestrator inference through.
fn is_ephemeral_run(sandbox: &str) -> bool {
    if let Some(idx) = sandbox.rfind("-run-") {
        let suffix = &sandbox[idx + 5..];
        return !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

fn normalize_registry_host(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn image_registry_host(image: &str) -> String {
    let first = image.trim().split('/').next().unwrap_or_default();
    if first.contains('.') || first.contains(':') || first == "localhost" {
        first.to_ascii_lowercase()
    } else {
        "docker.io".to_string()
    }
}

fn public_registry(registry: &str) -> bool {
    matches!(
        registry,
        "docker.io" | "registry-1.docker.io" | "mcr.microsoft.com" | "public.ecr.aws"
    )
}

fn descendant_sandbox_objects(sandboxes: &[DynamicObject], root: &str) -> Vec<DynamicObject> {
    let mut descendants = Vec::new();
    let mut frontier = vec![root.to_string()];
    let mut seen = std::collections::HashSet::from([root.to_string()]);
    while let Some(parent) = frontier.pop() {
        for sandbox in sandboxes {
            let Some(name) = sandbox.metadata.name.as_ref() else {
                continue;
            };
            let is_child = sandbox
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get("kars.azure.com/parent"))
                == Some(&parent);
            if is_child && seen.insert(name.clone()) {
                descendants.push(sandbox.clone());
                frontier.push(name.clone());
            }
        }
    }
    descendants
}

fn mission_evidence_key(cm: &ConfigMap, legacy_label: &str) -> Option<String> {
    cm.metadata
        .annotations
        .as_ref()
        .and_then(|annotations| {
            annotations
                .get("kars.azure.com/mission-evidence-key")
                .filter(|value| !value.trim().is_empty())
                .cloned()
        })
        .or_else(|| {
            cm.metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(legacy_label).cloned())
        })
}

fn mission_evidence_role(cm: &ConfigMap) -> Option<String> {
    cm.metadata.annotations.as_ref().and_then(|annotations| {
        annotations
            .get("kars.azure.com/mission-evidence-role")
            .filter(|value| !value.trim().is_empty())
            .cloned()
    })
}

fn mission_principal_name(cm: &ConfigMap) -> Option<String> {
    cm.metadata
        .annotations
        .as_ref()
        .and_then(|annotations| {
            annotations
                .get("kars.azure.com/mission-principal-name")
                .filter(|value| !value.trim().is_empty())
                .cloned()
        })
        .or_else(|| {
            cm.metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get("kars.azure.com/mission-principal").cloned())
        })
}

fn mission_output_candidate(
    cm: ConfigMap,
) -> Option<(
    String,
    Option<String>,
    std::collections::BTreeMap<String, String>,
)> {
    let evidence_key = mission_evidence_key(&cm, "kars.azure.com/mission-output")?;
    let role = mission_evidence_role(&cm);
    let principal_name = mission_principal_name(&cm);
    let mut data = cm.data.unwrap_or_default();
    if !data.contains_key("taskName") {
        if let Some(principal_name) = principal_name {
            data.insert("taskName".to_string(), principal_name);
        } else if data
            .get("assignmentNonce")
            .is_some_and(|nonce| nonce != &evidence_key)
        {
            data.insert("taskName".to_string(), evidence_key.clone());
        }
    }
    Some((evidence_key, role, data))
}

fn select_mission_output_records(
    records: Vec<(
        String,
        Option<String>,
        std::collections::BTreeMap<String, String>,
    )>,
) -> Vec<(String, std::collections::BTreeMap<String, String>)> {
    let mut grouped = std::collections::BTreeMap::<
        String,
        Vec<(
            String,
            Option<String>,
            std::collections::BTreeMap<String, String>,
        )>,
    >::new();
    for (key, role, data) in records {
        let task_name = data.get("taskName").cloned().unwrap_or_else(|| key.clone());
        grouped
            .entry(task_name)
            .or_default()
            .push((key, role, data));
    }
    grouped
        .into_values()
        .filter_map(|group| {
            group
                .into_iter()
                .max_by_key(|(key, role, data)| match role.as_deref() {
                    Some("current") => 4,
                    Some("canonical") => 3,
                    Some("archive") => 1,
                    Some(_) => 0,
                    None if data.get("assignmentNonce").is_none() => 3,
                    None if data.get("assignmentNonce") != Some(key) => 2,
                    None => 1,
                })
                .map(|(key, _, data)| (key, data))
        })
        .collect()
}

fn select_mission_evidence_records(
    records: Vec<(
        String,
        Option<String>,
        std::collections::BTreeMap<String, String>,
    )>,
) -> Vec<(String, std::collections::BTreeMap<String, String>)> {
    let mut grouped = std::collections::BTreeMap::<
        String,
        Vec<(
            String,
            Option<String>,
            std::collections::BTreeMap<String, String>,
        )>,
    >::new();
    for (key, role, data) in records {
        let identity = data
            .get("assignmentNonce")
            .cloned()
            .unwrap_or_else(|| key.clone());
        grouped.entry(identity).or_default().push((key, role, data));
    }
    grouped
        .into_values()
        .filter_map(|group| {
            group
                .into_iter()
                .max_by_key(|(key, role, data)| match role.as_deref() {
                    Some("archive" | "canonical") => 3,
                    Some("current") => 1,
                    Some(_) => 0,
                    None if data.get("assignmentNonce") == Some(key) => 2,
                    None => 1,
                })
                .map(|(key, _, data)| (key, data))
        })
        .collect()
}

fn project_mission_output_record(
    evidence_key: String,
    data: std::collections::BTreeMap<String, String>,
) -> MissionOutputRecord {
    let task_name = data
        .get("taskName")
        .cloned()
        .unwrap_or_else(|| evidence_key.clone());
    MissionOutputRecord {
        task_name,
        evidence_key,
        data,
    }
}

fn trace_record_identity(cm: &ConfigMap) -> Option<String> {
    if !cm
        .metadata
        .name
        .as_deref()
        .is_some_and(|name| name.starts_with("kars-mission-trace-"))
    {
        return None;
    }
    if mission_evidence_role(cm).as_deref() == Some("current") {
        return None;
    }
    let data = cm.data.as_ref()?;
    let trace = data.get("trace.json").filter(|trace| trace.len() > 2)?;
    if let Some(nonce) = data.get("assignmentNonce") {
        return Some(format!("nonce:{nonce}"));
    }
    let captured_at = data.get("capturedAt").map(String::as_str).unwrap_or("");
    Some(format!(
        "legacy:{captured_at}:{:x}",
        Sha256::digest(trace.as_bytes())
    ))
}
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;

use crate::kars::task::KarsTask;

#[derive(Debug, Clone)]
pub struct MissionOutputRecord {
    pub task_name: String,
    pub evidence_key: String,
    pub data: std::collections::BTreeMap<String, String>,
}

/// Classify the inherited inference provider from an optional `KARS_PROVIDER`
/// override plus the configured endpoint hosts. Mirrors the router's detection
/// (`inference-router/src/config.rs`): the three providers kars supports are
/// GitHub Copilot, GitHub Models, and Azure AI Foundry. Returns `(id, label,
/// note)`, or `None` when nothing identifiable is configured.
fn classify_provider(
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

/// A handle to the kars cluster, scoped per request to a namespace.
#[derive(Clone)]
pub struct Cluster {
    pub(super) client: Client,
}

/// Outcome of awaiting a mesh-driven run (`Cluster::await_mesh_run`).
pub enum MeshRunOutcome {
    /// The controller stamped `run-completed` and the deliverable is written.
    Completed(std::collections::BTreeMap<String, String>),
    /// The mesh peer acknowledged (`run-ack`) and is actively delivering, but
    /// hasn't finished within the wait — the caller must NOT single-turn (that
    /// would race the controller's deliverable write); the result lands async.
    InProgress,
    /// No `run-ack` appeared — the mesh peer never picked this up (no lease
    /// holder / relay down), so a single-turn fallback is safe.
    NeverProcessed,
}

/// A running agent's mesh identity, discovered from the AGT registry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentIdentity {
    /// The agent's decentralized mesh identifier, e.g. `did:mesh:<hash>`.
    pub did: String,
    /// Capabilities the agent advertises (includes the sandbox name + e.g.
    /// `task-execution`, `kars-agent`).
    pub capabilities: Vec<String>,
    /// Last time the registry saw the agent (RFC3339), when reported.
    pub last_seen: Option<String>,
    /// The agent's mesh reputation score, when reported.
    pub reputation_score: Option<f64>,
}

/// Honest, status-derived health of an agent's running pod (no metrics-server /
/// CPU-mem dependency). Answers "is this agent healthy right now".
#[derive(Debug, Clone, serde::Serialize)]
pub struct PodHealth {
    /// Ready containers vs total (e.g. 2/2). Fewer-than-total means degraded.
    pub ready_containers: i32,
    pub total_containers: i32,
    /// Cumulative container restarts — a crash-loop signal.
    pub restarts: i32,
    /// Seconds since the pod started (uptime).
    pub uptime_seconds: Option<i64>,
    /// The node the pod is scheduled on.
    pub node: Option<String>,
    /// A container's waiting reason (e.g. CrashLoopBackOff, ImagePullBackOff)
    /// when one isn't running — the honest unhealthy signal.
    pub waiting_reason: Option<String>,
}

/// Per-container state for the run-failure troubleshooter.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContainerState {
    pub name: String,
    pub ready: bool,
    pub restarts: i32,
    /// running / waiting / terminated / unknown.
    pub state: String,
    /// The waiting or terminated reason (ImagePullBackOff, OOMKilled, …).
    pub reason: Option<String>,
}

impl Cluster {
    #[cfg(test)]
    pub(crate) fn for_test_client(client: Client) -> Self {
        Self { client }
    }

    /// Build a cluster handle from the ambient configuration (kubeconfig
    /// locally, in-cluster ServiceAccount in production). Returns `None`-style
    /// errors as `anyhow` so the readiness probe can report honest status.
    pub async fn connect() -> anyhow::Result<Self> {
        let client = Client::try_default().await?;
        Ok(Self { client })
    }

    /// `KarsTask` API scoped to a namespace.
    pub fn tasks(&self, namespace: &str) -> Api<KarsTask> {
        Api::namespaced(self.client.clone(), namespace)
    }

    /// Task metadata for usage attribution: `name -> (namespace, created_by)`,
    /// listed across ALL namespaces so per-workspace (namespace) and per-user
    /// (the `kars.azure.com/created-by` annotation the Bridge stamps) budgets can
    /// attribute a run's tokens to the tenant that owns it. Team runs
    /// (`<team>-run-<epoch>`) are attributed to the parent team's creator.
    pub async fn list_task_meta(&self) -> std::collections::HashMap<String, (String, String)> {
        use kube::api::ListParams;
        let api: Api<KarsTask> = Api::all(self.client.clone());
        let mut out = std::collections::HashMap::new();
        let Ok(list) = api.list(&ListParams::default()).await else {
            return out;
        };
        for t in list.items {
            let name = t.metadata.name.clone().unwrap_or_default();
            let ns = t
                .metadata
                .namespace
                .clone()
                .unwrap_or_else(|| "kars-system".into());
            let created_by = t
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/created-by").cloned())
                .unwrap_or_else(|| "unattributed".into());
            out.insert(name, (ns, created_by));
        }
        out
    }

    /// `KarsTeam` API scoped to a namespace.
    pub fn teams(&self, namespace: &str) -> Api<crate::kars::team::KarsTeam> {
        Api::namespaced(self.client.clone(), namespace)
    }

    /// Read the operator-curated MCP profiles (named vetted server bundles),
    /// stored as `profiles.json` in the `kars-mcp-profiles` ConfigMap. Returns
    /// `[]` when unset. A profile is `{name, summary, servers:[mcpserver names]}`.
    pub async fn read_mcp_profiles(&self) -> String {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        cms.get_opt("kars-mcp-profiles")
            .await
            .ok()
            .flatten()
            .and_then(|cm| cm.data)
            .and_then(|d| d.get("profiles.json").cloned())
            .unwrap_or_else(|| "[]".to_string())
    }

    /// Persist the operator-curated MCP profiles (server-side apply).
    pub async fn write_mcp_profiles(&self, profiles_json: &str) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams};
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let patch = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": { "name": "kars-mcp-profiles", "labels": { "app.kubernetes.io/managed-by": "kars-bridge" } },
            "data": { "profiles.json": profiles_json },
        });
        cms.patch(
            "kars-mcp-profiles",
            &PatchParams::apply("kars-bridge/mcp-profiles").force(),
            &Patch::Apply(patch),
        )
        .await?;
        Ok(())
    }

    /// Persist a skill PACKAGE's files as the `karsskill-<name>` ConfigMap in
    /// kars-system. Each entry is `<flat filename> -> <content>` (SKILL.md +
    /// scripts). The controller mirrors this ConfigMap into a granting sandbox's
    /// namespace and mounts it into the agent's skills dir.
    pub async fn write_skill_package(
        &self,
        skill_name: &str,
        files: &std::collections::BTreeMap<String, String>,
        package_digest: &str,
    ) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams};
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let cm_name = format!("karsskill-{skill_name}");
        let patch = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": cm_name,
                "labels": {
                    "app.kubernetes.io/managed-by": "kars-bridge",
                    "kars.azure.com/skill": skill_name,
                },
                "annotations": {
                    "kars.azure.com/package-digest": package_digest,
                },
            },
            "data": files,
        });
        cms.patch(
            &cm_name,
            &PatchParams::apply("kars-bridge/skill-package").force(),
            &Patch::Apply(patch),
        )
        .await?;
        Ok(())
    }

    // ── Keyless git write: shared App + per-principal connections (§14) ──────

    /// The cluster-shared kars GitHub App credentials (App id + PEM private key)
    /// from `Secret kars-github-app` in kars-system. `None` when the operator
    /// hasn't configured the App — git write is simply off (fail-closed).
    pub async fn github_app_creds(&self) -> Result<Option<(String, String)>, kube::Error> {
        let (_, s) = self
            .integration_store(&self.core_namespace(), "kars-github-app")
            .await?;
        let Some(data) = s.data else { return Ok(None) };
        let read = |k: &str| -> Option<String> {
            data.get(k)
                .and_then(|v| String::from_utf8(v.0.clone()).ok())
        };
        let Some(id) = read("GITHUB_APP_ID") else {
            return Ok(None);
        };
        let Some(key) = read("GITHUB_APP_PRIVATE_KEY") else {
            return Ok(None);
        };
        if id.trim().is_empty() || key.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some((id.trim().to_string(), key)))
    }

    /// Read a principal's GitHub connection ConfigMap in the namespace.
    pub async fn read_github_connection(
        &self,
        ns: &str,
        connection_name: &str,
    ) -> Option<(String, String, Vec<String>)> {
        self.read_github_connection_result(ns, connection_name)
            .await
            .ok()
            .flatten()
    }

    /// Read a principal GitHub connection while preserving Kubernetes API
    /// failures for background jobs that must report an honest source status.
    pub async fn read_github_connection_result(
        &self,
        ns: &str,
        connection_name: &str,
    ) -> Result<Option<(String, String, Vec<String>)>, kube::Error> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        let Some(data) = api.get_opt(connection_name).await?.and_then(|cm| cm.data) else {
            return Ok(None);
        };
        let read = |key: &str| data.get(key).cloned();
        let Some(installation_id) = read("installation_id") else {
            return Ok(None);
        };
        let account = read("account").unwrap_or_default();
        let repos = read("repos")
            .and_then(|r| serde_json::from_str::<Vec<String>>(&r).ok())
            .unwrap_or_default();
        Ok(Some((installation_id, account, repos)))
    }

    /// Store a principal's GitHub connection. No token or credential is stored.
    pub async fn write_github_connection(
        &self,
        ns: &str,
        connection_name: &str,
        installation_id: &str,
        account: &str,
        repos: &[String],
    ) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams, PostParams};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        let data = std::collections::BTreeMap::from([
            ("installation_id".to_string(), installation_id.to_string()),
            ("account".to_string(), account.to_string()),
            ("repos".to_string(), serde_json::to_string(repos)?),
        ]);
        let patch = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": connection_name,
                "namespace": ns,
                "labels": { "app.kubernetes.io/managed-by": "kars-bridge", "kars.azure.com/github-connection": "true" },
            },
            "data": data,
        });
        if let Some(current) = api.get_opt(connection_name).await? {
            let grant = self.credential_grant(ns).await?;
            if current.metadata.deletion_timestamp.is_some()
                || current.uid().is_none()
                || !grant.document.data["spec"]["githubConnections"]
                    .as_array()
                    .is_some_and(|entries| {
                        entries.iter().any(|entry| {
                            entry["connection"]["name"] == connection_name
                                && entry["connection"]["uid"]
                                    == serde_json::json!(current.metadata.uid)
                        })
                    })
            {
                anyhow::bail!(
                    "Existing GitHub connection requires exact operator UID enrollment before mutation; no adoption"
                );
            }
            api.patch(connection_name,&PatchParams::default(),&Patch::Merge(serde_json::json!({
                "metadata":{"uid":current.metadata.uid,"resourceVersion":current.metadata.resource_version},"data":data
            }))).await?;
        } else {
            let created: ConfigMap = serde_json::from_value(patch)?;
            api.create(&PostParams::default(), &created).await?;
        }
        Ok(())
    }

    /// Remove only the named principal GitHub connection.
    pub async fn delete_github_connection(
        &self,
        ns: &str,
        connection_name: &str,
    ) -> anyhow::Result<()> {
        use kube::api::{DeleteParams, Preconditions};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        if let Some(current) = api.get_opt(connection_name).await? {
            if current.uid().is_none() || current.resource_version().is_none() {
                anyhow::bail!("GitHub connection identity is unavailable; no deletion");
            }
            api.delete(
                connection_name,
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: current.metadata.uid,
                        resource_version: current.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await?;
        }
        Ok(())
    }

    // ── Bridge engineering intake sources ───────────────────────────────────

    pub async fn read_engineering_source(
        &self,
        name: &str,
    ) -> Result<Option<ConfigMap>, kube::Error> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        api.get_opt(name).await
    }

    pub async fn list_engineering_sources(
        &self,
        limit: u32,
    ) -> Result<Vec<ConfigMap>, kube::Error> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let params = ListParams::default()
            .labels("bridge.kars.azure.com/engineering-source=true")
            .limit(limit);
        Ok(api.list(&params).await?.items)
    }

    pub async fn create_engineering_source(
        &self,
        name: &str,
        annotations: &std::collections::BTreeMap<String, String>,
        data: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), kube::Error> {
        use kube::api::PostParams;
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let config_map: ConfigMap = serde_json::from_value(serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": name,
                "namespace": "kars-system",
                "labels": {
                    "app.kubernetes.io/managed-by": "kars-bridge",
                    "bridge.kars.azure.com/engineering-source": "true",
                },
                "annotations": annotations,
            },
            "data": data,
        }))
        .expect("engineering source ConfigMap is valid");
        api.create(&PostParams::default(), &config_map)
            .await
            .map(|_| ())
    }

    pub async fn patch_engineering_source_data(
        &self,
        name: &str,
        data: &std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        api.patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({ "data": data })),
        )
        .await?;
        Ok(())
    }

    pub async fn claim_engineering_source(
        &self,
        name: &str,
        expected_config: &str,
        expected_status: &str,
        claimed_status: &str,
    ) -> Result<bool, kube::Error> {
        use kube::api::{Patch, PatchParams};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let patch = json_patch::Patch(vec![
            json_patch::PatchOperation::Test(json_patch::TestOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "config.json"]),
                value: serde_json::Value::String(expected_config.to_string()),
            }),
            json_patch::PatchOperation::Test(json_patch::TestOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "status.json"]),
                value: serde_json::Value::String(expected_status.to_string()),
            }),
            json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "status.json"]),
                value: serde_json::Value::String(claimed_status.to_string()),
            }),
        ]);
        match api
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Json::<ConfigMap>(patch),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(kube::Error::Api(error))
                if error.code == 404 || error.code == 409 || error.code == 422 =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn complete_engineering_source_claim(
        &self,
        name: &str,
        expected_claimed_status: &str,
        cursor: &str,
        completed_status: &str,
    ) -> Result<bool, kube::Error> {
        use kube::api::{Patch, PatchParams};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let patch = json_patch::Patch(vec![
            json_patch::PatchOperation::Test(json_patch::TestOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "status.json"]),
                value: serde_json::Value::String(expected_claimed_status.to_string()),
            }),
            json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "cursor.json"]),
                value: serde_json::Value::String(cursor.to_string()),
            }),
            json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "status.json"]),
                value: serde_json::Value::String(completed_status.to_string()),
            }),
        ]);
        match api
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Json::<ConfigMap>(patch),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(kube::Error::Api(error))
                if error.code == 404 || error.code == 409 || error.code == 422 =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn delete_engineering_source(&self, name: &str) -> anyhow::Result<()> {
        use kube::api::DeleteParams;
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        if api.get_opt(name).await?.is_some() {
            api.delete(name, &DeleteParams::default()).await?;
        }
        Ok(())
    }

    pub async fn replace_engineering_source(
        &self,
        mut current: ConfigMap,
        annotations: &std::collections::BTreeMap<String, String>,
        data: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), kube::Error> {
        use kube::api::PostParams;
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let name = current.metadata.name.clone().unwrap_or_default();
        current.metadata.annotations = Some(annotations.clone());
        current.data = Some(data.clone());
        api.replace(&name, &PostParams::default(), &current)
            .await
            .map(|_| ())
    }

    pub async fn delete_engineering_source_if_version(
        &self,
        name: &str,
        resource_version: String,
    ) -> Result<(), kube::Error> {
        use kube::api::{DeleteParams, Preconditions};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        api.delete(
            name,
            &DeleteParams {
                preconditions: Some(Preconditions {
                    resource_version: Some(resource_version),
                    uid: None,
                }),
                ..DeleteParams::default()
            },
        )
        .await
        .map(|_| ())
    }

    /// Read a team's knowledge-commons ConfigMap (`kars-commons-<commons>`) from
    /// the controller namespace. Returns the parsed index + raw entry content.
    pub async fn read_commons(
        &self,
        commons: &str,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let cm = cms
            .get_opt(&format!("kars-commons-{commons}"))
            .await
            .ok()??;
        cm.data
    }

    /// Read a team's task backlog (raw `tasks.json`, or `[]` when unset). Shared
    /// with the controller: the ConfigMap `kars-team-tasks-<team>` is the durable
    /// queue the Bridge appends to and the controller drains.
    pub async fn read_team_tasks(&self, team: &str) -> String {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        cms.get_opt(&format!("kars-team-tasks-{team}"))
            .await
            .ok()
            .flatten()
            .and_then(|cm| cm.data)
            .and_then(|d| d.get("tasks.json").cloned())
            .unwrap_or_else(|| "[]".to_string())
    }

    /// Read the hierarchical inference-budget config (`kars-inference-budgets`
    /// ConfigMap, key `budgets.json`). Returns the raw JSON string, or `"{}"`
    /// when unset — the budgets route parses it into the typed hierarchy.
    pub async fn read_inference_budgets(&self) -> String {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        cms.get_opt("kars-inference-budgets")
            .await
            .ok()
            .flatten()
            .and_then(|cm| cm.data)
            .and_then(|d| d.get("budgets.json").cloned())
            .unwrap_or_else(|| "{}".to_string())
    }

    /// Persist the hierarchical inference-budget config (server-side apply).
    pub async fn write_inference_budgets(&self, budgets_json: &str) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams};
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let patch = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": "kars-inference-budgets",
                "labels": { "app.kubernetes.io/managed-by": "kars-bridge" },
            },
            "data": { "budgets.json": budgets_json },
        });
        cms.patch(
            "kars-inference-budgets",
            &PatchParams::apply("kars-bridge/inference-budgets").force(),
            &Patch::Apply(patch),
        )
        .await?;
        Ok(())
    }

    /// Read the cluster-wide retention-policy default (`kars-retention-policy`
    /// ConfigMap, key `defaultTtlSeconds`) the controller's KarsTask retention
    /// reconciler reads. `0`/absent means "never auto-delete" (the safe
    /// default). Returns `0` on any read failure.
    pub async fn read_retention_policy(&self) -> i64 {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        cms.get_opt("kars-retention-policy")
            .await
            .ok()
            .flatten()
            .and_then(|cm| cm.data)
            .and_then(|d| {
                d.get("defaultTtlSeconds")
                    .and_then(|v| v.parse::<i64>().ok())
            })
            .unwrap_or(0)
    }

    /// Persist the cluster-wide retention-policy default (server-side apply).
    pub async fn write_retention_policy(&self, ttl_seconds: i64) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams};
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let patch = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": "kars-retention-policy",
                "labels": { "app.kubernetes.io/managed-by": "kars-bridge" },
            },
            "data": { "defaultTtlSeconds": ttl_seconds.to_string() },
        });
        cms.patch(
            "kars-retention-policy",
            &PatchParams::apply("kars-bridge/retention-policy").force(),
            &Patch::Apply(patch),
        )
        .await?;
        Ok(())
    }

    /// Which communication-channel env keys a team has configured. SECURITY:
    /// returns only the *key names* (e.g. `TELEGRAM_BOT_TOKEN`), never the token
    /// values — the Bridge must never echo a secret back to a browser.
    pub async fn team_channel_keys(
        &self,
        namespace: &str,
        team: &str,
    ) -> Result<Vec<String>, kube::Error> {
        let target = self
            .credential_target(namespace, "KarsTeam", team)
            .await?
            .ok_or_else(|| super::credentials::failure("Team credential target does not exist"))?;
        self.configured_channel_keys(namespace, Some(&target)).await
    }

    /// Merge channel credentials into a team's channel Secret (create if absent).
    /// SECURITY: token values are written straight into a K8s Secret and are
    /// never logged or returned. Existing keys not in `data` are preserved.
    pub async fn merge_team_channel(
        &self,
        namespace: &str,
        team: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        let target = self
            .credential_target(namespace, "KarsTeam", team)
            .await?
            .ok_or_else(|| super::credentials::failure("Team credential target does not exist"))?;
        self.write_agent_credentials(
            namespace,
            "KarsTeam",
            team,
            Some(&target.uid),
            data,
            Vec::new(),
        )
        .await?;
        Ok(())
    }

    /// Remove specific channel env keys from a team's channel Secret; delete the
    /// Secret entirely when no keys remain (so "disable all channels" is clean).
    pub async fn remove_team_channel_keys(
        &self,
        namespace: &str,
        team: &str,
        keys: &[String],
    ) -> anyhow::Result<()> {
        let target = self
            .credential_target(namespace, "KarsTeam", team)
            .await?
            .ok_or_else(|| super::credentials::failure("Team credential target does not exist"))?;
        self.write_agent_credentials(
            namespace,
            "KarsTeam",
            team,
            Some(&target.uid),
            std::collections::BTreeMap::new(),
            keys.to_vec(),
        )
        .await?;
        Ok(())
    }

    // ─── Workspace-level (agent-agnostic) channels ───────────────────────────
    // The same channel model as a team's, but scoped to the WORKSPACE (secret
    // `kars-workspace-channels` in kars-system), configured on the Connections
    // tab. The controller propagates it into EVERY run sandbox — mission or team —
    // so any agent can report over Telegram/Slack/Discord/WhatsApp.

    /// The env-key names present in the workspace channel Secret (no values).
    pub async fn workspace_channel_keys(
        &self,
        namespace: &str,
    ) -> Result<Vec<String>, kube::Error> {
        let mut keys = self.configured_channel_keys(namespace, None).await?;
        if self.teams_configured().await? {
            keys.push("TEAMS_ENABLED".into());
        }
        Ok(keys)
    }

    /// Merge channel credentials into the workspace channel Secret (create if
    /// absent). Token values are written straight into a K8s Secret, never logged
    /// or returned. Existing keys not in `data` are preserved.
    pub async fn merge_workspace_channel(
        &self,
        namespace: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        self.write_agent_credentials(namespace, "Workspace", namespace, None, data, Vec::new())
            .await?;
        Ok(())
    }

    /// Remove specific channel env keys from the workspace channel Secret; delete
    /// the Secret entirely when no keys remain.
    pub async fn remove_workspace_channel_keys(
        &self,
        namespace: &str,
        keys: &[String],
    ) -> anyhow::Result<()> {
        self.write_agent_credentials(
            namespace,
            "Workspace",
            namespace,
            None,
            std::collections::BTreeMap::new(),
            keys.to_vec(),
        )
        .await?;
        Ok(())
    }

    /// `KarsReceipt` API scoped to a namespace.
    pub fn receipts(&self, namespace: &str) -> Api<crate::kars::receipt::KarsReceipt> {
        Api::namespaced(self.client.clone(), namespace)
    }

    /// Write Teams gateway credentials into the dedicated `kars-bridge-teams` Secret.
    /// This Secret is mounted ONLY by the Teams gateway pod — never propagated to
    /// sandbox pods. Uses Server-Side Apply so the BFF can create-or-update idempotently.
    pub async fn write_dedicated_teams_secret(
        &self,
        namespace: &str,
        name: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        self.mutate_integration(namespace, name, |keys| keys.extend(data.clone()))
            .await?;
        Ok(())
    }

    /// Revoke Teams bot credentials while retaining the BFF-only internal
    /// secret and role map required for a healthy BFF rollout.
    pub async fn disable_dedicated_teams_secret(
        &self,
        namespace: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        self.mutate_integration(namespace, name, |keys| {
            for key in ["client-id", "tenant-id", "client-secret"] {
                keys.remove(key);
            }
        })
        .await?;
        Ok(())
    }

    /// Restart BFF and enable/disable the Teams gateway so Secret and role-map
    /// changes become effective immediately.
    pub async fn reconcile_teams_deployments(
        &self,
        namespace: &str,
        gateway_name: &str,
        bff_name: &str,
        _enabled: bool,
    ) -> anyhow::Result<()> {
        self.request_teams_reconcile(namespace, gateway_name, bff_name)
            .await?;
        Ok(())
    }

    /// `KarsApproval` API scoped to a namespace.
    pub fn approvals(&self, namespace: &str) -> Api<crate::kars::approval::KarsApproval> {
        Api::namespaced(self.client.clone(), namespace)
    }

    /// `KarsSREAction` API, cluster-wide. This is an operator-persona,
    /// platform-level surface (the kars-sre agent's proposals), not scoped to
    /// a workspace namespace — mirrors the `KarsTask` `Api::all` pattern used
    /// for cross-namespace operator views.
    pub fn sre_actions_all(&self) -> Api<crate::kars::sre_action::KarsSREAction> {
        Api::all(self.client.clone())
    }

    /// `KarsSREAction` API scoped to a namespace (for approve/reject patches,
    /// which must target the CR's own namespace).
    pub fn sre_actions(&self, namespace: &str) -> Api<crate::kars::sre_action::KarsSREAction> {
        Api::namespaced(self.client.clone(), namespace)
    }

    /// Find the name of the **Running** pod for a sandbox in its namespace.
    /// A task-materialized sandbox runs in namespace `kars-<sandbox>`; its pod
    /// carries `kars.azure.com/sandbox=<sandbox>`. Returns `None` if no Running
    /// pod is found.
    /// Whether a Deployment matching `name` exists in `namespace` (best-effort;
    /// false on any API error). Used to detect optional integrations like the
    /// Headlamp dashboard (`headlamp` deployment in the `headlamp` namespace).
    pub async fn deployment_exists(&self, namespace: &str, name: &str) -> bool {
        use k8s_openapi::api::apps::v1::Deployment;
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        matches!(api.get_opt(name).await, Ok(Some(_)))
    }

    /// Every pod in the kars-relevant namespaces (all `kars*` namespaces plus
    /// `agentmesh`), for the operator diagnostics scan. Uses a cluster-wide list
    /// then filters, so it's one API call regardless of sandbox count.
    pub async fn all_pods(&self) -> Vec<k8s_openapi::api::core::v1::Pod> {
        let pods: Api<Pod> = Api::all(self.client.clone());
        pods.list(&ListParams::default())
            .await
            .map(|l| {
                l.items
                    .into_iter()
                    .filter(|p| {
                        let ns = p.metadata.namespace.as_deref().unwrap_or("");
                        ns.starts_with("kars") || ns == "agentmesh"
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn running_pod_for_sandbox(&self, sandbox: &str) -> Option<String> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &ns);
        let list = pods
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={sandbox}")))
            .await
            .ok()?;
        list.items.into_iter().find_map(|p| {
            let phase = p.status.as_ref().and_then(|s| s.phase.as_deref());
            if phase == Some("Running") {
                p.metadata.name
            } else {
                None
            }
        })
    }

    /// Honest health of a sandbox's running pod: container readiness, restart
    /// count, uptime, and node. No metrics-server dependency (no CPU/mem) — these
    /// are status-derived signals that answer "is this agent healthy right now".
    /// `None` when no pod is running for the sandbox.
    pub async fn sandbox_pod_health(&self, sandbox: &str) -> Option<PodHealth> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &ns);
        let list = pods
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={sandbox}")))
            .await
            .ok()?;
        let pod = list
            .items
            .into_iter()
            .find(|p| p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))?;
        let status = pod.status.as_ref();
        let cs = status.and_then(|s| s.container_statuses.as_ref());
        let total = cs.map(|c| c.len()).unwrap_or(0) as i32;
        let ready = cs
            .map(|c| c.iter().filter(|s| s.ready).count())
            .unwrap_or(0) as i32;
        let restarts = cs
            .map(|c| c.iter().map(|s| s.restart_count).sum())
            .unwrap_or(0);
        // Uptime from the pod start time.
        let uptime_seconds = status
            .and_then(|s| s.start_time.as_ref())
            .map(|t| (chrono::Utc::now() - t.0).num_seconds().max(0));
        // A container stuck waiting (e.g. CrashLoopBackOff) is the honest
        // unhealthy signal — surface the reason.
        let waiting_reason = cs.and_then(|c| {
            c.iter().find_map(|s| {
                s.state
                    .as_ref()
                    .and_then(|st| st.waiting.as_ref())
                    .and_then(|w| w.reason.clone())
            })
        });
        Some(PodHealth {
            ready_containers: ready,
            total_containers: total,
            restarts,
            uptime_seconds,
            node: pod.spec.as_ref().and_then(|s| s.node_name.clone()),
            waiting_reason,
        })
    }

    /// Read recent logs from a sandbox pod container (best-effort). Powers the
    /// live run-failure troubleshooter, which surfaces the REAL agent output as
    /// evidence rather than pattern-matching a status string.
    pub async fn read_sandbox_logs(
        &self,
        sandbox: &str,
        container: &str,
        tail: i64,
    ) -> Option<String> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &ns);
        let list = pods
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={sandbox}")))
            .await
            .ok()?;
        let pod_name = list.items.into_iter().find_map(|p| p.metadata.name)?;
        let lp = kube::api::LogParams {
            container: Some(container.to_string()),
            tail_lines: Some(tail),
            timestamps: false,
            ..Default::default()
        };
        pods.logs(&pod_name, &lp).await.ok()
    }

    /// Per-container status for a sandbox pod (name, ready, restarts, and the
    /// current state reason — Running / a waiting reason like ImagePullBackOff /
    /// a terminated reason like OOMKilled). Used by the troubleshooter.
    pub async fn sandbox_container_states(&self, sandbox: &str) -> Vec<ContainerState> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &ns);
        let Ok(list) = pods
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={sandbox}")))
            .await
        else {
            return Vec::new();
        };
        let Some(pod) = list.items.into_iter().next() else {
            return Vec::new();
        };
        let cs = pod
            .status
            .as_ref()
            .and_then(|s| s.container_statuses.as_ref());
        cs.map(|list| {
            list.iter()
                .map(|c| {
                    let (state, reason) = if let Some(st) = c.state.as_ref() {
                        if st.running.is_some() {
                            ("running".to_string(), None)
                        } else if let Some(w) = st.waiting.as_ref() {
                            ("waiting".to_string(), w.reason.clone())
                        } else if let Some(t) = st.terminated.as_ref() {
                            ("terminated".to_string(), t.reason.clone())
                        } else {
                            ("unknown".to_string(), None)
                        }
                    } else {
                        ("unknown".to_string(), None)
                    };
                    ContainerState {
                        name: c.name.clone(),
                        ready: c.ready,
                        restarts: c.restart_count,
                        state,
                        reason,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// Find ANY running sandbox's namespace + pod, so the Bridge can route an
    /// orchestrator/composer model call through an existing secure inference
    /// router (via `router_chat`). This is how the envelope composer reaches the
    /// model on workload-identity clusters — without a static token, reusing the
    /// same governed path agents use. Prefers a persistent sandbox; falls back
    /// to any Running sandbox pod. Returns `(namespace, pod)`.
    /// Ranked list of stable sandbox `(namespace, pod)` candidates whose
    /// inference router the Bridge can route an orchestrator/composer model call
    /// through. Excludes ephemeral standing-run sandboxes (short-lived / busy),
    /// requires the router container ready, and orders freshest-first (a
    /// recently (re)started pod runs the current router image with valid
    /// provider auth). The caller tries them in order so a single sandbox with
    /// stale auth or a warming router is skipped gracefully.
    pub async fn running_sandbox_candidates(&self) -> Vec<(String, String)> {
        let pods: Api<Pod> = Api::all(self.client.clone());
        let Ok(list) = pods
            .list(&ListParams::default().labels("kars.azure.com/sandbox"))
            .await
        else {
            return Vec::new();
        };
        let mut candidates: Vec<&Pod> = list
            .items
            .iter()
            .filter(|p| {
                let phase = p.status.as_ref().and_then(|s| s.phase.as_deref());
                if phase != Some("Running") {
                    return false;
                }
                let name = p.metadata.name.as_deref().unwrap_or_default();
                let sandbox = p
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|l| l.get("kars.azure.com/sandbox"))
                    .map(String::as_str)
                    .unwrap_or(name);
                // Prefer stable sandboxes, but do NOT exclude ephemeral team-run
                // sandboxes — on a teams-only cluster they are the ONLY inference
                // path the orchestrator has. We sort them last (below) so a stable
                // sandbox always wins when one exists.
                let _ = sandbox;
                p.status
                    .as_ref()
                    .and_then(|s| s.container_statuses.as_ref())
                    .map(|cs| cs.iter().any(|c| c.name == "inference-router" && c.ready))
                    .unwrap_or(false)
            })
            .collect();
        candidates.sort_by(|a, b| {
            // Stable sandboxes before ephemeral run sandboxes, then freshest first.
            let eph = |p: &&Pod| -> bool {
                let n = p.metadata.name.as_deref().unwrap_or_default();
                let sb = p
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|l| l.get("kars.azure.com/sandbox"))
                    .map(String::as_str)
                    .unwrap_or(n);
                is_ephemeral_run(sb)
            };
            let ta = a.metadata.creation_timestamp.as_ref().map(|t| t.0);
            let tb = b.metadata.creation_timestamp.as_ref().map(|t| t.0);
            eph(a).cmp(&eph(b)).then(tb.cmp(&ta)) // stable first, then freshest
        });
        candidates
            .into_iter()
            .filter_map(|p| Some((p.metadata.namespace.clone()?, p.metadata.name.clone()?)))
            .collect()
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

    /// The live egress enforcement mode of a sandbox — read from the
    /// `KarsSandbox.spec.networkPolicy.egressMode` the controller materialized.
    /// `"Learn"` (default) observes + records every domain the agent reaches
    /// without denying; `"Strict"` denies anything outside the allowlist. This
    /// is the real, cluster-truth mode — not derived from the blueprint.
    pub async fn sandbox_egress_mode(&self, sandbox: &str) -> Option<String> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", "KarsSandbox");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), "kars-system", &ar);
        let sb = api.get_opt(sandbox).await.ok().flatten()?;
        Some(
            sb.data
                .get("spec")
                .and_then(|s| s.get("networkPolicy"))
                .and_then(|n| n.get("egressMode"))
                .and_then(|m| m.as_str())
                .unwrap_or("Learn")
                .to_string(),
        )
    }

    /// Read only the declared private observation capability. The legacy
    /// agent-shared admin token and apiserver header tricks are never fallbacks.
    pub async fn sandbox_learned_domains(&self, sandbox: &str) -> anyhow::Result<Vec<String>> {
        self.private_learned_domains(sandbox)
            .await
            .map_err(Into::into)
    }

    /// The resolved egress allowlist the sandbox actually enforces — read from
    /// the `karssandbox-<sandbox>-egress-allowlist` ConfigMap the controller
    /// compiles into the sandbox namespace. Each entry is the exact host(:port)
    /// the agent is permitted to reach. Empty in Learn mode (nothing pinned).
    pub async fn sandbox_allowlist(&self, sandbox: &str) -> Vec<String> {
        let ns = format!("kars-{sandbox}");
        let name = format!("karssandbox-{sandbox}-egress-allowlist");
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), &ns);
        let Some(cm) = cms.get_opt(&name).await.ok().flatten() else {
            return Vec::new();
        };
        let Some(raw) = cm.data.and_then(|d| d.get("allowlist.json").cloned()) else {
            return Vec::new();
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Vec::new();
        };
        parsed
            .get("endpoints")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        let host = e.get("host").and_then(|h| h.as_str())?;
                        match e.get("port").and_then(|p| p.as_u64()) {
                            Some(p) => Some(format!("{host}:{p}")),
                            None => Some(host.to_string()),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Persist a mission's run output into a namespaced ConfigMap
    /// `kars-mission-output-<task>` in `kars-system`, so the deliverable is a
    /// durable, readable cluster object (the §16 artifact record, minimal form).
    /// Server-side apply, idempotent per task.
    pub async fn write_mission_output(
        &self,
        task: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams};
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let name = format!("kars-mission-output-{task}");
        let patch = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": { "name": name, "labels": { "kars.azure.com/mission-output": task } },
            "data": data,
        });
        cms.patch(
            &name,
            &PatchParams::apply("kars-bridge/mission-output").force(),
            &Patch::Apply(patch),
        )
        .await?;
        Ok(())
    }

    /// Read a mission's persisted run output ConfigMap, if present.
    pub async fn read_mission_output(
        &self,
        task: &str,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        self.configmap_data(&format!("kars-mission-output-{task}"))
            .await
    }

    /// Read a mission's persisted artifact set — the complete file set the
    /// agent produced over the mesh, written by the controller to
    /// `kars-mission-artifacts-<task>`. Text artifacts come back as `data`
    /// (filename → content); binary artifacts are reported by name + size via
    /// the output ConfigMap's manifest (their bytes live in the ConfigMap's
    /// `binaryData` and aren't inlined here). Returns `None` when the mission
    /// produced no artifacts (honest empty, never fabricated).
    pub async fn read_mission_artifacts(
        &self,
        task: &str,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        self.configmap_data(&format!("kars-mission-artifacts-{task}"))
            .await
    }

    /// Read a single artifact file's raw bytes for download — text artifacts
    /// from the ConfigMap's `data`, binary ones from `binaryData` (base64). The
    /// filename is matched against the same sanitized key the manifest exposes.
    /// Returns `(bytes, is_binary)` or `None` when the file isn't found. This is
    /// the Bridge-native fetch path so operators never need `kubectl`.
    pub async fn read_mission_artifact_bytes(
        &self,
        task: &str,
        key: &str,
    ) -> Option<(Vec<u8>, bool)> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let cm = cms
            .get_opt(&format!("kars-mission-artifacts-{task}"))
            .await
            .ok()
            .flatten()?;
        if let Some(text) = cm.data.as_ref().and_then(|d| d.get(key)) {
            return Some((text.clone().into_bytes(), false));
        }
        // `binaryData` values are `ByteString`, already base64-decoded by the API
        // client into raw bytes — serve them directly.
        if let Some(bytes) = cm.binary_data.as_ref().and_then(|d| d.get(key)) {
            return Some((bytes.0.clone(), true));
        }
        None
    }

    /// Read a mission's persisted execution trace — the clean per-tool audit
    /// record the controller wrote to `kars-mission-trace-<task>`. Returns the
    /// raw `trace.json` string (a JSON array of round/tool events) when present.
    pub async fn read_mission_trace(&self, task: &str) -> Option<String> {
        self.configmap_data(&format!("kars-mission-trace-{task}"))
            .await
            .and_then(|d| d.get("trace.json").cloned())
    }

    pub async fn read_mission_progress(&self, task: &str) -> Option<serde_json::Value> {
        self.configmap_data(&format!("kars-mission-progress-{task}"))
            .await
            .and_then(|data| data.get("checkpoint.json").cloned())
            .and_then(|raw| serde_json::from_str(&raw).ok())
    }

    /// LIVE per-agent execution trace, straight from a running sandbox's router
    /// (`GET /telemetry/trace` — a PUBLIC in-pod endpoint, reached via the
    /// apiserver pod-proxy; no admin token required). Unlike the persisted
    /// `kars-mission-trace-<task>` ConfigMap (written once, at delivery), this
    /// ticks WHILE the agent works, so the activity stream is genuinely live.
    /// Returns the router's `events` array (round/tool shape); empty on any
    /// error or when the sandbox has no running pod yet.
    pub async fn sandbox_live_trace(&self, sandbox: &str) -> Vec<serde_json::Value> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(self.client.clone(), &ns);
        let pod_list = pods.list(&ListParams::default()).await;
        if let Err(e) = &pod_list {
            tracing::warn!(target: "kars_bridge::live_trace", %ns, error = %e, "pod list failed");
        }
        let Some(pod) = pod_list
            .ok()
            .and_then(|l| {
                l.items.into_iter().find(|p| {
                    p.status
                        .as_ref()
                        .and_then(|s| s.phase.as_deref())
                        .map(|ph| ph == "Running")
                        .unwrap_or(false)
                })
            })
            .and_then(|p| p.metadata.name)
        else {
            tracing::warn!(target: "kars_bridge::live_trace", %ns, "no running pod found");
            return Vec::new();
        };
        let path = format!("/api/v1/namespaces/{ns}/pods/{pod}:8443/proxy/telemetry/trace");
        let Ok(req) = http::Request::builder()
            .method(http::Method::GET)
            .uri(&path)
            .body(Vec::new())
        else {
            tracing::warn!(target: "kars_bridge::live_trace", %path, "request build failed");
            return Vec::new();
        };
        match self.client.request_text(req).await {
            Ok(text) => {
                let n = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("events").and_then(|e| e.as_array()).cloned())
                    .unwrap_or_default();
                tracing::debug!(target: "kars_bridge::live_trace", %pod, events = n.len(), body_len = text.len(), "live trace ok");
                n
            }
            Err(e) => {
                tracing::warn!(target: "kars_bridge::live_trace", %path, error = %e, "proxy request failed");
                Vec::new()
            }
        }
    }

    /// Names of the sub-agent sandboxes a principal spawned at run time — the
    /// complete transitive `kars.azure.com/parent` tree. Used to aggregate the
    /// WHOLE agent tree's live activity, not just direct children.
    pub async fn sub_agent_sandboxes(
        &self,
        namespace: &str,
        parent_sandbox: &str,
    ) -> Vec<DynamicObject> {
        self.list_kind(namespace, "KarsSandbox")
            .await
            .map(|items| descendant_sandbox_objects(&items, parent_sandbox))
            .unwrap_or_default()
    }

    pub async fn sub_agent_sandbox_names(
        &self,
        namespace: &str,
        parent_sandbox: &str,
    ) -> Vec<String> {
        self.sub_agent_sandboxes(namespace, parent_sandbox)
            .await
            .iter()
            .filter_map(|sandbox| sandbox.metadata.name.clone())
            .collect()
    }

    /// Count missions with a real per-tool execution-trace record — the live
    /// telemetry substrate. Counts `kars-mission-trace-*` ConfigMaps carrying a
    /// non-empty trace, not deliverable count (the two can differ).
    pub async fn count_trace_records(&self) -> usize {
        use kube::api::ListParams;
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        cms.list(&ListParams::default())
            .await
            .map(|l| {
                l.items
                    .iter()
                    .filter_map(trace_record_identity)
                    .collect::<std::collections::HashSet<_>>()
                    .len()
            })
            .unwrap_or(0)
    }

    /// List every mission that has produced a captured deliverable — one entry
    /// per `kars-mission-output-*` ConfigMap. New records retain the full
    /// evidence key in an annotation because nonce-scoped label values can
    /// exceed Kubernetes' 63-byte limit; legacy records fall back to the label.
    /// Returns `(task, data)` pairs so the caller can build the cross-mission
    /// Artifacts index from real, durable records (never fabricated). Sorted by
    /// `finishedAt` descending so the most recent deliverables surface first.
    pub async fn list_mission_outputs(&self) -> Vec<MissionOutputRecord> {
        use kube::api::ListParams;
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let list = match cms
            .list(&ListParams::default().labels("kars.azure.com/mission-output"))
            .await
        {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };
        let records: Vec<(
            String,
            Option<String>,
            std::collections::BTreeMap<String, String>,
        )> = list
            .items
            .into_iter()
            .filter_map(mission_output_candidate)
            .collect();
        let mut out = select_mission_output_records(records)
            .into_iter()
            .map(|(evidence_key, data)| project_mission_output_record(evidence_key, data))
            .collect::<Vec<_>>();
        out.sort_by(|a, b| {
            b.data
                .get("finishedAt")
                .cloned()
                .unwrap_or_default()
                .cmp(&a.data.get("finishedAt").cloned().unwrap_or_default())
        });
        out
    }

    /// List each nonce-scoped execution exactly once for accounting, efficiency,
    /// and historical evidence. Immutable archives/canonical records are kept;
    /// task-keyed current-pointer mirrors are excluded.
    pub async fn list_mission_output_evidence(&self) -> Vec<MissionOutputRecord> {
        use kube::api::ListParams;
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let list = match cms
            .list(&ListParams::default().labels("kars.azure.com/mission-output"))
            .await
        {
            Ok(list) => list,
            Err(_) => return Vec::new(),
        };
        let records = list
            .items
            .into_iter()
            .filter_map(mission_output_candidate)
            .collect();
        let mut out = select_mission_evidence_records(records)
            .into_iter()
            .map(|(evidence_key, data)| project_mission_output_record(evidence_key, data))
            .collect::<Vec<_>>();
        out.sort_by(|left, right| {
            right
                .data
                .get("finishedAt")
                .cloned()
                .unwrap_or_default()
                .cmp(&left.data.get("finishedAt").cloned().unwrap_or_default())
        });
        out
    }

    /// Request a **mesh-driven agent run** of a task by stamping the
    /// `kars.azure.com/run-requested` annotation with a fresh nonce. The core
    /// controller (a live mesh peer) watches this annotation, discovers the
    /// agent over the mesh, delivers the objective straight into the agent's
    /// native loop (gated by the AGT `task:execute` policy), captures the
    /// reply, writes it to `kars-mission-output-<task>`, and stamps
    /// `kars.azure.com/run-completed` with the same nonce. This is the Bridge
    /// *consuming* a neutral core capability — the Bridge never reaches into
    /// the agent itself. Returns the nonce to correlate completion.
    pub async fn request_mesh_run(&self, ns: &str, name: &str) -> anyhow::Result<String> {
        use kube::api::{Patch, PatchParams};
        // In-flight guard: if a run is already pending (run-requested set to a
        // nonce the controller hasn't completed yet), REUSE that nonce instead of
        // stamping a fresh one. Two concurrent triggers (double-click, cadence +
        // run-now) would otherwise each mint a distinct nonce; the controller acks
        // only the last, the first caller's await never matches → it single-turns
        // while the mesh also delivers → the task executes twice and the outputs
        // clobber. Reusing the pending nonce makes both callers await the same run.
        if let Ok(Some(task)) = self.tasks(ns).get_opt(name).await {
            let ann = task.metadata.annotations.unwrap_or_default();
            let requested = ann.get("kars.azure.com/run-requested").cloned();
            let completed = ann.get("kars.azure.com/run-completed").cloned();
            if let Some(req) = requested.filter(|r| !r.is_empty())
                && completed.as_deref() != Some(req.as_str())
            {
                return Ok(req);
            }
        }
        let nonce = format!(
            "run-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let patch = serde_json::json!({
            "metadata": { "annotations": { "kars.azure.com/run-requested": nonce } }
        });
        self.tasks(ns)
            .patch(name, &PatchParams::default(), &Patch::Merge(patch))
            .await?;
        // Re-read and adopt whatever nonce actually won the annotation, so two
        // truly-simultaneous triggers converge on the SAME run instead of each
        // awaiting its own (last-write-wins) nonce.
        if let Ok(Some(task)) = self.tasks(ns).get_opt(name).await
            && let Some(actual) = task
                .metadata
                .annotations
                .and_then(|a| a.get("kars.azure.com/run-requested").cloned())
                .filter(|r| !r.is_empty())
        {
            return Ok(actual);
        }
        Ok(nonce)
    }

    /// Poll the task's `kars.azure.com/run-completed` annotation until it
    /// equals `nonce` (the controller stamps it once the mesh round-trip is
    /// done) or `timeout` elapses. Returns the freshly-written mission output
    /// on completion, or `None` on timeout.
    /// Outcome of awaiting a mesh run. Distinguishes "the mesh peer never picked
    /// this up" (safe to fall back to a single turn) from "it acknowledged and is
    /// actively delivering" (must NOT single-turn — that would race the
    /// controller's deliverable write).
    pub async fn await_mesh_run(
        &self,
        ns: &str,
        name: &str,
        nonce: &str,
        timeout: std::time::Duration,
    ) -> MeshRunOutcome {
        let deadline = std::time::Instant::now() + timeout;
        let mut saw_ack = false;
        let mut saw_activity = false;
        loop {
            if let Ok(Some(task)) = self.tasks(ns).get_opt(name).await {
                let ann = task.metadata.annotations.clone().unwrap_or_default();
                if ann.get("kars.azure.com/run-ack").map(String::as_str) == Some(nonce) {
                    saw_ack = true;
                }
                if ann.get("kars.azure.com/run-completed").map(String::as_str) == Some(nonce) {
                    return match self.read_mission_output(name).await {
                        Some(out) => MeshRunOutcome::Completed(out),
                        None => MeshRunOutcome::InProgress,
                    };
                }
            }
            // LIVE-ACTIVITY signal — the robust "a real agent loop is running"
            // proof that works even against an OLD controller that never stamps
            // run-ack. If the sandbox's router is emitting rounds/tool calls, a
            // genuine run is in flight and we must NEVER single-turn over it
            // (that produced a garbage one-shot deliverable that clobbered the
            // real streaming run). Latch it once seen.
            if !saw_activity && !self.sandbox_live_trace(name).await.is_empty() {
                saw_activity = true;
            }
            if std::time::Instant::now() >= deadline {
                return if saw_ack || saw_activity {
                    MeshRunOutcome::InProgress
                } else {
                    MeshRunOutcome::NeverProcessed
                };
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// Clear the pending `run-requested` annotation — used when the BFF gives up
    /// on the mesh path and single-turns, so a late mesh-peer recovery doesn't
    /// ALSO deliver + write the output (double-write).
    pub async fn clear_run_request(&self, ns: &str, name: &str) {
        use kube::api::{Patch, PatchParams};
        let patch = serde_json::json!({
            "metadata": { "annotations": { "kars.azure.com/run-requested": serde_json::Value::Null } }
        });
        let _ = self
            .tasks(ns)
            .patch(name, &PatchParams::default(), &Patch::Merge(patch))
            .await;
    }

    /// Discover a running agent's **mesh identity** from the AGT registry — the
    /// harness-neutral discovery layer. Every runtime adapter registers its
    /// agent under capabilities that include the sandbox name; we query
    /// `/v1/discover?capability=<sandbox>` through the Kubernetes services/proxy
    /// subresource (the registry is a ClusterIP service the BFF reaches via the
    /// API server) and return the most-recently-seen DID + its capabilities and
    /// last-seen time. This proves the agent is a real, live mesh participant
    /// and is the discovery prerequisite for mesh-driven task delivery. Returns
    /// `None` when the registry is unreachable or the agent isn't registered
    /// (honest empty, never fabricated).
    pub async fn discover_agent_identity(&self, sandbox: &str) -> Option<AgentIdentity> {
        let path = format!(
            "/api/v1/namespaces/agentmesh/services/agentmesh-registry:8080/proxy/v1/discover?capability={sandbox}&limit=10"
        );
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri(path)
            .body(Vec::new())
            .ok()?;
        let text = self.client.request_text(req).await.ok()?;
        let body: serde_json::Value = serde_json::from_str(&text).ok()?;
        let results = body.get("results")?.as_array()?;
        // Pick the most-recently-seen registration for this sandbox.
        let best = results
            .iter()
            .filter(|r| {
                r.get("capabilities")
                    .and_then(|c| c.as_array())
                    .map(|caps| caps.iter().any(|c| c.as_str() == Some(sandbox)))
                    .unwrap_or(false)
            })
            .max_by_key(|r| {
                r.get("last_seen")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            })?;
        Some(AgentIdentity {
            did: best.get("did")?.as_str()?.to_string(),
            capabilities: best
                .get("capabilities")
                .and_then(|c| c.as_array())
                .map(|caps| {
                    caps.iter()
                        .filter_map(|c| c.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            last_seen: best
                .get("last_seen")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            reputation_score: best.get("reputation_score").and_then(|v| v.as_f64()),
        })
    }

    /// Read a ConfigMap's `data` map from `kars-system` (e.g. the receipt
    /// inclusion-log signed checkpoint). Returns `None` when absent.
    pub async fn configmap_data(
        &self,
        name: &str,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        cms.get_opt(name).await.ok().flatten().and_then(|c| c.data)
    }

    /// Like `configmap_data` but distinguishes a genuine API error from an absent
    /// ConfigMap: `Ok(None)` means "not found", `Err` means the read actually
    /// failed. Use on critical read-modify-write paths so a transient cluster
    /// error can't be mistaken for "no prior data" and silently clobber it.
    pub async fn configmap_data_result(
        &self,
        name: &str,
    ) -> Result<Option<std::collections::BTreeMap<String, String>>, kube::Error> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        Ok(cms.get_opt(name).await?.and_then(|c| c.data))
    }

    /// List `kars-system` ConfigMaps matching a label selector, retaining each
    /// object name so callers can verify ordered segmented stores.
    pub async fn configmaps_data_by_label(
        &self,
        selector: &str,
    ) -> Result<Vec<(String, std::collections::BTreeMap<String, String>)>, kube::Error> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        Ok(cms
            .list(&ListParams::default().labels(selector))
            .await?
            .items
            .into_iter()
            .filter_map(|cm| Some((cm.metadata.name?, cm.data.unwrap_or_default())))
            .collect())
    }

    /// Read-modify-write a `kars-system` ConfigMap's `data` under OPTIMISTIC
    /// CONCURRENCY: a single atomic JSON Patch (RFC 6902) — a `test` op
    /// asserting `resourceVersion` hasn't moved, followed by `add`/`remove`
    /// ops for the actual data changes — retried on failure. This makes
    /// concurrent writers serialize instead of silently clobbering each other
    /// (an SSA `force` apply of the whole `data` drops the other writer's
    /// fields; a `replace()`/PUT needs the `update` RBAC verb, which the
    /// BFF's ClusterRole never grants — confirmed live against the real
    /// ServiceAccount: a PUT-based CAS here 403s in an RBAC-enforced
    /// deployment. See `mutate_secret_keys` for the full rationale, including
    /// why a plain JSON *merge* patch alone can't do this: K8s doesn't honor
    /// `resourceVersion` as a precondition for merge patches, only for
    /// `test`-op JSON Patches, SSA, and PUT). Use for any read-append-write
    /// on a shared ConfigMap (e.g. review history).
    pub async fn update_configmap_data<F>(
        &self,
        name: &str,
        labels: &[(&str, &str)],
        mut modify: F,
    ) -> Result<(), kube::Error>
    where
        F: FnMut(&mut std::collections::BTreeMap<String, String>),
    {
        use k8s_openapi::api::core::v1::ConfigMap;
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
        use kube::api::{Patch, PostParams};
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let label_map: std::collections::BTreeMap<String, String> = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for _attempt in 0..6 {
            let existing = cms.get_opt(name).await?;
            let Some(current) = existing else {
                // Doesn't exist yet — a JSON Patch has nothing to patch onto;
                // create it fresh (a create-race surfaces as a 409 below).
                let mut data = std::collections::BTreeMap::new();
                modify(&mut data);
                let cm = ConfigMap {
                    metadata: ObjectMeta {
                        name: Some(name.to_string()),
                        labels: (!label_map.is_empty()).then(|| label_map.clone()),
                        ..Default::default()
                    },
                    data: Some(data),
                    ..Default::default()
                };
                match cms.create(&PostParams::default(), &cm).await {
                    Ok(_) => return Ok(()),
                    Err(kube::Error::Api(ae)) if ae.code == 409 => continue,
                    Err(e) => return Err(e),
                }
            };
            let Some(rv) = current.metadata.resource_version.clone() else {
                continue; // no resourceVersion to pin to — re-read and retry.
            };
            let before = current.data.clone().unwrap_or_default();
            let mut after = before.clone();
            modify(&mut after);

            let mut ops: Vec<json_patch::PatchOperation> = vec![json_patch::PatchOperation::Test(
                json_patch::TestOperation {
                    path: json_patch::jsonptr::PointerBuf::from_tokens([
                        "metadata",
                        "resourceVersion",
                    ]),
                    value: serde_json::Value::String(rv),
                },
            )];
            if current.data.is_none() {
                ops.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                    path: json_patch::jsonptr::PointerBuf::from_tokens(["data"]),
                    value: serde_json::json!({}),
                }));
            }
            for key in before.keys() {
                if !after.contains_key(key) {
                    ops.push(json_patch::PatchOperation::Remove(
                        json_patch::RemoveOperation {
                            path: json_patch::jsonptr::PointerBuf::from_tokens([
                                "data",
                                key.as_str(),
                            ]),
                        },
                    ));
                }
            }
            for (key, value) in &after {
                ops.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                    path: json_patch::jsonptr::PointerBuf::from_tokens(["data", key.as_str()]),
                    value: serde_json::Value::String(value.clone()),
                }));
            }
            if !label_map.is_empty() {
                if current.metadata.labels.is_none() {
                    ops.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                        path: json_patch::jsonptr::PointerBuf::from_tokens(["metadata", "labels"]),
                        value: serde_json::json!({}),
                    }));
                }
                for (k, v) in &label_map {
                    ops.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                        path: json_patch::jsonptr::PointerBuf::from_tokens([
                            "metadata",
                            "labels",
                            k.as_str(),
                        ]),
                        value: serde_json::Value::String(v.clone()),
                    }));
                }
            }

            match cms
                .patch(
                    name,
                    &kube::api::PatchParams::default(),
                    &Patch::Json::<ConfigMap>(json_patch::Patch(ops)),
                )
                .await
            {
                Ok(_) => return Ok(()),
                // 422 = the `test` op failed (resourceVersion moved under us,
                // i.e. a real concurrent writer) — re-read and retry. 409
                // covers any other conflict (e.g. a create race).
                Err(kube::Error::Api(ae)) if ae.code == 422 || ae.code == 409 => continue,
                Err(e) => return Err(e),
            }
        }
        Err(kube::Error::Api(kube::core::ErrorResponse {
            status: "Failure".into(),
            message: format!("exhausted optimistic-concurrency retries writing {name}"),
            reason: "Conflict".into(),
            code: 409,
        }))
    }

    /// Read all team digest logs (`kars-team-digest-*`) across teams, flattened
    /// and newest-first. Best-effort.
    pub async fn list_team_digests(&self) -> Vec<serde_json::Value> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let lp = ListParams::default().labels("kars.azure.com/team-digest");
        let mut out: Vec<serde_json::Value> = Vec::new();
        if let Ok(list) = cms.list(&lp).await {
            for cm in list.items {
                if let Some(log) = cm.data.as_ref().and_then(|d| d.get("log.json"))
                    && let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(log)
                {
                    out.extend(entries);
                }
            }
        }
        out.sort_by(|a, b| {
            b.get("at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(a.get("at").and_then(|v| v.as_str()).unwrap_or(""))
        });
        out
    }

    /// Read a task's review record (`kars-mission-review-<task>`), if any.
    pub async fn read_review(
        &self,
        task: &str,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        self.configmap_data(&format!("kars-mission-review-{task}"))
            .await
    }

    /// Write a task's review record (`kars-mission-review-<task>`), SSA-merged.
    pub async fn write_review(
        &self,
        task: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams};
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let name = format!("kars-mission-review-{task}");
        let patch = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": { "name": name, "labels": { "kars.azure.com/mission-review": task } },
            "data": data,
        });
        cms.patch(
            &name,
            &PatchParams::apply("kars-bridge-bff").force(),
            &Patch::Apply(patch),
        )
        .await?;
        Ok(())
    }

    /// Re-drive a task on reviewer feedback without mutating its immutable spec.
    /// The revision objective is nonce-bound and digest-protected in annotations;
    /// the controller verifies it before constructing the signed task contract.
    pub async fn redrive_with_revision(
        &self,
        ns: &str,
        name: &str,
        revised_objective: &str,
    ) -> anyhow::Result<String> {
        use kube::api::{Patch, PatchParams};
        // In-flight guard (same rationale as request_mesh_run): if a run is
        // already pending, don't stamp a second concurrent redrive — reuse the
        // pending nonce so two concurrent request_changes reviews can't double-
        // execute the producing agent. The revision remains nonce-scoped.
        if let Ok(Some(task)) = self.tasks(ns).get_opt(name).await {
            let ann = task.metadata.annotations.clone().unwrap_or_default();
            let requested = ann.get("kars.azure.com/run-requested").cloned();
            let completed = ann.get("kars.azure.com/run-completed").cloned();
            if let Some(req) = requested.filter(|r| !r.is_empty())
                && completed.as_deref() != Some(req.as_str())
            {
                let encoded = BASE64_STANDARD.encode(revised_objective.as_bytes());
                let digest = format!("sha256:{:x}", Sha256::digest(revised_objective.as_bytes()));
                let patch = serde_json::json!({
                    "metadata": { "annotations": {
                        "kars.azure.com/run-objective-nonce": req.clone(),
                        "kars.azure.com/run-objective-b64": encoded,
                        "kars.azure.com/run-objective-digest": digest
                    }}
                });
                self.tasks(ns)
                    .patch(name, &PatchParams::default(), &Patch::Merge(patch))
                    .await?;
                return Ok(req);
            }
        }
        let nonce = format!(
            "rev-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let encoded = BASE64_STANDARD.encode(revised_objective.as_bytes());
        let digest = format!("sha256:{:x}", Sha256::digest(revised_objective.as_bytes()));
        let patch = serde_json::json!({
            "metadata": { "annotations": {
                "kars.azure.com/run-requested": nonce.clone(),
                "kars.azure.com/run-objective-nonce": nonce.clone(),
                "kars.azure.com/run-objective-b64": encoded,
                "kars.azure.com/run-objective-digest": digest
            }}
        });
        self.tasks(ns)
            .patch(name, &PatchParams::default(), &Patch::Merge(patch))
            .await?;
        // Adopt whichever nonce won, so concurrent redrives converge on one run.
        if let Ok(Some(task)) = self.tasks(ns).get_opt(name).await
            && let Some(actual) = task
                .metadata
                .annotations
                .and_then(|a| a.get("kars.azure.com/run-requested").cloned())
                .filter(|r| !r.is_empty())
        {
            return Ok(actual);
        }
        Ok(nonce)
    }

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

    /// Read the receipt-signing public-key anchor published by the controller
    /// to the `kars-receipt-pubkey` ConfigMap in `kars-system`. This is the
    /// out-of-band trust root a verifier checks against — never a key embedded
    /// in a receipt. Returns `(key_id, public_key_b64, scheme)`.
    pub async fn receipt_pubkey_anchor(&self) -> Option<(String, String, String)> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let cm = cms.get_opt("kars-receipt-pubkey").await.ok().flatten()?;
        let data = cm.data?;
        Some((
            data.get("keyId")?.clone(),
            data.get("publicKey")?.clone(),
            data.get("scheme").cloned().unwrap_or_default(),
        ))
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

    /// Read-only readiness check of the required APIs, bounded across all requests.
    pub async fn ping(&self, namespace: &str) -> anyhow::Result<()> {
        const REQUIRED_KARS_APIS: &[(&str, &str)] = &[
            ("KarsSandbox", "karssandboxes"),
            ("KarsTask", "karstasks"),
            ("KarsTeam", "karsteams"),
            ("KarsProfile", "karsprofiles"),
            ("KarsSkill", "karsskills"),
            ("KarsApproval", "karsapprovals"),
            ("EgressApproval", "egressapprovals"),
            ("KarsReceipt", "karsreceipts"),
            ("McpServer", "mcpservers"),
            ("InferencePolicy", "inferencepolicies"),
            ("ToolPolicy", "toolpolicies"),
            ("KarsMemory", "karsmemories"),
            ("KarsEval", "karsevals"),
            ("KarsSREAction", "karssreactions"),
            ("KarsCredentialGrant", "karscredentialgrants"),
        ];

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        for (kind, plural) in REQUIRED_KARS_APIS {
            let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
            let mut ar = ApiResource::from_gvk(&gvk);
            ar.plural = (*plural).to_string();
            let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
            tokio::time::timeout_at(deadline, api.list(&ListParams::default().limit(1)))
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "required Kars API kars.azure.com/v1alpha1/{kind} readiness check timed out"
                    )
                })?
                .map_err(|error| {
                    anyhow::anyhow!(
                        "required Kars API kars.azure.com/v1alpha1/{kind} is unavailable: {error}"
                    )
                })?;
        }
        Ok(())
    }

    /// True iff a CRD with the given plural.group name is installed (e.g.
    /// `karssandboxes.kars.azure.com`). Used by the System view to report
    /// honest wiring status read from the cluster, not asserted.
    pub async fn crd_installed(&self, name: &str) -> bool {
        let crds: Api<CustomResourceDefinition> = Api::all(self.client.clone());
        crds.get_opt(name).await.ok().flatten().is_some()
    }

    /// Count resources of an arbitrary kars CRD kind in a namespace, via the
    /// dynamic API so the BFF need not model every CRD it merely *counts*.
    /// Returns `None` when the CRD is not installed.
    pub async fn count_kind(&self, namespace: &str, kind: &str) -> Option<usize> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
        match api.list(&ListParams::default()).await {
            Ok(list) => Some(list.items.len()),
            Err(_) => None,
        }
    }

    /// Create a kars CRD object from a JSON spec in `namespace`. Used to file a
    /// request resource (e.g. a temporary `EgressApproval`) the controller then
    /// reconciles through human approval — the BFF never widens posture itself.
    /// Ensure the standing **orchestrator sandbox** exists — a persistent,
    /// non-ephemeral sandbox whose inference router the Bridge orchestrator
    /// (compose) always routes through. Without it, compose can only borrow a
    /// running agent's router, so on a teams-only cluster (all ephemeral runs)
    /// it has no cold-start inference path. Idempotent SSA; safe to call on every
    /// startup.
    pub async fn ensure_orchestrator_sandbox(&self) -> Result<(), kube::Error> {
        const NAME: &str = "bridge-orchestrator";
        const NS: &str = "kars-system";
        let inference = serde_json::json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "InferencePolicy",
            "metadata": { "name": format!("{NAME}-inference"), "namespace": NS,
                "labels": { "kars.azure.com/managed-by": "kars-bridge" } },
            "spec": {
                "appliesTo": { "sandboxName": NAME },
                "modelPreference": { "primary": { "provider": "github-copilot", "deployment": "claude-opus-4.8" } },
            },
        });
        self.apply_kind(NS, "InferencePolicy", inference, true)
            .await?;
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
        self.apply_kind(NS, "KarsSandbox", sandbox, true).await?;
        Ok(())
    }

    pub async fn create_kind(
        &self,
        namespace: &str,
        kind: &str,
        body: serde_json::Value,
    ) -> Result<DynamicObject, kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
        let obj: DynamicObject = serde_json::from_value(body).map_err(|e| {
            kube::Error::Api(kube::core::ErrorResponse {
                status: "Failure".into(),
                message: e.to_string(),
                reason: "BadRequest".into(),
                code: 400,
            })
        })?;
        api.create(&kube::api::PostParams::default(), &obj).await
    }

    /// Server-Side Apply a `kars.azure.com` CRD — the Kubernetes-native
    /// declarative upsert (the same operation `kubectl apply` performs): creates
    /// the object on first apply, edits it on re-apply. The Bridge owns its
    /// fields under the stable `kars-bridge` field manager, so the controller,
    /// other tools, and a human's `kubectl edit` can co-own different fields
    /// without clobbering each other (tracked in `metadata.managedFields`).
    ///
    /// `force = false` (default) surfaces a 409 field-ownership conflict when
    /// another manager owns a field this apply sets — the caller decides whether
    /// to override. `force = true` takes ownership of the applied fields. The
    /// real authorization boundary is RBAC on the Bridge ServiceAccount + the
    /// CRD's admission/CEL validation — not this method.
    pub async fn apply_kind(
        &self,
        namespace: &str,
        kind: &str,
        body: serde_json::Value,
        force: bool,
    ) -> Result<DynamicObject, kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
        let obj: DynamicObject = serde_json::from_value(body).map_err(|e| {
            kube::Error::Api(kube::core::ErrorResponse {
                status: "Failure".into(),
                message: e.to_string(),
                reason: "BadRequest".into(),
                code: 400,
            })
        })?;
        let name = obj.metadata.name.clone().unwrap_or_default();
        let mut pp = kube::api::PatchParams::apply("kars-bridge");
        if force {
            pp = pp.force();
        }
        api.patch(&name, &pp, &kube::api::Patch::Apply(&obj)).await
    }

    /// Delete a namespaced kars CRD by kind + name. Foreground propagation so
    /// the controller's finalizers run (revoking any downstream state) before
    /// the object disappears. The RBAC boundary is the Bridge ServiceAccount's
    /// `delete` verb on the resource; a 403/404 surfaces to the caller.
    pub async fn delete_kind(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
    ) -> Result<(), kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
        let dp = kube::api::DeleteParams::foreground();
        api.delete(name, &dp).await?;
        Ok(())
    }

    /// Delete a standing team and its owned substrate. Deleting the `KarsTeam`
    /// CRD cascade-removes its runs + sandboxes (owner references); this then
    /// best-effort sweeps the team's auxiliary records the controller writes
    /// alongside the CRD — shared memory, task backlog, engineering source, and
    /// write-only channel secret — so a deleted team leaves nothing behind.
    /// Aux cleanup is best-effort: a missing aux object is not an error.
    /// Delete a mission (KarsTask) and sweep the ConfigMaps the controller keyed
    /// on its name — the deliverable, artifacts, live trace, and review record.
    /// Without the sweep, a deleted mission's outputs keep surfacing on the
    /// Artifacts page and its direct URL keeps resolving from output-only
    /// history (same class of orphan the team delete sweep fixes).
    pub async fn delete_task(&self, namespace: &str, name: &str) -> Result<(), kube::Error> {
        // The CRD itself (foreground cascade → sandbox + child resources).
        self.delete_kind(namespace, "KarsTask", name).await?;
        self.sweep_mission_artifacts(name).await;
        Ok(())
    }

    /// Best-effort deletion of the ConfigMaps the controller keys on a mission's
    /// name (deliverable, files, trace, review). Used by mission delete and the
    /// output-only cleanup path.
    pub async fn sweep_mission_artifacts(&self, name: &str) {
        use k8s_openapi::api::core::v1::ConfigMap;
        use kube::api::DeleteParams;
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        for cm in [
            format!("kars-mission-output-{name}"),
            format!("kars-mission-artifacts-{name}"),
            format!("kars-mission-trace-{name}"),
            format!("kars-mission-review-{name}"),
        ] {
            let _ = cms.delete(&cm, &DeleteParams::default()).await;
        }
    }

    pub async fn delete_team(
        &self,
        namespace: &str,
        name: &str,
        uid: &str,
        version: &str,
    ) -> Result<(), kube::Error> {
        self.teams(namespace)
            .delete(
                name,
                &kube::api::DeleteParams {
                    propagation_policy: Some(kube::api::PropagationPolicy::Foreground),
                    preconditions: Some(kube::api::Preconditions {
                        uid: Some(uid.into()),
                        resource_version: Some(version.into()),
                    }),
                    ..Default::default()
                },
            )
            .await?;
        // Core owns Team/source/commons cleanup. Historical or ambiguous
        // name-keyed records are retained rather than deleting another UID's data.
        Ok(())
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
    /// Upsert a Secret via server-side apply, merging keys without clobbering
    /// existing ones. Used to store agent credentials (write-only); the value is
    /// never read back through any endpoint.
    pub async fn upsert_secret(
        &self,
        namespace: &str,
        name: &str,
        body: serde_json::Value,
    ) -> Result<(), kube::Error> {
        let data = body
            .get("stringData")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| super::credentials::failure("Integration update requires stringData"))?;
        let values = data
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_string()))
                    .ok_or_else(|| {
                        super::credentials::failure("Integration value must be a string")
                    })
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
        self.mutate_integration(namespace, name, |keys| keys.extend(values.clone()))
            .await
    }

    /// Delete a write-only credential Secret this Bridge authored (e.g. the
    /// shared `kars-github-app` Secret on disconnect). A 404 is not an error —
    /// the secret is already absent, which is the caller's desired end state.
    pub async fn delete_secret(&self, namespace: &str, name: &str) -> Result<(), kube::Error> {
        self.mutate_integration(namespace, name, |keys| keys.clear())
            .await
    }

    /// List all objects of a kars CRD `kind` across **all** namespaces, as
    /// dynamic objects the caller projects into a DTO. This is the generic
    /// read the operator surfaces use so the BFF need not type every CRD it
    /// merely lists. Returns `Err` only on a real API failure; an absent CRD
    /// surfaces as `Ok(vec![])` so the caller can render an honest empty state.
    pub async fn list_kind_all(&self, kind: &str) -> Result<Vec<DynamicObject>, kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::all_with(self.client.clone(), &ar);
        match api.list(&ListParams::default()).await {
            Ok(list) => Ok(list.items),
            // A 404 means the CRD isn't installed — honest empty, not an error.
            Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    pub async fn list_metrics_all(
        &self,
        kind: &str,
        plural: &str,
    ) -> Result<Vec<DynamicObject>, kube::Error> {
        let ar = ApiResource {
            group: "metrics.k8s.io".into(),
            version: "v1beta1".into(),
            api_version: "metrics.k8s.io/v1beta1".into(),
            kind: kind.into(),
            plural: plural.into(),
        };
        let api: Api<DynamicObject> = Api::all_with(self.client.clone(), &ar);
        Ok(api.list(&ListParams::default()).await?.items)
    }

    pub async fn list_nodes(&self) -> Result<Vec<Node>, kube::Error> {
        let api: Api<Node> = Api::all(self.client.clone());
        Ok(api.list(&ListParams::default()).await?.items)
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

    /// List objects of a kars CRD `kind` across all namespaces filtered by a
    /// label selector — used to find an agent's spawned sub-agents, which the
    /// inference router labels `kars.azure.com/parent=<sandbox>`. Absent CRD →
    /// `Ok(vec![])`.
    pub async fn list_kind_labeled(
        &self,
        kind: &str,
        selector: &str,
    ) -> Result<Vec<DynamicObject>, kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::all_with(self.client.clone(), &ar);
        match api.list(&ListParams::default().labels(selector)).await {
            Ok(list) => Ok(list.items),
            Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// List objects of a kars CRD `kind` within a namespace, as dynamic
    /// objects. Absent CRD → `Ok(vec![])`.
    pub async fn list_kind(
        &self,
        namespace: &str,
        kind: &str,
    ) -> Result<Vec<DynamicObject>, kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
        match api.list(&ListParams::default()).await {
            Ok(list) => Ok(list.items),
            Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Fetch a single kars CRD object by kind + namespace + name.
    /// Merge-patch annotations onto a namespaced kars CRD's metadata. Used by
    /// the operator skill-admission gate to record the review verdict, the
    /// approver, and the version digest the approval is locked to — a real,
    /// auditable admission record on the object itself (RBAC: the Bridge SA's
    /// `patch` verb). A `None` value removes the annotation.
    pub async fn annotate_kind(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
        annotations: &[(&str, Option<String>)],
    ) -> Result<DynamicObject, kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
        let mut ann = serde_json::Map::new();
        for (k, v) in annotations {
            ann.insert((*k).to_string(), serde_json::json!(v));
        }
        let patch = serde_json::json!({ "metadata": { "annotations": ann } });
        api.patch(
            name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(&patch),
        )
        .await
    }

    /// Apply a strategic **merge patch** to a kars CRD object — used for small,
    /// in-place edits (e.g. an operator changing an InferencePolicy's token
    /// budget). Unlike SSA this doesn't take field-manager ownership of the whole
    /// spec, so it co-exists with the controller's own management.
    pub async fn merge_patch_kind(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
        patch: serde_json::Value,
    ) -> Result<DynamicObject, kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
        api.patch(
            name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(&patch),
        )
        .await
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

    /// Read a single key's value from a Secret (base64-decoded UTF-8). `None`
    /// when the secret/key is absent. Used by the Foundry preflight to make a
    /// real authenticated call with the onboarded key — the key never leaves the
    /// BFF process.
    pub async fn read_secret_value(
        &self,
        namespace: &str,
        secret: &str,
        key: &str,
    ) -> Result<Option<String>, kube::Error> {
        let (_, s) = self.integration_store(namespace, secret).await?;
        if let Some(v) = s.data.as_ref().and_then(|d| d.get(key)) {
            return String::from_utf8(v.0.clone())
                .map(Some)
                .map_err(|_| super::credentials::failure("Credential value is not UTF-8"));
        }
        Ok(None)
    }

    /// Read every key of a Secret as UTF-8 strings (base64-decoded). Empty map
    /// when the secret doesn't exist. Used for the multi-provider inference
    /// Secret, whose keys ARE the literal env var names the router reads
    /// (`KARS_PROVIDER_<TAG>_ENDPOINT`, `COPILOT_GITHUB_TOKEN`, ...) — listing
    /// requires reading the whole key set, not one key at a time.
    pub async fn read_secret_all(
        &self,
        namespace: &str,
        secret: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, kube::Error> {
        let (_, s) = self.integration_store(namespace, secret).await?;
        Ok(Self::decode_secret_data(&s))
    }

    /// Read-modify-write a Secret's full key set under real optimistic
    /// concurrency (CAS): a single atomic JSON Patch (RFC 6902) — a `test` op
    /// asserting `resourceVersion` hasn't moved, followed by `add`/`remove`
    /// ops for the actual key changes — retried on failure.
    ///
    /// Why JSON Patch specifically, not `replace()`/PUT or a plain JSON merge
    /// patch:
    ///   - `replace()` (PUT) is the "update" RBAC verb, which the BFF's
    ///     ClusterRole deliberately never grants (write access here is
    ///     `create`/`patch` only) — using it would 403 in any real
    ///     RBAC-enforced deployment. Confirmed live against the actual
    ///     ServiceAccount (not a developer's cluster-admin kubeconfig).
    ///   - A plain JSON *merge* patch (RFC 7396, what this function used
    ///     before) uses the `patch` verb correctly, but the K8s API does NOT
    ///     honor `resourceVersion` as a precondition for merge patches —
    ///     confirmed live: a merge patch carrying a stale resourceVersion
    ///     still applies. So a merge patch alone has no way to detect a
    ///     concurrent writer.
    ///   - JSON Patch's `test` op DOES enforce the precondition atomically
    ///     alongside the real mutation (confirmed live: a stale
    ///     resourceVersion in a `test` op → the whole patch is rejected,
    ///     HTTP 422, and none of the following ops apply) — and it's still
    ///     the `patch` verb, so no RBAC widening is needed.
    ///   - Field removal still works here (unlike Server-Side-Apply, whose
    ///     merge semantics never remove an absent key) via an explicit
    ///     `remove` op per dropped key.
    pub async fn mutate_secret_keys(
        &self,
        namespace: &str,
        secret: &str,
        mutate: impl Fn(&mut std::collections::BTreeMap<String, String>),
    ) -> Result<(), kube::Error> {
        self.mutate_integration(namespace, secret, mutate).await
    }

    /// Decode a Secret's `data` (+ any pending `stringData`) into a flat map,
    /// the shared helper behind both `read_secret_all` and the CAS loop above.
    fn decode_secret_data(
        s: &k8s_openapi::api::core::v1::Secret,
    ) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        if let Some(d) = s.data.as_ref() {
            for (k, v) in d {
                if let Ok(s) = String::from_utf8(v.0.clone()) {
                    out.insert(k.clone(), s);
                }
            }
        }
        if let Some(d) = s.string_data.as_ref() {
            for (k, v) in d {
                out.insert(k.clone(), v.clone());
            }
        }
        out
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

    pub async fn get_kind(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
    ) -> Result<Option<DynamicObject>, kube::Error> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind);
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), namespace, &ar);
        api.get_opt(name).await
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

    pub async fn configure_bridge_orchestrator_model(
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
            .is_some_and(&policy_ready)
        {
            return Ok(());
        }
        self.merge_patch_kind(
            "kars-system",
            "InferencePolicy",
            "bridge-orchestrator-inference",
            serde_json::json!({
                "spec": {
                    "modelPreference": {
                        "primary": {
                            "provider": provider,
                            "deployment": deployment
                        },
                        "fallback": []
                    }
                }
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
        let revision = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().to_string())
            .unwrap_or_else(|_| format!("{provider}:{deployment}"));
        self.merge_patch_kind(
            "kars-system",
            "KarsSandbox",
            "bridge-orchestrator",
            serde_json::json!({
                "metadata": {
                    "annotations": {
                        "kars.azure.com/orchestrator-model-revision": revision
                    }
                }
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
        // Changing the mounted policy deliberately rolls the orchestrator pod.
        // Wait for the controller's exact generation + router-echo confirmation,
        // not merely the policy write or a fixed short pod-start assumption.
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
                .is_some_and(&policy_ready);
            if ready {
                return Ok(());
            }
            tokio::time::sleep(ORCHESTRATOR_POLICY_POLL_INTERVAL).await;
        }
        Err(format!(
            "timed out waiting for the bridge orchestrator router to enforce {provider}/{deployment}"
        ))
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

    // ─── Local (in-cluster) inference — AI Runway ModelDeployment ───────────
    // See docs/local-inference.md. kars does NOT install AI Runway/KAITO —
    // an operator does that once via their own helm/kubectl, exactly like the
    // GitHub App or Azure AI Foundry connection. kars-bridge only detects
    // presence and manages `ModelDeployment` objects on top, in a namespace it
    // owns (LOCAL_INFERENCE_NAMESPACE), never anyone else's.

    /// Whether AI Runway's `ModelDeployment` CRD is installed in this
    /// cluster. A cheap, read-only check (list with a 1-item limit) — the
    /// Bridge already holds `customresourcedefinitions: get/list` (used for
    /// CRD-schema introspection elsewhere), so this needs no new RBAC beyond
    /// the narrow `modeldeployments.airunway.ai` grant added alongside it.
    pub async fn local_inference_available(&self) -> bool {
        let gvk = GroupVersionKind::gvk("airunway.ai", "v1alpha1", "ModelDeployment");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), LOCAL_INFERENCE_NAMESPACE, &ar);
        api.list(&ListParams::default().limit(1)).await.is_ok()
    }

    /// Server-Side Apply a `ModelDeployment` (create-or-update), namespaced to
    /// `LOCAL_INFERENCE_NAMESPACE`. Mirrors `apply_kind`'s shape but targets
    /// AI Runway's own API group instead of `kars.azure.com`. Ensures the
    /// namespace exists first — the Bridge's own namespace, never created by
    /// AI Runway/KAITO's install, so this is the one place it needs to.
    pub async fn apply_model_deployment(
        &self,
        name: &str,
        spec: serde_json::Value,
    ) -> Result<DynamicObject, kube::Error> {
        let namespaces: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
        if let Some(namespace) = namespaces.get_opt(LOCAL_INFERENCE_NAMESPACE).await? {
            if namespace.metadata.deletion_timestamp.is_some() {
                return Err(super::credentials::failure(
                    "Local inference namespace is terminating",
                ));
            }
        } else {
            let namespace = serde_json::from_value(serde_json::json!({
                "apiVersion":"v1","kind":"Namespace","metadata":{"name":LOCAL_INFERENCE_NAMESPACE,
                    "labels":{"app.kubernetes.io/managed-by":"kars-bridge"}}
            }))
            .map_err(|_| {
                super::credentials::failure("Local inference namespace metadata invalid")
            })?;
            namespaces
                .create(&kube::api::PostParams::default(), &namespace)
                .await?;
        }
        let gvk = GroupVersionKind::gvk("airunway.ai", "v1alpha1", "ModelDeployment");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), LOCAL_INFERENCE_NAMESPACE, &ar);
        let obj: DynamicObject = serde_json::from_value(serde_json::json!({
            "apiVersion": "airunway.ai/v1alpha1",
            "kind": "ModelDeployment",
            "metadata": {
                "name": name,
                "namespace": LOCAL_INFERENCE_NAMESPACE,
                "labels": {"app.kubernetes.io/managed-by": "kars-bridge"},
            },
            "spec": spec,
        }))
        .map_err(|e| {
            kube::Error::Api(kube::core::ErrorResponse {
                status: "Failure".into(),
                message: e.to_string(),
                reason: "BadRequest".into(),
                code: 400,
            })
        })?;
        api.patch(
            name,
            &kube::api::PatchParams::apply("kars-bridge").force(),
            &kube::api::Patch::Apply(&obj),
        )
        .await
    }

    /// List every `ModelDeployment` in the cluster. Discovery is read-only
    /// across namespaces so an existing operator-managed AI Runway deployment
    /// is visible without being recreated under `kars-local-inference`.
    pub async fn list_model_deployments(&self) -> Result<Vec<DynamicObject>, kube::Error> {
        let gvk = GroupVersionKind::gvk("airunway.ai", "v1alpha1", "ModelDeployment");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::all_with(self.client.clone(), &ar);
        Ok(api.list(&ListParams::default()).await?.items)
    }

    /// Read one `ModelDeployment`'s current state (status included).
    pub async fn get_model_deployment(
        &self,
        name: &str,
    ) -> Result<Option<DynamicObject>, kube::Error> {
        let gvk = GroupVersionKind::gvk("airunway.ai", "v1alpha1", "ModelDeployment");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), LOCAL_INFERENCE_NAMESPACE, &ar);
        api.get_opt(name).await
    }

    /// Delete a `ModelDeployment` (foreground — the provider controller's
    /// owner-referenced `Workspace`/pods/Service cascade with it).
    pub async fn delete_model_deployment(&self, name: &str) -> Result<(), kube::Error> {
        let gvk = GroupVersionKind::gvk("airunway.ai", "v1alpha1", "ModelDeployment");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), LOCAL_INFERENCE_NAMESPACE, &ar);
        api.delete(name, &kube::api::DeleteParams::foreground())
            .await?;
        Ok(())
    }

    /// Real-capacity GPU node scan (read-only `nodes: get/list`) so the
    /// wizard can offer GPU-tier models only when the cluster can actually
    /// schedule them — never a hardcoded guess. Returns the count of
    /// schedulable nodes advertising `nvidia.com/gpu` capacity and the
    /// distinct GPU product names found (from the `nvidia.com/gpu.product`
    /// NFD/GPU-feature-discovery label, when present).
    pub async fn gpu_node_summary(&self) -> Result<GpuNodeSummary, kube::Error> {
        use k8s_openapi::api::core::v1::Node;
        let api: Api<Node> = Api::all(self.client.clone());
        let nodes = api.list(&ListParams::default()).await?;
        let mut gpu_node_count = 0u32;
        let mut products = std::collections::BTreeSet::new();
        for n in &nodes.items {
            let has_gpu = n
                .status
                .as_ref()
                .and_then(|s| s.capacity.as_ref())
                .map(|c| c.contains_key("nvidia.com/gpu"))
                .unwrap_or(false);
            if has_gpu {
                gpu_node_count += 1;
                if let Some(product) = n
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|l| l.get("nvidia.com/gpu.product"))
                {
                    products.insert(product.clone());
                }
            }
        }
        Ok(GpuNodeSummary {
            gpu_node_count,
            gpu_products: products.into_iter().collect(),
        })
    }

    /// Rich, LIVE status for one in-flight (or settled) local model deploy —
    /// what powers the deploy progress tracker's percentage + activity feed.
    /// Sourced entirely from real cluster signals (no synthetic spinner):
    ///   • the `ModelDeployment` CR's ordered `status.conditions` + phase,
    ///   • the KAITO pod(s) selected by `airunway.ai/model-deployment=<name>`
    ///     (container waiting reason / running / ready), and
    ///   • the namespace's Kubernetes Events for those pods (Pulling, Pulled,
    ///     Failed, BackOff, Started, …) — the actual activity feed.
    /// The percentage is milestone-derived (validated → workspace → scheduled
    /// → image pulled → running), so it only advances on real progress.
    pub async fn local_deployment_live_status(
        &self,
        name: &str,
    ) -> Result<LocalDeployLiveStatus, kube::Error> {
        use k8s_openapi::api::core::v1::{Event, Pod};

        let cr = self.get_model_deployment(name).await?;
        let mut status = LocalDeployLiveStatus {
            name: name.to_string(),
            found: cr.is_some(),
            ..Default::default()
        };
        if let Some(cr) = &cr {
            let st = cr.data.get("status");
            status.phase = st
                .and_then(|s| s.get("phase"))
                .and_then(|p| p.as_str())
                .map(str::to_string);
            status.message = st
                .and_then(|s| s.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_string);
            if let Some(reps) = st.and_then(|s| s.get("replicas")) {
                status.replicas_desired =
                    reps.get("desired").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                status.replicas_ready =
                    reps.get("ready").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            }
            if let Some(conds) = st
                .and_then(|s| s.get("conditions"))
                .and_then(|c| c.as_array())
            {
                for c in conds {
                    status.conditions.push(DeployCondition {
                        cond_type: c
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        status: c
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        reason: c
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        message: c
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    });
                }
            }
        }

        // Pod(s) for this deployment (KAITO stamps airunway.ai/model-deployment).
        let pods_api: Api<Pod> = Api::namespaced(self.client.clone(), LOCAL_INFERENCE_NAMESPACE);
        let lp = ListParams::default().labels(&format!("airunway.ai/model-deployment={name}"));
        let mut pod_names: Vec<String> = Vec::new();
        if let Ok(pods) = pods_api.list(&lp).await {
            for p in &pods.items {
                let pod_name = p.metadata.name.clone().unwrap_or_default();
                pod_names.push(pod_name.clone());
                let phase = p
                    .status
                    .as_ref()
                    .and_then(|s| s.phase.clone())
                    .unwrap_or_default();
                let mut ready = false;
                let mut waiting_reason: Option<String> = None;
                let mut waiting_message: Option<String> = None;
                let mut running = false;
                if let Some(cs) = p
                    .status
                    .as_ref()
                    .and_then(|s| s.container_statuses.as_ref())
                {
                    for c in cs {
                        ready = ready || c.ready;
                        if let Some(state) = &c.state {
                            if let Some(w) = &state.waiting {
                                waiting_reason = w.reason.clone();
                                waiting_message = w.message.clone();
                            }
                            if state.running.is_some() {
                                running = true;
                            }
                        }
                    }
                }
                status.pods.push(DeployPodState {
                    name: pod_name,
                    phase,
                    ready,
                    running,
                    waiting_reason,
                    waiting_message,
                });
            }
        }

        // Real Kubernetes events for the CR + its pods — the live activity feed.
        let events_api: Api<Event> =
            Api::namespaced(self.client.clone(), LOCAL_INFERENCE_NAMESPACE);
        if let Ok(events) = events_api.list(&ListParams::default()).await {
            for e in &events.items {
                let obj = e.involved_object.name.clone().unwrap_or_default();
                if obj != name && !pod_names.contains(&obj) {
                    continue;
                }
                let time = e
                    .last_timestamp
                    .as_ref()
                    .map(|t| t.0.to_rfc3339())
                    .or_else(|| e.event_time.as_ref().map(|t| t.0.to_rfc3339()));
                status.activities.push(DeployActivity {
                    time,
                    reason: e.reason.clone().unwrap_or_default(),
                    message: e.message.clone().unwrap_or_default(),
                    event_type: e.type_.clone().unwrap_or_default(),
                    count: e.count.unwrap_or(1),
                });
            }
            // Oldest → newest so the feed reads like a log.
            status.activities.sort_by(|a, b| a.time.cmp(&b.time));
        }

        // Terminal failure detection from real pod container state.
        for p in &status.pods {
            if let Some(reason) = &p.waiting_reason
                && matches!(
                    reason.as_str(),
                    "ImagePullBackOff"
                        | "ErrImagePull"
                        | "CrashLoopBackOff"
                        | "CreateContainerError"
                        | "InvalidImageName"
                )
            {
                status.failed = true;
                status.failure_reason = Some(reason.clone());
                status.failure_message = p.waiting_message.clone();
            }
        }
        status.ready = status.phase.as_deref() == Some("Running")
            || (status.replicas_desired > 0 && status.replicas_ready >= status.replicas_desired);

        // Milestone-derived percentage — advances only on real progress.
        status.percent = compute_deploy_percent(&status);
        Ok(status)
    }
}

/// One `ModelDeployment.status.conditions[]` entry, browser-facing.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DeployCondition {
    #[serde(rename = "type")]
    pub cond_type: String,
    pub status: String,
    pub reason: String,
    pub message: String,
}

/// Live state of one KAITO pod backing a local model deployment.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DeployPodState {
    pub name: String,
    pub phase: String,
    pub ready: bool,
    pub running: bool,
    pub waiting_reason: Option<String>,
    pub waiting_message: Option<String>,
}

/// One real Kubernetes Event — an entry in the live activity feed.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DeployActivity {
    pub time: Option<String>,
    pub reason: String,
    pub message: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub count: i32,
}

/// The full live status the deploy tracker renders.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LocalDeployLiveStatus {
    pub name: String,
    pub found: bool,
    pub phase: Option<String>,
    pub message: Option<String>,
    pub percent: u8,
    pub ready: bool,
    pub failed: bool,
    pub failure_reason: Option<String>,
    pub failure_message: Option<String>,
    pub replicas_desired: u32,
    pub replicas_ready: u32,
    pub conditions: Vec<DeployCondition>,
    pub pods: Vec<DeployPodState>,
    pub activities: Vec<DeployActivity>,
}

/// Milestone-derived deploy percentage from real signals. Each milestone the
/// deployment has genuinely reached sets a floor; nothing here advances on a
/// timer alone (the client adds a small time-based ease WITHIN the current
/// band for visible motion, but never past the next real milestone).
fn compute_deploy_percent(s: &LocalDeployLiveStatus) -> u8 {
    if s.ready {
        return 100;
    }
    let cond_true = |t: &str| {
        s.conditions
            .iter()
            .any(|c| c.cond_type == t && c.status == "True")
    };
    let mut pct: u8 = if s.found { 8 } else { 3 };
    if cond_true("Validated") {
        pct = pct.max(15);
    }
    if cond_true("ProviderSelected") || cond_true("ProviderCompatible") {
        pct = pct.max(25);
    }
    if cond_true("ResourceCreated") {
        pct = pct.max(38);
    }
    // Pod exists & scheduled (has a phase beyond nothing).
    if s.pods
        .iter()
        .any(|p| !p.phase.is_empty() && p.phase != "Unknown")
    {
        pct = pct.max(52);
    }
    // Container running (image pulled, process started) but not yet Ready.
    if s.pods.iter().any(|p| p.running) {
        pct = pct.max(88);
    }
    pct
}

/// Namespace the Bridge creates its own `ModelDeployment` objects in — never
/// the operator's `default` or the AI Runway/KAITO system namespaces, so
/// listing is naturally scoped to what the Bridge itself manages.
pub const LOCAL_INFERENCE_NAMESPACE: &str = "kars-local-inference";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GpuNodeSummary {
    pub gpu_node_count: u32,
    pub gpu_products: Vec<String>,
}

#[cfg(test)]
mod provider_tests {
    use super::{
        classify_provider, descendant_sandbox_objects, image_registry_host, mission_evidence_key,
        mission_output_candidate, normalize_registry_host, project_mission_output_record,
        public_registry, select_mission_evidence_records, select_mission_output_records,
        trace_record_identity,
    };
    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::api::DynamicObject;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn id(r: Option<(String, String, String)>) -> Option<String> {
        r.map(|(i, _, _)| i)
    }

    #[test]
    fn descendant_sandboxes_include_nested_agents_once() {
        let sandbox = |name: &str, parent: Option<&str>| -> DynamicObject {
            serde_json::from_value(json!({
                "apiVersion": "kars.azure.com/v1alpha1",
                "kind": "KarsSandbox",
                "metadata": {
                    "name": name,
                    "labels": parent.map(|parent| json!({"kars.azure.com/parent": parent}))
                }
            }))
            .expect("sandbox")
        };
        let items = vec![
            sandbox("child", Some("root")),
            sandbox("grandchild", Some("child")),
            sandbox("unrelated", Some("other")),
        ];

        let names = descendant_sandbox_objects(&items, "root")
            .into_iter()
            .filter_map(|sandbox| sandbox.metadata.name)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            names,
            std::collections::HashSet::from(["child".to_string(), "grandchild".to_string(),])
        );
    }

    #[test]
    fn registry_matching_covers_private_runtime_images() {
        assert_eq!(
            image_registry_host("example.azurecr.io/kars-runtime-hermes:latest"),
            "example.azurecr.io"
        );
        assert_eq!(
            normalize_registry_host("https://example.azurecr.io/v1/"),
            "example.azurecr.io"
        );
        assert!(!public_registry("example.azurecr.io"));
        assert!(public_registry("mcr.microsoft.com"));
    }

    #[test]
    fn mission_evidence_annotation_restores_long_nonce_identity() {
        let full = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
        let mut config_map = ConfigMap::default();
        config_map.metadata.annotations = Some(BTreeMap::from([(
            "kars.azure.com/mission-evidence-key".to_string(),
            full.to_string(),
        )]));
        config_map.metadata.labels = Some(BTreeMap::from([(
            "kars.azure.com/mission-output".to_string(),
            "stock-monitor-persistent-qual-principal-assig-0123456789ab".to_string(),
        )]));

        assert_eq!(
            mission_evidence_key(&config_map, "kars.azure.com/mission-output").as_deref(),
            Some(full)
        );
    }

    #[test]
    fn mission_evidence_label_remains_legacy_fallback() {
        let mut config_map = ConfigMap::default();
        config_map.metadata.labels = Some(BTreeMap::from([(
            "kars.azure.com/mission-output".to_string(),
            "team-run-100".to_string(),
        )]));

        assert_eq!(
            mission_evidence_key(&config_map, "kars.azure.com/mission-output").as_deref(),
            Some("team-run-100")
        );
    }

    #[test]
    fn legacy_principal_label_restores_stable_task_name() {
        let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
        let mut config_map = ConfigMap::default();
        config_map.metadata.labels = Some(BTreeMap::from([
            (
                "kars.azure.com/mission-output".to_string(),
                nonce.to_string(),
            ),
            (
                "kars.azure.com/mission-principal".to_string(),
                "stock-monitor-persistent-qual-principal".to_string(),
            ),
        ]));
        config_map.data = Some(BTreeMap::from([(
            "assignmentNonce".to_string(),
            nonce.to_string(),
        )]));

        let (_, _, data) = mission_output_candidate(config_map).expect("candidate");
        assert_eq!(
            data.get("taskName").map(String::as_str),
            Some("stock-monitor-persistent-qual-principal")
        );
    }

    #[test]
    fn ordinary_mission_enumeration_keeps_the_task_pointer() {
        let first_nonce = "run-1784912062312097896";
        let latest_nonce = "run-1784915840189332732";
        let first = BTreeMap::from([
            ("assignmentNonce".to_string(), first_nonce.to_string()),
            ("taskName".to_string(), "kompli-research".to_string()),
        ]);
        let latest = BTreeMap::from([
            ("assignmentNonce".to_string(), latest_nonce.to_string()),
            ("taskName".to_string(), "kompli-research".to_string()),
        ]);
        let selected = select_mission_output_records(vec![
            (first_nonce.to_string(), Some("archive".to_string()), first),
            (
                latest_nonce.to_string(),
                Some("archive".to_string()),
                latest.clone(),
            ),
            (
                "kompli-research".to_string(),
                Some("current".to_string()),
                latest,
            ),
        ]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "kompli-research");
    }

    #[test]
    fn explicit_current_pointer_beats_legacy_archive_for_same_task() {
        let old_nonce = "rev-1";
        let latest_nonce = "rev-2";
        let legacy = BTreeMap::from([
            ("assignmentNonce".to_string(), old_nonce.to_string()),
            ("taskName".to_string(), "kompli-research".to_string()),
        ]);
        let current = BTreeMap::from([
            ("assignmentNonce".to_string(), latest_nonce.to_string()),
            ("taskName".to_string(), "kompli-research".to_string()),
        ]);
        let selected = select_mission_output_records(vec![
            (old_nonce.to_string(), None, legacy),
            (
                "kompli-research".to_string(),
                Some("current".to_string()),
                current,
            ),
        ]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "kompli-research");
    }

    #[test]
    fn legacy_ordinary_run_archives_do_not_become_phantom_tasks() {
        let old_nonce = "run-1784912062312097896";
        let latest_nonce = "run-1784915840189332732";
        let mut archive = ConfigMap::default();
        archive.metadata.labels = Some(BTreeMap::from([
            (
                "kars.azure.com/mission-output".to_string(),
                old_nonce.to_string(),
            ),
            (
                "kars.azure.com/mission-principal".to_string(),
                "kompli-research".to_string(),
            ),
        ]));
        archive.data = Some(BTreeMap::from([(
            "assignmentNonce".to_string(),
            old_nonce.to_string(),
        )]));
        let mut current = ConfigMap::default();
        current.metadata.labels = Some(BTreeMap::from([(
            "kars.azure.com/mission-output".to_string(),
            "kompli-research".to_string(),
        )]));
        current.data = Some(BTreeMap::from([(
            "assignmentNonce".to_string(),
            latest_nonce.to_string(),
        )]));
        let selected = select_mission_output_records(vec![
            mission_output_candidate(archive).expect("archive"),
            mission_output_candidate(current).expect("current"),
        ]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "kompli-research");
    }

    #[test]
    fn persistent_team_latest_enumeration_keeps_the_current_pointer() {
        let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
        let data = BTreeMap::from([
            ("assignmentNonce".to_string(), nonce.to_string()),
            (
                "taskName".to_string(),
                "stock-monitor-persistent-qual-principal".to_string(),
            ),
            (
                "team".to_string(),
                "stock-monitor-persistent-qual".to_string(),
            ),
        ]);
        let selected = select_mission_output_records(vec![
            (nonce.to_string(), Some("archive".to_string()), data.clone()),
            (
                "stock-monitor-persistent-qual-principal".to_string(),
                Some("current".to_string()),
                data,
            ),
        ]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "stock-monitor-persistent-qual-principal");
    }

    #[test]
    fn accounting_enumeration_keeps_archives_and_drops_current_pointers() {
        let nonce = "run-1784915840189332732";
        let data = BTreeMap::from([
            ("assignmentNonce".to_string(), nonce.to_string()),
            ("taskName".to_string(), "kompli-research".to_string()),
        ]);
        let selected = select_mission_evidence_records(vec![
            (nonce.to_string(), Some("archive".to_string()), data.clone()),
            (
                "kompli-research".to_string(),
                Some("current".to_string()),
                data,
            ),
        ]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, nonce);
    }

    #[test]
    fn accounting_enumeration_keeps_each_rerun_archive() {
        let first_nonce = "rev-1";
        let latest_nonce = "rev-2";
        let selected = select_mission_evidence_records(vec![
            (
                first_nonce.to_string(),
                Some("archive".to_string()),
                BTreeMap::from([("assignmentNonce".to_string(), first_nonce.to_string())]),
            ),
            (
                latest_nonce.to_string(),
                Some("archive".to_string()),
                BTreeMap::from([("assignmentNonce".to_string(), latest_nonce.to_string())]),
            ),
            (
                "kompli-research".to_string(),
                Some("current".to_string()),
                BTreeMap::from([("assignmentNonce".to_string(), latest_nonce.to_string())]),
            ),
        ]);

        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|(key, _)| key == first_nonce));
        assert!(selected.iter().any(|(key, _)| key == latest_nonce));
    }

    #[test]
    fn persistent_archive_projects_stable_task_and_separate_evidence_key() {
        let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
        let data = BTreeMap::from([
            ("assignmentNonce".to_string(), nonce.to_string()),
            (
                "taskName".to_string(),
                "stock-monitor-persistent-qual-principal".to_string(),
            ),
            (
                "team".to_string(),
                "stock-monitor-persistent-qual".to_string(),
            ),
        ]);
        let projected = project_mission_output_record(nonce.to_string(), data);

        assert_eq!(
            projected.task_name,
            "stock-monitor-persistent-qual-principal"
        );
        assert_eq!(projected.evidence_key, nonce);
    }

    #[test]
    fn mirrored_trace_records_share_one_counting_identity() {
        let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
        let data = BTreeMap::from([
            ("assignmentNonce".to_string(), nonce.to_string()),
            (
                "trace.json".to_string(),
                r#"[{"kind":"round"}]"#.to_string(),
            ),
            ("capturedAt".to_string(), "2026-07-24T19:00:00Z".to_string()),
        ]);
        let mut archive = ConfigMap::default();
        archive.metadata.name = Some(format!("kars-mission-trace-{nonce}"));
        archive.metadata.annotations = Some(BTreeMap::from([(
            "kars.azure.com/mission-evidence-role".to_string(),
            "archive".to_string(),
        )]));
        archive.data = Some(data.clone());
        let mut current = ConfigMap::default();
        current.metadata.name =
            Some("kars-mission-trace-stock-monitor-persistent-qual-principal".to_string());
        current.metadata.annotations = Some(BTreeMap::from([(
            "kars.azure.com/mission-evidence-role".to_string(),
            "current".to_string(),
        )]));
        current.data = Some(data);

        assert!(trace_record_identity(&archive).is_some());
        assert!(trace_record_identity(&current).is_none());
    }

    #[test]
    fn explicit_override_wins() {
        let eps = vec!["https://models.github.ai/inference".to_string()];
        assert_eq!(
            id(classify_provider(Some("github-copilot"), &eps, None)).as_deref(),
            Some("github-copilot")
        );
        assert_eq!(
            id(classify_provider(
                Some("github-models"),
                &eps,
                Some("gho_x")
            ))
            .as_deref(),
            Some("github-models")
        );
        assert_eq!(
            id(classify_provider(Some("foundry"), &[], None)).as_deref(),
            Some("azure-foundry")
        );
    }

    #[test]
    fn github_endpoint_with_oauth_token_is_copilot() {
        // The real localkarstest shape: models.github.ai + a gho_ OAuth token.
        let eps = vec!["https://models.github.ai/inference".to_string()];
        assert_eq!(
            id(classify_provider(None, &eps, Some("gho_"))).as_deref(),
            Some("github-copilot")
        );
        assert_eq!(
            id(classify_provider(None, &eps, Some("ghu_"))).as_deref(),
            Some("github-copilot")
        );
    }

    #[test]
    fn github_endpoint_with_pat_is_models() {
        let eps = vec!["https://models.github.ai/inference".to_string()];
        assert_eq!(
            id(classify_provider(None, &eps, Some("ghp_"))).as_deref(),
            Some("github-models")
        );
        assert_eq!(
            id(classify_provider(None, &eps, None)).as_deref(),
            Some("github-models")
        );
    }

    #[test]
    fn copilot_endpoint_is_copilot() {
        let eps = vec!["https://api.githubcopilot.com".to_string()];
        assert_eq!(
            id(classify_provider(None, &eps, None)).as_deref(),
            Some("github-copilot")
        );
    }

    #[test]
    fn foundry_endpoint_and_empty() {
        let eps = vec!["https://my-proj.openai.azure.com".to_string()];
        assert_eq!(
            id(classify_provider(None, &eps, None)).as_deref(),
            Some("azure-foundry")
        );
        assert_eq!(id(classify_provider(None, &[], None)), None);
    }

    #[test]
    fn local_inference_endpoint_is_not_mislabeled_as_foundry() {
        // A promoted local model's endpoint is always a Service DNS name in
        // the Bridge-owned kars-local-inference namespace — must be labeled
        // distinctly, not fall into the generic Foundry bucket every other
        // unrecognized endpoint gets.
        let eps = vec!["http://my-model.kars-local-inference.svc.cluster.local:80".to_string()];
        assert_eq!(
            id(classify_provider(None, &eps, None)).as_deref(),
            Some("local-inference")
        );
    }
}
