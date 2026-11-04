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
const SETUP: &str = "Optional, off by default. A cluster operator runs Helm; Bridge only reads reports. Core uses datapathWitness.enabled=true/false; existing standalone owners retain their chart. See deploy/ebpf-witness/README.md. Never adopt an existing observer without operator review.";

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
    #[serde(deserialize_with = "required_nullable_string")]
    pub egress_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_uid: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub declaration_diagnostic: Option<String>,
}

fn required_nullable_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

fn present_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn selected_scope() -> String {
    "selected".into()
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
    /// Validated operator intent, not an assertion of complete fleet coverage.
    pub scope: Option<String>,
    pub owner_release: Option<String>,
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
    #[serde(default = "selected_scope")]
    scope: String,
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
    #[serde(default = "selected_scope")]
    scope: String,
    publisher_uid: Option<String>,
    status: Option<String>,
    diagnostic: Option<String>,
    coverage: Option<String>,
    started_at: Option<String>,
    nodes_targeted: Option<Vec<String>>,
    nodes_with_events: Option<Vec<String>>,
    event_count: Option<u64>,
}

fn unavailable_diagnostic(code: Option<&str>) -> &'static str {
    match code {
        Some("sandbox_inventory_changed_during_capture") => {
            "Sandbox inventory changed during capture. This window was discarded to avoid assigning traffic to the wrong worker. Waiting for the next sample."
        }
        Some("gadget_nodes_changed_during_capture") => {
            "Capture nodes changed during the window. This window was discarded to avoid presenting mismatched node coverage. Waiting for the next sample."
        }
        Some("declaration_changed_during_capture") => {
            "Egress declarations changed during capture. This window was discarded rather than comparing traffic against a different baseline. Waiting for the next sample."
        }
        Some("sample_expired_before_publication") => {
            "The capture window expired before publication. No current sample is available. Waiting for the next sample."
        }
        Some("gadget_not_ready" | "gadget_node_set_incomplete") => {
            "Capture agents are not ready on every targeted node. No current sample is available; check the observer workload and node readiness."
        }
        Some("capture_failed" | "capture_ended_early" | "capture_interrupted") => {
            "Capture did not complete. No current sample is available; inspect operator-controlled capture logs."
        }
        Some("capture_output_limit") => {
            "Capture exceeded its output limit. The incomplete window is not presented as current evidence; inspect operator-controlled capture logs."
        }
        Some(
            "capture_invalid_json" | "capture_event_schema_or_attribution" | "capture_diagnostics",
        ) => {
            "Capture output could not be validated. No current sample is available; inspect operator-controlled capture logs."
        }
        _ => {
            "The aggregator reported capture, node readiness, or declaration failure. No current sample is available; inspect operator-controlled logs."
        }
    }
}

fn owner(cm: &ConfigMap) -> Option<&str> {
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
    let release = annotation("meta.helm.sh/release-name");
    (label("kars.azure.com/witness-addon") == Some("true")
        && label("app.kubernetes.io/managed-by") == Some("Helm")
        && matches!(release, Some("kars" | "kars-datapath-witness"))
        && annotation("meta.helm.sh/release-namespace") == Some("kars-system")
        && cm.metadata.deletion_timestamp.is_none())
    .then_some(release)
    .flatten()
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

// Match the producer's declared_host contract; normalization happens upstream.
// The shared fixture checks exact matches and dot-prefixed wildcard suffixes.
fn declared_host(host: &str, declared: &[String]) -> bool {
    declared.iter().any(|item| {
        host == item
            || item
                .strip_prefix('*')
                .is_some_and(|suffix| suffix.starts_with('.') && host.ends_with(suffix))
    })
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
        scope: None,
        owner_release: None,
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
                    if owner(&cm).is_some()
                        && value.schema_version == 1
                        && value.release_revision > 0
                        && value.config_digest.len() == 64
                        && value.config_digest.bytes().all(|c| c.is_ascii_hexdigit())
                        && match value.scope.as_str() {
                            "all" => value.sandboxes.is_empty(),
                            "selected" => {
                                value.sandboxes.len() <= 50
                                    && distinct(&value.sandboxes)
                                    && value.sandboxes.iter().all(|s| valid_sandbox(s))
                                    && (!value.enabled || !value.sandboxes.is_empty())
                            }
                            _ => false,
                        } =>
                {
                    dto.requested_enabled = Some(value.enabled);
                    dto.scope = Some(value.scope.clone());
                    dto.owner_release = owner(&cm).map(str::to_owned);
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
        if owner(&cm) == dto.owner_release.as_deref() && dto.requested_enabled == Some(true) {
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
    if owner(&cm) != dto.owner_release.as_deref()
        || doc.schema_version != Some(1)
        || doc.publisher_uid.is_none()
        || doc.publisher_uid != cm.metadata.uid
        || doc.release_revision != Some(settings.release_revision)
        || doc.config_digest.as_deref() != Some(settings.config_digest.as_str())
        || doc.scope != settings.scope
        || doc.coverage.as_deref() != Some("partial")
    {
        return dto;
    }
    if doc.status.as_deref() == Some("unavailable") {
        dto.state = "unavailable";
        dto.diagnostic = unavailable_diagnostic(doc.diagnostic.as_deref());
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
        || (settings.scope == "selected" && doc.sandboxes.len() != settings.sandboxes.len())
    {
        return dto;
    }
    let mut seen = std::collections::BTreeSet::new();
    for sandbox in &doc.sandboxes {
        let expected_beyond = sandbox
            .observed_dns
            .iter()
            .filter(|host| !declared_host(host, &sandbox.declared_hosts))
            .collect::<std::collections::BTreeSet<_>>();
        let expected_verdict = if sandbox.egress_mode.as_deref() == Some("Learn") {
            "LEARN"
        } else if !expected_beyond.is_empty() {
            "BEYOND-DECLARED"
        } else if sandbox.observed_dns.is_empty() && sandbox.observed_connects == 0 {
            "NO-TRAFFIC"
        } else {
            "NO-BEYOND-OBSERVED"
        };
        let valid_declaration = if sandbox.verdict == "UNAVAILABLE" {
            sandbox.egress_mode.is_none()
                && matches!(
                    sandbox.declaration_diagnostic.as_deref(),
                    Some("baseline_unavailable" | "declaration_invalid")
                )
                && sandbox.declared_hosts.is_empty()
                && sandbox.beyond_declared.is_empty()
                && sandbox.unused_declared.is_empty()
        } else {
            sandbox.declaration_diagnostic.is_none()
                && matches!(sandbox.egress_mode.as_deref(), Some("Learn" | "Strict"))
                && sandbox.verdict == expected_verdict
                && sandbox
                    .beyond_declared
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    == expected_beyond
                && sandbox
                    .unused_declared
                    .iter()
                    .all(|host| sandbox.declared_hosts.contains(host))
        };
        if !valid_sandbox(&sandbox.sandbox)
            || (settings.scope == "selected" && !settings.sandboxes.contains(&sandbox.sandbox))
            || (settings.scope == "all"
                && sandbox
                    .sandbox_uid
                    .as_ref()
                    .is_none_or(|uid| uid.trim().is_empty()))
            || sandbox.namespace != format!("kars-{}", sandbox.sandbox)
            || !seen.insert(&sandbox.sandbox)
            || !valid_declaration
            || sandbox.observed_connects > count
            || sandbox.observed_dns.len() as u64 > count
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
