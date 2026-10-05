// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{AgentIdentity, Cluster, MeshRunOutcome};
use crate::kars::task::KarsTask;
use crate::providers::signing::sha256_hex;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, Patch, PatchParams};

const REQUESTED: &str = "kars.azure.com/run-requested";
const COMPLETED: &str = "kars.azure.com/run-completed";

#[derive(Debug, thiserror::Error)]
pub enum MissionRunError {
    #[error("{0}")]
    Conflict(&'static str),
    #[error("{0}")]
    Cluster(#[from] kube::Error),
}

fn run_identity(task: &KarsTask) -> Result<(&str, &str, &str, &str), MissionRunError> {
    let meta = &task.metadata;
    match (
        meta.namespace.as_deref(),
        meta.name.as_deref(),
        meta.uid.as_deref(),
        meta.resource_version.as_deref(),
    ) {
        (Some(ns), Some(name), Some(uid), Some(rv))
            if !ns.is_empty()
                && !name.is_empty()
                && !uid.is_empty()
                && !rv.is_empty()
                && meta.deletion_timestamp.is_none()
                && task
                    .spec
                    .execution
                    .as_ref()
                    .is_some_and(|execution| execution.launch) =>
        {
            Ok((ns, name, uid, rv))
        }
        _ => Err(MissionRunError::Conflict(
            "the mission must be current and launched; reload before requesting a run",
        )),
    }
}

fn annotation<'a>(task: &'a KarsTask, key: &str) -> Option<&'a str> {
    task.metadata
        .annotations
        .as_ref()?
        .get(key)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

fn run_nonce(prefix: &str, uid: &str, rv: &str) -> String {
    // The API-server revision is unique for this UID. The patch's CAS admits at
    // most one request from that revision, including across BFF processes.
    format!(
        "{prefix}-{}",
        sha256_hex(serde_json::json!([uid, rv]).to_string().as_bytes())
    )
}

impl Cluster {
    /// Persist a run request against the caller's authorized Task snapshot.
    /// Pending requests are reused without mutation. A new request uses UID/RV
    /// preconditions; conflicts and ambiguous responses never trigger execution
    /// through another path. Execution still requires a deployed Core dispatcher.
    pub async fn request_mesh_run(&self, task: &KarsTask) -> Result<String, MissionRunError> {
        let (ns, name, uid, rv) = run_identity(task)?;
        if let Some(requested) = annotation(task, REQUESTED)
            && annotation(task, COMPLETED) != Some(requested)
        {
            return Ok(requested.to_owned());
        }
        let nonce = run_nonce("run", uid, rv);
        let patch = serde_json::json!({
            "metadata": { "uid": uid, "resourceVersion": rv,
                "annotations": { "kars.azure.com/run-requested": nonce } }
        });
        self.tasks(ns)
            .patch(name, &PatchParams::default(), &Patch::Merge(patch))
            .await?;
        Ok(nonce)
    }

    /// Await output for this exact Task UID and nonce, never a later revision
    /// observed during the output read. Missing ACKs are not proof of non-delivery
    /// and never permit a fallback or cancellation. Unbound router traces are not
    /// evidence of activity for this run.
    pub async fn await_mesh_run(
        &self,
        expected: &KarsTask,
        nonce: &str,
        timeout: std::time::Duration,
    ) -> MeshRunOutcome {
        let Ok((ns, name, uid, _)) = run_identity(expected) else {
            return MeshRunOutcome::InProgress;
        };
        let mut saw_ack = false;
        let outcome = tokio::time::timeout(timeout, async {
            loop {
                if let Ok(Some(task)) = self.tasks(ns).get_opt(name).await {
                    if task.metadata.uid.as_deref() != Some(uid)
                        || task.metadata.name.as_deref() != Some(name)
                        || task.metadata.namespace.as_deref() != Some(ns)
                        || task.metadata.deletion_timestamp.is_some()
                        || annotation(&task, REQUESTED) != Some(nonce)
                    {
                        return MeshRunOutcome::InProgress;
                    }
                    if annotation(&task, "kars.azure.com/run-ack") == Some(nonce) {
                        saw_ack = true;
                    }
                    if annotation(&task, COMPLETED) == Some(nonce) {
                        return match self.read_mission_output(name).await {
                            Some(out)
                                if ns == "kars-system"
                                    && out.get("taskUid").map(String::as_str) == Some(uid)
                                    && out.get("assignmentNonce").map(String::as_str)
                                        == Some(nonce) =>
                            {
                                MeshRunOutcome::Completed(out)
                            }
                            _ => MeshRunOutcome::InProgress,
                        };
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        })
        .await;
        outcome.unwrap_or(if saw_ack {
            MeshRunOutcome::InProgress
        } else {
            MeshRunOutcome::NeverProcessed
        })
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
        self.review_snapshot("kars-system", task).await.ok()??.data
    }

    pub async fn review_snapshot(
        &self,
        ns: &str,
        task: &str,
    ) -> Result<Option<ConfigMap>, MissionRunError> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        let name = format!("kars-mission-review-{task}");
        let snapshot = cms.get_opt(&name).await?;
        if snapshot.as_ref().is_some_and(|cm| {
            cm.metadata.name.as_deref() != Some(name.as_str())
                || cm.metadata.namespace.as_deref() != Some(ns)
                || cm.metadata.uid.as_deref().is_none_or(str::is_empty)
                || cm
                    .metadata
                    .resource_version
                    .as_deref()
                    .is_none_or(str::is_empty)
                || cm.metadata.deletion_timestamp.is_some()
        }) {
            return Err(MissionRunError::Conflict(
                "the review record is stale; reload before reviewing",
            ));
        }
        Ok(snapshot)
    }

    /// Write exactly the review snapshot read by the caller, without adopting
    /// concurrent decisions. This is not a transaction with the Task run patch.
    pub async fn write_review(
        &self,
        task: &KarsTask,
        prior: Option<&ConfigMap>,
        data: std::collections::BTreeMap<String, String>,
    ) -> Result<(), MissionRunError> {
        use kube::api::PostParams;
        let ns = task
            .metadata
            .namespace
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(MissionRunError::Conflict("mission namespace is missing"))?;
        let task_name = task
            .metadata
            .name
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(MissionRunError::Conflict("mission name is missing"))?;
        let uid = task
            .metadata
            .uid
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(MissionRunError::Conflict("mission UID is missing"))?;
        let name = format!("kars-mission-review-{task_name}");
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        if data.get("taskUid").map(String::as_str) != Some(uid) {
            return Err(MissionRunError::Conflict(
                "review must be bound to the mission UID",
            ));
        }
        if let Some(previous) = prior {
            if previous.metadata.namespace.as_deref() != Some(ns)
                || previous.metadata.name.as_deref() != Some(&name)
                || previous.metadata.deletion_timestamp.is_some()
                || previous.metadata.uid.as_deref().is_none_or(str::is_empty)
                || previous
                    .metadata
                    .resource_version
                    .as_deref()
                    .is_none_or(str::is_empty)
                || previous
                    .data
                    .as_ref()
                    .and_then(|d| d.get("taskUid"))
                    .map(String::as_str)
                    != Some(uid)
            {
                return Err(MissionRunError::Conflict(
                    "the review record is stale or unbound; reload before reviewing",
                ));
            }
            let mut next = previous.clone();
            next.data = Some(data);
            cms.replace(&name, &PostParams::default(), &next).await?;
        } else {
            let next = ConfigMap {
                metadata: kube::core::ObjectMeta {
                    name: Some(name),
                    namespace: Some(ns.into()),
                    labels: Some(std::collections::BTreeMap::from([(
                        "kars.azure.com/mission-review".into(),
                        task_name.into(),
                    )])),
                    ..Default::default()
                },
                data: Some(data),
                ..Default::default()
            };
            cms.create(&PostParams::default(), &next).await?;
        }
        Ok(())
    }

    /// Create a distinct revision from the exact completed run reviewed by the
    /// caller. Never replace a pending objective or adopt another review's nonce.
    /// Retired missions must be explicitly relaunched through the normal gates.
    pub async fn redrive_with_revision(
        &self,
        task: &KarsTask,
        reviewed_nonce: &str,
        revised_objective: &str,
    ) -> Result<String, MissionRunError> {
        let (ns, name, uid, rv) = run_identity(task)?;
        if reviewed_nonce.is_empty()
            || revised_objective.trim().is_empty()
            || annotation(task, REQUESTED) != Some(reviewed_nonce)
            || annotation(task, COMPLETED) != Some(reviewed_nonce)
        {
            return Err(MissionRunError::Conflict(
                "the reviewed run is no longer current or a revision is pending; reload before requesting changes",
            ));
        }
        let nonce = run_nonce("rev", uid, rv);
        let patch = serde_json::json!({
            "metadata": { "uid": uid, "resourceVersion": rv, "annotations": {
                "kars.azure.com/run-requested": nonce,
                "kars.azure.com/run-objective-nonce": nonce,
                "kars.azure.com/run-objective-b64": BASE64_STANDARD.encode(revised_objective.as_bytes()),
                "kars.azure.com/run-objective-digest": format!("sha256:{}", sha256_hex(revised_objective.as_bytes()))
            }}
        });
        self.tasks(ns)
            .patch(name, &PatchParams::default(), &Patch::Merge(patch))
            .await?;
        Ok(nonce)
    }
}
