---
name: foundry-memory
description: Persistent conversation memory via Azure AI Foundry threads. Memory survives pod restarts.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_AGENT_ID"]}, "primaryEnv": "FOUNDRY_AGENT_ID"}}
---

# Foundry Memory — Persistent Threads

You have access to persistent memory via Azure AI Foundry threads. Use this when the user asks you to remember something, recall previous conversations, or when you need context that should survive across sessions.

## Why use this instead of local files

Local files in `/sandbox/` are lost when the pod restarts. Foundry threads persist server-side — your memory survives restarts, upgrades, and redeployments.

## Endpoints

All requests go through the AzureClaw inference router at `http://localhost:8443`. Authentication is handled automatically (IMDS). You never need API keys.

The Foundry Agent ID is available as `$FOUNDRY_AGENT_ID`.

## Operations

### Create a new thread (start a memory session)

```bash
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads \
  -H "Content-Type: application/json" \
  -d '{}'
```

Returns `{"id": "thread_abc123", ...}`. Save the thread ID for subsequent messages.

### Add a message to a thread (store memory)

```bash
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/messages \
  -H "Content-Type: application/json" \
  -d '{"role": "user", "content": "Remember: the project deadline is March 30."}'
```

### List messages in a thread (recall memory)

```bash
curl -s http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/messages
```

### List all threads (browse memory sessions)

```bash
curl -s http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads
```

## When to use

- User says "remember this", "save this for later", "don't forget"
- User asks "what did we discuss about X", "what was the deadline"
- You need to persist context across sessions (e.g., project state, preferences)
- You want to maintain a running log of decisions or findings

## When NOT to use

- For temporary scratch data within a single conversation (use local files)
- For storing large files (use the foundry-knowledge skill instead)

## Thread management tips

- Create one thread per topic or project for organization
- Add messages with clear labels: "Project: X, Decision: Y"
- List threads to find the right context before answering recall questions
