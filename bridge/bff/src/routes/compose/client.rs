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
/// Two reachable paths, in priority order:
///   1. **Ops override** — an explicit `BRIDGE_ORCHESTRATOR_{ENDPOINT,TOKEN,MODEL}`
///      triple (a dedicated composer endpoint the operator configured).
///   2. **Native** — route through a Running sandbox's inference router via the
///      `pods/proxy` subresource. The router injects the provider's auth +
///      integration headers (Copilot/Foundry) and enforces governance, so this
///      works on workload-identity clusters with NO static token in the Bridge —
///      reusing exactly the secure path agents use.
pub(super) async fn orchestrator_complete(
    cluster: &crate::kars::cluster::Cluster,
    system: &str,
    user: &str,
    default_model: &str,
    max_tokens: u32,
) -> anyhow::Result<(String, String)> {
    // 1. Ops override — direct endpoint/token/model.
    if let (Ok(endpoint), Ok(token), Ok(model)) = (
        std::env::var("BRIDGE_ORCHESTRATOR_ENDPOINT"),
        std::env::var("BRIDGE_ORCHESTRATOR_TOKEN"),
        std::env::var("BRIDGE_ORCHESTRATOR_MODEL"),
    ) && !endpoint.trim().is_empty()
        && !token.trim().is_empty()
        && !model.trim().is_empty()
    {
        let raw = call_llm(&endpoint, &token, &model, system, user, max_tokens).await?;
        return Ok((raw, model));
    }

    // 2. Native — through a running sandbox's secure inference router. Try each
    //    stable candidate in turn so a sandbox with stale provider auth or a
    //    warming router is skipped rather than failing the whole compose.
    //    Claude models use the native Anthropic `/v1/messages` path (the
    //    OpenAI-compat path returns empty content for Claude).
    let candidates = cluster.running_sandbox_candidates().await;
    if candidates.is_empty() {
        anyhow::bail!(
            "orchestrator has no inference path yet — the standing `bridge-orchestrator` sandbox is still starting (retry shortly), or set BRIDGE_ORCHESTRATOR_{{ENDPOINT,TOKEN,MODEL}} to route directly at Azure AI Foundry / Azure OpenAI (scales better for many teams)"
        );
    }
    let is_claude = default_model.to_ascii_lowercase().contains("claude");
    let mut last_err = String::from("no candidate router returned content");
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
                return Ok((content, format!("{default_model} (cluster router)")));
            }
            Ok(_) => last_err = "router returned empty content".into(),
            Err(e) => last_err = e.to_string(),
        }
    }
    anyhow::bail!("{last_err}")
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

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": max_tokens,
    });
    let text = cluster.router_chat(ns, pod, &body).await?;
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("router returned non-JSON: {e}"))?;
    Ok(parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string())
}

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
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": max_tokens,
    });
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
    let content = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("no completion content"))?;
    Ok(content)
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
