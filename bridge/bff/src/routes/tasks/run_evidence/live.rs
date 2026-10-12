// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Reply, contract, did, identifier, safe_integer, valid_agent_name};
use crate::kars::{cluster::Cluster, task::KarsTask};
use crate::providers::signing::sha256_hex;
use crate::routes::tasks::router_activity::{self, RouterActivity};
use k8s_openapi::api::core::v1::ConfigMap;
use serde::Serialize;
use serde_json::Value;

/// Provisional display evidence only; never a result, artifact or review authority.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct LiveActivity {
    task_uid: String,
    run_nonce: String,
    assignment_id: String,
    agent_name: String,
    agent_did: String,
    dispatcher_did: String,
    sandbox_uid: String,
    pod_uid: String,
    runtime_boot_id: String,
    attempt_uid: String,
    progress_sequence: usize,
    observations: RouterActivity,
    #[serde(skip)]
    source: SourcePin,
}

// Keep append-only source integrity private; the public DTO contains only display evidence.
#[derive(Debug, PartialEq, Eq)]
struct SourcePin {
    fixed: String,
    journal: Vec<String>,
    updated_at: chrono::DateTime<chrono::FixedOffset>,
}

impl LiveActivity {
    fn identity(&self) -> [&str; 10] {
        [
            &self.task_uid,
            &self.run_nonce,
            &self.assignment_id,
            &self.agent_name,
            &self.agent_did,
            &self.dispatcher_did,
            &self.sandbox_uid,
            &self.pod_uid,
            &self.runtime_boot_id,
            &self.attempt_uid,
        ]
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamUpdate {
    Snapshot(String),
    Unchanged,
    Unavailable,
    Revoked,
}

/// Once published, provisional evidence cannot switch custody or restart in place.
#[derive(Default)]
pub(crate) struct StreamPin {
    last: Option<LiveActivity>,
    scope: Option<String>,
    revoked: bool,
}

impl StreamPin {
    pub(crate) fn has_snapshot(&self) -> bool {
        self.last.is_some()
    }

    fn revoke(&mut self) -> StreamUpdate {
        self.last = None;
        self.revoked = true;
        StreamUpdate::Revoked
    }

    pub(crate) fn observe(&mut self, snapshot: Option<LiveActivity>) -> StreamUpdate {
        if self.revoked {
            return StreamUpdate::Revoked;
        }
        let Some(snapshot) = snapshot else {
            return if self.has_snapshot() {
                self.revoke()
            } else {
                StreamUpdate::Unavailable
            };
        };
        if let Some(last) = &self.last {
            if last.identity() != snapshot.identity()
                || last.source.fixed != snapshot.source.fixed
                || !snapshot.source.journal.starts_with(&last.source.journal)
                || snapshot.source.updated_at < last.source.updated_at
                || snapshot.progress_sequence < last.progress_sequence
                || snapshot.observations.response_count() < last.observations.response_count()
                || self
                    .scope
                    .as_deref()
                    .is_some_and(|scope| snapshot.observations.scope_id() != Some(scope))
                || (snapshot.progress_sequence == last.progress_sequence && snapshot != *last)
            {
                return self.revoke();
            }
            if snapshot == *last {
                return StreamUpdate::Unchanged;
            }
        }
        let Ok(data) = serde_json::to_string(&snapshot) else {
            return if self.has_snapshot() {
                self.revoke()
            } else {
                StreamUpdate::Unavailable
            };
        };
        if self.scope.is_none() {
            self.scope = snapshot.observations.scope_id().map(str::to_owned);
        }
        self.last = Some(snapshot);
        StreamUpdate::Snapshot(data)
    }
}

pub(super) fn evidence_valid(reply: &Reply) -> bool {
    reply.evidence.as_ref().is_none_or(|e| {
        !e.model.trim().is_empty()
            && e.model.len() <= 253
            && safe_integer(e.rounds)
            && e.usage.as_ref().is_none_or(|u| {
                safe_integer(u.prompt_tokens)
                    && safe_integer(u.completion_tokens)
                    && safe_integer(u.total_tokens)
                    && u.prompt_tokens.checked_add(u.completion_tokens) == Some(u.total_tokens)
            })
    }) && (reply.status == "accepted" || reply.version == 1 || reply.evidence.is_some())
}

/// The Task must be freshly re-authorized after reading this ConfigMap.
pub(crate) fn from_attempt(cm: &ConfigMap, task: &KarsTask) -> Option<LiveActivity> {
    let name = task.metadata.name.as_deref()?;
    let uid = task.metadata.uid.as_deref().filter(|s| identifier(s))?;
    let namespace = task.metadata.namespace.as_deref()?;
    let annotations = task.metadata.annotations.as_ref()?;
    let nonce = annotations
        .get("kars.azure.com/run-requested")
        .filter(|s| identifier(s))?;
    let owners = cm.metadata.owner_references.as_ref()?;
    let controllers: Vec<_> = owners
        .iter()
        .filter(|o| o.controller == Some(true))
        .collect();
    if namespace != "kars-system"
        || task.metadata.deletion_timestamp.is_some()
        || annotations.get("kars.azure.com/run-completed") == Some(nonce)
        || cm.metadata.deletion_timestamp.is_some()
        || cm.metadata.namespace.as_deref() != Some(namespace)
        || cm.metadata.name.as_deref() != Some(&Cluster::mission_attempt_name(uid, nonce))
        || cm
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || controllers.len() != 1
        || controllers[0].api_version != "kars.azure.com/v1alpha1"
        || controllers[0].kind != "KarsTask"
        || controllers[0].name != name
        || controllers[0].uid != uid
    {
        return None;
    }
    let attempt_uid = cm.metadata.uid.as_deref().filter(|s| identifier(s))?;
    let raw = cm
        .data
        .as_ref()?
        .get("attempt.json")
        .filter(|s| s.len() <= 900 * 1024)?;
    let attempt: Value = serde_json::from_str(raw).ok()?;
    let candidate = attempt.get("candidate")?;
    let assignment = attempt.get("assignment")?;
    let value = attempt.get("reply")?;
    let reply: Reply = serde_json::from_value(value.clone()).ok()?;
    let agent_name = candidate
        .get("agentName")?
        .as_str()
        .filter(|s| valid_agent_name(s))?;
    let content = candidate
        .get("content")?
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 192 * 1024)?;
    if attempt.get("version")?.as_u64()? != 1
        || attempt.get("phase")? != "running"
        || attempt.get("error").is_some()
        || attempt
            .get("ownerSession")?
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 253)
        || candidate.get("namespace")? != namespace
        || reply.kind != "mission:reply"
        || reply.status != "running"
        || reply.task_name != name
        || reply.task_uid != uid
        || reply.run_nonce != *nonce
        || !did(&reply.agent_did)
        || !did(&reply.dispatcher_did)
        || reply.agent_did == reply.dispatcher_did
        || contract::valid(&reply).is_none()
        || !evidence_valid(&reply)
        || assignment.get("type")? != "mission:assign"
        || assignment.get("content")?.as_str()? != content
        || attempt.get("contentDigest")?.as_str()? != format!("sha256:{}", sha256_hex(content))
        || candidate.get("reviewedPhase") != assignment.get("reviewedPhase")
        || candidate.get("inputArtifacts") != assignment.get("inputArtifacts")
        || candidate.get("requiredOutputs") != assignment.get("requiredOutputs")
        || value.get("activityFormat")? != "router-observations-v1"
        || value
            .get("artifactFormat")
            .is_some_and(|v| Some(v) != assignment.get("artifactFormat"))
        || ["output", "artifacts", "error"]
            .iter()
            .any(|key| value.get(key).is_some())
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
        let field = value.get(key)?.as_str()?;
        if !identifier(field)
            || candidate.get(key)?.as_str()? != field
            || assignment.get(key)?.as_str()? != field
        {
            return None;
        }
    }
    for key in ["assignmentId", "bootId"] {
        if !identifier(value.get(key)?.as_str()?) {
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
        if assignment.get(key) != value.get(key) {
            return None;
        }
    }
    let started = chrono::DateTime::parse_from_rfc3339(attempt.get("startedAt")?.as_str()?).ok()?;
    let updated = chrono::DateTime::parse_from_rfc3339(attempt.get("updatedAt")?.as_str()?).ok()?;
    if updated < started {
        return None;
    }
    let events = attempt.get("events")?.as_array()?;
    if events.is_empty() || events.len() > 128 {
        return None;
    }
    let mut last_at = started;
    let mut running = false;
    let mut previous: Option<Reply> = None;
    for event in events {
        let at = chrono::DateTime::parse_from_rfc3339(event.get("at")?.as_str()?).ok()?;
        let status = event.get("status")?.as_str()?;
        if at < last_at
            || at > updated
            || !matches!(status, "accepted" | "running")
            || (running && status == "accepted")
            || event
                .as_object()?
                .keys()
                .any(|key| !matches!(key.as_str(), "at" | "status" | "evidence"))
        {
            return None;
        }
        let mut frame = value.clone();
        let fields = frame.as_object_mut()?;
        fields.remove("routerObservations");
        fields.insert("status".into(), Value::String(status.into()));
        fields.remove("evidence");
        if let Some(e) = event.get("evidence") {
            fields.insert("evidence".into(), e.clone());
        }
        let parsed: Reply = serde_json::from_value(frame).ok()?;
        if contract::valid(&parsed).is_none() || !evidence_valid(&parsed) {
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
        running |= status == "running";
        last_at = at;
    }
    let last = events.last()?;
    if last.get("status")? != "running" || last.get("evidence") != value.get("evidence") {
        return None;
    }
    let observations = router_activity::parse(value.get("routerObservations")?.as_str()?)?;
    let source = SourcePin {
        fixed: sha256_hex(
            serde_json::to_vec(&[
                candidate,
                assignment,
                attempt.get("contentDigest")?,
                attempt.get("ownerSession")?,
                attempt.get("startedAt")?,
            ])
            .ok()?,
        ),
        journal: events
            .iter()
            .map(|event| sha256_hex(event.to_string()))
            .collect(),
        updated_at: updated,
    };
    Some(LiveActivity {
        task_uid: reply.task_uid,
        run_nonce: reply.run_nonce,
        assignment_id: reply.assignment_id,
        agent_name: agent_name.into(),
        agent_did: reply.agent_did,
        dispatcher_did: reply.dispatcher_did,
        sandbox_uid: reply.sandbox_uid,
        pod_uid: reply.pod_uid,
        runtime_boot_id: reply.boot_id,
        attempt_uid: attempt_uid.into(),
        progress_sequence: events.len(),
        observations,
        source,
    })
}

#[cfg(test)]
pub(crate) mod tests;
