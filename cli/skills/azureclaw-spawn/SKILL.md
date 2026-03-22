---
name: azureclaw-spawn
description: Spawn secure isolated sub-agent sandboxes via the AzureClaw router API (localhost:8443). Use curl to spawn, send tasks via AGT mesh, receive replies, and destroy sub-agents.
metadata: {"openclaw": {"always": true}}
---

# AzureClaw Sub-Agent Spawn

Spawn secure isolated sub-agent sandboxes via the AzureClaw router API at localhost:8443. Each sub-agent runs in its own K8s namespace with full security controls. Use bash with curl for ALL operations.

## When to use

- **Delegation**: spawn a specialist for a focused task (e.g. security audit, code review)
- **Parallel work**: run multiple agents on different tasks simultaneously
- **Isolation**: give untrusted code its own sandboxed environment

## Step 1: Spawn a sub-agent

```bash
curl -s -X POST http://localhost:8443/sandbox/spawn \
  -H 'Content-Type: application/json' \
  -d '{"name":"my-worker","model":"gpt-4.1","governance":true,"trust_threshold":500}'
```

## Step 2: Wait for Running

Poll every 5 seconds until phase is "Running":

```bash
curl -s http://localhost:8443/sandbox/my-worker/status
```

## Step 3: Send a task via AGT mesh

```bash
curl -s -X POST http://localhost:8443/agt/mesh/send \
  -H 'Content-Type: application/json' \
  -d '{"to_agent":"my-worker","content":"Your task description here","type":"task_request"}'
```

Wait 15 seconds for DNS propagation before sending. The sub-agent will auto-process the task via its local AI model and send the result back.

## Step 4: Check inbox for the sub-agent response

Wait 30-60 seconds for the sub-agent to process and reply, then:

```bash
curl -s http://localhost:8443/agt/mesh/inbox
```

The response will contain the sub-agent's `task_response` with the result.

## Step 5: Destroy when done

```bash
curl -s -X DELETE http://localhost:8443/sandbox/my-worker
```

## Important notes

- Each sub-agent runs in its **own isolated namespace** — it cannot see your files
- AGT mesh is the only communication channel (trust-gated, audited)
- Each sub-agent has its own token budget, blocklist, and Content Safety
- Names must be DNS-safe: lowercase alphanumeric + hyphens, 1-63 chars
- Only you (the parent) can destroy sub-agents you spawned
