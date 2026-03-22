---
name: azureclaw-spawn
description: Spawn, manage, and communicate with isolated sub-agent sandboxes using AzureClaw plugin slash commands. Each sub-agent gets its own K8s namespace with full security controls and AGT mesh communication.
metadata: {"openclaw": {"requires": {"env": ["SANDBOX_NAME"]}, "primaryEnv": "SANDBOX_NAME"}}
---

# AzureClaw Sub-Agent Spawn

Use the `/azureclaw-spawn` family of slash commands to create, monitor, and
tear down isolated sub-agent sandboxes. Each sub-agent gets its own:
- Isolated K8s namespace with NetworkPolicy
- Inference router sidecar (IMDS auth, Content Safety, token budgets)
- Domain blocklist (auto-refreshing threat intelligence)
- AGT governance (trust-gated mesh communication)

## When to spawn a sub-agent

- **Delegation**: spawn a specialist for a focused task
- **Parallel work**: run multiple agents on different tasks
- **Isolation**: give untrusted code its own sandbox

## Spawn a sub-agent

```
/azureclaw-spawn my-scout --model gpt-4.1 --governance
```

Options:
- `--model <name>` — model deployment (default: gpt-4.1)
- `--governance` — enable AGT governance + mesh communication
- `--trust-threshold <n>` — AGT trust threshold (default: 500)
- `--learn-egress` — enable egress learn mode
- `--token-budget-daily <n>` — daily token limit

## Check status

```
/azureclaw-spawn-status my-scout
```

Wait until phase is "Running" before sending mesh messages.

## List your sub-agents

```
/azureclaw-spawn-list
```

## Send a task via AGT mesh

Once the sub-agent is Running, send it a task through the AGT mesh:

```bash
curl -s -X POST http://localhost:8443/agt/mesh/send \
  -H 'Content-Type: application/json' \
  -d '{"to_agent":"my-scout","content":"Your task here","type":"task_request"}'
```

## Check your inbox for responses

```bash
curl -s http://localhost:8443/agt/mesh/inbox
```

## Destroy when done

```
/azureclaw-spawn-destroy my-scout
```

## Complete workflow

1. `/azureclaw-spawn analyst --model gpt-4.1 --governance`
2. `/azureclaw-spawn-status analyst` — wait for Running
3. Send task via AGT mesh (curl to localhost:8443/agt/mesh/send)
4. Check inbox (curl to localhost:8443/agt/mesh/inbox)
5. `/azureclaw-spawn-destroy analyst`

## Important notes

- Each sub-agent runs in its **own isolated namespace** — it cannot see your files
- AGT mesh is the only communication channel (trust-gated, audited)
- Each sub-agent has its own token budget, blocklist, and Content Safety
- Names must be DNS-safe: lowercase alphanumeric + hyphens, 1-63 chars
- Only you (the parent) can destroy sub-agents you spawned
