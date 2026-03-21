---
name: foundry-web-search
description: Real-time web search via Azure AI Foundry Responses API with bing_grounding tool. Get current information with citations — no egress policy needed.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Web Search — Bing Grounding (Responses API)

You have access to real-time web search via the Foundry Responses API `bing_grounding` tool. Results come with inline URL citations. No sandbox egress policy exceptions needed — search runs server-side.

**Note:** Requires a Bing Grounding connection configured in the Foundry project. If not available, the model will answer from its training data instead.

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Operations

### Search the web

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"What are the latest Azure AI Foundry announcements?","tools":[{"type":"bing_grounding","bing_grounding":{"search_configurations":[{"project_connection_id":"/connections/bing"}]}}],"store":false}'
```

The response includes the answer with inline URL citations.

### Simple search (no Bing connection needed)

If Bing is not configured, use plain chat — the model answers from training data:

```bash
curl -s -X POST 'http://localhost:8443/v1/chat/completions' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","messages":[{"role":"user","content":"What are the main features of Azure AI Foundry?"}]}'
```

## When to use

- User asks about current events, news, recent changes
- Need real-time data: weather, stock prices, sports scores
- Verifying facts with web sources
- User says "search the web", "look up", "what's the latest"

## When NOT to use

- For information in uploaded documents (use foundry-knowledge)
- For calculations or data analysis (use foundry-code)
- For recalling previous conversations (use foundry-memory)
