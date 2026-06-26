# kars OpenClaw plugin (`runtimes/openclaw/`)

The **kars OpenClaw plugin** is the agent-side runtime surface for kars. When a sandbox boots, the [OpenClaw](https://github.com/openclawai/openclaw) gateway auto-discovers and loads the plugin from `~/.openclaw-data/extensions/kars/`. From that point on, the agent's tool surface is the **24 governance-aware tools** the plugin registers — every privileged OpenClaw built-in is replaced with a kars equivalent that routes through the inference router and is subject to AGT governance.

| Property | Value |
|---|---|
| **Plugin ID** | `kars` (manifest: `runtimes/openclaw/openclaw.plugin.json`) |
| **Source** | `runtimes/openclaw/src/` (~2,770 LOC TypeScript) |
| **Process / UID** | Loads into the agent container (UID 1000) |
| **Network egress** | None directly — every outbound call goes via the inference router (UID 1001) on `127.0.0.1:8443` |
| **Mesh session ownership** | This plugin **owns** the Signal Protocol session (X3DH + Double Ratchet + KNOCK) via `@microsoft/agent-governance-sdk`. The router only WebSocket-bridges opaque ciphertext. |

For the conceptual split between plugin-owned mesh and router-owned governance/audit, see [Architecture → The mesh](architecture.md#the-mesh) and [AGT boundary](architecture/agt-boundary.md).

---

## Registered tools (24 total)

Authoritative source: `runtimes/openclaw/openclaw.plugin.json` → `contracts.tools[]`. The plugin enforces this contract at startup — OpenClaw refuses to register tools not declared here.

### Mesh + sub-agents (9)

| Tool | What it does |
|---|---|
| `kars_discover` | Lookup sibling agents on the AgentMesh registry by name or capability. |
| `kars_spawn` | Create a governed sub-agent — materialises a fresh `KarsSandbox` CR, the controller reconciles it into its own pod with its own policy / network policy / identity. Requires a `role` arg (e.g. `"data analyst"`) when more than one sibling will exist (to enable the peer roster, see [architecture.md → Multi-agent peer roster](architecture.md#the-mesh)). |
| `kars_spawn_list` | Enumerate currently-running sub-agents. |
| `kars_spawn_status` | Pod / runtime / mesh status for one sub-agent. |
| `kars_spawn_destroy` | Graceful tear-down of a sub-agent (deletes the CR; controller reaps the pod + namespace). |
| `kars_mesh_send` | Send a message to a sibling. Encryption is via Double Ratchet inside the plugin; the router sees ciphertext only. |
| `kars_mesh_inbox` | Poll for incoming messages (decrypted plugin-side). |
| `kars_mesh_await` | Block until a message arrives from a specific sender (with timeout). |
| `kars_mesh_transfer_file` | Chunked file transfer over mesh; per-chunk Double-Ratchet-encrypted. Used for pipelines like `analyst → viz → writer` where viz hands a `scorecard.png` to writer. |

### Handoff (3)

| Tool | What it does |
|---|---|
| `kars_handoff_request` | Ask another sandbox to take over the current session (e.g. escalate to a more-privileged peer). |
| `kars_handoff_confirm` | Accept (or reject) an incoming handoff request. |
| `kars_handoff_status` | Inspect the state of an in-flight handoff. |

### Network (1)

| Tool | What it does |
|---|---|
| `http_fetch` | Single outbound HTTP fetch, governance-gated. Subject to the L7 egress allowlist (`KarsSandbox.spec.networkPolicy.allowlistRef`) + the auto-refreshing OISD + URLhaus blocklist + any active `EgressApproval` overlay. |

### Foundry data plane (10)

All Foundry tools route through the router which adds Entra Agent ID auth (when `--mesh-trust=entra`), per-request content-safety inspection (`prompt_filter_results` parsing on Foundry providers), hash-chained audit entries, and token-budget enforcement.

| Tool | What it does |
|---|---|
| `foundry_code_execute` | Python code execution inside a Foundry-managed container (matplotlib, numpy, pandas pre-installed). Output files at `/mnt/data/*` are auto-downloaded into `/sandbox/.openclaw/workspace/<filename>`. |
| `foundry_image_generation` | Image generation via `gpt-image-1`. PNG persists directly into the sandbox FS at `/sandbox/.openclaw/workspace/<output_filename>`. |
| `foundry_web_search` | Real-time web search via Bing Grounding (auto-discovered from the Foundry project's connections). |
| `foundry_file_search` | Retrieval-augmented search (RAG) over uploaded files. |
| `foundry_memory` | Persistent long-term memory store (per-sandbox scope by default; cross-sandbox sharing via `KarsMemory` CRD). |
| `foundry_conversations` | Manage multi-turn conversation state on the Foundry side. |
| `foundry_deployments` | Discover model deployments + connections + indexes in the Foundry project. Exercised at every sandbox startup (the bootstrap probes `/deployments`, `/connections`, `/indexes`). |
| `foundry_evaluations` | 🧪 **Experimental — not yet exercised in any verified run.** Implementation surface for the Azure AI Foundry OpenAI Evals API (operations: `list / create / run / get_run / list_evaluators`). The tool is registered and the router proxies the calls, but no scenario or example currently invokes it end-to-end. Treat as a placeholder for future evaluation workflows — wiring it into an agent that actually calls it will likely surface integration gaps. Unrelated to the `KarsEval` CRD, which has its own controller-driven runner pipeline. |
| `foundry_agents` | 🧪 **Experimental — not yet exercised in any verified run.** Implementation surface for querying / invoking **Azure AI Foundry prompt-agents** (the platform's hosted agent abstraction, distinct from kars sandboxes). Registered and proxied by the router, but no scenario currently invokes it. Treat as a placeholder for future Foundry-agent integration patterns. |
| `foundry_download_file` | Download files generated by Foundry tools into the sandbox FS. |

### Channels (1)

| Tool | What it does |
|---|---|
| `telegram_status` | Emit a status update to the configured Telegram channel (when the sandbox was launched with `--telegram-token`). See [Channels & plugins](channels-plugins.md) for channel wiring. |

---

## Skills (10 — agent-facing how-to)

Each skill is a `SKILL.md` markdown file under `runtimes/openclaw/skills/<name>/`. OpenClaw exposes them as a discovery surface so the LLM knows when and how to invoke each tool. Skills are the agent-facing contract; tools are the runtime contract.

| Skill | Covers | What it teaches the agent |
|---|---|---|
| `kars-spawn` | `kars_spawn`, `kars_spawn_*`, `kars_mesh_*`, `kars_handoff_*`, `kars_discover` | When to spawn a sub-agent vs. handoff; the peer-roster pattern; chunked file transfer; multi-agent pipelines. |
| `agt-governance` | `http_fetch` + the AGT decision contract for every tool call | What "denied" means; how to reason about an allow/deny verdict; retry / approval semantics. |
| `foundry-web-search` | `foundry_web_search` | Query-construction patterns; citation discipline; when to broaden queries. |
| `foundry-code` | `foundry_code_execute` | Container ephemerality (file persistence rules); embedding JSON correctly in the code argument; matplotlib idioms. |
| `foundry-memory` | `foundry_memory` | When to use memory vs. conversations; scope discipline; search vs. update flows. |
| `foundry-knowledge` | `foundry_file_search` + RAG patterns | Index lifecycle; query strategies; result-filtering. |
| `foundry-conversations` | `foundry_conversations` | When to persist state on Foundry side vs. inline. |
| `foundry-evaluations` | `foundry_evaluations` | 🧪 Experimental — skill markdown exists but the tool is not yet exercised in any verified run. Documents eval-definition setup against the Foundry OpenAI Evals API. (Separate from the `KarsEval` CRD's reproducible-corpus pipeline.) |
| `foundry-agents` | `foundry_agents` | 🧪 Experimental — skill markdown exists but the tool is not yet exercised in any verified run. Documents Foundry prompt-agent inspection + invocation patterns. |
| `foundry-deployments` | `foundry_deployments` | Discovery flow at startup; model preference vs. policy. |

Skills directory: `runtimes/openclaw/skills/`. Each subdirectory contains `SKILL.md` plus the agent-facing examples and policy.

---

## How OpenClaw loads the plugin

1. The sandbox-image entrypoint (`sandbox-images/openclaw/entrypoint.sh` line ~784, `cat > "$OPENCLAW_CONFIG"`) writes `openclaw.json` with the kars provider config pointing at the inference router (`baseUrl: http://127.0.0.1:8443/v1`).
2. The entrypoint drops the plugin tarball at `~/.openclaw-data/extensions/kars/`.
3. OpenClaw boots, auto-discovers the plugin via its standard extension-discovery mechanism, and the plugin's `register` / `activate` hooks register the 24 tools via `api.registerTool({...})` (call sites in `runtimes/openclaw/src/index.ts:2525, 2746`).
4. From this point on the agent can only call tools the plugin has registered. The OpenClaw built-ins are denied via `tools.deny` config.

See [Upstream alignment](upstream-alignment.md) for why kars uses this standard extension mechanism rather than forking OpenClaw.

---

## Companion plugin: `@kars/mesh`

For users running OpenClaw on their **laptop** and wanting to delegate heavy work to a governed AKS sandbox, the separate **`@kars/mesh`** plugin pairs a local agent with a remote kars cluster. See **[`@kars/mesh` plugin](mesh-plugin.md)**.

---

## See also

- **[Architecture → The mesh](architecture.md#the-mesh)** — why the Signal session is plugin-owned, not router-owned.
- **[AGT boundary](architecture/agt-boundary.md)** — what AGT enforces vs. what kars enforces.
- **[Runtimes](runtimes.md)** — how OpenClaw fits among the eight first-class agent runtimes.
- **[Channels & plugins](channels-plugins.md)** — Telegram / Slack / Discord / WhatsApp channel wiring + 3rd-party API plugins.
- **[CRD reference → KarsSandbox](api/crd-reference.md#karssandbox--the-agent)** — `spec.runtime.openclaw` configuration.
