use super::Cluster;
use crate::kars::task::KarsTask;
use k8s_openapi::api::core::v1::Node;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{Api, DynamicObject, GroupVersionKind, ListParams};
use kube::core::ApiResource;

impl Cluster {
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

    /// `KarsReceipt` API scoped to a namespace.
    pub fn receipts(&self, namespace: &str) -> Api<crate::kars::receipt::KarsReceipt> {
        Api::namespaced(self.client.clone(), namespace)
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
}
