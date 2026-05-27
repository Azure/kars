---
name: mesh-federation
description: Pair with an kars cluster and offload heavy tasks to governed cloud sandboxes with GPU / foundation-model inference / Azure AI services, or communicate with other agents over end-to-end encrypted AgentMesh. Triggers on natural-language intents like "offload to the cloud", "run this on Azure", "ask my cluster to…", "send a message to agent X", "who is on the mesh", "check my inbox", "is my offload done".
metadata: {"openclaw": {"always": true}}
---

# kars Mesh Federation — Cloud Offload & Inter-Agent Messaging

You have the **kars Mesh** plugin installed. It federates this OpenClaw agent with an kars cluster and other agents via end-to-end encrypted AgentMesh (Signal Protocol). You can:

1. Delegate heavy/long-running tasks to a governed cloud sandbox that has GPU, Azure AI Foundry models, Content Safety, and AGT governance.
2. Send encrypted messages to any agent on the mesh (another kars sandbox, an external operator, a specialist agent).
3. Discover agents by capability or name.
4. Read your encrypted inbox.

---

## When to pick this skill

Use this skill when the user's intent matches any of:

- **"offload this to the cloud"**, "run this on Azure", "run it on my cluster", "do it in the cloud sandbox", "delegate to the cluster"
- "too heavy for local" / "I need GPU" / "this will take too long here" / "use the big model"
- "pair with the cluster", "connect to kars", "set up federation"
- "send a message to <agent>", "tell <agent> to …", "notify <agent>"
- "check my inbox", "any new messages?", "did <agent> reply?"
- "who's on the mesh?", "find agents that can <X>", "discover <capability>"
- "how's the offload going?", "is the task done?", "status of the job"

Any of the above should prompt you to reach for the tools below instead of attempting the work locally (especially if local tools would be slow, require a large model, or lack the capability entirely).

---

## Tool reference

### `mesh_pair` — one-time pairing
**Signature:** `mesh_pair(token: string)`

**When:** Before any other tool. If the user gives you a string starting with `azcp_1_…`, immediately call `mesh_pair` with it. Pairing state is persisted to `~/.kars/pairings.json` — you only pair once per cluster.

**Returns:** A success block with your AMID, token budget, and relay RTT.

---

### `cloud_offload` — delegate a task to the cluster
**Signature:** `cloud_offload(task: string, files?: string[], model?: string, timeout_minutes?: number)`

**Parameter aliases accepted** (use whichever feels natural; all resolve to `task`): `task | prompt | description | request | content | instruction | query`. For files: `files | file_paths`. For timeout: `timeout_minutes | timeout`.

**Behaviour:**
- Returns immediately (typically in <1 s) with a confirmation and a `request_id`. The actual work happens asynchronously on the cluster.
- The cluster spawns a fresh, isolated sandbox pod for this task. The sandbox gets the task via env vars and **proactively announces itself** to you with an `offload_hello` message the moment it's online.
- After announcing, the sandbox streams progress heartbeats (every ~20 s), output files, and a final summary — all via E2E-encrypted mesh messages. You do **not** need to poll for these; just call `offload_status`.

**When to use:**
- The user asks for deep research, large refactors, security audits, data processing, or anything GPU-heavy.
- Any request that would take more than a minute or two locally.
- Any request that needs a capability you don't have here (e.g., a larger model, Azure Content Safety, Foundry memory store).

**Examples:**
```
cloud_offload(task: "Analyze this codebase for OWASP Top 10 vulns; focus on SQLi and XSS. Return a markdown report.", files: ["src/auth.ts","src/db.ts"], timeout_minutes: 15)
cloud_offload(task: "Research the latest community trends in AI agent sandboxing: GitHub repos, blog posts, open issues. Produce a markdown summary with links.", timeout_minutes: 30)
```

**Limits:**
- 30 MB per file. Total task size is bounded by your paired budget (default 500 K tokens).
- One offload in-flight per pairing. If one is active, finish or cancel it before starting another.

---

### `offload_status` — watch progress
**Signature:** `offload_status()`  *(no arguments)*

**Returns:** A human-readable snapshot of the current offload with the most recent stages. Phases progress through:

| Phase | Icon | Meaning |
|---|---|---|
| `submitted` | 📤 | Request packaged locally |
| `validating` | 🔎 | Cluster checking budget / pairing |
| `spawning` | 🚀 | Controller creating the sandbox CRD |
| `scheduled` | 📅 | Kubernetes scheduling the pod |
| `ready` | 🟢 | Sandbox pod running |
| `acknowledged` | 👋 | Sandbox sent `offload_hello` — task accepted and running |
| `verifying` | 🏓 | (legacy fallback) pinging sandbox over mesh |
| `uploading` | 📦 | Transferring input files |
| `running` | ⚙️ | Task executing; heartbeats arriving every ~20 s |
| `returning` | 📥 | Sandbox streaming output files back |
| `done` | ✅ | Task complete — summary + files ready |
| `error` | ❌ | Something failed — error message shown |

**When:** Call every ~5–15 s while waiting for a task. Stop as soon as phase is `done` or `error`. Do **not** tight-loop — one call per conversational turn is plenty.

---

### `mesh_send` — send an encrypted message to another agent
**Signature:** `mesh_send(to: string, message: string)`

**Parameter aliases:** `to | to_amid | to_agent | target | amid | recipient` for the recipient; `message | content | body | text` for the body. You can pass a display name (e.g. `"offload-parent"`) or a raw AMID (base58, 20+ chars) — display names are resolved via the registry.

**When:**
- Relaying a message to another agent ("ask the security-auditor to re-check PR #42").
- Notifying an offload sandbox you spawned (e.g., "stop, I got the answer locally").
- Any peer-to-peer coordination.

**Example:**
```
mesh_send(to: "offload-parent", message: "I've finished the initial draft — see files in your inbox")
```

---

### `mesh_inbox` — read incoming messages
**Signature:** `mesh_inbox(limit?: number)`

**When:** The user asks "any updates?", "did X reply?", or to check on an offload that you started a while ago. Also use this as the primary way to pick up progress/results from a cloud offload (offload_status is a cleaner formatted view of the same messages).

---

### `discover` — list agents on the mesh
**Signature:** `discover(capability?: string, limit?: number)`

**When:** The user asks who is on the mesh, or you want to find an agent with a specific capability (e.g., `"security-audit"`, `"offload"`, `"assistant"`). Without a capability, `discover` aggregates across well-known seed capabilities (kars-agent, task-execution, cluster-controller, offload, mesh-peer, etc.) and returns a deduped list.

---

## Typical conversation flows

### Pairing then offloading
```
User: here's a pairing token azcp_1_eyJ...
  → mesh_pair(token: "azcp_1_eyJ...")
User: great — now research latest trends in AI agent sandboxing
  → cloud_offload(task: "research latest trends in AI agent sandboxing ...")
  → (brief wait, then) offload_status()
  → Continue calling offload_status each turn until phase=done
  → Report summary + files back to the user
```

### Checking on a running offload
```
User: how's that research going?
  → offload_status()
  → Report current phase + elapsed time + last heartbeat
```

### Sending a peer message
```
User: tell the cluster sandbox I'm done
  → mesh_send(to: "offload-sandbox-name", message: "...")
```

---

## Important operational notes

- **Pairing is pre-req for everything except `mesh_pair` itself.** If a tool returns "Not paired", stop and ask the user for a pairing token.
- **Proactive offload:** Modern kars sandboxes *self-announce* the moment they're ready and begin work immediately. You'll see phase `acknowledged` (👋) within ~20 s of `ready`. If you don't, the cluster may be running an older sandbox image and will fall back to the ping/upload/dispatch flow (still works, just slower).
- **Tokens are metered.** `offload_status` surfaces budget consumption. If an offload fails with "budget exceeded", ask the admin for a new pairing token with a larger budget.
- **No secrets in tasks.** Never put API keys or passwords in `cloud_offload` task text or files. The sandbox has its own managed identity for Azure access.
- **One offload at a time** per pairing. Cancel (or let it finish) before starting another.
- **Files are transferred via E2E-encrypted chunked transfer** with SHA-256 integrity. Each file ≤ 30 MB.
- **Timeouts:** default 30 min; max configurable via `timeout_minutes`. The sandbox auto-terminates on timeout.

## Common errors

| Message | Cause | Fix |
|---|---|---|
| `❌ cloud_offload requires a task parameter…` | No recognizable task field passed | Re-issue with `task: "..."` |
| `❌ Not paired with any kars cluster` | No pairing persisted | Call `mesh_pair` with a valid token |
| `❌ Mesh connection is not live` | WebSocket to relay dropped | Wait a few seconds — plugin auto-reconnects; retry |
| `❌ Offload already in progress` | Prior offload still running | `offload_status` to check / wait for `done` |
| `❌ Sandbox '...' is registered but not responding to mesh pings` | Legacy flow, sandbox came up but E2E not ready | Usually self-heals on retry; modern sandboxes skip this via `offload_hello` |
| `No agents found on the mesh.` | No agents registered under queried capability | Try without filter, or use a different capability name |

