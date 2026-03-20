---
name: foundry-web-search
description: Real-time web search via Azure AI Foundry. Get current information with citations — no egress policy needed.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_AGENT_ID"]}, "primaryEnv": "FOUNDRY_AGENT_ID"}}
---

# Foundry Web Search — Real-Time Web Grounding

You have access to real-time web search via Azure AI Foundry. Use this to answer questions about current events, look up documentation, or verify facts — all with inline citations.

## Why use this instead of curl

Your sandbox has restricted network egress (default-deny). Foundry web_search runs server-side — no egress policy exceptions needed. Results come with proper citations. No risk of the agent accessing malicious URLs.

## Endpoints

All requests go through `http://localhost:8443`. Auth is automatic. Agent ID: `$FOUNDRY_AGENT_ID`.

## Operations

### Search the web via a run

```bash
# Create a thread
THREAD_ID=$(curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads \
  -H "Content-Type: application/json" -d '{}' | jq -r .id)

# Add the question
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/messages \
  -H "Content-Type: application/json" \
  -d '{"role": "user", "content": "What are the latest Azure AI Foundry announcements?"}'

# Run with web_search
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/runs \
  -H "Content-Type: application/json" \
  -d '{"tools": [{"type": "web_search"}]}'
```

The response includes the answer with inline citations linking to source URLs.

## When to use

- User asks about current events: "what's the latest on X"
- User needs real-time data: stock prices, weather, news
- You need to verify a fact or look up documentation
- User says "search the web", "look up", "what's happening with"

## When NOT to use

- For information in uploaded documents (use foundry-knowledge)
- For calculations or data analysis (use foundry-code)
- For recalling previous conversations (use foundry-memory)

## Citations

Web search results include citations. Always include them in your response so the user can verify sources. Format: [Source Title](url).
