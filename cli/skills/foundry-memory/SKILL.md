---
name: foundry-memory
description: Persistent long-term memory via Foundry Memory Store APIs. User preferences and chat summaries survive pod restarts — no Foundry hosted agent needed.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Memory — Memory Store APIs (Direct)

You have access to persistent long-term memory via Foundry Memory Store APIs. Memory survives pod restarts, upgrades, and session boundaries. This uses direct APIs — no Foundry hosted agent needed.

## Why use this instead of local files

Local files in `/sandbox/` are lost when the pod restarts. Foundry Memory Store is managed and supports:
- **User profile memory**: preferences, name, restrictions (retrieve once per session)
- **Chat summary memory**: distilled summaries of past conversations (cross-session continuity)

The system automatically extracts, consolidates, and deduplicates memories.

## Endpoints

All requests go through `http://localhost:8443`. Auth is automatic (IMDS).

## Operations

### Create a memory store

```bash
curl -s -X POST "http://localhost:8443/memory-stores?api-version=v1" \
  -H "Content-Type: application/json" \
  -d '{"name":"agent-memory","user_profile_details":["name","preferences","expertise"],"chat_summary_enabled":true}'
```

### Write a memory

```bash
curl -s -X POST "http://localhost:8443/memory-stores/agent-memory/memories?api-version=v1" \
  -H "Content-Type: application/json" \
  -d '{"scope":"user-123","content":"User prefers concise responses. Expert in security.","type":"user_profile"}'
```

### Search memories

```bash
curl -s -X POST "http://localhost:8443/memory-stores/agent-memory/memories/search?api-version=v1" \
  -H "Content-Type: application/json" \
  -d '{"scope":"user-123","query":"user preferences"}'
```

### List memories

```bash
curl -s "http://localhost:8443/memory-stores/agent-memory/memories?scope=user-123&api-version=v1"
```

## When to use

- User says "remember this", "save this for later"
- You need to persist user preferences across sessions
- You want conversation summaries that survive pod restarts
- User asks "what do you know about me"

## When NOT to use

- For searching documents (use foundry-knowledge skill)
- For temporary scratch data (use local files)
- For real-time web info (use foundry-web-search skill)
