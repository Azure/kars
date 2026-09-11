use super::{AgentIdentity, Cluster, MeshRunOutcome};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::Api;
use sha2::{Digest, Sha256};

impl Cluster {
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
}
