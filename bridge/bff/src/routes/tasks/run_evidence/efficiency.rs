// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Efficiency-only projection of an already ownership-fenced terminal.
//! No source text or provisional router observations leave this reader.

use super::{Reply, contract, safe_integer, terminal};
use crate::kars::cluster::Cluster;
use crate::providers::signing::sha256_hex;
use k8s_openapi::api::core::v1::ConfigMap;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Measurements {
    pub harness: String,
    pub package: String,
    pub tool_calls: Option<i64>,
    pub tool_fail: Option<i64>,
}

/// Output has passed `list_mission_deliverables`' Task UID/owner fence.
/// The retained attempt supplies historical runtime/content, never today's spec.
pub(crate) fn from_attempt(
    data: &BTreeMap<String, String>,
    cm: &ConfigMap,
) -> Option<Measurements> {
    let (run, _) = terminal(data)?;
    let name = data.get("taskName")?;
    let owners = cm.metadata.owner_references.as_ref()?;
    let controllers: Vec<_> = owners
        .iter()
        .filter(|o| o.controller == Some(true))
        .collect();
    if cm.metadata.namespace.as_deref() != Some("kars-system")
        || cm.metadata.name.as_deref()
            != Some(&Cluster::mission_attempt_name(
                &run.task_uid,
                &run.run_nonce,
            ))
        || cm.metadata.deletion_timestamp.is_some()
        || cm.metadata.uid.as_deref().is_none_or(str::is_empty)
        || cm
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || controllers.len() != 1
        || controllers[0].api_version != "kars.azure.com/v1alpha1"
        || controllers[0].kind != "KarsTask"
        || controllers[0].name != *name
        || controllers[0].uid != run.task_uid
    {
        return None;
    }
    let raw = cm
        .data
        .as_ref()?
        .get("attempt.json")
        .filter(|s| s.len() <= 900 * 1024)?;
    let attempt: Value = serde_json::from_str(raw).ok()?;
    let candidate = attempt.get("candidate")?;
    let assignment = attempt.get("assignment")?;
    let reply = attempt.get("reply")?;
    let committed: Value = serde_json::from_str(data.get("evidence.json")?).ok()?;
    let content = candidate
        .get("content")?
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 192 * 1024)?;
    let harness = match candidate.get("runtime") {
        Some(Value::String(runtime)) if matches!(runtime.as_str(), "OpenClaw" | "Hermes") => {
            runtime.clone()
        }
        // Older attempts did not record their runtime. Do not infer one.
        None => String::new(),
        _ => return None,
    };
    if attempt.get("version")? != 1
        || attempt.get("phase")?.as_str()? != run.status
        || reply != &committed
        || attempt.get("startedAt")?.as_str()? != run.started_at
        || attempt.get("updatedAt")?.as_str()? != run.finished_at
        || attempt
            .get("ownerSession")?
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 253)
        || candidate.get("namespace")? != "kars-system"
        || candidate.get("agentName")?.as_str()? != run.agent_name
        || assignment.get("type")? != "mission:assign"
        || assignment.get("content")?.as_str()? != content
        || attempt.get("contentDigest")?.as_str()? != format!("sha256:{}", sha256_hex(content))
        || candidate.get("reviewedPhase") != assignment.get("reviewedPhase")
        || candidate.get("inputArtifacts") != assignment.get("inputArtifacts")
        || candidate.get("requiredOutputs") != assignment.get("requiredOutputs")
    {
        return None;
    }
    for key in [
        "taskName",
        "taskUid",
        "runNonce",
        "agentDid",
        "dispatcherDid",
        "sandboxUid",
        "podUid",
    ] {
        if candidate.get(key) != reply.get(key) || assignment.get(key) != reply.get(key) {
            return None;
        }
    }
    for key in [
        "version",
        "assignmentId",
        "bootId",
        "activityFormat",
        "reviewedPhase",
        "phaseDigest",
        "inputDigest",
        "requiredOutputs",
    ] {
        if assignment.get(key) != reply.get(key) {
            return None;
        }
    }
    if reply
        .get("artifactFormat")
        .is_some_and(|v| Some(v) != assignment.get("artifactFormat"))
    {
        return None;
    }
    let events = attempt
        .get("events")?
        .as_array()
        .filter(|e| !e.is_empty() && e.len() <= 128)?;
    let mut previous: Option<Reply> = None;
    let mut last_at = chrono::DateTime::parse_from_rfc3339(&run.started_at).ok()?;
    let finished = chrono::DateTime::parse_from_rfc3339(&run.finished_at).ok()?;
    for event in events {
        let at = chrono::DateTime::parse_from_rfc3339(event.get("at")?.as_str()?).ok()?;
        let status = event.get("status")?.as_str()?;
        if at < last_at
            || at > finished
            || !matches!(
                status,
                "accepted" | "running" | "succeeded" | "failed" | "rejected"
            )
            || event
                .as_object()?
                .keys()
                .any(|k| !matches!(k.as_str(), "at" | "status" | "evidence"))
            || previous.as_ref().is_some_and(|p| {
                matches!(p.status.as_str(), "succeeded" | "failed" | "rejected")
                    || (p.status == "running" && status == "accepted")
                    || (matches!(p.status.as_str(), "accepted" | "running") && status == "rejected")
            })
        {
            return None;
        }
        let mut frame = reply.clone();
        let fields = frame.as_object_mut()?;
        fields.remove("routerObservations");
        fields.insert("status".into(), Value::String(status.into()));
        fields.remove("evidence");
        if status != "succeeded" {
            fields.remove("output");
            fields.remove("artifacts");
        }
        if let Some(evidence) = event.get("evidence") {
            fields.insert("evidence".into(), evidence.clone());
        }
        let parsed: Reply = serde_json::from_value(frame).ok()?;
        if contract::valid(&parsed).is_none() || !super::live::evidence_valid(&parsed) {
            return None;
        }
        if let Some(before) = previous.as_ref().and_then(|p| p.evidence.as_ref()) {
            let after = parsed.evidence.as_ref()?;
            if before.model != after.model || after.rounds < before.rounds {
                return None;
            }
            match (&before.usage, &after.usage) {
                (Some(a), Some(b))
                    if b.prompt_tokens < a.prompt_tokens
                        || b.completion_tokens < a.completion_tokens
                        || b.total_tokens < a.total_tokens =>
                {
                    return None;
                }
                (Some(_), None) => return None,
                (None, Some(_)) if before.rounds > 0 => return None,
                _ => {}
            }
        }
        previous = Some(parsed);
        last_at = at;
    }
    let last = events.last()?;
    if last.get("status") != reply.get("status") || last.get("evidence") != reply.get("evidence") {
        return None;
    }
    // Whole-phase counts are validated by the terminal contract. Returned-response
    // router observations are partial: do not turn them into whole-run tool totals.
    let phase = reply.get("evidence").and_then(|e| e.get("phase"));
    let calls = phase
        .and_then(|p| p.get("attemptedToolCalls"))
        .and_then(Value::as_i64)
        .filter(|n| safe_integer(*n));
    let successes = phase
        .and_then(|p| p.get("successfulToolCalls"))
        .and_then(Value::as_i64)
        .filter(|n| safe_integer(*n));
    Some(Measurements {
        harness,
        // Hash exact complete input, including reviewed contract: no prefix
        // collisions, case folding, sensitive objective text or current-spec reuse.
        package: sha256_hex(
            serde_json::to_vec(&[
                &Value::String(content.into()),
                &assignment
                    .get("reviewedPhase")
                    .cloned()
                    .unwrap_or(Value::Null),
                &assignment
                    .get("inputDigest")
                    .cloned()
                    .unwrap_or(Value::Null),
                &assignment
                    .get("requiredOutputs")
                    .cloned()
                    .unwrap_or(Value::Null),
            ])
            .ok()?,
        ),
        tool_calls: calls,
        tool_fail: calls
            .zip(successes)
            .and_then(|(a, b)| a.checked_sub(b))
            .filter(|n| *n >= 0),
    })
}
