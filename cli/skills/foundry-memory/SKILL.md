---
name: foundry-memory
description: Persistent long-term memory via Foundry Memory Store APIs. User preferences and chat summaries survive pod restarts — no Foundry hosted agent needed.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Memory — Memory Store APIs

You have access to persistent long-term memory via Azure AI Foundry Memory Store. Memory survives pod restarts, upgrades, and session boundaries. The system uses embedding models for semantic search and chat models to extract/consolidate memories automatically.

## Why use this instead of local files

Local files in `/sandbox/` are ephemeral. Foundry Memory Store is managed, uses vector search (text-embedding-3-small), and supports:
- **User profile memory**: preferences, name, restrictions
- **Chat summary memory**: distilled summaries of past conversations

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Operations

### Create a memory store

```bash
curl -s -X POST 'http://localhost:8443/memory_stores?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"name":"agent-memory","description":"Agent persistent memory","definition":{"kind":"default","chat_model":"gpt-4.1","embedding_model":"text-embedding-3-small","options":{"user_profile_enabled":true,"chat_summary_enabled":true}}}'
```

### Store memories from conversation

```bash
curl -s -X POST 'http://localhost:8443/memory_stores/agent-memory:update_memories?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"items":[{"role":"user","content":"I prefer dark roast coffee and code in Rust","type":"message"},{"role":"assistant","content":"Noted!","type":"message"}],"scope":"user-123","update_delay":0}'
```

Returns `{"update_id": "...", "status": "queued"}`. The system asynchronously extracts memories using the chat model.

### Search memories (with semantic embedding)

```bash
curl -s -X POST 'http://localhost:8443/memory_stores/agent-memory:search_memories?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"scope":"user-123","items":[{"role":"user","content":"What coffee does the user like?","type":"message"}],"options":{"max_memories":10}}'
```

Returns `memories[]` array with content, kind (user_profile/chat_summary), and usage (embedding_tokens).

### List all memories for scope (no embedding)

```bash
curl -s -X POST 'http://localhost:8443/memory_stores/agent-memory:search_memories?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"scope":"user-123","options":{"max_memories":50}}'
```

### Delete memories for a scope

```bash
curl -s -X POST 'http://localhost:8443/memory_stores/agent-memory:delete_scope?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"scope":"user-123"}'
```

## When to use

- User says "remember this", "save this for later"
- Start of session: search memories for the user to personalize
- After meaningful conversation: store key facts via update_memories
- User asks "what do you know about me"

## When NOT to use

- For searching documents (use foundry-knowledge skill)
- For temporary scratch data (use local files)
- For real-time web info (use foundry-web-search skill)
