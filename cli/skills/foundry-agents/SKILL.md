---
name: foundry-agents
description: Create and manage Foundry prompt agents via the Agents API. Define agent instructions, model, and tools — no hosted containers needed.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Agents — Prompt Agent Management

You can create, list, update, and delete Foundry prompt agents via the Agents API. Prompt agents define instructions, model selection, and available tools. They can be invoked via the Responses API.

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Operations

### Create a prompt agent

```bash
curl -s -X POST 'http://localhost:8443/agents?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"name":"my-agent","definition":{"kind":"prompt","model":"gpt-4.1","instructions":"You are a helpful assistant","tools":[{"type":"code_interpreter","container":{"type":"auto"}}]}}'
```

### List agents

```bash
curl -s 'http://localhost:8443/agents?api-version=2025-11-15-preview'
```

### Get a specific agent

```bash
curl -s 'http://localhost:8443/agents/my-agent?api-version=2025-11-15-preview'
```

### Invoke an agent via Responses API

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"input":"Hello","agent":{"name":"my-agent","type":"agent_reference"}}'
```

### Delete an agent

```bash
curl -s -X DELETE 'http://localhost:8443/agents/my-agent?api-version=2025-11-15-preview'
```

## When to use

- Create reusable agent configurations with specific tools and instructions
- Invoke agents with consistent behavior across conversations
- Manage agent versions for A/B testing

## When NOT to use

- For one-off queries (just use /v1/chat/completions or /openai/responses directly)
- For memory management (use foundry-memory skill)
