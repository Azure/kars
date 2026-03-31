# AzureClaw Demo Guide

> A step-by-step live demo script covering everything from first launch to multi-agent collaboration with E2E encryption. **Total runtime: ~40 minutes** (or pick individual parts).

---

## Part 1: Zero to Agent (5 minutes)

### Path A: Local Dev (Docker)

The fastest way to see AzureClaw in action — a fully secured agent sandbox running locally.

```bash
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli && npm install && npm run build && npm link
azureclaw dev --build
```

On first run you'll be prompted for Azure OpenAI credentials:
```
? Azure OpenAI Endpoint: https://your-resource.openai.azure.com
? Azure OpenAI API Key:  sk-...
```

> 💡 **Tip:** Set credentials beforehand with `azureclaw credentials` to skip the prompt.

**What you should see:** Docker pulls the sandbox image, builds it locally, and drops you into the agent TUI. The full security stack (iptables, seccomp, read-only rootfs) runs inside the container.

#### 🔥 Moment: The Security Banner

Once connected, run `/azureclaw-security` to display the security posture:

```
AzureClaw Security Posture

Kernel: 6.x.x-azurelinux
User: agent (uid=1000)
Isolation: enhanced (runc + seccomp)
Root filesystem: read-only
Capabilities: ALL dropped
Seccomp: Localhost (azureclaw-strict)
Network: default-deny egress + iptables UID guard
Inference: routed through AzureClaw inference router
Foundry Agent API: proxied via localhost:8443/agents/*
Auth: IMDS (kubelet MI, zero keys)
```

Every line is an active security control — not a policy document.

#### 🔥 Moment: Egress Guard in Action

Ask the agent to reach the internet directly:

```
🦞 You: Fetch https://example.com using curl
```

**What happens:** The agent runs `curl` as UID 1000 — iptables blocks it. Only the inference router (UID 1001) has outbound access.

Now ask using the governed path:

```
🦞 You: Search the web for "Azure AI agent security best practices"
```

**What happens:** The agent calls `foundry_web_search`, which routes through Foundry's Bing grounding server-side — no egress needed. Real results come back with citations.

> **Under the hood:** The iptables egress guard uses UID-based rules. UID 1000 (agent) can only reach localhost and DNS. UID 1001 (router) handles all outbound traffic. This means even if the agent is compromised, it cannot exfiltrate data.

---

### Path B: AKS Production

For the full production experience with Kata VM isolation, namespace separation, and real Kubernetes infrastructure:

```bash
# 1. Login to Azure
az login

# 2. Build and push all 6 container images
azureclaw push

# 3. Deploy everything (AKS + ACR + Foundry + sandbox)
azureclaw up

# 4. Verify cluster health
azureclaw operator

# 5. Connect to first agent
azureclaw connect my-assistant
```

**What's different from dev mode:**

| Feature | Dev (Docker) | AKS (Production) |
|---------|-------------|-------------------|
| Isolation | runc + seccomp | Kata VM (own kernel, AMD SEV-SNP) |
| Networking | Docker bridge + iptables | Kubernetes NetworkPolicy + iptables |
| Identity | API key in env | Workload Identity (zero keys) |
| Multi-agent | Single container | Per-agent namespace + CRD |
| Scaling | Single machine | AKS node pool auto-scale |

Run `/azureclaw-security` on AKS and note the difference:

```
Isolation: confidential (Kata VM)
Seccomp: RuntimeDefault (VM boundary)
```

The agent runs inside its own lightweight VM — container escape attempts are trapped at the VM boundary, never reaching the host kernel.

---

## Part 2: Foundry Superpowers (10 minutes)

The agent has 9 Foundry skills built in. No API keys to configure — they work through Azure AI Foundry's managed infrastructure.

### 2.1 Web Search (Bing Grounding)

```
🦞 You: What are the latest developments in AI agent security this week?
```

**What happens:** The agent calls `foundry_web_search`. Foundry's Bing grounding executes the search server-side and returns results with inline URL citations.

**Expected output:**
```
Based on recent developments in AI agent security:

1. **Microsoft announced...** [source](https://...)
2. **NIST published...** [source](https://...)
...
```

> **Under the hood:** `foundry_web_search` runs entirely server-side on Foundry. The agent never makes an outbound HTTP call — no egress policy exception needed. This is the governed alternative to raw web access.

### 2.2 Code Execution

```
🦞 You: Write a Python script that generates the first 20 numbers in the
Fibonacci sequence and plots them on a chart
```

**What happens:** The agent calls `foundry_code_execute` with the Python code. Foundry runs it in a managed sandbox with pandas, numpy, and matplotlib pre-installed. The output (including chart data) comes back to the agent.

🔥 **Try chaining it:**
```
🦞 You: Now modify that script to also calculate the golden ratio approximation
at each step and add it as a second line on the chart
```

The agent iterates on the code — each execution is stateless and sandboxed on Foundry.

### 2.3 Memory (Persistent Facts)

Store something:
```
🦞 You: Remember that our compliance deadline is April 15th and the
auditor's name is Sarah Chen
```

**What happens:** The agent calls `foundry_memory` with `operation: "update"`, storing the fact in a Foundry-managed memory store scoped to this sandbox.

Now test recall:
```
🦞 You: When is our compliance deadline and who is the auditor?
```

**What happens:** The agent calls `foundry_memory` with `operation: "search"`, retrieves the stored facts via semantic search, and responds accurately.

> **Under the hood:** Memory is backed by Azure AI Foundry's memory store — a vector database with chat and embedding models. Each agent gets its own scoped store (`memory-{agent-name}`), created automatically on first use.

### 2.4 Image Generation

```
🦞 You: Create an architecture diagram showing a secure multi-agent system
with encrypted communication channels between three agents
```

**What happens:** The agent calls `foundry_image_generation` with the prompt. The `gpt-image-1` model generates an image and returns base64-encoded data.

> **Note:** Image generation requires the `gpt-image-1` model to be deployed in your Foundry project.

---

## Part 3: Multi-Agent Collaboration (15 minutes)

🔥 **This is the headline feature.** Agents spawn sub-agents, communicate via E2E-encrypted Signal Protocol messages, and collaborate on tasks — all governed by trust scores and policy rules.

### 3.1 Spawn a Sub-Agent

```
🦞 You: Spawn a research assistant named "researcher" to help with
market analysis
```

**What happens:**
1. Agent calls `azureclaw_spawn` with `name: "researcher", model: "gpt-4.1", governance: true`
2. AzureClaw creates a new `ClawSandbox` CRD in Kubernetes
3. A new namespace (`azureclaw-researcher`) is created with full isolation
4. The sub-agent pod starts with its own iptables, seccomp, and NetworkPolicy
5. AGT governance registers the new agent with an AMID (Agent Mesh IDentity)
6. KNOCK handshake establishes trust between parent and sub-agent

**Expected output:**
```
✅ Sub-agent "researcher" spawned successfully
   Phase: Running
   Namespace: azureclaw-researcher
   Model: gpt-4.1
   Governance: enabled
   AMID: discovered
```

> **Under the hood:** The KNOCK protocol is a trust-gated session establishment. Before any messages can flow, both agents must have a trust score ≥ 500 (on a 0–1000 scale). The X3DH key exchange (Extended Triple Diffie-Hellman) establishes a shared secret, and the Double Ratchet protocol provides forward secrecy for every subsequent message.

### 3.2 Inter-Agent Communication (E2E Encrypted)

```
🦞 You: Ask @researcher to find the latest AI security vulnerabilities
reported in the past week and summarize the top 5
```

**What happens:**
1. Agent calls `azureclaw_mesh_send` with `to_agent: "researcher"` and the task content
2. Message is encrypted using Signal Protocol (Double Ratchet)
3. Encrypted payload routes through the AgentMesh relay (WebSocket on port 8765)
4. Researcher agent receives, decrypts, processes the task using its own tools
5. Response is encrypted and sent back
6. Parent agent decrypts and displays the result

**Expected output:**
```
📨 Message sent to researcher (E2E encrypted, Signal Protocol)
⏳ Waiting for reply...

📬 Reply from researcher:
Here are the top 5 AI security vulnerabilities reported this week:
1. ...
2. ...
```

🔥 **Key point:** The AgentMesh relay never sees plaintext. It forwards encrypted blobs. Even if the relay is compromised, message content is protected.

#### Check your inbox manually:

```
🦞 You: Check my mesh inbox for any new messages
```

The agent calls `azureclaw_mesh_inbox` and lists all pending messages with sender, timestamp, and content.

### 3.3 Agent Discovery

```
🦞 You: Find all agents available in the registry
```

**What happens:** The agent calls `azureclaw_discover` with `query: "*"` to list all registered agents.

**Expected output:**
```
Agents found in AgentMesh registry:

1. my-assistant
   AMID: agt-xxxx-...
   Tier: standard
   Capabilities: [web_search, code_execute, memory]
   Trust Score: 850
   Status: online

2. researcher
   AMID: agt-yyyy-...
   Tier: standard
   Capabilities: [web_search, code_execute]
   Trust Score: 750
   Status: online
```

You can also search by capability:
```
🦞 You: Discover agents that can do code execution
```

### 3.4 Multi-Agent Workflow

Chain everything together for maximum impact:

```
🦞 You: Spawn an agent named "analyst" to help with data analysis.
Then ask @analyst to write and execute a Python script that
calculates compound interest for a $10,000 investment at 7%
over 30 years with monthly compounding.
```

**What happens:**
1. `azureclaw_spawn` creates the "analyst" agent
2. `azureclaw_mesh_send` sends the task (E2E encrypted)
3. The analyst agent uses `foundry_code_execute` to run the Python script
4. Results flow back encrypted through the mesh

🔥 **This demonstrates the full loop:** spawn → encrypt → send → execute → respond — all governed, all encrypted, all isolated.

### 3.5 Clean Up Sub-Agents

```
🦞 You: List all my sub-agents and then destroy the researcher
```

The agent calls `azureclaw_spawn_list` to show all spawned agents, then `azureclaw_spawn_destroy` to tear down the researcher's namespace, deployment, and all resources.

---

## Part 4: Operator Experience (5 minutes)

### 4.1 Operator Dashboard

```bash
azureclaw operator
```

**What you see:** A full-screen TUI dashboard showing:
- All running agents with status, model, and namespace
- Trust scores per agent (displayed with agent names, not raw AMIDs)
- Egress activity and blocked requests
- Resource usage and token consumption
- Audit trail entries

Navigate with arrow keys. Select an agent to see detailed information.

### 4.2 Egress Learning

Deploy an agent in learn mode to discover what domains it needs:

```bash
# Deploy with egress learning enabled
azureclaw add research-bot --model gpt-4.1 --learn-egress

# Connect and use the agent normally
azureclaw connect research-bot
```

Let the agent work for a while — it will make outbound requests as needed. Then review what it discovered:

```bash
# Review all learned domains
azureclaw egress research-bot --learned

# Review pending (unapproved) egress requests
azureclaw egress research-bot --pending

# Approve specific domains
azureclaw egress research-bot --approve

# Lock down to only the learned set
azureclaw egress research-bot --enforce
```

Or manage domains individually via policy:

```bash
# Allow a specific host
azureclaw policy allow research-bot api.example.com

# Check current policy
azureclaw policy get research-bot

# Revoke access
azureclaw policy deny research-bot api.example.com
```

> **Under the hood:** Egress learning mode records every outbound domain the agent contacts through the `http_fetch` proxy. Once you're satisfied with the allowlist, `--enforce` switches from learn mode to strict mode — any domain not on the list is blocked.

### 4.3 Live Credential Rotation

Rotate channel tokens or plugin API keys on a running agent — no restart required:

```bash
azureclaw credentials update my-assistant \
  --telegram-token "NEW_TOKEN" \
  --brave-api-key "NEW_KEY"
```

**What happens:** The CLI updates the Kubernetes secret. The pod picks up the new credentials without restarting — the agent continues its conversation uninterrupted.

### 4.4 Model Hot-Switching

Switch models on a running agent — no redeployment needed:

```bash
# Check current model
azureclaw model get my-assistant

# List all available models (200+ via Foundry catalog)
azureclaw model list my-assistant

# Hot-swap to a different model
azureclaw model set my-assistant gpt-5-mini
```

Or from inside the agent TUI:
```
/switch-model gpt-5-mini
```

Available models include `gpt-4.1`, `gpt-5-mini`, `gpt-4o`, `DeepSeek-V3.2`, `Phi-4`, `Meta-Llama-3.1-405B-Instruct`, `o3-mini`, and 200+ more via the Foundry catalog.

---

## Part 5: Security Deep Dive (10 minutes)

> 📖 **Full walkthrough:** See [DEMO.md](DEMO.md) — "Operation Claw Shield" — for the complete 30-minute attack/defense simulation.

### Quick Summary: 8 Security Layers

AzureClaw implements defense-in-depth with 8 layers. In the full demo, a compromised agent attempts each attack vector and is stopped at every layer:

| Attack | Defense Layer | What Blocks It |
|--------|--------------|----------------|
| Prompt injection (poisoned document) | Content Safety + Prompt Shields | Detected before model execution |
| Data exfiltration to C2 server | iptables egress guard | UID 1000 blocked from outbound |
| Container escape (runc CVE) | Kata VM boundary | Escape trapped in VM, not host |
| Lateral movement to other pods | Namespace isolation + NetworkPolicy | Pod-to-pod traffic denied |
| Privilege escalation | seccomp + ALL capabilities dropped | Dangerous syscalls blocked |
| IMDS credential theft (169.254.169.254) | iptables UID-based rules | Agent UID 1000 cannot reach IMDS |
| Token budget exhaustion | Token budget enforcement | Per-sandbox daily limits |
| Post-incident forensics | eBPF tracing + Azure Monitor | Full audit trail |

### Quick Security Demo

```bash
# Start eBPF tracing on an agent
azureclaw trace my-assistant --exec --network --files --dns
```

**What you see:** Real-time stream of every process execution, network connection, file access, and DNS query inside the sandbox. This is how you detect anomalous behavior.

```bash
# Verify isolation level
azureclaw status my-assistant
```

Shows health, model, isolation level, tokens used, and security posture.

```bash
# Destroy a compromised sandbox instantly
azureclaw destroy compromised-agent --yes
```

The entire namespace — pod, secrets, network policies — is torn down. Other agents are unaffected.

```bash
# Re-deploy a clean replacement
azureclaw up --name compromised-agent-v2 --isolation confidential \
  --model gpt-4.1 --foundry-endpoint https://your-resource.openai.azure.com
```

---

## Part 6: Channel Integration (5 minutes)

### Telegram Bot

```bash
# Deploy with Telegram channel enabled
azureclaw add telegram-bot --model gpt-4.1 \
  --channels telegram --telegram-token "BOT_TOKEN"

# Connect to see the agent's perspective
azureclaw connect telegram-bot
```

**What you see:** Messages sent to the Telegram bot appear in the agent's conversation. The agent processes them and responds — the reply flows back to Telegram automatically.

🔥 **Key point:** The Telegram token is stored as a Kubernetes secret and injected into the pod. The agent can use it to receive and send messages, but iptables prevents exfiltrating it — all outbound traffic goes through the governed inference router.

### Other Channels

Same pattern for Slack (`--channels slack --slack-token "xoxb-..."`), Discord (`--channels discord --discord-token "..."`), and WhatsApp (`--channels whatsapp` — uses QR code pairing at runtime).

### Web UI

Every agent also exposes a web interface:

```bash
azureclaw connect my-assistant --web
```

Opens a browser at `localhost:18789` with a chat UI — useful for demos where you want a visual interface instead of the terminal TUI.

---

## Part 7: AGT Governance (5 minutes)

The Agent Governance Toolkit (AGT v3.0.0) provides trust scoring, policy enforcement, and tamper-proof audit trails.

### Inside the Agent TUI

```
/azureclaw-agt                        # Trust score, policy rules, audit chain status
/azureclaw-agt check shell:rm -rf /   # Test if an action would be allowed
```

The policy engine evaluates against 10 default rules and returns allow/deny with the triggering rule.

**Trust scores** range 0–1000 (threshold: 500 for KNOCK handshake, ±200 per update). Agents that violate policies lose trust; agents below 500 cannot establish new E2E encrypted sessions.

**Audit trail** uses a SHA-256 Merkle tree — every governance decision is recorded and tamper-detectable.

---

## Appendix: Demo Cheat Sheet

### Quick Commands

| Part | Command | What It Shows |
|------|---------|---------------|
| Setup (Dev) | `azureclaw dev --build` | Local sandbox in 30 seconds |
| Setup (AKS) | `azureclaw up` | Full production deployment |
| Security banner | `/azureclaw-security` | All active protections |
| Web Search | Ask about today's news | `foundry_web_search` — Bing grounding |
| Code Execution | Ask to write + run Python | `foundry_code_execute` — Foundry sandbox |
| Memory | "Remember X" → "What was X?" | `foundry_memory` — persistent facts |
| Image Gen | "Create an image of..." | `foundry_image_generation` — gpt-image-1 |
| File Search | "Search my documents for..." | `foundry_file_search` — RAG pipeline |
| Spawn Agent | "Spawn an agent named X" | `azureclaw_spawn` — isolated sub-agent |
| Send Task | "Ask @X to do Y" | `azureclaw_mesh_send` — E2E encrypted |
| Check Inbox | "Check my inbox" | `azureclaw_mesh_inbox` — pending messages |
| Discover | "Find all agents" | `azureclaw_discover` — registry search |
| Operator | `azureclaw operator` | Live TUI dashboard |
| Egress | `azureclaw egress X --learned` | Egress learning results |
| Model Switch | `azureclaw model set X gpt-5-mini` | Hot-swap model |
| Trace | `azureclaw trace X --exec --network` | eBPF real-time tracing |
| Security Demo | See [DEMO.md](DEMO.md) | 8-layer attack/defense |

### All 17 Agent Tools

**AzureClaw (7):** `azureclaw_spawn` · `azureclaw_spawn_status` · `azureclaw_mesh_send` · `azureclaw_mesh_inbox` · `azureclaw_spawn_destroy` · `azureclaw_spawn_list` · `azureclaw_discover`

**Egress (1):** `http_fetch`

**Foundry (9):** `foundry_web_search` · `foundry_code_execute` · `foundry_image_generation` · `foundry_file_search` · `foundry_memory` · `foundry_conversations` · `foundry_evaluations` · `foundry_deployments` · `foundry_agents`

### Slash Commands (Inside Agent TUI)

`/azureclaw` (status) · `/azureclaw-security` (posture) · `/azureclaw-agt` (governance) · `/azureclaw-agt check <action>` (policy eval) · `/azureclaw-models` (list models) · `/switch-model <model>` (hot-swap) · `/azureclaw-spawn <name>` (spawn) · `/azureclaw-spawn-list` · `/azureclaw-spawn-status <name>` · `/azureclaw-spawn-destroy <name>`

### Recommended Demo Flow (5-Minute Speed Run)

```bash
# 1. Launch
azureclaw dev --build

# 2. Show security (30s)
/azureclaw-security

# 3. Web search (30s)
"What's happening in AI security today?"

# 4. Code execution (60s)
"Write and run a Python script that calculates pi to 100 digits"

# 5. Spawn + E2E mesh (120s)
"Spawn an agent named helper"
"Ask @helper to summarize the benefits of zero-trust architecture"

# 6. Operator view (30s)
# (In another terminal)
azureclaw operator
```
