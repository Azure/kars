use super::Cluster;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DynamicObject, ListParams};

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

impl Cluster {
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
}
