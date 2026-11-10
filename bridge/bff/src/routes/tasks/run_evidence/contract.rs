// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Reply;
use crate::kars::task::{
    ExecutionPhase, ExecutionRequiredToolCall,
    execution_plan::{valid_deliverable_name, validate_reviewed_fetch_phase},
};

const MAX_ARTIFACTS: usize = 16;
const MAX_ARTIFACT_BYTES: usize = 128 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PhaseEvidence {
    name: String,
    attempted_tool_calls: i64,
    successful_tool_calls: i64,
    #[serde(default)]
    completed_required_tool_calls: i64,
    min_tool_calls: i32,
    max_tool_calls: i32,
}

fn ecmascript_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn phase(value: &Value) -> Option<ExecutionPhase> {
    let mut phase: ExecutionPhase = serde_json::from_value(value.clone()).ok()?;
    let name = phase.name.as_bytes();
    if name.is_empty()
        || name.len() > 48
        || name.first() == Some(&b'-')
        || name.last() == Some(&b'-')
        || !name
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        || phase.objective.trim_matches(ecmascript_whitespace).len() < 20
        || phase.objective.len() > 1200
        || phase.min_tool_calls < 0
        || phase.max_tool_calls < phase.min_tool_calls
        || (phase.execution_mode.as_deref() != Some("native") && phase.max_tool_calls > 32)
        || validate_reviewed_fetch_phase(&phase).is_err()
        || phase.capabilities.len()
            > if phase.execution_mode.as_deref() == Some("native") {
                6
            } else {
                3
            }
        || phase.capabilities.iter().any(|c| {
            !matches!(
                c.as_str(),
                "filesystem-read" | "filesystem-write" | "network"
            ) && !(matches!(c.as_str(), "memory" | "github-pr" | "python-execute")
                && phase.execution_mode.as_deref() == Some("native"))
        })
        || (phase.capabilities.iter().any(|c| c == "python-execute")
            && !["filesystem-read", "filesystem-write", "network"]
                .iter()
                .all(|required| phase.capabilities.iter().any(|c| c == required)))
        || (phase.min_tool_calls > 0 && phase.capabilities.is_empty())
    {
        return None;
    }
    phase.capabilities.sort();
    if phase.capabilities.windows(2).any(|pair| pair[0] == pair[1]) {
        return None;
    }
    Some(phase)
}

pub(super) fn phase_digest(phase: &ExecutionPhase) -> Option<String> {
    // The wire digest preserves declaration order and includes even empty requiredToolCalls.
    // General execution-plan serialization intentionally omits that empty array.
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Canonical<'a> {
        name: &'a str,
        objective: &'a str,
        capabilities: &'a [String],
        required_tool_calls: &'a [ExecutionRequiredToolCall],
        min_tool_calls: i32,
        max_tool_calls: i32,
        fresh_context: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_mode: Option<&'a str>,
    }
    let bytes = serde_json::to_vec(&Canonical {
        name: &phase.name,
        objective: &phase.objective,
        capabilities: &phase.capabilities,
        required_tool_calls: &phase.required_tool_calls,
        min_tool_calls: phase.min_tool_calls,
        max_tool_calls: phase.max_tool_calls,
        fresh_context: phase.fresh_context,
        execution_mode: phase.execution_mode.as_deref(),
    })
    .ok()?;
    Some(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn digest(value: &Value) -> bool {
    value
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn required_outputs(value: &Value) -> Option<Vec<&str>> {
    let values = value
        .as_array()
        .filter(|names| names.len() <= MAX_ARTIFACTS)?;
    let mut names = values
        .iter()
        .map(|v| v.as_str().filter(|name| valid_deliverable_name(name)))
        .collect::<Option<Vec<_>>>()?;
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return None;
    }
    Some(names)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PeerContext {
    root_uid: String,
    state_name: String,
    slot: String,
    peers: Vec<Peer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Peer {
    role: String,
    phase: String,
    slot: String,
    agent_name: String,
}

fn peer_slot(slot: &str) -> bool {
    let b = slot.as_bytes();
    b.len() == 5
        && b[0] == b'r'
        && (b'0'..=b'7').contains(&b[1])
        && b[2] == b'-'
        && b[3] == b'p'
        && (b'0'..=b'7').contains(&b[4])
}

fn peer_context(value: &Value) -> Option<()> {
    let context: PeerContext = serde_json::from_value(value.clone()).ok()?;
    let state = context.state_name.strip_prefix("kars-reviewed-")?;
    if !super::identifier(&context.root_uid)
        || state.len() != 40
        || !state
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || (context.slot != "synthesis" && !peer_slot(&context.slot))
        || context.peers.len() > 64
    {
        return None;
    }
    let name = |s: &str| {
        !s.is_empty()
            && s.len() <= 48
            && !s.starts_with('-')
            && !s.ends_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let mut slots = std::collections::BTreeSet::new();
    let mut phases = std::collections::BTreeSet::new();
    let mut roles = std::collections::BTreeMap::new();
    let mut indices = std::collections::BTreeMap::new();
    for peer in &context.peers {
        if !name(&peer.role)
            || !name(&peer.phase)
            || !peer_slot(&peer.slot)
            || peer.agent_name != format!("rp-{}-{}", &state[..20], peer.slot)
            || !slots.insert(&peer.slot)
            || !phases.insert((&peer.role, &peer.phase))
        {
            return None;
        }
        let index = &peer.slot[..2];
        if roles
            .insert(&peer.role, index)
            .is_some_and(|previous| previous != index)
            || indices
                .insert(index, &peer.role)
                .is_some_and(|previous| previous != &peer.role)
        {
            return None;
        }
    }
    if context.slot != "synthesis" && !slots.contains(&context.slot) {
        return None;
    }
    Some(())
}

fn artifacts_valid(reply: &Reply) -> bool {
    let fields = &reply.extensions;
    if fields.get("artifactFormat").is_some_and(|v| v != "text-v1") {
        return false;
    }
    let Some(value) = fields.get("artifacts") else {
        return true;
    };
    (reply.status == "succeeded"
        || (reply.status == "failed"
            && fields.get("reviewedPhase").is_some_and(|phase| {
                phase["executionMode"] == "native"
                    && phase["capabilities"]
                        .as_array()
                        .is_some_and(|capabilities| {
                            capabilities.iter().any(|c| c == "python-execute")
                        })
            })
            && value.as_object().is_some_and(|artifacts| {
                artifacts.len() == 1
                    && artifacts
                        .get("kars-python-executions.json")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.len() <= 65536)
            })))
        && fields.get("artifactFormat").is_some_and(|v| v == "text-v1")
        && value.as_object().is_some_and(|artifacts| {
            artifacts.len() <= MAX_ARTIFACTS
                && artifacts.iter().all(|(name, content)| {
                    valid_deliverable_name(name)
                        && content
                            .as_str()
                            .is_some_and(|s| !s.trim_matches(ecmascript_whitespace).is_empty())
                })
        })
        && value.as_object().is_some_and(|artifacts| {
            let ordinary: serde_json::Map<String, Value> = artifacts
                .iter()
                .filter(|(name, _)| name.as_str() != super::super::router_activity::ARTIFACT)
                .map(|(name, content)| (name.clone(), content.clone()))
                .collect();
            serde_json::to_vec(&ordinary).is_ok_and(|bytes| bytes.len() <= MAX_ARTIFACT_BYTES)
                && artifacts
                    .get(super::super::router_activity::ARTIFACT)
                    .is_none_or(|content| {
                        let wrapped =
                            serde_json::json!({(super::super::router_activity::ARTIFACT): content});
                        serde_json::to_vec(&wrapped).is_ok_and(|bytes| {
                            bytes.len() <= super::super::router_activity::MAX_ACTIVITY_BYTES
                        })
                    })
        })
}

pub(super) fn within_wire_bound(value: &Value) -> bool {
    if !serde_json::to_vec(value)
        .is_ok_and(|bytes| bytes.len() <= super::super::router_activity::MAX_MISSION_BYTES)
    {
        return false;
    }
    let mut base = value.clone();
    let Some(fields) = base.as_object_mut() else {
        return false;
    };
    fields.remove("routerObservations");
    if let Some(artifacts) = fields.get_mut("artifacts").and_then(Value::as_object_mut) {
        artifacts.remove(super::super::router_activity::ARTIFACT);
    }
    serde_json::to_vec(&base).is_ok_and(|bytes| bytes.len() <= 192 * 1024)
}

/// Checks reply contract integrity, not permission to execute the reviewed plan.
/// Replies carry input digests only; upstream document bytes belong to assignments.
pub(super) fn valid(reply: &Reply) -> Option<()> {
    let fields = &reply.extensions;
    if let Some(observations) = fields.get("routerObservations") {
        if !matches!(reply.status.as_str(), "running" | "failed")
            || fields.get("activityFormat")?.as_str()? != "router-observations-v1"
        {
            return None;
        }
        let observations = observations.as_str()?;
        if observations.len() > super::super::router_activity::MAX_ACTIVITY_BYTES {
            return None;
        }
        crate::routes::tasks::router_activity::parse(observations)?;
    }
    if fields.contains_key("inputArtifacts")
        || (!matches!(reply.version, 4 | 5) && fields.contains_key("requiredOutputs"))
        || (reply.version != 5 && fields.contains_key("peerContext"))
        || !artifacts_valid(reply)
        || reply
            .evidence
            .as_ref()
            .is_some_and(|e| e.extensions.keys().any(|k| k != "phase"))
    {
        return None;
    }
    match reply.version {
        1 => {
            if ["reviewedPhase", "phaseDigest", "inputDigest"]
                .iter()
                .any(|k| fields.contains_key(*k))
                || reply
                    .evidence
                    .as_ref()
                    .is_some_and(|e| e.extensions.contains_key("phase"))
            {
                return None;
            }
        }
        2..=5 => {
            if reply.version == 5 {
                peer_context(fields.get("peerContext")?)?;
            }
            if matches!(reply.version, 4 | 5) {
                let required = required_outputs(fields.get("requiredOutputs")?)?;
                if reply.status == "succeeded"
                    && required.iter().any(|name| {
                        !fields
                            .get("artifacts")
                            .and_then(Value::as_object)
                            .is_some_and(|artifacts| artifacts.contains_key(*name))
                    })
                {
                    return None;
                }
            }
            let phase = phase(fields.get("reviewedPhase")?)?;
            if fields.get("phaseDigest")?.as_str()? != phase_digest(&phase)? {
                return None;
            }
            if reply.version == 2 {
                if fields.contains_key("inputDigest") {
                    return None;
                }
            } else if (reply.version == 3 || fields.contains_key("inputDigest"))
                && (!digest(fields.get("inputDigest")?)
                    || phase.max_tool_calls == 0
                    || !phase.capabilities.iter().any(|c| c == "filesystem-read"))
            {
                return None;
            }
            if let Some(evidence) = &reply.evidence {
                let counts: PhaseEvidence =
                    serde_json::from_value(evidence.extensions.get("phase")?.clone()).ok()?;
                if counts.name != phase.name
                    || counts.min_tool_calls != phase.min_tool_calls
                    || counts.max_tool_calls != phase.max_tool_calls
                    || counts.successful_tool_calls < 0
                    || counts.completed_required_tool_calls < 0
                    || counts.completed_required_tool_calls > counts.successful_tool_calls
                    || counts.completed_required_tool_calls > phase.required_tool_calls.len() as i64
                    || (reply.status == "succeeded"
                        && counts.completed_required_tool_calls
                            != phase.required_tool_calls.len() as i64)
                    || counts.attempted_tool_calls < counts.successful_tool_calls
                    || counts.attempted_tool_calls > i64::from(phase.max_tool_calls)
                    || (reply.status == "succeeded"
                        && counts.successful_tool_calls < i64::from(phase.min_tool_calls))
                {
                    return None;
                }
            }
        }
        _ => return None,
    }
    Some(())
}
