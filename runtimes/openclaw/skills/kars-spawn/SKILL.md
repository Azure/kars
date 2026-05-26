---
name: kars-spawn
description: Spawn secure isolated sub-agent sandboxes, delegate tasks via AGT mesh, receive results, and destroy sub-agents. Uses the kars_spawn, kars_mesh_send, kars_mesh_inbox, and kars_spawn_destroy tools.
metadata: {"openclaw": {"always": true}}
---

# Kars Sub-Agent Spawn

Spawn secure isolated sub-agent sandboxes on AKS. Each sub-agent runs in its own K8s namespace with full security controls (NetworkPolicy, seccomp, read-only rootfs). Use the Kars tools for ALL operations — do NOT use curl.

## Available tools

- **kars_spawn** — create a new sub-agent ({name, model, governance})
- **kars_spawn_status** — check sub-agent phase ({name})
- **kars_spawn_list** — list all spawned sub-agents
- **kars_mesh_send** — send a task via AGT mesh ({to_agent, content})
- **kars_mesh_inbox** — check inbox for sub-agent responses
- **kars_spawn_destroy** — tear down a sub-agent ({name})

## When to use

- **Delegation**: spawn a specialist sub-agent for a focused task (security audit, code review, data analysis)
- **Parallel work**: run multiple sub-agents on different tasks simultaneously
- **Isolation**: give untrusted code its own sandboxed environment

## Workflow

1. **kars_spawn** with `name`, `model` (default: parent's), `governance: true`, and **`role`** (the persona — e.g. `"data analyst"`, `"visualization engineer"`, `"technical writer"`). The platform tracks the role in a Peer roster and prepends it to every mesh_send so siblings can resolve role references to canonical names.
2. **kars_spawn_status** — poll every 5s until phase is "Running"
3. **kars_mesh_send** — send the task to the sub-agent
4. Wait 30-60 seconds for the sub-agent to process and auto-reply
5. **kars_mesh_inbox** — read the sub-agent's response
6. **kars_spawn_destroy** — clean up when done

### Multi-agent peer roster (CRITICAL for pipelines)

When you spawn multiple sub-agents that will hand work to each other (e.g. `analyst → viz → writer`), always pass a `role` to every spawn. The platform automatically prepends a `Peer roster:` block to every mesh_send:

```
Peer roster (use these EXACT agent names with mesh_send / mesh_transfer_file ...):
  - analyst — data analyst
  - viz — visualization engineer
  - writer — technical writer
```

This eliminates LLM guessing when sub-agents reference each other by role ("hand to the writer"). Without `role`, the roster lists names only and your sub-agents may misroute artifacts.

## File sharing between agents — CRITICAL

Sub-agents are **completely isolated containers** with separate filesystems.
They CANNOT read each other's files or yours. The AGT mesh is the ONLY channel.

**To share files between agents, you MUST pass file contents inside mesh messages.**

### Multi-agent collaboration pattern (e.g. Red/Blue/Judge):

```
1. Spawn blue-team, red-team, judge
2. mesh_send to blue-team: "Write a proposal. Return the FULL TEXT in your reply."
3. Read blue-team's reply → extract the proposal text
4. mesh_send to red-team: "Here is the proposal:\n\n<paste full text>\n\nFind 3 flaws."
5. Read red-team's reply → extract the critique text
6. mesh_send to blue-team: "Here is the critique:\n\n<paste full text>\n\nRevise your proposal."
7. Read blue-team's reply → extract revised proposal
8. mesh_send to judge: "Proposal:\n\n<paste>\n\nCritique:\n\n<paste>\n\nRevisions:\n\n<paste>\n\nScore it."
9. Read judge's verdict
10. Destroy all sub-agents
```

### Key rules:
- **Always ask sub-agents to return file contents in their reply** — not just "save to file"
- **You (the parent) are the relay** — extract text from each reply and forward to the next agent
- **Never tell a sub-agent to read a file another agent wrote** — the file doesn't exist in its container
- Sub-agents CAN write files locally for their own use, but other agents can't see them

## Important notes

- Each sub-agent runs in its own isolated namespace — it cannot see your files or other agents' files
- AGT mesh is the only communication channel (E2E encrypted, trust-gated, audited)
- Each sub-agent has its own token budget, blocklist, and Content Safety
- Names must be DNS-safe: lowercase alphanumeric + hyphens, 1-63 chars
- Only you (the parent) can destroy sub-agents you spawned
