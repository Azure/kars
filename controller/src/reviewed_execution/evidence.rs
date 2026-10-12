// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

// Mission publication owns evidence without the controller's GC-blocking flag.
pub(super) fn owned(map: &ConfigMap, task: &KarsTask) -> Result<()> {
    live(&map.metadata)?;
    let mut expected = task
        .controller_owner_ref(&())
        .ok_or_else(|| anyhow::anyhow!("missing Task UID"))?;
    expected.block_owner_deletion = None;
    ensure!(
        map.metadata.namespace == task.metadata.namespace
            && map.metadata.owner_references.as_ref() == Some(&vec![expected]),
        "mission evidence outside exact Task custody"
    );
    Ok(())
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing mission {field}"))
}
fn number(value: &Value, field: &str) -> Result<u64> {
    value[field]
        .as_u64()
        .filter(|n| *n <= 9_007_199_254_740_991)
        .ok_or_else(|| anyhow::anyhow!("invalid measured {field}"))
}
fn measured(value: &Value, phase: &ExecutionPhase, success: bool) -> Result<()> {
    text(value, "model")?;
    let rounds = number(value, "rounds")?;
    let p = &value["phase"];
    let attempted = number(p, "attemptedToolCalls")?;
    let successful = number(p, "successfulToolCalls")?;
    ensure!(
        p["name"] == phase.name
            && p["minToolCalls"] == phase.min_tool_calls
            && p["maxToolCalls"] == phase.max_tool_calls
            && successful <= attempted
            && attempted <= phase.max_tool_calls as u64
            && (!success || successful >= phase.min_tool_calls as u64),
        "phase tool evidence violates reviewed limits"
    );
    if !value["usage"].is_null() {
        let usage = &value["usage"];
        ensure!(
            number(usage, "promptTokens")? + number(usage, "completionTokens")?
                == number(usage, "totalTokens")?,
            "invalid measured usage"
        );
    }
    ensure!(
        !success
            || (!value["usage"].is_null()
                && number(&value["usage"], "totalTokens")? > 0
                && rounds > 0),
        "success requires measured usage"
    );
    Ok(())
}
fn time(value: &Value, field: &str) -> Result<chrono::DateTime<chrono::FixedOffset>> {
    Ok(chrono::DateTime::parse_from_rfc3339(text(value, field)?)?)
}

pub(super) fn success(record: &Value, task: &KarsTask, phase: &ExecutionPhase) -> Result<()> {
    success_with_outputs(record, task, phase, vec![HANDOFF.into()])
}

pub(super) fn success_with_outputs(
    record: &Value,
    task: &KarsTask,
    phase: &ExecutionPhase,
    required: Vec<String>,
) -> Result<()> {
    let c = &record["candidate"];
    let a = &record["assignment"];
    let reply = &record["reply"];
    let nonce = ann(task, RUN).ok_or_else(|| anyhow::anyhow!("missing phase nonce"))?;
    let content = crate::mission_delivery::admission::objective(task, nonce)?;
    ensure!(
        record["version"] == 1
            && record["phase"] == "succeeded"
            && a["type"] == "mission:assign"
            && reply["type"] == "mission:reply"
            && reply["status"] == "succeeded"
            && c["taskUid"] == json!(task.uid())
            && c["taskName"] == task.name_any()
            && c["namespace"] == json!(task.namespace())
            && c["runNonce"] == nonce
            && c["content"] == content
            && a["content"] == content
            && record["contentDigest"] == digest(&content),
        "phase objective/identity correlation mismatch"
    );
    text(record, "ownerSession")?;
    text(c, "agentName")?;
    for field in [
        "taskName",
        "taskUid",
        "sandboxUid",
        "podUid",
        "runNonce",
        "agentDid",
        "dispatcherDid",
    ] {
        text(c, field)?;
        ensure!(
            c[field] == a[field] && c[field] == reply[field],
            "phase target correlation mismatch: {field}"
        );
    }
    for field in ["agentDid", "dispatcherDid"] {
        let did = text(c, field)?;
        ensure!(
            did.starts_with("did:mesh:")
                && did.len() == 41
                && did[9..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid mission DID"
        );
    }
    for field in ["assignmentId", "bootId"] {
        text(a, field)?;
        ensure!(
            a[field] == reply[field],
            "phase assignment correlation mismatch"
        );
    }
    let runtime = crate::kars_task::blueprint::effective_runtime_name(&task.spec);
    let activity_matches = match runtime {
        "OpenClaw" => {
            a["activityFormat"] == "router-observations-v1"
                && a.get("activityFormat") == reply.get("activityFormat")
        }
        "Hermes" => {
            (a.get("activityFormat").is_none() || a["activityFormat"] == "router-observations-v1")
                && a.get("activityFormat") == reply.get("activityFormat")
        }
        _ => false,
    };
    ensure!(
        (c["runtime"] == runtime || (runtime == "OpenClaw" && c.get("runtime").is_none()))
            && a["artifactFormat"] == "text-v1"
            && reply["artifactFormat"] == "text-v1"
            && activity_matches,
        "phase runtime or evidence negotiation mismatch"
    );
    let inputs: Vec<Input> = if c.get("inputArtifacts").is_some() {
        serde_json::from_value(c["inputArtifacts"].clone())?
    } else {
        vec![]
    };
    ensure!(
        c.get("inputArtifacts") == a.get("inputArtifacts"),
        "assignment inputs changed"
    );
    let expected = contract(phase, &inputs, required.clone())?;
    for field in [
        "version",
        "reviewedPhase",
        "phaseDigest",
        "inputDigest",
        "requiredOutputs",
    ] {
        ensure!(
            a.get(field) == expected.get(field) && reply.get(field) == expected.get(field),
            "phase contract mismatch: {field}"
        );
    }
    ensure!(
        c["reviewedPhase"] == expected["reviewedPhase"]
            && c["requiredOutputs"] == expected["requiredOutputs"],
        "candidate contract mismatch"
    );
    text(reply, "output")?;
    measured(&reply["evidence"], phase, true)?;
    let artifacts = reply["artifacts"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("missing explicit artifacts"))?;
    ensure!(
        artifacts.len() <= 16
            && serde_json::to_vec(artifacts)?.len() <= 128 * 1024
            && artifacts.iter().all(|(name, value)| !name.is_empty()
                && name.len() <= 128
                && name.as_bytes()[0].is_ascii_alphanumeric()
                && !matches!(name.as_str(), "response.md" | "constructor" | "prototype")
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                && value
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty() && s.len() <= 65536))
            && required.iter().all(|name| artifacts.contains_key(name)),
        "invalid artifacts or missing reviewed required output"
    );
    let events = record["events"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .ok_or_else(|| anyhow::anyhow!("missing bounded event journal"))?;
    let mut prior_time = time(record, "startedAt")?;
    let end = time(record, "updatedAt")?;
    let mut previous: Option<&Value> = None;
    let mut running = false;
    for (i, event) in events.iter().enumerate() {
        let at = time(event, "at")?;
        let status = text(event, "status")?;
        ensure!(
            at >= prior_time
                && at <= end
                && matches!(status, "accepted" | "running" | "succeeded")
                && (!running || status != "accepted")
                && (status != "succeeded" || i == events.len() - 1),
            "invalid mission event ordering"
        );
        if status != "accepted" || !event["evidence"].is_null() {
            measured(&event["evidence"], phase, status == "succeeded")?;
        }
        if let Some(prior) = previous {
            ensure!(
                event["evidence"]["model"] == prior["model"]
                    && number(&event["evidence"], "rounds")? >= number(prior, "rounds")?,
                "mission evidence regressed"
            );
            for field in ["attemptedToolCalls", "successfulToolCalls"] {
                ensure!(
                    number(&event["evidence"]["phase"], field)? >= number(&prior["phase"], field)?,
                    "phase evidence regressed"
                );
            }
            ensure!(
                !prior["usage"].is_null()
                    || number(prior, "rounds")? == 0
                    || event["evidence"]["usage"].is_null(),
                "unknown usage became known"
            );
            if !prior["usage"].is_null() && !event["evidence"]["usage"].is_null() {
                for field in ["promptTokens", "completionTokens", "totalTokens"] {
                    ensure!(
                        number(&event["evidence"]["usage"], field)?
                            >= number(&prior["usage"], field)?,
                        "usage regressed"
                    );
                }
            }
        }
        if !event["evidence"].is_null() {
            previous = Some(&event["evidence"]);
        }
        running |= status == "running";
        prior_time = at;
    }
    ensure!(
        events.last().unwrap()["status"] == "succeeded"
            && events.last().unwrap()["evidence"] == reply["evidence"],
        "terminal journal differs from reply"
    );
    Ok(())
}

pub(super) fn canonical(map: &ConfigMap, task: &KarsTask, key: &str, kind: &str) -> Result<()> {
    owned(map, task)?;
    ensure!(
        map.immutable == Some(true)
            && map.name_any() == format!("kars-mission-{kind}-{key}")
            && map
                .labels()
                .get(&format!("kars.azure.com/mission-{kind}"))
                .map(String::as_str)
                == Some(key),
        "not immutable canonical evidence"
    );
    for (field, value) in [
        ("evidence-key", key),
        ("evidence-role", "canonical"),
        ("principal-name", &task.name_any()),
        ("task-uid", &task.uid().unwrap()),
        ("run-nonce", ann(task, RUN).unwrap()),
    ] {
        ensure!(
            map.annotations()
                .get(&format!("kars.azure.com/mission-{field}"))
                .map(String::as_str)
                == Some(value),
            "canonical evidence metadata mismatch"
        );
    }
    Ok(())
}

pub(super) fn projection(record: &Value, output: &ConfigMap, artifacts: &ConfigMap) -> Result<()> {
    let c = &record["candidate"];
    let a = &record["assignment"];
    let reply = &record["reply"];
    let mut expected: BTreeMap<String, String> = BTreeMap::new();
    for field in [
        "taskName",
        "taskUid",
        "agentName",
        "agentDid",
        "dispatcherDid",
        "sandboxUid",
        "podUid",
    ] {
        expected.insert(field.into(), text(c, field)?.into());
    }
    for (field, value) in [
        ("assignmentNonce", &c["runNonce"]),
        ("assignmentId", &a["assignmentId"]),
        ("runtimeBootId", &a["bootId"]),
        ("output", &reply["output"]),
        ("startedAt", &record["startedAt"]),
        ("finishedAt", &record["updatedAt"]),
        ("model", &reply["evidence"]["model"]),
    ] {
        expected.insert(
            field.into(),
            value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid canonical field"))?
                .into(),
        );
    }
    expected.insert("status".into(), "ok".into());
    expected.insert("error".into(), reply["error"].as_str().unwrap_or("").into());
    expected.insert(
        "totalTokens".into(),
        number(&reply["evidence"]["usage"], "totalTokens")?.to_string(),
    );
    expected.insert("usageKnown".into(), "true".into());
    let mut files: BTreeMap<String, String> = serde_json::from_value(reply["artifacts"].clone())?;
    files
        .entry("response.md".into())
        .or_insert_with(|| reply["output"].as_str().unwrap().into());
    expected.insert("artifactCount".into(), files.len().to_string());
    let mut actual = output
        .data
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing canonical data"))?;
    let encoded = actual
        .remove("evidence.json")
        .ok_or_else(|| anyhow::anyhow!("missing canonical evidence"))?;
    ensure!(
        serde_json::from_str::<Value>(&encoded)? == *reply
            && expected == actual
            && artifacts.data.as_ref() == Some(&files),
        "canonical projection differs from durable terminal"
    );
    Ok(())
}
