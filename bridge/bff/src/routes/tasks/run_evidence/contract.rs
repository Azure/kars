// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Reply;
use crate::kars::task::ExecutionPhase;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PhaseEvidence {
    name: String,
    attempted_tool_calls: i64,
    successful_tool_calls: i64,
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
        || phase.max_tool_calls > 32
        || !phase.required_tool_calls.is_empty()
        || phase.capabilities.len() > 2
        || phase
            .capabilities
            .iter()
            .any(|c| !matches!(c.as_str(), "filesystem-read" | "filesystem-write"))
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
    // The wire digest includes the empty requiredToolCalls array and this field order.
    // General execution-plan serialization intentionally omits that empty array.
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Canonical<'a> {
        name: &'a str,
        objective: &'a str,
        capabilities: &'a [String],
        required_tool_calls: [(); 0],
        min_tool_calls: i32,
        max_tool_calls: i32,
        fresh_context: bool,
    }
    let bytes = serde_json::to_vec(&Canonical {
        name: &phase.name,
        objective: &phase.objective,
        capabilities: &phase.capabilities,
        required_tool_calls: [],
        min_tool_calls: phase.min_tool_calls,
        max_tool_calls: phase.max_tool_calls,
        fresh_context: phase.fresh_context,
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

/// Checks terminal contract integrity, not permission to execute the reviewed plan.
/// Replies carry input digests only; upstream document bytes belong to assignments.
pub(super) fn valid(reply: &Reply) -> Option<()> {
    let fields = &reply.extensions;
    if fields.contains_key("inputArtifacts")
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
        2 | 3 => {
            let phase = phase(fields.get("reviewedPhase")?)?;
            if fields.get("phaseDigest")?.as_str()? != phase_digest(&phase)? {
                return None;
            }
            if reply.version == 2 {
                if fields.contains_key("inputDigest") {
                    return None;
                }
            } else if !digest(fields.get("inputDigest")?)
                || phase.max_tool_calls == 0
                || !phase.capabilities.iter().any(|c| c == "filesystem-read")
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
