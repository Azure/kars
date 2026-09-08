// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Namespace-local, Team-owned digest log. Create never adopts another log;
//! replace uses resourceVersion CAS and preserves unrelated metadata and data.

use crate::kars_team::KarsTeam;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use k8s_openapi::{
    api::core::v1::ConfigMap,
    apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference},
};
use kube::{Api, Client, ResourceExt, api::PostParams};
use serde::{Deserialize, Serialize};

const MAX_DIGESTS: usize = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestEntry {
    pub team: String,
    pub at: String,
    pub reporting_to: Option<String>,
    pub health: String,
    pub summary: String,
    pub runs_generated: i64,
    pub runs_delivered: i64,
    pub tokens_spent: i64,
    pub knowledge_entries: i64,
    /// Idempotent publication slot: status retries do not append duplicates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
}

fn cm_name(team: &str) -> String {
    format!("kars-team-digest-{team}")
}

fn checked_log(cm: &ConfigMap, team: &KarsTeam) -> Result<Vec<DigestEntry>> {
    let uid = team
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .context("Team has no UID")?;
    let owners = cm.metadata.owner_references.as_deref().unwrap_or_default();
    if cm.metadata.namespace != team.metadata.namespace
        || owners
            .iter()
            .filter(|owner| owner.controller == Some(true))
            .count()
            != 1
        || !owners.iter().any(|owner| {
            owner.controller == Some(true)
                && owner.uid == uid
                && owner.kind == "KarsTeam"
                && owner.api_version == "kars.azure.com/v1alpha1"
                && owner.name == team.name_any()
        })
    {
        bail!(
            "refusing to adopt foreign digest ConfigMap '{}'",
            cm.name_any()
        );
    }
    if cm.metadata.deletion_timestamp.is_some() {
        bail!("digest ConfigMap is terminating");
    }
    let value = cm
        .data
        .as_ref()
        .and_then(|data| data.get("log.json"))
        .context("digest log.json is missing")?;
    serde_json::from_str(value).context("digest log.json is malformed")
}

#[allow(clippy::too_many_arguments)]
pub async fn publish(
    client: &Client,
    team: &KarsTeam,
    reporting_to: Option<&str>,
    health: &str,
    summary: &str,
    runs_generated: i64,
    runs_delivered: i64,
    tokens_spent: i64,
    knowledge_entries: i64,
) -> Result<()> {
    let namespace = team
        .namespace()
        .filter(|namespace| !namespace.is_empty())
        .context("Team has no namespace")?;
    let uid = team
        .metadata
        .uid
        .clone()
        .filter(|uid| !uid.is_empty())
        .context("Team has no UID")?;
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace);
    let name = cm_name(&team.name_any());
    let existing = cms.get_opt(&name).await.context("get digest ConfigMap")?;
    let mut log = match &existing {
        Some(cm) => checked_log(cm, team)?,
        None => Vec::new(),
    };
    let slot = crate::providers::signing::content_digest(&serde_json::to_vec(&(
        &uid,
        team.metadata.generation,
        team.status
            .as_ref()
            .and_then(|status| status.last_digest_at.as_deref()),
    ))?);
    if log
        .iter()
        .any(|entry| entry.slot.as_deref() == Some(slot.as_str()))
    {
        return Ok(());
    }
    log.push(DigestEntry {
        team: team.name_any(),
        at: Utc::now().to_rfc3339(),
        reporting_to: reporting_to.map(str::to_owned),
        health: health.into(),
        summary: summary.into(),
        runs_generated,
        runs_delivered,
        tokens_spent,
        knowledge_entries,
        slot: Some(slot),
    });
    if log.len() > MAX_DIGESTS {
        drop(log.drain(..log.len() - MAX_DIGESTS));
    }
    let is_new = existing.is_none();
    let mut cm = existing.unwrap_or_else(|| ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace),
            owner_references: Some(vec![OwnerReference {
                api_version: "kars.azure.com/v1alpha1".into(),
                kind: "KarsTeam".into(),
                name: team.name_any(),
                uid,
                controller: Some(true),
                block_owner_deletion: Some(true),
            }]),
            ..Default::default()
        },
        ..Default::default()
    });
    cm.metadata
        .labels
        .get_or_insert_with(Default::default)
        .insert("kars.azure.com/team-digest".into(), team.name_any());
    cm.data
        .get_or_insert_with(Default::default)
        .insert("log.json".into(), serde_json::to_string(&log)?);
    if is_new {
        cms.create(&PostParams::default(), &cm)
            .await
            .context("create digest ConfigMap")?;
    } else {
        if cm.metadata.uid.as_ref().is_none_or(String::is_empty)
            || cm
                .metadata
                .resource_version
                .as_ref()
                .is_none_or(String::is_empty)
        {
            bail!("digest ConfigMap lacks UID/resourceVersion");
        }
        cms.replace(&name, &PostParams::default(), &cm)
            .await
            .context("replace digest ConfigMap (CAS)")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_team::KarsTeamSpec;

    #[test]
    fn digest_never_adopts_or_erases_foreign_or_malformed_logs() {
        let mut team = KarsTeam::new("watch", KarsTeamSpec::default());
        team.metadata.namespace = Some("tenant-a".into());
        team.metadata.uid = Some("team-uid".into());
        let mut cm = ConfigMap::default();
        cm.metadata.namespace = team.metadata.namespace.clone();
        cm.data = Some([("log.json".into(), "[]".into())].into());
        assert!(checked_log(&cm, &team).is_err());
        cm.metadata.owner_references = Some(vec![OwnerReference {
            api_version: "kars.azure.com/v1alpha1".into(),
            kind: "KarsTeam".into(),
            name: "watch".into(),
            uid: "team-uid".into(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }]);
        assert!(checked_log(&cm, &team).unwrap().is_empty());
        cm.metadata.namespace = Some("tenant-b".into());
        assert!(checked_log(&cm, &team).is_err());
        cm.metadata.namespace = team.metadata.namespace.clone();
        cm.data
            .as_mut()
            .unwrap()
            .insert("log.json".into(), "{broken".into());
        assert!(checked_log(&cm, &team).is_err());
        cm.data = None;
        assert!(checked_log(&cm, &team).is_err());
    }
}
