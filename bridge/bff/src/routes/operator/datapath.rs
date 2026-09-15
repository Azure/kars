// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use k8s_openapi::api::core::v1::ConfigMap;
use serde::{Deserialize, Serialize};

use super::super::require_cluster;
use crate::error::AppResult;
use crate::state::AppState;

const MAX_AGE_SECONDS: i64 = 180;
const MAX_BODY_BYTES: usize = 512 * 1024;
const SETUP: &str = "Optional, off by default. A cluster operator runs Helm; Bridge only reads reports. See deploy/ebpf-witness/README.md and deploy/helm/kars-datapath-witness (enabled=true/false). Never adopt an existing observer without operator review.";

#[derive(Debug, Deserialize, Serialize)]
pub struct DatapathWitnessSandbox {
    pub namespace: String,
    pub sandbox: String,
    pub declared_hosts: Vec<String>,
    pub observed_dns: Vec<String>,
    pub observed_connects: u64,
    pub beyond_declared: Vec<String>,
    pub unused_declared: Vec<String>,
    pub verdict: String,
    #[serde(default)]
    pub egress_mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DatapathWitnessDto {
    /// Compatibility field: true ONLY for a fresh, validated, nonempty sample.
    /// Not installation or enforcement state. New clients must use `state`.
    pub enabled: bool,
    pub generated_at: Option<String>,
    pub window_seconds: Option<u32>,
    pub sandboxes: Vec<DatapathWitnessSandbox>,
    pub install_hint: String,
    pub state: &'static str,
    pub requested_enabled: Option<bool>,
    pub settings_state: &'static str,
    /// Reading ConfigMaps cannot establish Helm/DaemonSet installation state.
    pub installation_state: &'static str,
    pub diagnostic: &'static str,
    pub age_seconds: Option<i64>,
    pub max_age_seconds: i64,
    pub coverage: &'static str,
    pub nodes_targeted: Vec<String>,
    pub nodes_with_events: Vec<String>,
    pub event_count: Option<u64>,
}

#[derive(Deserialize)]
struct Settings {
    schema_version: u32,
    enabled: bool,
    release_revision: u64,
    config_digest: String,
    sandboxes: Vec<String>,
}

#[derive(Deserialize)]
struct WitnessDoc {
    generated_at: String,
    window_seconds: u32,
    sandboxes: Vec<DatapathWitnessSandbox>,
    schema_version: Option<u32>,
    release_revision: Option<u64>,
    config_digest: Option<String>,
    publisher_uid: Option<String>,
    status: Option<String>,
    coverage: Option<String>,
    started_at: Option<String>,
    nodes_targeted: Option<Vec<String>>,
    nodes_with_events: Option<Vec<String>>,
    event_count: Option<u64>,
}

fn owned(cm: &ConfigMap) -> bool {
    let label = |key: &str| {
        cm.metadata
            .labels
            .as_ref()
            .and_then(|m| m.get(key))
            .map(String::as_str)
    };
    let annotation = |key: &str| {
        cm.metadata
            .annotations
            .as_ref()
            .and_then(|m| m.get(key))
            .map(String::as_str)
    };
    label("kars.azure.com/witness-addon") == Some("true")
        && label("app.kubernetes.io/managed-by") == Some("Helm")
        && annotation("meta.helm.sh/release-name") == Some("kars-datapath-witness")
        && annotation("meta.helm.sh/release-namespace") == Some("kars-system")
        && cm.metadata.deletion_timestamp.is_none()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
}

fn distinct(names: &[String]) -> bool {
    names
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == names.len()
}

fn valid_sandbox(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 58
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn classify(
    settings: Result<Option<ConfigMap>, kube::Error>,
    report: Result<Option<ConfigMap>, kube::Error>,
    now: DateTime<Utc>,
) -> DatapathWitnessDto {
    let mut dto = DatapathWitnessDto {
        enabled: false,
        generated_at: None,
        window_seconds: None,
        sandboxes: vec![],
        install_hint: SETUP.into(),
        state: "missing",
        requested_enabled: None,
        settings_state: "missing",
        installation_state: "unknown",
        diagnostic: "No report found. An observer may still be installed; operator inspection is required.",
        age_seconds: None,
        max_age_seconds: MAX_AGE_SECONDS,
        coverage: "unknown",
        nodes_targeted: vec![],
        nodes_with_events: vec![],
        event_count: None,
    };
    let settings = match settings {
        Err(_) => {
            dto.settings_state = "unavailable";
            None
        }
        Ok(None) => None,
        Ok(Some(cm)) => {
            let parsed = cm
                .data
                .as_ref()
                .and_then(|d| d.get("settings.json"))
                .filter(|raw| raw.len() <= MAX_BODY_BYTES)
                .and_then(|raw| serde_json::from_str::<Settings>(raw).ok());
            match parsed {
                Some(value)
                    if owned(&cm)
                        && value.schema_version == 1
                        && value.release_revision > 0
                        && value.config_digest.len() == 64
                        && value.config_digest.bytes().all(|c| c.is_ascii_hexdigit())
                        && value.sandboxes.len() <= 50
                        && distinct(&value.sandboxes)
                        && value.sandboxes.iter().all(|s| valid_sandbox(s))
                        && (!value.enabled || !value.sandboxes.is_empty()) =>
                {
                    dto.requested_enabled = Some(value.enabled);
                    dto.settings_state = "known";
                    Some(value)
                }
                _ => {
                    dto.settings_state = "invalid";
                    None
                }
            }
        }
    };
    if dto.settings_state == "unavailable" {
        dto.state = "unavailable";
        dto.diagnostic = "The operator-intent ConfigMap could not be read. No installation or observation claim is available.";
        return dto;
    }
    if dto.settings_state == "invalid" {
        dto.state = "invalid";
        dto.diagnostic =
            "Operator-intent data or ownership is invalid. Operator review is required.";
        return dto;
    }
    let cm = match report {
        Err(_) => {
            dto.state = "unavailable";
            dto.diagnostic =
                "The witness ConfigMap could not be read (API, transport, or permission failure).";
            return dto;
        }
        Ok(value) => value,
    };
    if dto.requested_enabled == Some(false) {
        dto.state = "disabled";
        dto.diagnostic = "The Helm release requests off. This is not proof that all observers or a legacy installation have stopped.";
        return dto;
    }
    let Some(cm) = cm else {
        if dto.requested_enabled == Some(true) {
            dto.state = "pending";
            dto.diagnostic = "Enablement is requested, but no report exists. Check the operator's Helm rollout; installation is not confirmed.";
        }
        return dto;
    };
    let Some(body) = cm.data.as_ref().and_then(|d| d.get("witness.json")) else {
        if owned(&cm) && dto.requested_enabled == Some(true) {
            dto.state = "pending";
            dto.diagnostic =
                "The release created its report object; the aggregator has not published a sample.";
        } else {
            dto.state = "invalid";
            dto.diagnostic = "The witness ConfigMap exists but has no witness.json report.";
        }
        return dto;
    };
    dto.state = "invalid";
    dto.diagnostic = "The witness report is malformed, incomplete, future-dated, or has mismatched publisher/release identity.";
    if body.len() > MAX_BODY_BYTES {
        return dto;
    }
    let Ok(doc) = serde_json::from_str::<WitnessDoc>(body) else {
        return dto;
    };
    let Ok(generated) = DateTime::parse_from_rfc3339(&doc.generated_at) else {
        return dto;
    };
    let age = now.signed_duration_since(generated).num_seconds();
    if age < -30 || !(5..=60).contains(&doc.window_seconds) {
        return dto;
    }
    dto.generated_at = Some(doc.generated_at.clone());
    dto.window_seconds = Some(doc.window_seconds);
    dto.age_seconds = Some(age.max(0));
    if age > MAX_AGE_SECONDS {
        dto.state = "stale";
        dto.diagnostic = "The last report is older than 180 seconds. It is not current observation or proof of disablement.";
        return dto;
    }
    if doc.schema_version.is_none() {
        dto.state = "legacy";
        dto.diagnostic = "A legacy report exists, but capture health and release identity are unverified. Operator review is required; do not install a second observer.";
        return dto;
    }
    let Some(settings) = settings else {
        dto.diagnostic = "A report exists without verified operator-intent metadata. Installation and release identity are unknown.";
        return dto;
    };
    if !owned(&cm)
        || doc.schema_version != Some(1)
        || doc.publisher_uid.is_none()
        || doc.publisher_uid != cm.metadata.uid
        || doc.release_revision != Some(settings.release_revision)
        || doc.config_digest.as_deref() != Some(settings.config_digest.as_str())
        || doc.coverage.as_deref() != Some("partial")
    {
        return dto;
    }
    if doc.status.as_deref() == Some("unavailable") {
        dto.state = "unavailable";
        dto.diagnostic = "The aggregator reported capture, node readiness, or declaration failure. No current sample is available; inspect operator-controlled logs.";
        return dto;
    }
    let (Some(nodes), Some(event_nodes), Some(count), Some(start)) = (
        doc.nodes_targeted,
        doc.nodes_with_events,
        doc.event_count,
        doc.started_at,
    ) else {
        return dto;
    };
    let Ok(started) = DateTime::parse_from_rfc3339(&start) else {
        return dto;
    };
    let elapsed = generated.signed_duration_since(started).num_seconds();
    let status = doc.status.as_deref();
    if nodes.is_empty()
        || nodes.len() > 10000
        || !distinct(&nodes)
        || !distinct(&event_nodes)
        || nodes.iter().any(|n| !valid_name(n))
        || event_nodes.iter().any(|n| !nodes.contains(n))
        || !(i64::from(doc.window_seconds)..=MAX_AGE_SECONDS).contains(&elapsed)
        || !matches!(
            (status, count),
            (Some("empty"), 0) | (Some("observed"), 1..)
        )
        || (count == 0) != event_nodes.is_empty()
        || doc.sandboxes.len() != settings.sandboxes.len()
    {
        return dto;
    }
    let mut seen = std::collections::BTreeSet::new();
    for sandbox in &doc.sandboxes {
        let expected_verdict = if sandbox.egress_mode.as_deref() == Some("Learn") {
            "LEARN"
        } else if !sandbox.beyond_declared.is_empty() {
            "BEYOND-DECLARED"
        } else if sandbox.observed_dns.is_empty() && sandbox.observed_connects == 0 {
            "NO-TRAFFIC"
        } else {
            "NO-BEYOND-OBSERVED"
        };
        if !settings.sandboxes.contains(&sandbox.sandbox)
            || sandbox.namespace != format!("kars-{}", sandbox.sandbox)
            || !seen.insert(&sandbox.sandbox)
            || !matches!(sandbox.egress_mode.as_deref(), Some("Learn" | "Strict"))
            || !matches!(
                sandbox.verdict.as_str(),
                "LEARN" | "NO-TRAFFIC" | "NO-BEYOND-OBSERVED" | "BEYOND-DECLARED"
            )
            || sandbox.verdict != expected_verdict
            || sandbox.observed_connects > count
            || sandbox.observed_dns.len() as u64 > count
            || sandbox
                .beyond_declared
                .iter()
                .any(|host| !sandbox.observed_dns.contains(host))
            || sandbox
                .unused_declared
                .iter()
                .any(|host| !sandbox.declared_hosts.contains(host))
            || [
                &sandbox.declared_hosts,
                &sandbox.observed_dns,
                &sandbox.beyond_declared,
                &sandbox.unused_declared,
            ]
            .into_iter()
            .any(|hosts| {
                !distinct(hosts)
                    || hosts.len() > 10000
                    || hosts
                        .iter()
                        .any(|h| h.is_empty() || h.len() > 253 || h.chars().any(char::is_control))
            })
            || (count == 0 && (!sandbox.observed_dns.is_empty() || sandbox.observed_connects > 0))
        {
            return dto;
        }
    }
    dto.enabled = count > 0;
    dto.state = if count > 0 { "observed" } else { "empty" };
    dto.diagnostic = if count > 0 {
        "Fresh bounded DNS/TCP sample. Observation is partial: not proof of complete kernel coverage, successful connections, or enforcement."
    } else {
        "Capture commands completed with no in-scope events. Empty traffic and DaemonSet readiness do not prove complete kernel coverage."
    };
    dto.coverage = "partial";
    dto.nodes_targeted = nodes;
    dto.nodes_with_events = event_nodes;
    dto.event_count = Some(count);
    dto.sandboxes = doc.sandboxes;
    dto
}

pub async fn datapath_witness(
    State(state): State<AppState>,
) -> AppResult<Json<DatapathWitnessDto>> {
    let cluster = require_cluster(&state)?;
    let (settings, report) = tokio::join!(
        cluster.datapath_witness_configmap(true),
        cluster.datapath_witness_configmap(false),
    );
    if settings.is_err() || report.is_err() {
        tracing::warn!("optional datapath witness ConfigMap read unavailable");
    }
    Ok(Json(classify(settings, report, Utc::now())))
}

#[cfg(test)]
#[path = "datapath_tests.rs"]
mod tests;
