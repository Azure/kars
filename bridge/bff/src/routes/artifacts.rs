// kars Bridge BFF — the cross-mission Artifacts index.
//
// The Artifacts surface (design note §16) lists the real deliverables missions
// have produced — the files captured by the controller from the agent loop over
// the mesh, persisted as durable `kars-mission-output-*` / `kars-mission-artifacts-*`
// ConfigMaps. This is a read-only projection of those real records; it never
// fabricates a deliverable and is honestly empty until a mission produces one.

use axum::{
    Json,
    extract::{Extension, State},
};
use kube::ResourceExt;
use serde::Deserialize;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::output_is_owned_by;
use crate::state::AppState;

/// `sha256:<hex>` content-address over arbitrary bytes (16-byte short form,
/// matching the controller's receipt-digest convention).
fn content_address(bytes: &[u8]) -> String {
    let full = Sha256::digest(bytes);
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

/// `GET /api/artifacts` — the cross-mission deliverable index, built from the
/// real persisted mission-output records.
pub async fn list_artifacts(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<ArtifactsIndexDto>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;

    let outputs = cluster.list_mission_output_evidence().await;
    let mut missions = Vec::with_capacity(outputs.len());
    let mut live_tasks = HashSet::new();
    let mut live_evidence = HashSet::new();
    for record in outputs {
        let task = record.task_name;
        let evidence_key = record.evidence_key;
        let data = record.data;
        if !output_is_owned_by(&data, &principal) {
            continue;
        }
        live_tasks.insert(task.clone());
        live_evidence.insert(evidence_key.clone());
        // Read the real artifact content so each file can be content-addressed.
        let contents = cluster
            .read_mission_artifacts(&evidence_key)
            .await
            .unwrap_or_default();
        // The artifact manifest (names + sizes) is recorded on the output CM by
        // the controller; parse it for the file list. Absent → no files (the
        // single-turn run path produces only a text summary, no file set).
        let files: Vec<ArtifactFileDto> = data
            .get("artifacts")
            .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(raw).ok())
            .map(|entries| {
                entries
                    .into_iter()
                    .filter_map(|e| {
                        let name = e.get("name")?.as_str()?.to_string();
                        let size_bytes = e.get("size_bytes").and_then(|v| v.as_i64());
                        // Inline text content lets us content-address; binary
                        // artifacts may carry a manifest digest instead.
                        let inline = contents.get(&name);
                        let content_address = inline
                            .map(|c| content_address(c.as_bytes()))
                            .or_else(|| e.get("sha256").and_then(|v| v.as_str()).map(String::from));
                        let did = content_address
                            .as_ref()
                            .map(|ca| format!("did:kars:{}", ca.trim_start_matches("sha256:")));
                        Some(ArtifactFileDto {
                            name,
                            size_bytes,
                            // Honest downloadability: the Bridge can only serve a
                            // file whose bytes are inlined in the artifacts CM.
                            // A manifest-digest-only (binary) entry is listed with
                            // its provenance but NOT marked downloadable — else the
                            // download link would 404.
                            has_content: inline.is_some(),
                            content_address,
                            did,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let assignment_identity = data
            .get("assignmentNonce")
            .cloned()
            .unwrap_or_else(|| evidence_key.clone());
        let task_review = cluster.read_review(&task).await.unwrap_or_default();
        let review = if task_review.get("assignmentNonce") == Some(&assignment_identity) {
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
        let mut lineage = String::new();
        for f in &files {
            if let Some(ca) = &f.content_address {
                lineage.push_str(ca);
                lineage.push('\n');
            }
        }
        if let Some(s) = &summary {
            lineage.push_str(&content_address(s.as_bytes()));
        }
        let deliverable_did = (!lineage.is_empty()).then(|| {
            format!(
                "did:kars:{}",
                content_address(lineage.as_bytes()).trim_start_matches("sha256:")
            )
        });
        missions.push(MissionArtifactsDto {
            evidence_key: Some(evidence_key),
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
    for team in cluster.list_kind_all("KarsTeam").await.unwrap_or_default() {
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
        let Some(commons) = cluster.read_commons(commons_name).await else {
            continue;
        };
        let entries = commons
            .get("index.json")
            .and_then(|raw| serde_json::from_str::<Vec<ArchivedCommonsEntry>>(raw).ok())
            .unwrap_or_default();
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
            let taskforce = entry.source_task.starts_with(&format!("{team_name}-run-"));
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
            missions.push(MissionArtifactsDto {
                task: task_name,
                evidence_key: Some(entry.source_task),
                team: Some(team_name.clone()),
                archived: true,
                display_name: Some(entry.title),
                objective: (!charter.is_empty()).then(|| charter.to_string()),
                model: None,
                finished_at: Some(entry.created_at),
                status: Some("ok".into()),
                review_status: "none".into(),
                review_revision: 0,
                files: Vec::new(),
                summary: Some(crate::routes::tasks::deliverable_text(&content)),
                excerpt: Some(crate::routes::tasks::deliverable_excerpt(&content)),
                pull_requests: pull_requests_with_repo_hint(&content, &repos),
                deliverable_did: Some(format!(
                    "did:kars:{}",
                    entry.digest.trim_start_matches("sha256:")
                )),
            });
        }
    }

    // Newest deliverable first, so "Latest deliverable" surfaces and the index
    // reads chronologically rather than in arbitrary cluster-list order.
    missions.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));

    Ok(Json(ArtifactsIndexDto { missions }))
}
