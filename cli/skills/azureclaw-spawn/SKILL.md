---
name: azureclaw-spawn
description: Spawn new AzureClaw sandboxed sub-agents using the azureclaw CLI. Each sub-agent gets its own isolated sandbox with security controls and optional AGT mesh communication.
metadata: {"openclaw": {"requires": {"env": ["SANDBOX_NAME"]}, "primaryEnv": "SANDBOX_NAME"}}
---

# AzureClaw Spawn — Create Sub-Agents

You can spawn new sandboxed agents using the `azureclaw` CLI. Each spawned agent gets its own:
- Isolated namespace with NetworkPolicy
- Inference router sidecar (IMDS auth, Content Safety, token budgets)
- Domain blocklist (auto-refreshing threat intelligence)
- Optional AGT governance (trust-gated mesh communication back to you)

**Important**: You do NOT have kubectl or K8s access. Always use the `azureclaw` CLI.

## When to use this

- **Delegation**: spawn a specialist agent for a specific task
- **Parallel work**: run multiple agents simultaneously on different tasks
- **Isolation**: give untrusted code its own sandbox with tighter controls

## How to spawn

### Basic spawn

```bash
azureclaw add sub-analyst --model gpt-4.1
```

### Spawn with AGT governance (enables mesh communication with you)

```bash
azureclaw add sub-analyst --model gpt-4.1 --governance --trust-threshold 500
```

### Spawn with learn mode (discover needed domains, then lock down)

```bash
azureclaw add sub-analyst --model gpt-4.1 --learn-egress
```

### Spawn with a different model

```bash
azureclaw add sub-coder --model DeepSeek-V3.2
```

### Spawn with token budget limits

```bash
azureclaw add sub-analyst --model gpt-4.1 --token-budget-daily 100000 --token-budget-per-request 4096
```

### Dry run (preview the CRD without creating)

```bash
azureclaw add sub-analyst --model gpt-4.1 --governance --dry-run
```

## Check status

```bash
azureclaw status sub-analyst
```

## Communicate with spawned agent (AGT mesh)

If governance is enabled on both you and the sub-agent, use the AGT mesh:

```bash
# Send a task to the sub-agent
curl -s -X POST http://localhost:8443/agt/mesh/send \
  -H 'Content-Type: application/json' \
  -d '{"to_agent": "sub-analyst", "content": "Analyze the security posture of our cluster", "type": "task_request"}'

# Check for responses
curl -s http://localhost:8443/agt/mesh/inbox
```

## Connect to the spawned agent (interactive)

```bash
azureclaw connect sub-analyst
```

## Tear down

```bash
azureclaw destroy sub-analyst
```

## Important notes

- The sub-agent runs in its **own isolated namespace** — it cannot see your files or memory
- AGT mesh is the only way to communicate between agents (trust-gated, audited)
- Each sub-agent has its own token budget, blocklist, and Content Safety
- The sandbox name must be unique across the cluster
- Memory Store scopes are per-agent — each agent uses its own `SANDBOX_NAME` as scope
- You do NOT have kubectl access — always use the `azureclaw` CLI

## Tear down spawned agent

```bash
kubectl delete clawsandbox sub-analyst -n azureclaw-system
```

## Important notes

- The sub-agent runs in its **own isolated namespace** — it cannot see your files or memory
- AGT mesh is the only way to communicate between agents (trust-gated, audited)
- Each sub-agent has its own token budget, blocklist, and Content Safety
- The sandbox name must be unique across the cluster
- Memory Store scopes are per-agent — use the agent's `SANDBOX_NAME` env var as scope
