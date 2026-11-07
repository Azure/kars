// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::builtin_memory::{Identity, MAX_BYTES, Store};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

const CONFIG: &str = "/etc/kars/builtin-memory/builtin-memory.json";

#[derive(Deserialize)]
struct Configuration {
    identity: Identity,
    map_uid: String,
    api_url: String,
    ca: String,
    token: String,
    expires_at: i64,
    sandbox_uid: String,
}

fn check_map(map: &Value, config: &Configuration) -> Result<Store> {
    let identity = &config.identity;
    ensure!(
        map["metadata"]["name"] == identity.map_name()
            && map["metadata"]["namespace"] == identity.namespace
            && map["metadata"]["uid"] == config.map_uid
            && map["metadata"]["resourceVersion"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            && map["metadata"]["deletionTimestamp"].is_null(),
        "private memory object identity changed"
    );
    ensure!(
        map["metadata"]["ownerReferences"]
            == json!([{"apiVersion":"kars.azure.com/v1alpha1","kind":identity.owner_kind,"name":identity.owner_name,"uid":identity.owner_uid,"controller":true,"blockOwnerDeletion":false}]),
        "private memory owner changed"
    );
    let data = map["data"]["memory.json"]
        .as_str()
        .context("private memory payload missing")?;
    ensure!(data.len() <= MAX_BYTES, "private memory payload too large");
    let store: Store = serde_json::from_str(data).context("invalid private memory payload")?;
    store.validate(identity)?;
    Ok(store)
}

pub(crate) async fn call(args: &Value) -> Result<Value> {
    // Read projected credentials afresh: expiration/rotation never selects a new backend.
    let bytes = tokio::fs::read(CONFIG)
        .await
        .context("built-in memory is not provisioned; no backend fallback was attempted")?;
    let config: Configuration =
        serde_json::from_slice(&bytes).context("invalid built-in memory credential projection")?;
    config.identity.validate()?;
    ensure!(
        config.expires_at > chrono::Utc::now().timestamp() + 5
            && !config.token.is_empty()
            && !config.sandbox_uid.is_empty()
            && !config.map_uid.is_empty(),
        "built-in memory credential expired or invalid"
    );
    let mut url =
        reqwest::Url::parse(&config.api_url).context("invalid built-in memory API URL")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "invalid built-in memory API origin"
    );
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("invalid API origin"))?
        .extend([
            "api",
            "v1",
            "namespaces",
            &config.identity.namespace,
            "configmaps",
            &config.identity.map_name(),
        ]);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .add_root_certificate(
            reqwest::Certificate::from_pem(config.ca.as_bytes())
                .context("invalid Kubernetes CA")?,
        )
        .build()?;
    tokio::time::timeout(Duration::from_secs(25), execute(args, &config, &client, url))
        .await.context("built-in memory deadline exceeded; write outcome may be unknown, read back before retrying")?
}

async fn execute(
    args: &Value,
    config: &Configuration,
    client: &reqwest::Client,
    url: reqwest::Url,
) -> Result<Value> {
    let scope = config.identity.scope();
    match args.get("scope") {
        None | Some(Value::Null) => {}
        Some(Value::String(requested)) if requested == &scope => {}
        _ => anyhow::bail!(
            "built-in memory permits only the exact private owner/role scope; no shared grant exists"
        ),
    }
    let operation = args["operation"]
        .as_str()
        .context("memory operation missing")?;
    ensure!(
        matches!(operation, "search" | "update" | "delete_scope"),
        "invalid memory operation"
    );
    let text = args
        .get("text")
        .or_else(|| args.get("query"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    ensure!(text.len() <= 16384, "memory text exceeds 16384 bytes");
    if operation == "update" {
        ensure!(!text.is_empty(), "memory update requires nonempty text");
        ensure!(
            matches!(
                args.get("role").and_then(Value::as_str).unwrap_or("user"),
                "user" | "assistant"
            ),
            "invalid memory role"
        );
    }
    let limit = args
        .get("top_k")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .min(512) as usize;
    ensure!(limit > 0, "top_k must be positive");
    for _ in 0..4 {
        let response = client
            .get(url.clone())
            .bearer_auth(&config.token)
            .send()
            .await
            .context("built-in memory API transport failure")?;
        ensure!(
            response.status().is_success(),
            "built-in memory read returned HTTP {}; existing backend retained",
            response.status().as_u16()
        );
        let mut map: Value = response
            .json()
            .await
            .context("invalid built-in memory API response")?;
        let mut store = check_map(&map, config)?;
        if operation == "search" {
            let memories: Vec<Value> = store.search(text, limit).into_iter().map(|r| json!({"memory_id":r.memory_id,"memory":r.text,"text":r.text,"updated_at":r.updated_at})).collect();
            return Ok(
                json!({"backend":"builtin","retrieval":"lexical","scope":scope,"memories":memories}),
            );
        }
        let id = if operation == "update" {
            Some(store.update(text, &config.sandbox_uid, &chrono::Utc::now().to_rfc3339())?)
        } else {
            store.records.clear();
            None
        };
        map["data"]["memory.json"] = serde_json::to_string(&store)?.into();
        let response = client
            .put(url.clone())
            .bearer_auth(&config.token)
            .json(&map)
            .send()
            .await
            .context("built-in memory write outcome is unknown; read back before retrying")?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            continue;
        }
        ensure!(
            response.status().is_success(),
            "built-in memory write returned HTTP {}; existing backend retained",
            response.status().as_u16()
        );
        let persisted: Value = response.json().await.context(
            "built-in memory write acknowledgement is invalid; read back before retrying",
        )?;
        ensure!(
            check_map(&persisted, config)? == store,
            "built-in memory persistence was not acknowledged"
        );
        return Ok(
            json!({"backend":"builtin","scope":scope,"status":"completed","memory_id":id,"update_id":id,"approved_knowledge":false}),
        );
    }
    anyhow::bail!("built-in memory concurrent writes exceeded retry bound; no write acknowledged")
}

#[cfg(test)]
#[path = "builtin_memory_client_tests.rs"]
mod tests;
