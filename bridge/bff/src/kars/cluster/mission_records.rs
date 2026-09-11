use super::{Cluster, MissionOutputRecord};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::Api;
use sha2::{Digest, Sha256};

pub(super) fn mission_evidence_key(cm: &ConfigMap, legacy_label: &str) -> Option<String> {
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

pub(super) fn mission_output_candidate(
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

pub(super) fn select_mission_output_records(
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

pub(super) fn select_mission_evidence_records(
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

pub(super) fn project_mission_output_record(
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

pub(super) fn trace_record_identity(cm: &ConfigMap) -> Option<String> {
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

impl Cluster {
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
}
