---
name: foundry-agents
description: Query and inspect Foundry prompt agents and invoke Foundry tools via the Responses API. OpenClaw is the orchestrator — Foundry provides managed AI services.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Services — Agent Tools via Responses API

OpenClaw is the agent orchestrator. Foundry provides managed AI services that OpenClaw agents can use via the Responses API. **You do NOT create Foundry agents** — you call Foundry tools directly.

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Calling Foundry Tools via Responses API

### Code Interpreter (run Python)

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"Calculate fibonacci(20) using Python","tools":[{"type":"code_interpreter","container":{"type":"auto"}}],"store":false}'
```

### Web Search (Bing grounding)

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"What are the latest Rust language features?","tools":[{"type":"bing_grounding"}],"store":false}'
```

### Memory Search (cross-session recall)

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"What beverage does the user prefer?","tools":[{"type":"memory_search","memory_store_name":"kars-memory","scope":"default"}],"store":false}'
```

## Querying Foundry Infrastructure

### List existing agents in the project

```bash
curl -s 'http://localhost:8443/agents?api-version=2025-11-15-preview'
```

### List model deployments

```bash
curl -s 'http://localhost:8443/deployments?api-version=2025-11-15-preview'
```

## When to use

- Call Foundry tools (code interpreter, web search, memory) via Responses API
- Inspect what Foundry agents or models exist in the project
- Use Foundry as a service layer for OpenClaw agent capabilities

## When NOT to use

- Do NOT create Foundry agents — OpenClaw is the agent orchestrator
- For memory CRUD operations (use foundry-memory skill)
- For simple chat completions (use /v1/chat/completions directly)
