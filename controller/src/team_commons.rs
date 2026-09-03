// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Team **knowledge commons** — the standing org's shared, provenance-tracked
//! memory (design note §14).
//!
//! A team accumulates knowledge across its standing-operation runs. The commons
//! is the durable, in-cluster store of that knowledge: a ConfigMap
//! `kars-commons-<team>` in the controller namespace, owned by the `KarsTeam`,
//! holding an append-only set of **entries**. Each entry carries full
//! provenance — *which* task authored it, *when*, and a content digest — so the
//! commons is auditable, not a black box.
//!
//! Two load-bearing paths make this real shared memory rather than a display:
//!
//! * **Write path (autonomous):** when a standing-operation run completes, the
//!   team reconciler harvests its deliverable into a new commons entry. The team
//!   literally remembers what each run learned.
//! * **Read path (functional):** when the charter loop mints the next run, the
//!   most recent commons entries are injected as *prior knowledge* into the run
//!   objective — so the team builds on what it already knows instead of starting
//!   cold every tick.
//!
//! The store is ConfigMap-backed so it is honest and reproducible on a plain
//! (kind) cluster with no external dependency, and bounded to the ConfigMap
//! budget (oldest entries are pruned first).

use anyhow::{Context, Result};
use chrono::Utc;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api, Client,
    api::{Patch, PatchParams},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Soft cap on retained entries (oldest pruned first) to stay within the
/// ConfigMap ~1 MiB budget with headroom for content.
const MAX_ENTRIES: usize = 64;
/// Per-entry content cap (characters). Deliverables larger than this are stored
/// truncated in the commons — the full artifact lives in the run's own output.
const MAX_ENTRY_CHARS: usize = 4096;
/// How many recent entries to surface as prior knowledge on the next run.
const PRIOR_KNOWLEDGE_ENTRIES: usize = 5;

/// One provenance-tracked record in a team's knowledge commons.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommonsEntry {
    /// Stable id — the source run task name, so a run contributes at most once.
    pub id: String,
    /// Human-readable title (derived from the run objective).
    pub title: String,
    /// The task that authored this knowledge (provenance).
    pub author: String,
    /// The standing-operation run this entry was harvested from (provenance).
    pub source_task: String,
    /// RFC3339 creation time.
    pub created_at: String,
    /// `sha256:` digest over the entry content (integrity / dedup).
    pub digest: String,
    /// Size of the stored content in bytes.
    pub size_bytes: i64,
}

fn namespace() -> String {
    std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into())
}

/// ConfigMap name for a team's commons.
#[must_use]
pub fn commons_cm_name(commons: &str) -> String {
    format!("kars-commons-{commons}")
}

fn content_key(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("entry-{safe}")
}

fn digest_of(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    let mut out = String::from("sha256:");
    for b in &d[..16] {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Read the entry index for a commons. Missing/empty ⇒ `[]`.
fn read_index(cm: &ConfigMap) -> Vec<CommonsEntry> {
    cm.data
        .as_ref()
        .and_then(|d| d.get("index.json"))
        .and_then(|s| serde_json::from_str::<Vec<CommonsEntry>>(s).ok())
        .unwrap_or_default()
}

/// Ensure the commons ConfigMap exists, owned by the team. Idempotent SSA that
/// only seeds metadata (never clobbers existing entries — `data` is omitted on
/// the create so a present ConfigMap's content is preserved).
pub async fn ensure_commons(
    client: &Client,
    commons: &str,
    owner: serde_json::Value,
) -> Result<()> {
    let ns = namespace();
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    let name = commons_cm_name(commons);
    if cms
        .get_opt(&name)
        .await
        .context("get commons cm")?
        .is_some()
    {
        return Ok(());
    }
    let patch = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": name,
            "ownerReferences": [owner],
            "labels": { "kars.azure.com/commons": commons },
        },
        "data": { "index.json": "[]" },
    });
    cms.patch(
        &name,
        &PatchParams::apply(crate::field_managers::CLAW_TEAM).force(),
        &Patch::Apply(patch),
    )
    .await
    .context("create commons cm")?;
    Ok(())
}

/// Append a provenance-tracked entry to the commons, unless an entry with the
/// same `id` already exists (a run contributes at most once). Returns `true`
/// when a new entry was written.
pub async fn record_entry(
    client: &Client,
    commons: &str,
    id: &str,
    title: &str,
    author: &str,
    source_task: &str,
    content: &str,
) -> Result<bool> {
    let ns = namespace();
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    let name = commons_cm_name(commons);

    let existing = cms.get_opt(&name).await.context("get commons cm")?;
    let mut index = existing.as_ref().map(read_index).unwrap_or_default();
    if index.iter().any(|e| e.id == id) {
        return Ok(false);
    }

    let trimmed: String = content.chars().take(MAX_ENTRY_CHARS).collect();
    let entry = CommonsEntry {
        id: id.to_string(),
        title: title.chars().take(160).collect(),
        author: author.to_string(),
        source_task: source_task.to_string(),
        created_at: Utc::now().to_rfc3339(),
        digest: digest_of(&trimmed),
        size_bytes: trimmed.len() as i64,
    };

    // Rebuild data from the existing ConfigMap, preserving prior entry content.
    let mut data: BTreeMap<String, String> = existing.and_then(|cm| cm.data).unwrap_or_default();
    data.insert(content_key(&entry.id), trimmed);
    index.push(entry);

    // Prune oldest entries (and their content) beyond the budget.
    while index.len() > MAX_ENTRIES {
        let dropped = index.remove(0);
        data.remove(&content_key(&dropped.id));
    }
    data.insert(
        "index.json".into(),
        serde_json::to_string(&index).unwrap_or_else(|_| "[]".into()),
    );

    let patch = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": name,
            "labels": { "kars.azure.com/commons": commons },
        },
        "data": data,
    });
    cms.patch(
        &name,
        &PatchParams::apply(crate::field_managers::CLAW_TEAM).force(),
        &Patch::Apply(patch),
    )
    .await
    .context("write commons entry")?;
    Ok(true)
}

/// Build the **prior-knowledge** preamble injected into the next run objective —
/// the read path that makes the commons functional memory. Returns an empty
/// string when the commons has no entries (a cold team starts honestly).
pub async fn prior_knowledge(client: &Client, commons: &str) -> String {
    let ns = namespace();
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    let name = commons_cm_name(commons);
    let Ok(Some(cm)) = cms.get_opt(&name).await else {
        return String::new();
    };
    let index = read_index(&cm);
    if index.is_empty() {
        return String::new();
    }
    let data = cm.data.unwrap_or_default();
    let recent: Vec<&CommonsEntry> = index.iter().rev().take(PRIOR_KNOWLEDGE_ENTRIES).collect();
    let mut out = String::from(
        "\n\nPrior knowledge from your team's shared memory (most recent first) — \
         build on this rather than starting over:\n",
    );
    for e in recent {
        let snippet = data
            .get(&content_key(&e.id))
            .map(|c| {
                let s: String = c.chars().take(400).collect();
                s.replace('\n', " ")
            })
            .unwrap_or_default();
        out.push_str(&format!("- [{}] {}: {}\n", e.created_at, e.title, snippet));
    }
    out
}

/// Number of entries currently in a team's commons (shared-memory size).
pub async fn entry_count(client: &Client, commons: &str) -> i64 {
    let ns = namespace();
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    let name = commons_cm_name(commons);
    match cms.get_opt(&name).await {
        Ok(Some(cm)) => read_index(&cm).len() as i64,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commons_cm_name_is_stable() {
        assert_eq!(commons_cm_name("repo-watch"), "kars-commons-repo-watch");
    }

    #[test]
    fn content_key_sanitizes() {
        assert_eq!(content_key("repo-watch-run-1"), "entry-repo-watch-run-1");
        assert_eq!(content_key("a/b c"), "entry-a_b_c");
    }

    #[test]
    fn digest_has_prefix_and_is_stable() {
        let a = digest_of("hello");
        let b = digest_of("hello");
        assert!(a.starts_with("sha256:"));
        assert_eq!(a, b);
        assert_ne!(a, digest_of("world"));
    }

    #[test]
    fn read_index_handles_missing_and_malformed() {
        let empty = ConfigMap::default();
        assert!(read_index(&empty).is_empty());
        let mut data = BTreeMap::new();
        data.insert("index.json".to_string(), "not json".to_string());
        let cm = ConfigMap {
            data: Some(data),
            ..Default::default()
        };
        assert!(read_index(&cm).is_empty());
    }
}
