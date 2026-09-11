// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::tasks::{deliverable_text, require_cluster};

use super::require_owned_team;

/// The on-disk commons index entry (mirrors the controller's `CommonsEntry`).
#[derive(Debug, Deserialize)]
pub struct CommonsIndexEntry {
    pub id: String,
    pub title: String,
    pub author: String,
    pub source_task: String,
    pub created_at: String,
    pub digest: String,
    pub size_bytes: i64,
}

/// Browser-facing commons entry — the index record plus resolved content.
#[derive(Debug, Serialize)]
pub struct CommonsEntryDto {
    pub id: String,
    pub title: String,
    pub author: String,
    pub source_task: String,
    pub created_at: String,
    pub digest: String,
    pub size_bytes: i64,
    pub content: Option<String>,
}

/// Browser-facing commons response.
#[derive(Debug, Serialize)]
pub struct CommonsResponse {
    pub commons: String,
    pub count: i64,
    pub entries: Vec<CommonsEntryDto>,
}

fn commons_entry_key(id: &str) -> String {
    format!(
        "entry-{}",
        id.chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '_'
                }
            },)
            .collect::<String>()
    )
}

/// Derive a meaningful, distinct title for a commons entry. The controller
/// historically titled every entry by the team charter's first line, so the
/// Knowledge tab showed 50+ identical rows. We recover a real headline from the
/// (already envelope-unwrapped) content: the first markdown heading, else the
/// first substantive line, capped. Falls back to the stored title only when the
/// content yields nothing usable. `charter_line` is passed so we can recognize
/// (and replace) the legacy charter-as-title rows.
fn commons_title(stored: &str, content: &str, charter_line: &str) -> String {
    let derive = || -> Option<String> {
        let lines: Vec<&str> = content.lines().collect();
        let clean = |line: &str| -> Option<String> {
            let heading = line
                .trim()
                .trim_start_matches('#')
                .trim()
                .trim_start_matches("**")
                .trim_end_matches("**")
                .trim();
            // Drop leading noise — stray "?" placeholders (where an emoji was
            // stripped upstream), bullets, dashes — so the title starts on a word.
            let heading = heading
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .trim();
            if !heading.chars().any(char::is_alphanumeric) {
                return None;
            }
            let lower = heading.to_ascii_lowercase();
            if [
                "kars sandbox - secure ai runtime",
                "foundry project",
                "model:",
                "sandbox id",
                "security summary",
                "capabilities",
                "role plan",
                "role roster",
                "roles spawned",
            ]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
            {
                return None;
            }

            let title: String = heading.chars().take(90).collect();
            Some(if heading.chars().count() > 90 {
                format!("{}…", title.trim_end())
            } else {
                title
            })
        };
        // Prefer the first real markdown heading near the top — briefings lead
        // with a status sentence then a "## …" headline, which reads far better
        // as a title than the preamble line.
        for line in lines.iter().take(14) {
            if line.trim_start().starts_with('#')
                && let Some(t) = clean(line)
            {
                return Some(t);
            }
        }
        // Otherwise the first substantive line.
        lines.iter().find_map(|l| clean(l))
    };
    // Replace the legacy "title == charter" rows and any empty title. The
    // controller stored the title as the charter's first line *truncated to 160
    // chars*, so we match by prefix rather than equality.
    let stored_t = stored.trim();
    let stored_lower = stored_t.to_ascii_lowercase();
    let cl = charter_line.trim();
    let legacy = stored_t.is_empty()
        || stored_t == cl
        || (stored_t.len() >= 24 && cl.starts_with(stored_t))
        || (cl.len() >= 24 && stored_t.starts_with(cl))
        || ["kars sandbox", "role plan", "role roster", "current state"]
            .iter()
            .any(|prefix| stored_lower.starts_with(prefix));
    if legacy {
        derive().unwrap_or_else(|| stored_t.to_string())
    } else {
        stored_t.to_string()
    }
}

/// `GET /api/namespaces/:ns/teams/:name/commons` — the team's shared,
/// provenance-tracked knowledge commons (design note §14). Each entry records
/// which run authored it, when, and a content digest.
pub async fn get_team_commons(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<CommonsResponse>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    // Commons name defaults to the team name when unset.
    let commons = team
        .spec
        .knowledge_commons
        .clone()
        .unwrap_or_else(|| name.clone());

    let data = cluster.read_commons(&commons).await.unwrap_or_default();
    let index: Vec<CommonsIndexEntry> = data
        .get("index.json")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    // The charter's first line is what the controller historically used as every
    // entry's title; we use it to recognize and replace those duplicate rows.
    let charter_line = team
        .spec
        .charter
        .lines()
        .next()
        .unwrap_or(&team.spec.charter)
        .to_string();

    // Newest first, with content resolved from the companion keys. We *heal* two
    // legacy defects here so the Knowledge tab is readable for entries written
    // before the source-side fixes: (1) content stored as the raw agent JSON
    // envelope is unwrapped to its prose deliverable; (2) the duplicate
    // charter-as-title is replaced with a real headline derived from that prose.
    let mut entries: Vec<CommonsEntryDto> = index
        .into_iter()
        .rev()
        .map(|e| {
            let key = format!(
                "entry-{}",
                e.id.chars()
                    .map(
                        |c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                            c
                        } else {
                            '_'
                        }
                    )
                    .collect::<String>()
            );
            let content = data.get(&key).map(|c| deliverable_text(c));
            let title = match content.as_deref() {
                Some(c) => commons_title(&e.title, c, &charter_line),
                None => e.title.clone(),
            };
            CommonsEntryDto {
                id: e.id,
                title,
                author: e.author,
                source_task: e.source_task,
                created_at: e.created_at,
                digest: e.digest,
                size_bytes: e.size_bytes,
                content,
            }
        })
        .collect();
    let total_entries = entries.len() as i64;
    entries.truncate(50);

    Ok(Json(CommonsResponse {
        commons,
        count: total_entries,
        entries,
    }))
}

/// `GET /api/namespaces/:ns/teams/:name/runs/:run/archive` — retrieve one
/// durable archived run directly from the full commons index.
pub async fn get_archived_run(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, run)): Path<(String, String, String)>,
) -> AppResult<Json<CommonsEntryDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let taskforce = run.starts_with(&format!("{name}-run-"));
    let persistent = run.starts_with(&format!("{name}-principal-assign-"));
    if !taskforce && !persistent {
        return Err(AppError::NotFound);
    }
    let commons_name = team
        .spec
        .knowledge_commons
        .as_deref()
        .filter(|commons| !commons.trim().is_empty())
        .unwrap_or(&name);
    let data = cluster
        .read_commons(commons_name)
        .await
        .ok_or(AppError::NotFound)?;
    let entry = data
        .get("index.json")
        .and_then(|raw| serde_json::from_str::<Vec<CommonsIndexEntry>>(raw).ok())
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|entry| entry.id == run || entry.source_task == run)
        })
        .ok_or(AppError::NotFound)?;
    let content = data
        .get(&commons_entry_key(&entry.id))
        .map(|content| deliverable_text(content));
    let charter_line = team
        .spec
        .charter
        .lines()
        .next()
        .unwrap_or(&team.spec.charter);
    let title = content
        .as_deref()
        .map(|content| commons_title(&entry.title, content, charter_line))
        .unwrap_or(entry.title);
    Ok(Json(CommonsEntryDto {
        id: entry.id,
        title,
        author: entry.author,
        source_task: entry.source_task,
        created_at: entry.created_at,
        digest: entry.digest,
        size_bytes: entry.size_bytes,
        content,
    }))
}

/// One event in a team's continuous ledger.
#[derive(Debug, Serialize)]
pub struct LedgerEvent {
    pub at: String,
    pub kind: String,
    pub summary: String,
    pub task: Option<String>,
    pub tokens: Option<i64>,
}

/// `GET /api/namespaces/:ns/teams/:name/ledger` — the team's continuous ledger
/// (§14): a streaming, append-only timeline of everything the standing
/// operation has done, composed from the durable records the controller already
/// writes (generated runs + their deliverables/tokens + harvested knowledge +
/// published digests). Newest first.
pub async fn get_team_ledger(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<Vec<LedgerEvent>>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let mut events: Vec<LedgerEvent> = Vec::new();

    // Run deliverables (delivery events, with token cost).
    let run_prefix = format!("{name}-run-");
    for record in cluster.list_mission_output_evidence().await {
        let data = record.data;
        let task = data
            .get("assignmentNonce")
            .cloned()
            .unwrap_or(record.evidence_key);
        let belongs_to_team = data.get("team") == Some(&name)
            || task.starts_with(&run_prefix)
            || task.starts_with(&format!("{name}-principal-assign-"));
        if !belongs_to_team {
            continue;
        }
        let at = data.get("finishedAt").cloned().unwrap_or_default();
        let tokens = data.get("totalTokens").and_then(|t| t.parse::<i64>().ok());
        let ok = data.get("status").map(String::as_str) == Some("ok");
        events.push(LedgerEvent {
            at,
            kind: if ok {
                "delivery".into()
            } else {
                "delivery_error".into()
            },
            summary: data
                .get("output")
                .map(|o| {
                    deliverable_text(o)
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .chars()
                        .take(140)
                        .collect::<String>()
                })
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "run completed".into()),
            task: Some(task),
            tokens,
        });
    }

    // Harvested knowledge (commons entries). Heal the legacy charter-as-title so
    // the ledger reads "Learned: <real headline>" rather than the same charter
    // line on every knowledge event.
    if let Some(cm) = cluster.read_commons(&name).await
        && let Some(idx) = cm.get("index.json")
        && let Ok(entries) = serde_json::from_str::<Vec<CommonsIndexEntry>>(idx)
    {
        let charter_line = cluster
            .teams(&ns)
            .get_opt(&name)
            .await
            .ok()
            .flatten()
            .map(|t| {
                t.spec
                    .charter
                    .lines()
                    .next()
                    .unwrap_or(&t.spec.charter)
                    .to_string()
            })
            .unwrap_or_default();
        for e in entries {
            let key = format!(
                "entry-{}",
                e.id.chars()
                    .map(
                        |c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                            c
                        } else {
                            '_'
                        }
                    )
                    .collect::<String>()
            );
            let title = match cm.get(&key) {
                Some(c) => commons_title(&e.title, &deliverable_text(c), &charter_line),
                None => e.title.clone(),
            };
            events.push(LedgerEvent {
                at: e.created_at,
                kind: "knowledge".into(),
                summary: format!("Learned: {title}"),
                task: Some(e.source_task),
                tokens: None,
            });
        }
    }

    // Published digests (report events).
    for d in cluster.list_team_digests().await {
        if d.get("team").and_then(|v| v.as_str()) != Some(name.as_str()) {
            continue;
        }
        events.push(LedgerEvent {
            at: d
                .get("at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            kind: "digest".into(),
            summary: d
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            task: None,
            tokens: None,
        });
    }

    events.sort_by(|a, b| b.at.cmp(&a.at));
    events.truncate(100);
    Ok(Json(events))
}
