// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{AgentIdentity, AgentIdentityClient};
use serde_json::{Value, json};

const ENDPOINT: &str = "https://graph.microsoft.com/v1.0/servicePrincipals/microsoft.graph.agentIdentity";

pub(crate) fn guid(value: &str) -> bool {
    value.len() == 36 && value.bytes().enumerate().all(|(i, b)| {
        if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_digit() || (b'a'..=b'f').contains(&b) }
    })
}

pub(crate) fn tags(namespace: &str, deployment: &str, root: &str, journal: &str) -> Vec<String> {
    vec!["kars-role:mission-dispatcher".into(), format!("kars-namespace-uid:{namespace}"),
        format!("kars-dispatcher-uid:{deployment}"), format!("kars-root-uid:{root}"), format!("kars-journal-uid:{journal}")]
}

fn validate(value: Value, blueprint: &str, tags: &[String]) -> Result<AgentIdentity, &'static str> {
    if value.get("@odata.type").and_then(Value::as_str) != Some("#microsoft.graph.agentIdentity") {
        return Err("dispatcher Graph response is not a typed Agent Identity");
    }
    let parsed: AgentIdentity = serde_json::from_value(value).map_err(|_| "invalid dispatcher Graph identity")?;
    let mut expected = tags.to_vec(); expected.sort();
    let mut actual = parsed.tags.clone(); actual.sort();
    if !guid(&parsed.id) || !guid(&parsed.app_id) || parsed.app_id == blueprint
        || parsed.agent_identity_blueprint_id.as_deref() != Some(blueprint) || actual != expected {
        return Err("dispatcher Graph identity ownership mismatch");
    }
    Ok(parsed)
}

impl AgentIdentityClient {
    pub(crate) async fn find_dispatcher(&self, tags: &[String]) -> Result<Option<AgentIdentity>, &'static str> {
        let token = self.graph_token().await.map_err(|_| "dispatcher Graph authentication failed")?;
        let filter = tags.iter().map(|tag| format!("tags/any(t:t eq '{}')", tag.replace('\'', "''"))).collect::<Vec<_>>().join(" and ");
        let response = self.http.get(ENDPOINT).bearer_auth(token).header("OData-Version", "4.0")
            .header("ConsistencyLevel", "eventual").query(&[("$filter", filter.as_str()), ("$top", "2"), ("$count", "true")])
            .timeout(std::time::Duration::from_secs(30)).send().await.map_err(|_| "dispatcher Graph lookup failed")?
            .error_for_status().map_err(|_| "dispatcher Graph lookup rejected")?;
        let value: Value = response.json().await.map_err(|_| "invalid dispatcher Graph lookup response")?;
        if value.get("@odata.nextLink").is_some() { return Err("dispatcher Graph lookup is incomplete or ambiguous"); }
        let rows = value.get("value").and_then(Value::as_array).ok_or("invalid dispatcher Graph lookup response")?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => validate(row.clone(), &self.config.blueprint_client_id, tags).map(Some),
            _ => Err("multiple owned dispatcher Agent Identities; explicit recovery required"),
        }
    }

    /// Only a freshly created and pinned journal may call this once. Ambiguous POSTs are never retried.
    pub(crate) async fn create_dispatcher(&self, tags: &[String], sponsors: &[String]) -> Result<AgentIdentity, &'static str> {
        if sponsors.is_empty() || sponsors.iter().any(|s| !guid(s)) { return Err("dispatcher requires valid Agent Identity sponsors"); }
        let token = self.graph_token().await.map_err(|_| "dispatcher Graph authentication failed")?;
        let body = json!({"displayName": "Kars mission dispatcher", "agentIdentityBlueprintId": self.config.blueprint_client_id,
            "tags": tags, "sponsors@odata.bind": sponsors.iter().map(|id| format!("https://graph.microsoft.com/v1.0/users/{id}")).collect::<Vec<_>>()});
        let response = self.http.post(ENDPOINT).bearer_auth(token).header("OData-Version", "4.0").json(&body)
            .timeout(std::time::Duration::from_secs(30)).send().await.map_err(|_| "dispatcher Graph create uncertain; lookup-only recovery required")?
            .error_for_status().map_err(|_| "dispatcher Graph create rejected; lookup-only recovery required")?;
        let value = response.json().await.map_err(|_| "dispatcher Graph create response uncertain; lookup-only recovery required")?;
        validate(value, &self.config.blueprint_client_id, tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BLUEPRINT: &str = "11111111-1111-1111-1111-111111111111";
    fn identity() -> Value { json!({"@odata.type":"#microsoft.graph.agentIdentity", "id":"22222222-2222-2222-2222-222222222222",
        "appId":"33333333-3333-3333-3333-333333333333", "displayName":"dispatcher", "agentIdentityBlueprintId":BLUEPRINT,
        "tags":tags("ns", "dep", "root", "journal")}) }
    #[test]
    fn dispatcher_typed_identity_requires_exact_owner_and_real_app_id() {
        let expected = tags("ns", "dep", "root", "journal");
        assert!(validate(identity(), BLUEPRINT, &expected).is_ok());
        for (key, value) in [("@odata.type", json!("#microsoft.graph.servicePrincipal")), ("appId", json!(BLUEPRINT)),
            ("appId", Value::Null), ("agentIdentityBlueprintId", json!("other")), ("id", json!("bad")), ("tags", json!([]))] {
            let mut row = identity(); row[key] = value;
            assert!(validate(row, BLUEPRINT, &expected).is_err(), "{key}");
        }
        assert!(validate(identity(), BLUEPRINT, &tags("ns", "replacement", "root", "journal")).is_err());
    }
}
