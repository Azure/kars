// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/// Find and parse the first top-level JSON object in a model response (it may be
/// fenced or prefixed with prose). Returns `None` when there's no parseable object.
pub(super) fn extract_json_object(raw: &str) -> Option<serde_json::Value> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&raw[start..=end]).ok()
}

/// Complete the orchestrator prompt, returning `(raw_model_output, source)`.
///
/// When finite funding is unconfigured, two legacy paths, in priority order:
///   1. **Ops override** — an explicit `BRIDGE_ORCHESTRATOR_{ENDPOINT,TOKEN,MODEL}`
///      triple (a dedicated composer endpoint the operator configured).
///   2. **Native** — route through the standing composer's inference router via
///      the `pods/proxy` subresource. The router injects the provider's auth +
///      integration headers (Copilot/Foundry) and enforces governance, so this
///      works on workload-identity clusters with NO static token in the Bridge —
///      reusing exactly the secure path agents use.
/// Finite funding selects only the verified standing actor router before these
/// legacy paths; any refusal on that route is terminal, never a fallback signal.
pub(super) async fn orchestrator_complete(
    cluster: &crate::kars::cluster::Cluster,
    system: &str,
    user: &str,
    default_model: &str,
    max_tokens: u32,
) -> anyhow::Result<(String, String)> {
    let funded_route = cluster.funded_orchestrator_route().await?;
    // Explicit legacy endpoints are independent of the standing policy, but
    // cannot bypass a funded actor or an unresolved account pin.
    if funded_route.is_none()
        && let (Ok(endpoint), Ok(token), Ok(model)) = (
            std::env::var("BRIDGE_ORCHESTRATOR_ENDPOINT"),
            std::env::var("BRIDGE_ORCHESTRATOR_TOKEN"),
            std::env::var("BRIDGE_ORCHESTRATOR_MODEL"),
        )
        && !endpoint.trim().is_empty()
        && !token.trim().is_empty()
        && !model.trim().is_empty()
    {
        let raw = call_llm(&endpoint, &token, &model, system, user, max_tokens).await?;
        return Ok((raw, model));
    }
    let selected = cluster.bridge_orchestrator_model_route().await?;
    if let Some(model) = &selected {
        cluster.wait_for_bridge_orchestrator_model(&model.provider, &model.deployment)
            .await.map_err(anyhow::Error::msg)?;
    }
    let default_model = selected.as_ref().map(|model| model.deployment.as_str()).unwrap_or(default_model);
    if let Some((namespace, pod)) = funded_route {
        let content = orchestrator_via_router(
            cluster,
            &namespace,
            &pod,
            default_model,
            OrchestratorPrompt { system, user },
            default_model.to_ascii_lowercase().contains("claude"),
            max_tokens,
        )
        .await?;
        if content.trim().is_empty() {
            anyhow::bail!(
                "The funded orchestrator router returned no content; fallback is forbidden"
            );
        }
        return Ok((
            content,
            format!("{default_model} (funded orchestrator router)"),
        ));
    }
    // 2. Native — only through the standing composer's ready routers. Never
    //    borrow a customer's account or governance when the actor is unavailable.
    //    Claude models use the native Anthropic `/v1/messages` path (the
    //    OpenAI-compat path returns empty content for Claude).
    let candidates = cluster.running_sandbox_candidates().await?;
    if candidates.is_empty() {
        anyhow::bail!(
            "orchestrator has no inference path yet — the standing `bridge-orchestrator` sandbox is still starting (retry shortly), or set BRIDGE_ORCHESTRATOR_{{ENDPOINT,TOKEN,MODEL}} to route directly at Azure AI Foundry / Azure OpenAI (scales better for many teams)"
        );
    }
    let is_claude = default_model.to_ascii_lowercase().contains("claude");
    let mut failures = Vec::new();
    for (ns, pod) in candidates.iter().take(4) {
        match orchestrator_via_router(
            cluster,
            ns,
            pod,
            default_model,
            OrchestratorPrompt { system, user },
            is_claude,
            max_tokens,
        )
        .await
        {
            Ok(content) if !content.trim().is_empty() => {
                return Ok((content, format!("{default_model} (bridge-orchestrator router)")));
            }
            Ok(_) => failures.push(format!("{ns}/{pod}: router returned empty content")),
            Err(e) => failures.push(format!("{ns}/{pod}: {e}")),
        }
    }
    anyhow::bail!("No candidate router completed the request: {}", failures.join("; "))
}

struct OrchestratorPrompt<'a> {
    system: &'a str,
    user: &'a str,
}

/// Single orchestrator completion against one sandbox router (Anthropic
/// `/v1/messages` for Claude, OpenAI `/chat/completions` otherwise).
async fn orchestrator_via_router(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    pod: &str,
    model: &str,
    prompt: OrchestratorPrompt<'_>,
    is_claude: bool,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let OrchestratorPrompt { system, user } = prompt;
    if is_claude {
        let body = serde_json::json!({
            "model": model,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
            "max_tokens": max_tokens,
        });
        let text = cluster.router_messages(ns, pod, &body).await?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("router returned non-JSON: {e}"))?;
        if parsed.get("stop_reason").and_then(|value| value.as_str()) == Some("max_tokens") {
            anyhow::bail!("Orchestrator response was truncated at the requested {max_tokens}-token output limit; no complete package was received");
        }
        let content: String = parsed
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        return Ok(content);
    }

    let body = chat_completion_body(model, system, user, max_tokens);
    let text = cluster.router_chat(ns, pod, &body).await?;
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("router returned non-JSON: {e}"))?;
    chat_completion_content(&parsed, max_tokens)
}

pub(super) fn repair_failure(review_error: &str, error: &anyhow::Error) -> String {
    format!(
        "The orchestrator repair call failed ({error}); no corrected package was received. \
         Original proposal diagnostics: {review_error}. No funding or contracts were changed."
    )
}

fn chat_completion_content(parsed: &serde_json::Value, max_tokens: u32) -> anyhow::Result<String> {
    let choice = &parsed["choices"][0];
    match choice["finish_reason"].as_str() {
        Some("length") => anyhow::bail!(
            "Orchestrator response was truncated at the requested {max_tokens}-token output limit (including reasoning); no complete package was received"
        ),
        Some("content_filter") => anyhow::bail!("Orchestrator response was blocked by the provider content filter"),
        _ => {}
    }
    choice["message"]["content"]
        .as_str()
        .filter(|content| !content.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("no completion content"))
}

fn chat_completion_body(
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> serde_json::Value {
    // The router may select a reasoning deployment regardless of the requested
    // alias. The current Chat Completions limit bounds reasoning + visible output.
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_completion_tokens": max_tokens,
        "response_format": {"type": "json_object"},
    })
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

/// Call the orchestrator LLM (OpenAI-compatible chat/completions) and return
/// the assistant's text content.
async fn call_llm(
    endpoint: &str,
    token: &str,
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
    let body = chat_completion_body(model, system, user, max_tokens);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()?;
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "HTTP {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }
    let parsed: serde_json::Value = serde_json::from_str(&text)?;
    chat_completion_content(&parsed, max_tokens)
}

/// Extract the first balanced JSON object from a string, tolerating code fences
/// and leading/trailing prose that some models add despite instructions.
pub(super) fn extract_json(raw: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw.trim()) {
        return Some(v);
    }
    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&raw[start..=i]).ok();
                }
            }
            _ => {}
        }
    }
    None
}
