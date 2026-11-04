// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One reviewed standing-Team attempt. This is authorization, never evidence.
use chrono::{DateTime, Utc};
use kube::{
    Api, Client, ResourceExt,
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const RECORD: &str = "kars.azure.com/team-qualification";
pub const CONSUMED: &str = "kars.azure.com/team-qualification-consumed";
pub const PENDING: &str = "kars.azure.com/team-qualification-pending";
// Presentation copy is used by the BFF inclusion, not the controller.
#[allow(dead_code)]
pub const NOTE: &str = "Unqualified: one operator-reviewed Team attempt only, then pause for separate evidence review. No automatic promotion or repeat work. Current routes, resources, work intent and finite budgets remain bound; credentials, policy, isolation and content-bound skill approval remain enforced.";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub approved_by: String,
    pub approved_by_sub: String,
    pub approved_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub team_uid: String,
    pub package_sha256: String,
    pub resources: BTreeMap<String, String>,
    pub backlog_uid: String,
    pub item_id: String,
    pub work_sha256: String,
    pub request: String,
}

pub fn digest(value: &Value) -> String {
    fn canonical(value: &Value) -> String {
        match value {
            Value::Object(map) => format!(
                "{{{}}}",
                map.iter()
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(key, value)| format!("{}:{}", json!(key), canonical(value)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Value::Array(values) => format!(
                "[{}]",
                values.iter().map(canonical).collect::<Vec<_>>().join(",")
            ),
            _ => value.to_string(),
        }
    }
    format!("sha256:{:x}", Sha256::digest(canonical(value).as_bytes()))
}

pub fn package(team: &Value) -> String {
    let mut spec = team["spec"].clone();
    // Pausing is a safety action, not permission to change the reviewed authority.
    spec["paused"] = json!(true);
    digest(&json!({"namespace":team["metadata"]["namespace"],
        "name":team["metadata"]["name"], "uid":team["metadata"]["uid"], "spec":spec}))
}

pub fn validate(record: &Record, team: &Value, now: DateTime<Utc>) -> Result<(), String> {
    validate_package(record, team, now)?;
    let hex_digest = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let request_valid = record
        .request
        .strip_prefix("manual-")
        .and_then(|value| value.split_once('-'))
        .is_some_and(|(number, hash)| {
            number
                .parse::<i64>()
                .is_ok_and(|sequence| sequence > 0 && sequence.to_string() == number)
                && hex_digest(hash)
        });
    if record.backlog_uid.trim().is_empty()
        || record.item_id.trim().is_empty()
        || !hex_digest(&record.work_sha256)
        || !request_valid
    {
        return Err("Team qualification requires one exact backlog item and request".into());
    }
    Ok(())
}

pub fn validate_package(record: &Record, team: &Value, now: DateTime<Utc>) -> Result<(), String> {
    if record.approved_by.trim().is_empty()
        || record.approved_by_sub.trim().is_empty()
        || record.team_uid.is_empty()
        || team["metadata"]["uid"].as_str() != Some(&record.team_uid)
        || !team["metadata"]["deletionTimestamp"].is_null()
        || record.approved_at > now
        || record.expires_at <= now
        || record.expires_at <= record.approved_at
        || record.expires_at - record.approved_at > chrono::Duration::minutes(45)
        || record.package_sha256 != package(team)
    {
        return Err("Team qualification expired or its reviewed Team/work binding changed; review a fresh attempt".into());
    }
    Ok(())
}

pub fn api(client: Client, ns: &str, kind: &str) -> Api<DynamicObject> {
    let mut resource =
        ApiResource::from_gvk(&GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind));
    resource.plural = match kind {
        "KarsTeam" => "karsteams",
        "McpServer" => "mcpservers",
        "KarsMemory" => "karsmemories",
        "KarsSkill" => "karsskills",
        "ToolPolicy" => "toolpolicies",
        _ => unreachable!("fixed qualification resource kinds"),
    }
    .into();
    Api::namespaced_with(client, ns, &resource)
}

pub fn fingerprint(kind: &str, resource: &Value) -> Result<String, String> {
    let meta = &resource["metadata"];
    let status = &resource["status"];
    if meta["uid"].as_str().is_none_or(str::is_empty)
        || !meta["deletionTimestamp"].is_null()
        || !resource["spec"].is_object()
    {
        return Err(format!(
            "{kind} has no current resource identity/specification"
        ));
    }
    let version = match kind {
        "McpServer" => {
            if status["phase"] != "Ready"
                || !meta["generation"].is_number()
                || status["observedGeneration"] != meta["generation"]
            {
                return Err("Qualification MCP server must be currently Ready".into());
            }
            if status["toolSchemaDigest"]
                .as_str()
                .is_some_and(|v| !v.trim().is_empty())
            {
                json!({"schema":status["toolSchemaDigest"],"endpoint":status["endpoint"]})
            } else if status["mode"] == "External"
                && resource["spec"]["managed"].is_null()
                && status["endpoint"]
                    .as_str()
                    .is_some_and(|v| !v.trim().is_empty())
            {
                // External reconciliation binds configuration, not a tools/list probe.
                // The full live spec is included in the resource fingerprint below.
                json!({"externalConfiguration":{"endpoint":status["endpoint"]}})
            } else {
                return Err(
                    "Qualification MCP server requires a schema digest or current External endpoint configuration"
                        .into(),
                );
            }
        }
        "KarsMemory" => {
            if !matches!(status["phase"].as_str(), Some("Compiled" | "Ready"))
                || !meta["generation"].is_number()
                || status["observedGeneration"] != meta["generation"]
                || status["compiledDigest"]
                    .as_str()
                    .is_none_or(|v| v.trim().is_empty())
            {
                return Err(
                    "Qualification memory must have its current generation compiled".into(),
                );
            }
            json!({"backend":status["backend"],"digest":status["compiledDigest"]})
        }
        "KarsSkill" => json!(crate::skill_package::approved_digest(resource)?),
        "ToolPolicy" => Value::Null,
        _ => return Err("Unsupported qualification resource".into()),
    };
    Ok(digest(
        &json!({"kind":kind,"namespace":meta["namespace"],"name":meta["name"],
        "uid":meta["uid"],"spec":resource["spec"],"version":version}),
    ))
}

pub async fn resources(
    client: Client,
    ns: &str,
    team: &Value,
) -> Result<BTreeMap<String, String>, String> {
    let mut wanted = BTreeSet::<(String, String)>::new();
    let spec = &team["spec"];
    let mut blueprints = vec![&spec["blueprint"]];
    let mut envelopes = vec![&spec["envelope"]];
    if let Some(roles) = spec["roster"].as_array() {
        for role in roles {
            blueprints.push(&role["blueprint"]);
            envelopes.push(&role["envelope"]);
            for skill in role["skills"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                wanted.insert(("KarsSkill".into(), skill.into()));
            }
        }
    }
    for envelope in envelopes {
        if let Some(name) = envelope["toolPolicyRef"]["name"]
            .as_str()
            .filter(|v| !v.is_empty())
        {
            wanted.insert(("ToolPolicy".into(), name.into()));
        }
    }
    for bp in blueprints {
        for (field, kind) in [("mcpServers", "McpServer"), ("skills", "KarsSkill")] {
            for name in bp[field]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                wanted.insert((kind.into(), name.into()));
            }
        }
        for (field, kind) in [("memory", "KarsMemory"), ("toolPolicy", "ToolPolicy")] {
            if let Some(name) = bp[field].as_str().filter(|v| !v.is_empty()) {
                wanted.insert((kind.into(), name.into()));
            }
        }
    }
    let mut bound = BTreeMap::new();
    while let Some((kind, name)) = wanted.pop_first() {
        let key = format!("{kind}/{name}");
        if bound.contains_key(&key) {
            continue;
        }
        let resource = api(client.clone(), ns, &kind)
            .get(&name)
            .await
            .map_err(|e| e.to_string())?;
        let value = serde_json::to_value(resource).map_err(|e| e.to_string())?;
        if kind == "KarsSkill" {
            if let Some(policy) = value["spec"]["boundingPolicy"]
                .as_str()
                .filter(|v| !v.is_empty())
            {
                wanted.insert(("ToolPolicy".into(), policy.into()));
            }
            for server in value["spec"]["mcpServers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                wanted.insert(("McpServer".into(), server.into()));
            }
        }
        bound.insert(key, fingerprint(&kind, &value)?);
    }
    Ok(bound)
}

pub async fn check(
    client: Client,
    ns: &str,
    name: &str,
    record: &Record,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let team = api(client.clone(), ns, "KarsTeam")
        .get(name)
        .await
        .map_err(|e| e.to_string())?;
    let team = serde_json::to_value(team).map_err(|e| e.to_string())?;
    validate(record, &team, now)?;
    if resources(client, ns, &team).await? != record.resources {
        return Err("Reviewed Team resource identity, specification or version changed".into());
    }
    Ok(())
}

pub fn record(team: &impl ResourceExt) -> Result<Option<Record>, String> {
    team.annotations()
        .get(RECORD)
        .map(|raw| {
            serde_json::from_str(raw)
                .map_err(|_| "Stored Team qualification authorization is malformed".into())
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn team() -> Value {
        json!({"metadata":{"name":"t","namespace":"n","uid":"u"},
        "spec":{"paused":true,"charter":"Review docs","blueprint":{"model":{"provider":"p","deployment":"d"}},"roster":[]}})
    }
    fn approval(t: &Value) -> Record {
        let now = Utc::now();
        Record {
            approved_by: "Operator".into(),
            approved_by_sub: "operator".into(),
            approved_at: now,
            expires_at: now + chrono::Duration::minutes(45),
            team_uid: "u".into(),
            package_sha256: package(t),
            resources: BTreeMap::new(),
            backlog_uid: "b".into(),
            item_id: "work".into(),
            work_sha256: "a".repeat(64),
            request: format!("manual-1-{}", "b".repeat(64)),
        }
    }
    #[test]
    fn exact_team_binding_rejects_replacement_edits_and_expiry_but_not_pause() {
        let t = team();
        let r = approval(&t);
        assert!(validate(&r, &t, r.approved_at).is_ok());
        for pointer in [
            "/metadata/uid",
            "/metadata/name",
            "/metadata/namespace",
            "/spec/charter",
            "/spec/blueprint/model/provider",
            "/spec/blueprint/model/deployment",
        ] {
            let mut changed = t.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("replacement");
            assert!(validate(&r, &changed, r.approved_at).is_err(), "{pointer}");
        }
        let mut resumed = t.clone();
        resumed["spec"]["paused"] = json!(false);
        assert!(validate(&r, &resumed, r.approved_at).is_ok());
        assert!(validate(&r, &t, r.expires_at).is_err());
        let mut extended = r.clone();
        extended.expires_at += chrono::Duration::seconds(1);
        assert!(validate(&extended, &t, r.approved_at).is_err());
    }
    #[test]
    fn mcp_binding_rejects_stale_schema_and_binds_identity_spec_endpoint() {
        let server = json!({"metadata":{"name":"docs","namespace":"n","uid":"s","generation":2},
            "spec":{"url":"https://docs.example/mcp"},"status":{"phase":"Ready","observedGeneration":2,
                "toolSchemaDigest":"sha256:a","endpoint":"https://docs.example/mcp"}});
        let bound = fingerprint("McpServer", &server).unwrap();
        for pointer in [
            "/metadata/uid",
            "/spec/url",
            "/status/endpoint",
            "/status/toolSchemaDigest",
        ] {
            let mut changed = server.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("changed");
            assert_ne!(fingerprint("McpServer", &changed).unwrap(), bound);
        }
        let mut stale = server;
        stale["status"]["observedGeneration"] = json!(1);
        assert!(fingerprint("McpServer", &stale).is_err());
    }
    #[test]
    fn external_mcp_binds_reconciled_configuration_without_inventing_a_schema() {
        let server = json!({"metadata":{"name":"docs","namespace":"n","uid":"s","generation":2},
            "spec":{"url":"https://docs.example/mcp","allowedTools":["read"]},
            "status":{"phase":"Ready","observedGeneration":2,"mode":"External",
                "endpoint":"https://docs.example/mcp"}});
        let bound = fingerprint("McpServer", &server).unwrap();
        for (pointer, value) in [
            ("/metadata/uid", json!("replacement")),
            ("/spec/url", json!("https://new.example/mcp")),
            ("/spec/allowedTools", json!(["read", "write"])),
            ("/status/endpoint", json!("https://new.example/mcp")),
        ] {
            let mut changed = server.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert_ne!(
                fingerprint("McpServer", &changed).unwrap(),
                bound,
                "{pointer}"
            );
        }
        let mut probed = server.clone();
        probed["status"]["toolSchemaDigest"] = json!("sha256:observed");
        assert_ne!(fingerprint("McpServer", &probed).unwrap(), bound);
        assert!(server["status"]["toolSchemaDigest"].is_null());
    }
    #[test]
    fn external_mcp_binding_keeps_readiness_identity_and_managed_schema_checks() {
        let server = json!({"metadata":{"name":"docs","namespace":"n","uid":"s","generation":2},
            "spec":{"url":"https://docs.example/mcp"},
            "status":{"phase":"Ready","observedGeneration":2,"mode":"External",
                "endpoint":"https://docs.example/mcp"}});
        for (pointer, value) in [
            ("/metadata/uid", Value::Null),
            ("/metadata/generation", Value::Null),
            ("/status/observedGeneration", json!(1)),
            ("/status/phase", json!("Degraded")),
            ("/status/mode", json!("Managed")),
            ("/status/mode", Value::Null),
            ("/status/endpoint", json!(" ")),
            ("/status/endpoint", Value::Null),
        ] {
            let mut changed = server.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(fingerprint("McpServer", &changed).is_err(), "{pointer}");
        }
        let mut changed = server.clone();
        changed["spec"]["managed"] = json!({"image":"managed:latest"});
        assert!(fingerprint("McpServer", &changed).is_err());
        changed["status"]["toolSchemaDigest"] = json!(" ");
        assert!(fingerprint("McpServer", &changed).is_err());
        changed = server;
        changed["metadata"]["deletionTimestamp"] = json!("2026-10-18T20:00:00Z");
        assert!(fingerprint("McpServer", &changed).is_err());
    }
    #[test]
    fn qualification_rejects_noncanonical_requests_future_or_anonymous_approvals() {
        let t = team();
        let r = approval(&t);
        for request in [
            "manual-1",
            "manual-01-invalid",
            "cadence-1-invalid",
            "manual-0-invalid",
        ] {
            let mut changed = r.clone();
            changed.request = request.into();
            assert!(validate(&changed, &t, r.approved_at).is_err());
        }
        let mut changed = r.clone();
        changed.work_sha256 = "g".repeat(64);
        assert!(validate(&changed, &t, r.approved_at).is_err());
        changed = r.clone();
        changed.approved_by_sub.clear();
        assert!(validate(&changed, &t, r.approved_at).is_err());
        assert!(validate(&r, &t, r.approved_at - chrono::Duration::seconds(1)).is_err());
    }
    #[test]
    fn qualification_binds_mixed_roles_credentials_and_policy_without_normalizing_routes() {
        let mut t = team();
        t["spec"]["roster"] = json!([{"name":"worker","blueprint":{"runtime":"Hermes","model":{"provider":"foundry","deployment":"small"}}}]);
        t["spec"]["envelope"] =
            json!({"budget":{"tokens":10000},"toolPolicyRef":{"name":"bounded"}});
        t["spec"]["blueprint"]["credentialBinding"] = json!({"uid":"credential-uid"});
        let r = approval(&t);
        for pointer in [
            "/spec/roster/0/blueprint/runtime",
            "/spec/roster/0/blueprint/model/provider",
            "/spec/roster/0/blueprint/model/deployment",
            "/spec/envelope/toolPolicyRef/name",
            "/spec/blueprint/credentialBinding/uid",
        ] {
            let mut changed = t.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("changed");
            assert!(validate(&r, &changed, r.approved_at).is_err(), "{pointer}");
        }
    }
    #[test]
    fn qualification_resources_require_ready_memory_and_independently_approved_skills() {
        let memory = json!({"metadata":{"name":"m","uid":"uid","generation":1},"spec":{"backend":"azure"},"status":{"phase":"Compiled","observedGeneration":1,"compiledDigest":"sha256:d"}});
        assert!(fingerprint("KarsMemory", &memory).is_ok());
        let mut stale = memory;
        stale["status"]["observedGeneration"] = json!(0);
        assert!(fingerprint("KarsMemory", &stale).is_err());
        let skill = json!({"metadata":{"name":"skill","uid":"uid"},"spec":{}});
        assert!(fingerprint("KarsSkill", &skill).is_err());
    }
}
