use super::Cluster;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, ListParams};

impl Cluster {
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
}
