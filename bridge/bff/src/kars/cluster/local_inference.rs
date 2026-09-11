use super::{
    Cluster, DeployActivity, DeployCondition, DeployPodState, GpuNodeSummary,
    LOCAL_INFERENCE_NAMESPACE, LocalDeployLiveStatus,
};
use kube::api::{Api, DynamicObject, GroupVersionKind, ListParams};
use kube::core::ApiResource;

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

impl Cluster {
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
