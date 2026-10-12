// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — the cross-mission Artifacts index.
//
// The Artifacts surface (design note §16) lists the real deliverables missions
// have produced — the files captured by the controller from the agent loop over
// the mesh, persisted as durable `kars-mission-output-*` / `kars-mission-artifacts-*`
// ConfigMaps. This is a read-only projection of those real records; it never
// fabricates a deliverable and is honestly empty until a mission produces one.

use crate::providers::signing::sha256;
use axum::{
    Json,
    extract::{Extension, State},
};
use kube::ResourceExt;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::output_is_owned_by;
use crate::state::AppState;

/// Existing 16-byte short content address used by artifact and lineage DIDs.
pub(crate) fn content_address(bytes: &[u8]) -> String {
    let full = sha256(bytes);
    let mut out = String::from("sha256:");
    for b in &full[..16] {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[derive(Debug, Serialize)]
pub struct ArtifactFileDto {
    pub name: String,
    pub size_bytes: Option<i64>,
    pub has_content: bool,
    /// `sha256:<hex>` over the file's actual content — the artifact's
    /// content-address. None for binary artifacts whose bytes aren't inlined.
    pub content_address: Option<String>,
    /// Content-addressed identifier (`did:kars:<digest>`) derived from the
    /// file's content-address, so the deliverable is referenceable by identity.
    pub did: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MissionArtifactsDto {
    /// The mission (KarsTask) that produced these deliverables.
    pub task: String,
    /// Exact nonce-scoped evidence archive backing this row.
    pub evidence_key: Option<String>,
    /// True only for the live Task UID's committed current revision.
    pub current: bool,
    pub parent: Option<String>,
    /// Standing team that produced this task-force run, when applicable.
    pub team: Option<String>,
    /// True when the disposable run was GC'd and this row comes from durable
    /// team shared memory.
    pub archived: bool,
    /// A clean human title for the mission (never the loop scaffold / raw slug).
    pub display_name: Option<String>,
    /// The mission's objective (for context in the index).
    pub objective: Option<String>,
    /// The model the run used.
    pub model: Option<String>,
    /// When the deliverable was produced (RFC3339).
    pub finished_at: Option<String>,
    /// `ok` / `error` — the run status the deliverable reflects.
    pub status: Option<String>,
    /// Review status for this deliverable: `none` | `approved` |
    /// `changes_requested` (the §16 review-loop state).
    pub review_status: String,
    /// Current review revision (incremented on each request-changes).
    pub review_revision: i64,
    /// The artifact file set (names + sizes). Content lives on the mission page.
    pub files: Vec<ArtifactFileDto>,
    /// The final text deliverable (the agent's summary), when present.
    pub summary: Option<String>,
    /// A clean 2–3 line preview for cards/rows — never the raw transcript.
    pub excerpt: Option<String>,
    /// Pull requests the mission opened (a first-class deliverable type). Parsed
    /// from the deliverable; the router authored them via the keyless git proxy.
    pub pull_requests: Vec<crate::routes::tasks::PullRequestRef>,
    /// Content-addressed deliverable identity: `did:kars:<digest>` over the
    /// ordered file content-addresses + summary. Pins the exact deliverable
    /// set the receipt vouches for; stable across reads, sensitive to content.
    pub deliverable_did: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ArtifactsIndexDto {
    pub missions: Vec<MissionArtifactsDto>,
}

fn deliverable_identity(files: &[ArtifactFileDto], summary: Option<&str>) -> Option<String> {
    let mut lineage = String::new();
    for file in files {
        if let Some(address) = &file.content_address {
            lineage.push_str(address);
            lineage.push('\n');
        }
    }
    if let Some(summary) = summary {
        lineage.push_str(&content_address(summary.as_bytes()));
    }
    (!lineage.is_empty()).then(|| {
        format!(
            "did:kars:{}",
            content_address(lineage.as_bytes()).trim_start_matches("sha256:")
        )
    })
}

fn archived_files(archive: &crate::team_memory::RunArchive) -> Vec<ArtifactFileDto> {
    let mut contents: std::collections::BTreeMap<_, _> = archive
        .documents
        .iter()
        .map(|document| (document.name.as_str(), document.content.as_str()))
        .collect();
    contents.insert("response.md", &archive.output);
    contents
        .into_iter()
        .map(|(name, content)| {
            let address = content_address(content.as_bytes());
            ArtifactFileDto {
                name: name.into(),
                size_bytes: Some(content.len() as i64),
                has_content: true,
                did: Some(format!(
                    "did:kars:{}",
                    address.trim_start_matches("sha256:")
                )),
                content_address: Some(address),
            }
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct ArchivedCommonsEntry {
    id: String,
    title: String,
    source_task: String,
    created_at: String,
    digest: String,
}

fn commons_content_key(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("entry-{safe}")
}

fn pull_requests_with_repo_hint(
    text: &str,
    repos: &[String],
) -> Vec<crate::routes::tasks::PullRequestRef> {
    let mut pull_requests = crate::routes::tasks::extract_pull_requests(text);
    if repos.len() == 1 {
        let repo = &repos[0];
        let lower = text.to_ascii_lowercase();
        let bytes = lower.as_bytes();
        let mut cursor = 0;
        while let Some(offset) = lower[cursor..].find("pr #") {
            let start = cursor + offset + 4;
            let number_text: String = bytes[start..]
                .iter()
                .map(|byte| *byte as char)
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(number) = number_text.parse::<i64>() {
                let candidate = crate::routes::tasks::PullRequestRef {
                    repo: repo.clone(),
                    number,
                    url: format!("https://github.com/{repo}/pull/{number}"),
                };
                if !pull_requests.contains(&candidate) {
                    pull_requests.push(candidate);
                }
            }
            cursor = start.saturating_add(number_text.len()).min(lower.len());
            if cursor >= lower.len() {
                break;
            }
        }
    }
    pull_requests
}

fn artifact_files(
    data: &std::collections::BTreeMap<String, String>,
    contents: Option<&k8s_openapi::api::core::v1::ConfigMap>,
) -> Vec<ArtifactFileDto> {
    let mut entries = data
        .get("artifacts")
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(raw).ok())
        .unwrap_or_default();
    if let Some(contents) = contents {
        entries.extend(
            contents
                .data
                .iter()
                .flat_map(|data| data.keys())
                .chain(contents.binary_data.iter().flat_map(|data| data.keys()))
                .map(|name| serde_json::json!({"name": name})),
        );
    }
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?.to_string();
            let key = crate::routes::tasks::artifact_key(&name);
            if !seen.insert(key.clone()) {
                return None;
            }
            let bytes = contents.and_then(|contents| {
                contents
                    .data
                    .as_ref()
                    .and_then(|data| data.get(&key))
                    .map(|text| text.as_bytes())
                    .or_else(|| {
                        contents
                            .binary_data
                            .as_ref()
                            .and_then(|data| data.get(&key))
                            .map(|bytes| bytes.0.as_slice())
                    })
            });
            let address = bytes.map(content_address).or_else(|| {
                entry
                    .get("sha256")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
            Some(ArtifactFileDto {
                name,
                size_bytes: bytes
                    .map(|bytes| bytes.len() as i64)
                    .or_else(|| entry.get("size_bytes").and_then(|value| value.as_i64())),
                has_content: bytes.is_some(),
                did: address
                    .as_ref()
                    .map(|address| format!("did:kars:{}", address.trim_start_matches("sha256:"))),
                content_address: address,
            })
        })
        .collect()
}

/// `GET /api/artifacts` — the cross-mission deliverable index, built from the
/// real persisted mission-output records.
pub async fn list_artifacts(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<ArtifactsIndexDto>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;

    let outputs = cluster
        .list_mission_deliverables()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let mut missions = Vec::with_capacity(outputs.len());
    let mut live_tasks = HashSet::new();
    let mut live_evidence = HashSet::new();
    for record in outputs {
        let task = record.output.task_name;
        let evidence_key = record.output.evidence_key;
        let data = record.output.data;
        if !output_is_owned_by(&data, &principal) {
            continue;
        }
        live_tasks.insert(task.clone());
        live_evidence.insert(evidence_key.clone());
        let files = artifact_files(&data, record.artifacts.as_ref());

        let task_review = cluster.read_review(&task).await.unwrap_or_default();
        let review = if record.current
            && crate::routes::review::review_matches_output(&data, &task_review)
        {
            task_review
        } else {
            Default::default()
        };
        let raw_output = data.get("output").cloned().unwrap_or_default();
        let mut status = data.get("status").cloned();
        if crate::routes::tasks::is_failure_shaped_output(&raw_output) {
            status = Some("error".into());
        }
        // Gate: a hung / errored / zero-output / no-material-change run is NOT a
        // deliverable and must not appear in the index or as "latest deliverable"
        // (audit f9/f13). It has no captured output the receipt can vouch for.
        let has_files = !files.is_empty();
        if !has_files && !crate::routes::tasks::is_real_deliverable(status.as_deref(), &raw_output)
        {
            continue;
        }
        let summary = data
            .get("output")
            .map(|o| crate::routes::tasks::deliverable_text(o));
        let excerpt = data
            .get("output")
            .map(|o| crate::routes::tasks::deliverable_excerpt(o))
            .filter(|s| !s.is_empty());
        // Deliverable DID: ordered file addresses + summary digest → one identity.
        let deliverable_did = deliverable_identity(&files, summary.as_deref());
        missions.push(MissionArtifactsDto {
            evidence_key: Some(evidence_key),
            current: record.current,
            parent: data.get("parent").cloned(),
            team: data.get("team").cloned(),
            archived: false,
            display_name: crate::routes::tasks::clean_display_name(
                &data.get("displayName").cloned(),
                data.get("objective").map(|s| s.as_str()).unwrap_or(""),
            ),
            objective: data
                .get("objective")
                .map(|o| crate::routes::tasks::clean_objective(o)),
            model: data.get("model").cloned(),
            finished_at: data.get("finishedAt").cloned(),
            status,
            review_status: review
                .get("status")
                .cloned()
                .unwrap_or_else(|| "none".to_string()),
            review_revision: review
                .get("revision")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0),
            summary,
            excerpt,
            files,
            pull_requests: crate::routes::tasks::extract_pull_requests(&raw_output),
            deliverable_did,
            task,
        });
    }

    // Team run resources are intentionally garbage-collected, but their
    // principal synthesis remains in the team commons. Include those archived
    // deliveries so old run links and PR provenance do not disappear.
    let can_view_all = principal
        .roles
        .iter()
        .any(|role| matches!(role.as_str(), "admin" | "operator"));
    for team in cluster.list_kind_all("KarsTeam").await? {
        let owned = team
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("kars.azure.com/owner-sub"))
            .is_some_and(|owner| owner == &principal.sub);
        if !owned && !can_view_all {
            continue;
        }
        let team_name = team.name_any();
        let commons_name = team
            .data
            .pointer("/spec/knowledgeCommons")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&team_name);
        let Some(commons) = cluster.read_commons(&team.metadata, commons_name).await? else {
            continue;
        };
        let entries: Vec<ArchivedCommonsEntry> =
            serde_json::from_str(&commons["index.json"]).map_err(anyhow::Error::from)?;
        let repos = team
            .data
            .pointer("/spec/blueprint/gitWrite/repos")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let charter = team
            .data
            .pointer("/spec/charter")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        for entry in entries {
            let taskforce = crate::routes::teams::is_run_name(&team_name, &entry.source_task);
            let persistent = entry
                .source_task
                .starts_with(&format!("{team_name}-principal-assign-"));
            if !taskforce && !persistent {
                continue;
            }
            if live_tasks.contains(&entry.source_task) || live_evidence.contains(&entry.source_task)
            {
                continue;
            }
            let Some(content) = commons.get(&commons_content_key(&entry.id)).cloned() else {
                continue;
            };
            if !crate::routes::tasks::is_real_deliverable(Some("ok"), &content) {
                continue;
            }
            let task_name = if persistent {
                format!("{team_name}-principal")
            } else {
                entry.source_task.clone()
            };
            let archive: Option<crate::team_memory::RunArchive> = commons
                .get(&crate::team_memory::archive_key(&entry.id))
                .map(|raw| serde_json::from_str(raw))
                .transpose()
                .map_err(anyhow::Error::from)?;
            let files = archive.as_ref().map(archived_files).unwrap_or_default();
            let summary = crate::routes::tasks::deliverable_text(
                archive
                    .as_ref()
                    .map(|archive| archive.output.as_str())
                    .unwrap_or(&content),
            );
            let archive_did = archive
                .as_ref()
                .and_then(|_| deliverable_identity(&files, Some(&summary)));
            missions.push(MissionArtifactsDto {
                task: task_name,
                evidence_key: Some(entry.source_task),
                current: false,
                parent: None,
                team: Some(team_name.clone()),
                archived: true,
                display_name: Some(entry.title),
                objective: (!charter.is_empty()).then(|| charter.to_string()),
                model: None,
                finished_at: Some(entry.created_at),
                status: Some("ok".into()),
                review_status: "none".into(),
                review_revision: 0,
                files,
                summary: Some(summary),
                excerpt: Some(crate::routes::tasks::deliverable_excerpt(&content)),
                pull_requests: pull_requests_with_repo_hint(&content, &repos),
                deliverable_did: archive_did.or_else(|| {
                    Some(format!(
                        "did:kars:{}",
                        entry.digest.trim_start_matches("sha256:")
                    ))
                }),
            });
        }
    }

    // Newest deliverable first, so "Latest deliverable" surfaces and the index
    // reads chronologically rather than in arbitrary cluster-list order.
    missions.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));

    Ok(Json(ArtifactsIndexDto { missions }))
}

#[cfg(test)]
mod digest_tests {
    use super::*;

    #[test]
    fn retained_files_preserve_live_deliverable_addresses_after_task_collection() {
        use crate::team_memory::{Document, RunArchive, Source, digest};

        let content = "Full decision\r\n日本語\n";
        let output = "Completed and attached: decision-log.md";
        let archive = RunArchive {
            version: 1,
            output: output.into(),
            output_sha256: digest(output),
            documents: vec![Document {
                name: "decision-log.md".into(),
                content: content.into(),
                sha256: digest(content),
                source: Source {
                    namespace: "kars-system".into(),
                    task_name: "clinic-run".into(),
                    task_uid: "collected-task".into(),
                    sandbox_uid: "sandbox".into(),
                    pod_uid: "pod".into(),
                    run_nonce: "nonce".into(),
                    assignment_id: "assignment".into(),
                    agent_did: format!("did:mesh:{}", "a".repeat(32)),
                    artifact_name: "decision-log.md".into(),
                },
            }],
            handoffs: vec![],
        };
        let canonical = serde_json::from_value(serde_json::json!({
            "metadata": {},
            "data": {"decision-log.md": content, "response.md": output}
        }))
        .unwrap();
        let live_files = artifact_files(&Default::default(), Some(&canonical));
        let retained_files = archived_files(&archive);
        assert_eq!(
            serde_json::to_value(&live_files).unwrap(),
            serde_json::to_value(&retained_files).unwrap()
        );
        let summary = crate::routes::tasks::deliverable_text(output);
        let identity = deliverable_identity(&live_files, Some(&summary));
        assert_eq!(
            identity,
            deliverable_identity(&retained_files, Some(&summary))
        );
        let mut revised = archive;
        revised.documents[0].content.push('\n');
        assert_ne!(
            identity,
            deliverable_identity(&archived_files(&revised), Some(&summary))
        );
    }

    #[test]
    fn artifact_addresses_keep_the_existing_sixteen_byte_short_form() {
        assert_eq!(
            content_address(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223"
        );
        assert_eq!(
            content_address(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb924"
        );
        assert_ne!(content_address(b"abc"), content_address(b"abc\n"));
    }

    #[test]
    fn file_index_uses_real_text_binary_and_legacy_manifest_without_duplicates() {
        let contents = serde_json::from_value(serde_json::json!({
            "metadata": {},
            "data": {"report.md": "A report", "folder_note.md": "note"},
            "binaryData": {"image.bin": "AAH/"}
        }))
        .unwrap();
        let files = artifact_files(&Default::default(), Some(&contents));
        assert_eq!(files.len(), 3);
        assert!(
            files
                .iter()
                .all(|file| file.has_content && file.content_address.is_some())
        );
        assert_eq!(
            files
                .iter()
                .find(|file| file.name == "image.bin")
                .unwrap()
                .size_bytes,
            Some(3)
        );
        let data = std::collections::BTreeMap::from([(
            "artifacts".into(),
            serde_json::json!([
                {"name":"folder/note.md", "size_bytes":900},
                {"name":"folder/note.md"},
                {"name":"missing.pdf", "size_bytes":23, "sha256":"sha256:legacy"}
            ])
            .to_string(),
        )]);
        let files = artifact_files(&data, Some(&contents));
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].name, "folder/note.md");
        assert_eq!(files[0].size_bytes, Some(4));
        assert!(!files[1].has_content);
        assert_eq!(files[1].content_address.as_deref(), Some("sha256:legacy"));
        assert_eq!(artifact_files(&data, None).len(), 2);
        assert!(
            artifact_files(&data, None)
                .iter()
                .all(|file| !file.has_content)
        );
    }
}
