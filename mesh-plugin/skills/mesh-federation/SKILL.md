---
name: mesh-federation
description: Pair with an AzureClaw cluster and offload tasks to governed cloud sandboxes via E2E encrypted AgentMesh. Enables any OpenClaw agent to leverage Azure GPU/inference without installing AzureClaw.
metadata: {"openclaw": {"always": true}}
---

# AzureClaw Mesh Federation — Cloud Offload & Inter-Agent Mesh

You have the AzureClaw Mesh plugin installed. This gives you secure access to an AzureClaw cluster for cloud offload, inter-agent communication, and agent discovery — all via E2E encrypted AgentMesh.

## Available tools

- **mesh_pair** — One-time pairing with an AzureClaw cluster using an admin-provided token
- **cloud_offload** — Delegate a task to a governed cloud sandbox (GPU, inference, Content Safety)
- **offload_status** — Check progress of an active cloud offload
- **mesh_send** — Send an E2E encrypted message to another agent on the mesh
- **mesh_inbox** — Read incoming messages from the encrypted mesh inbox
- **discover** — Find agents on the mesh by capability or name

## When to use

- **Cloud offload**: You need GPU compute, large model inference, or Azure AI services you don't have locally
- **Heavy tasks**: Code analysis, security audits, data processing that would be slow on your machine
- **Collaboration**: Communicate with other agents (AzureClaw-hosted or external) via the mesh
- **Discovery**: Find specialist agents available on the mesh (e.g., "security-auditor", "code-reviewer")

## First-time setup (pairing)

Before using any tool, you must pair with an AzureClaw cluster. The cluster admin gives you a pairing token:

```
mesh_pair(token: "azcp_1_eyJ...")
```

Pairing is one-time. After pairing, your identity is stored at `~/.azureclaw/identity.json` and the pairing at `~/.azureclaw/pairings.json`. All subsequent tool calls use the stored credentials automatically.

## Cloud offload workflow

1. **cloud_offload** with a task description (and optional files, model, timeout)
2. **offload_status** to monitor progress — watch for phase transitions: `validating` → `spawning` → `running` → `done`
3. When phase is `done`, the result summary and any output files are returned

### Example

```
cloud_offload(task: "Analyze this codebase for OWASP Top 10 vulnerabilities. Focus on SQL injection and XSS. Return a markdown report.", model: "gpt-4.1", timeout_minutes: 15)
```

Then poll:
```
offload_status()
```

## Inter-agent messaging

Send messages to any agent on the mesh. Messages are E2E encrypted with forward secrecy.

```
mesh_send(to: "security-auditor", message: "Please review PR #42 for auth bypass vulnerabilities")
mesh_inbox(limit: 5)
```

## Important notes

- **Pairing required first**: All tools except `mesh_pair` require an active pairing
- **Token budget**: Your pairing has a token budget (e.g., 500K tokens). `offload_status` shows remaining budget.
- **One offload at a time**: You can only have one active cloud offload. Wait for it to finish or time out.
- **Task prompt only**: `cloud_offload` sends a text task description. File transfer is not yet supported — describe what you need in the prompt.
- **Timeout**: Default 30 minutes. The sandbox auto-terminates after timeout.
- **No secrets in tasks**: Never include API keys, passwords, or tokens in task descriptions — the sandbox has its own managed identity.

## Error handling

| Error | Meaning | Action |
|-------|---------|--------|
| "Not paired" | No active pairing | Use `mesh_pair` with a token from your cluster admin |
| "Pairing expired" | Token TTL exceeded | Ask admin for a new pairing token |
| "Budget exceeded" | Token budget used up | Ask admin to increase budget or create new pairing |
| "No available slots" | All sandbox slots in use | Wait for current offload to finish |
| "Connection lost" | Relay connection dropped | Plugin auto-reconnects; retry in a few seconds |
