---
name: azureclaw-spawn
description: Spawn, manage, and communicate with sub-agent sandboxes using AzureClaw slash commands. Each sub-agent gets its own isolated K8s namespace with security controls and optional AGT mesh communication.
metadata: {"openclaw": {"requires": {"env": ["SANDBOX_NAME"]}, "primaryEnv": "SANDBOX_NAME"}}
---

# AzureClaw Spawn — Sub-Agent Management

You can spawn new sandboxed sub-agents using the `/azureclaw-spawn` slash command.
Each spawned agent gets its own:
- Isolated K8s namespace with NetworkPolicy
- Inference router sidecar (IMDS auth, Content Safety, token budgets)
- Domain blocklist (auto-refreshing threat intelligence)
- Optional AGT governance (trust-gated mesh communication back to you)

**Important**: You do NOT have kubectl, K8s API, or CLI access inside the sandbox.
All sub-agent management goes through slash commands, which call the inference
router sidecar at `localhost:8443`.

## When to spawn a sub-agent

- **Delegation**: spawn a specialist agent for a specific task
- **Parallel work**: run multiple agents simultaneously on different tasks
- **Isolation**: give untrusted code its own sandbox with tighter controls

## Slash commands

### Spawn a sub-agent

```
/azureclaw-spawn sub-analyst --model gpt-4.1 --governance
```

Options:
- `--model <name>` — model deployment (default: gpt-4.1)
- `--governance` — enable AGT governance + mesh communication
- `--trust-threshold <n>` — AGT trust threshold (default: 500)
- `--learn-egress` — enable egress learn mode
- `--token-budget-daily <n>` — daily token limit
- `--isolation <level>` — standard | enhanced | confidential

### List your sub-agents

```
/azureclaw-spawn-list
```

### Destroy a sub-agent

```
/azureclaw-spawn-destroy sub-analyst
```

## Communication via AGT mesh

If governance is enabled on both you and the sub-agent, use the AGT mesh
endpoints on your inference router:

```
# Send a task to the sub-agent
POST http://localhost:8443/agt/mesh/send
{"to_agent": "sub-analyst", "content": "Analyze the security posture", "type": "task_request"}

# Check for responses
GET http://localhost:8443/agt/mesh/inbox
```

## Examples

### Spawn a research assistant with governance

```
/azureclaw-spawn researcher --model gpt-4.1 --governance --trust-threshold 500
```

### Spawn a coder with a different model and learn mode

```
/azureclaw-spawn coder --model DeepSeek-V3.2 --learn-egress
```

### Spawn with token budget limits

```
/azureclaw-spawn analyst --model gpt-4.1 --token-budget-daily 100000
```

## Important notes

- The sub-agent runs in its **own isolated namespace** — it cannot see your files or memory
- AGT mesh is the only way to communicate between agents (trust-gated, audited)
- Each sub-agent has its own token budget, blocklist, and Content Safety
- The sandbox name must be unique across the cluster (DNS-safe, 1-63 chars)
- Memory Store scopes are per-agent — each agent uses its own `SANDBOX_NAME` as scope
- Only you (the parent) can destroy sub-agents you spawned
