// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Team **knowledge commons** — the standing org's shared, provenance-tracked
//! memory (design note §14).
//!
//! A team accumulates knowledge across its standing-operation runs. The commons
//! is the durable, in-cluster store of that knowledge: a ConfigMap
//! `kars-commons-<team>` alongside and exclusively owned by the `KarsTeam`,
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
//! Legacy global stores are never implicitly adopted or shared across teams.

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::{Api, Client, Resource, ResourceExt, api::PostParams};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::kars_team::KarsTeam;
use crate::providers::signing::content_digest;

#[path = "team_commons_prompt.rs"]
mod prompt;

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

struct CommonsIdentity {
    namespace: String,
    name: String,
    owner: OwnerReference,
}

impl CommonsIdentity {
    fn for_team(team: &KarsTeam) -> Result<Self> {
        let namespace = team.namespace().context("commons team has no namespace")?;
        ensure!(
            !namespace.trim().is_empty(),
            "commons team namespace is empty"
        );
        ensure!(
            team.metadata
                .name
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty()),
            "commons team has no name"
        );
        ensure!(
            team.metadata
                .uid
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty()),
            "commons team has no UID"
        );
        let name = commons_cm_name(&team.commons_name());
        ensure!(
            name.len() <= 253
                && name.split('.').all(|label| {
                    !label.is_empty()
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                        && label.as_bytes()[0].is_ascii_alphanumeric()
                        && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                }),
            "invalid commons ConfigMap name"
        );
        Ok(Self {
            namespace,
            name,
            owner: team
                .controller_owner_ref(&())
                .context("commons team has no owner identity")?,
        })
    }

    fn validate(&self, cm: &ConfigMap) -> Result<()> {
        ensure!(
            cm.metadata.namespace.as_deref() == Some(self.namespace.as_str())
                && cm.metadata.name.as_deref() == Some(self.name.as_str()),
            "commons ConfigMap namespace or name does not match the team"
        );
        let owners = cm.metadata.owner_references.as_deref().unwrap_or_default();
        ensure!(
            owners.len() == 1
                && owners[0].controller == Some(true)
                && owners[0].uid == self.owner.uid
                && owners[0].name == self.owner.name
                && owners[0].kind == self.owner.kind
                && owners[0].api_version == self.owner.api_version,
            "commons ConfigMap is not exclusively controller-owned by this KarsTeam"
        );
        ensure!(
            cm.metadata.deletion_timestamp.is_none(),
            "commons ConfigMap is terminating"
        );
        Ok(())
    }

    fn seed(&self, commons: &str) -> ConfigMap {
        ConfigMap {
            metadata: ObjectMeta {
                name: Some(self.name.clone()),
                namespace: Some(self.namespace.clone()),
                owner_references: Some(vec![self.owner.clone()]),
                labels: Some(BTreeMap::from([(
                    "kars.azure.com/commons".into(),
                    commons.into(),
                )])),
                ..Default::default()
            },
            data: Some(BTreeMap::from([("index.json".into(), "[]".into())])),
            ..Default::default()
        }
    }
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
    content_digest(s.as_bytes())
}

/// Validate the entire store, including old entries not shown in the next prompt.
fn read_index(cm: &ConfigMap) -> Result<Vec<CommonsEntry>> {
    let data = cm.data.as_ref().context("commons data is missing")?;
    let encoded = data.get("index.json").context("commons index is missing")?;
    let index: Vec<CommonsEntry> =
        serde_json::from_str(encoded).context("invalid commons index")?;
    ensure!(
        index.len() <= MAX_ENTRIES,
        "commons index exceeds its entry limit"
    );
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for entry in &index {
        ensure!(!entry.id.trim().is_empty(), "commons entry ID is empty");
        ensure!(ids.insert(entry.id.clone()), "duplicate commons entry ID");
        let key = content_key(&entry.id);
        ensure!(
            keys.insert(key.clone()),
            "commons entry IDs collide after key normalization"
        );
        let content = data.get(&key).context("commons entry content is missing")?;
        ensure!(
            entry.size_bytes == content.len() as i64 && entry.digest == digest_of(content),
            "commons entry content failed integrity validation"
        );
        chrono::DateTime::parse_from_rfc3339(&entry.created_at)
            .context("invalid commons entry timestamp")?;
    }
    ensure!(
        data.keys()
            .filter(|key| key.starts_with("entry-"))
            .all(|key| keys.contains(key)),
        "commons contains unindexed entry content"
    );
    Ok(index)
}

/// Create only when absent. A naming collision never adopts another team's data.
pub async fn ensure_commons(client: &Client, team: &KarsTeam) -> Result<()> {
    let identity = CommonsIdentity::for_team(team)?;
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &identity.namespace);
    if let Some(cm) = cms
        .get_opt(&identity.name)
        .await
        .context("get commons ConfigMap")?
    {
        identity.validate(&cm)?;
        read_index(&cm)?;
        return Ok(());
    }
    let created = cms
        .create(&PostParams::default(), &identity.seed(&team.commons_name()))
        .await
        .context("create commons ConfigMap; conflicts require a fresh reconcile")?;
    identity.validate(&created)?;
    read_index(&created)?;
    Ok(())
}

/// Append a provenance-tracked entry to the commons, unless an entry with the
/// same `id` already exists (a run contributes at most once). Returns `true`
/// when a new entry was written.
pub async fn record_entry(
    client: &Client,
    team: &KarsTeam,
    id: &str,
    title: &str,
    author: &str,
    source_task: &str,
    content: &str,
) -> Result<bool> {
    let identity = CommonsIdentity::for_team(team)?;
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &identity.namespace);
    let existing = cms
        .get(&identity.name)
        .await
        .context("get commons ConfigMap for append")?;
    let Some(updated) = prepare_entry_update(
        &identity,
        &existing,
        id,
        title,
        author,
        source_task,
        content,
    )?
    else {
        return Ok(false);
    };
    cms.replace(&identity.name, &PostParams::default(), &updated)
        .await
        .context(
            "replace commons ConfigMap; resourceVersion conflicts require a fresh reconcile",
        )?;
    Ok(true)
}

fn prepare_entry_update(
    identity: &CommonsIdentity,
    existing: &ConfigMap,
    id: &str,
    title: &str,
    author: &str,
    source_task: &str,
    content: &str,
) -> Result<Option<ConfigMap>> {
    identity.validate(existing)?;
    ensure!(
        existing
            .metadata
            .resource_version
            .as_ref()
            .is_some_and(|s| !s.is_empty())
            && existing
                .metadata
                .uid
                .as_ref()
                .is_some_and(|s| !s.is_empty()),
        "commons update requires its original UID and resourceVersion"
    );
    let mut index = read_index(existing)?;
    ensure!(
        !id.trim().is_empty() && content_key(id).len() <= 253,
        "invalid commons entry ID"
    );
    let trimmed: String = prompt::sanitize_untrusted(content)
        .chars()
        .take(MAX_ENTRY_CHARS)
        .collect();
    let entry = CommonsEntry {
        id: id.to_string(),
        title: prompt::metadata(title, 160),
        author: prompt::metadata(author, 253),
        source_task: prompt::metadata(source_task, 253),
        created_at: Utc::now().to_rfc3339(),
        digest: digest_of(&trimmed),
        size_bytes: trimmed.len() as i64,
    };

    if let Some(prior) = index.iter().find(|prior| prior.id == id) {
        let prior_content = existing
            .data
            .as_ref()
            .and_then(|data| data.get(&content_key(id)))
            .context("commons entry content is missing")?;
        // Old entries predate sanitization. An identical normalized retry must
        // remain idempotent without rewriting the original audited bytes.
        let normalized_prior: String = prompt::sanitize_untrusted(prior_content)
            .chars()
            .take(MAX_ENTRY_CHARS)
            .collect();
        ensure!(
            normalized_prior == trimmed
                && prompt::metadata(&prior.title, 160) == entry.title
                && prompt::metadata(&prior.author, 253) == entry.author
                && prompt::metadata(&prior.source_task, 253) == entry.source_task,
            "commons entry ID already exists with different content or provenance"
        );
        return Ok(None);
    }
    let mut updated = existing.clone();
    let data = updated.data.as_mut().context("commons data is missing")?;
    ensure!(
        !data.contains_key(&content_key(id)),
        "commons entry key collision"
    );
    data.insert(content_key(&entry.id), trimmed);
    index.push(entry);

    // Prune oldest entries (and their content) beyond the budget.
    while index.len() > MAX_ENTRIES {
        let dropped = index.remove(0);
        data.remove(&content_key(&dropped.id));
    }
    data.insert(
        "index.json".into(),
        serde_json::to_string(&index).context("encode commons index")?,
    );
    Ok(Some(updated))
}

/// Build the **prior-knowledge** preamble injected into the next run objective —
/// the read path that makes the commons functional memory. `max_chars` is the
/// remaining objective allowance after its fixed prefix and charter. Includes
/// only complete JSON entries plus their complete untrusted-data framing.
/// An empty store or insufficient allowance returns no history, but ownership
/// and full store integrity are still verified even with a zero allowance.
pub async fn prior_knowledge(client: &Client, team: &KarsTeam, max_chars: usize) -> Result<String> {
    let identity = CommonsIdentity::for_team(team)?;
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &identity.namespace);
    let cm = cms
        .get(&identity.name)
        .await
        .context("get commons prior knowledge")?;
    identity.validate(&cm)?;
    let index = read_index(&cm)?;
    prompt::prior_knowledge(&cm, &index, max_chars)
}

/// Number of entries currently in a team's commons (shared-memory size).
pub async fn entry_count(client: &Client, team: &KarsTeam) -> Result<i64> {
    let identity = CommonsIdentity::for_team(team)?;
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &identity.namespace);
    let cm = cms
        .get(&identity.name)
        .await
        .context("get commons entry count")?;
    identity.validate(&cm)?;
    Ok(read_index(&cm)?.len() as i64)
}

#[cfg(test)]
#[path = "team_commons_tests.rs"]
mod tests;
