// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Team **digest** publishing (design note §20) — the standing operation's
//! periodic report to the steering inbox.
//!
//! A standing team should *tell you how it's doing* without being asked. On its
//! digest cadence the reconciler appends a timestamped digest entry to a
//! `kars-team-digest-<team>` ConfigMap in the controller namespace. The Bridge
//! steering inbox surfaces these as informational entries alongside the
//! decision queue, so the operator gets the autonomous-monitoring report
//! (N runs, M delivered, tokens spent, knowledge accumulated, health) in one
//! place — the digest is a durable, re-readable record, not an ephemeral toast.

use anyhow::{Context, Result};
use chrono::Utc;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api, Client,
    api::{Patch, PatchParams},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// Keep the most recent N digests (rolling) within the ConfigMap budget.
const MAX_DIGESTS: usize = 30;

/// One published digest entry.
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
    /// The reporting channel this entry flows on — the verified `team→recipient`
    /// edge. Reports travel only this declared line (KNOCK-gated: a report is
    /// admitted to a recipient only when that team declares it as its
    /// reporting_to). Absent recipient ⇒ apex (reports to the human steerer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Whether delivery follows a verified reporting line (gated) vs broadcast.
    #[serde(default)]
    pub gated: bool,
}

fn namespace() -> String {
    std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into())
}

fn cm_name(team: &str) -> String {
    format!("kars-team-digest-{team}")
}

/// Append a digest entry to the team's digest log (rolling, newest kept).
#[allow(clippy::too_many_arguments)]
pub async fn publish(
    client: &Client,
    team: &str,
    reporting_to: Option<&str>,
    health: &str,
    summary: &str,
    runs_generated: i64,
    runs_delivered: i64,
    tokens_spent: i64,
    knowledge_entries: i64,
) -> Result<()> {
    let ns = namespace();
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    let name = cm_name(team);

    let mut log: Vec<DigestEntry> = cms
        .get_opt(&name)
        .await
        .context("get digest cm")?
        .and_then(|cm| cm.data)
        .and_then(|d| d.get("log.json").cloned())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    log.push(DigestEntry {
        team: team.to_string(),
        at: Utc::now().to_rfc3339(),
        reporting_to: reporting_to.map(str::to_string),
        health: health.to_string(),
        summary: summary.to_string(),
        runs_generated,
        runs_delivered,
        tokens_spent,
        knowledge_entries,
        channel: Some(match reporting_to {
            Some(r) => format!("{team}→{r}"),
            None => format!("{team}→steering"),
        }),
        gated: true,
    });
    while log.len() > MAX_DIGESTS {
        log.remove(0);
    }

    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert(
        "log.json".into(),
        serde_json::to_string(&log).unwrap_or_else(|_| "[]".into()),
    );
    let patch = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": name, "labels": { "kars.azure.com/team-digest": team } },
        "data": data,
    });
    cms.patch(
        &name,
        &PatchParams::apply(crate::field_managers::CLAW_TEAM).force(),
        &Patch::Apply(patch),
    )
    .await
    .context("write digest cm")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cm_name_is_stable() {
        assert_eq!(cm_name("repo-watch"), "kars-team-digest-repo-watch");
    }
}
