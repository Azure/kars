# kars Hermes plugin (`runtimes/hermes/`)

The **kars Hermes plugin** is the agent-side runtime surface for kars on top of the [Hermes Agent](https://github.com/NousResearch/hermes-agent) (Nous Research, MIT) — a Python 3.11+ agent harness with **20+ messaging channels**, **18+ inference providers**, **70+ built-in tools**, and a native MCP client. When a Hermes sandbox boots, the Hermes gateway auto-discovers the kars plugin from `$HERMES_HOME/plugins/kars/` and loads it; from that point on the agent's tool surface is the **17 governance-aware kars tools** (16 in local-registry mode) the plugin registers plus the 5 Hermes built-ins kars explicitly denies.

> **Parity vs OpenClaw — ~all major surfaces shipped.** OpenClaw's kars plugin registers 24 tools; Hermes registers 16 native + 5 via the platform MCP gateway = 21 LLM-reachable Foundry/kars tools. Surfaces:
> - **`kars_handoff_*`** (status / request / confirm) — implemented (`handoff.py`). `kars_handoff_status` always-on; the two mutation tools register only when `AGT_REGISTRY_MODE=global` (same gate OpenClaw uses; in local mode the router refuses mutations).
> - **`kars_mesh_transfer_file`** — implemented end-to-end (sender + receiver auto-save). File arrives at `/sandbox/incoming/<file_name>` and the LLM sees a short summary instead of the base64 blob.
> - **Native Foundry tools**: `foundry_memory`, `foundry_web_search`, `foundry_code_execute`, `foundry_image_generation`, `foundry_file_search` register as native `foundry_*` names matching OpenClaw. The 5 operator-tier Foundry tools (`conversations`, `evaluations`, `deployments`, `agents`, `download_file`) reach the agent via the platform MCP server at `http://127.0.0.1:8443/platform/mcp` — LLM sees them as `mcp__platform__foundry_*`.
> - **Multi-agent peer roster auto-prepend** — implemented (`mesh.py::_maybe_prepend_peer_roster`). When 2+ siblings are tracked in the spawn roster, every outbound mesh message gets an authoritative `Peer roster:` block (matching OpenClaw's behaviour at `agt-tools/agt.ts:545`).
> - **`telegram_status` agent-side tool** — Hermes uses the operator-side `channels.telegram` config flow; documented divergence.

| Property | Value |
|---|---|
| **Plugin ID** | `kars` (manifest: `runtimes/hermes/src/kars_runtime_hermes/plugin/plugin.yaml`) |
| **Source** | `runtimes/hermes/src/kars_runtime_hermes/` (~3,500 LOC Python) |
| **Process / UID** | Loads into the agent container (UID 1000) as the `hermes gateway run --accept-hooks` daemon (PID 1) |
| **Network egress** | None directly — every outbound call goes via the inference router (UID 1001) on `127.0.0.1:8443` |
| **Mesh session ownership** | This plugin **owns** the Signal Protocol session (X3DH + Double Ratchet + KNOCK) via `runtimes/agt-mesh-python/` (kars-agt-mesh) — the Python AGT MeshClient at TS-SDK byte-for-byte parity. The router only WebSocket-bridges opaque ciphertext. |

For the conceptual split between plugin-owned mesh and router-owned governance/audit, see [Architecture → The mesh](architecture.md#the-mesh) and [AGT boundary](architecture/agt-boundary.md).

---

## Registered tools (11 total) — NOT yet at OpenClaw parity

Authoritative source: each `register()` call across `runtimes/hermes/src/kars_runtime_hermes/plugin/*.py`. Verified by `kubectl logs <hermes-pod> -c agent | grep "registered"` on any running Hermes sandbox.

### Sub-agent spawn (4) — parity with OpenClaw

| Tool | What it does |
|---|---|
| `kars_spawn` | Create a governed sub-agent (Hermes-runtime by default — controller stamps `runtime.kind=Hermes` when the parent is Hermes). |
| `kars_spawn_list` | Enumerate currently-running sub-agents. |
| `kars_spawn_status` | Pod / runtime / mesh status for one sub-agent. |
| `kars_spawn_destroy` | Graceful tear-down of a sub-agent. |

### Mesh (3 functional + 1 stub) — partial parity

| Tool | What it does |
|---|---|
| `kars_mesh_send` | Send a message to a sibling. Encryption is via Double Ratchet inside the plugin; the router sees ciphertext only. Returns `delivered_via_agt_relay` (fire-and-forget) or `delivered_and_replied` (sync round-trip when the peer auto-responder is enabled). |
| `kars_mesh_inbox` | Drain the local inbox (decrypted plugin-side) without blocking. |
| `kars_mesh_await` | Block until a message arrives from a specific sender (with timeout). |
| `kars_mesh_transfer_file` ⚠ **stub** | Returns `"kars_mesh_transfer_file not yet implemented in mesh v0.1 (small-messages only). Use kars_mesh_send with chunked content."` — see follow-up `hermes-mesh-transfer-file`. |

The `mesh_worker` background loop (`KARS_MESH_AUTO_RESPONDER=1`, set by the controller on sub-agent containers) auto-decrypts every inbound and dispatches to the agent's LLM, publishing the resulting reply back through `kars_mesh_send`.

### Discovery (1)

| Tool | What it does |
|---|---|
| `kars_discover` | Look up sibling agents on the AGT registry by display name or capability. |

### Handoff (0) ❌ **NOT IMPLEMENTED**

`runtimes/hermes/src/kars_runtime_hermes/plugin/handoff.py` is currently `def register(ctx): pass`. The three OpenClaw tools `kars_handoff_request / kars_handoff_confirm / kars_handoff_status` are missing on the Hermes side. Tracked: `hermes-handoff-impl`.

### Foundry data plane (1 native + 9 via MCP) — partial parity

| Tool | Surface |
|---|---|
| `foundry_memory` (native) | `ctx.register_tool` direct — agent sees it as `foundry_memory`. Per-agent long-term memory backed by Azure AI Foundry Memory Store. Scoped via `agent:${CLUSTER_NAME}/${SANDBOX_NAME}` so memory survives pod restart and is per-sandbox-isolated. |
| `foundry_web_search`, `foundry_image_gen`, `foundry_code_execute`, `foundry_file_search`, `foundry_conversations`, `foundry_evaluations`, `foundry_deployments`, `foundry_agents`, `foundry_download_file` | Wired via the platform MCP server at `http://127.0.0.1:8443/platform/mcp` — Hermes' native MCP client connects on first use. **Agent sees them as `mcp__platform__foundry_*` rather than `foundry_*`.** Tracked: `hermes-native-foundry-tools`. |

### Network (1)

| Tool | What it does |
|---|---|
| `http_fetch` | Single outbound HTTP fetch, governance-gated. Subject to the L7 egress allowlist (`KarsSandbox.spec.networkPolicy.allowlistRef`) + the auto-refreshing OISD + URLhaus blocklist + any active `EgressApproval` overlay. Hermes' own `web_fetch` built-in is deregistered so this is the only path. |

### Hooks (governance + telemetry) — parity with OpenClaw

| Hook | What it does |
|---|---|
| `pre_tool_call` | AGT governance gate — every tool call is screened against the active policy profile (`developer` / `web` / `azure` / `minimal`) before the kernel executes it. Fail-closed with a 3-call grace window if the policy service is briefly unreachable. |
| `post_tool_call` | Telemetry — emits the standard kars OTel spans (`kars.tool.invocation`) so the operator-CLI topology and Headlamp mesh dashboard pick up Hermes-side tool activity identically to OpenClaw. |

### Multi-agent peer roster — ❌ NOT IMPLEMENTED

OpenClaw auto-prepends `Peer roster: name — role` to every outbound `kars_mesh_send` / `kars_mesh_transfer_file` when 2+ siblings exist. Hermes does not yet. In multi-agent pipelines (`analyst → viz → writer`), Hermes agents must resolve sibling names themselves. Tracked: `hermes-peer-roster`.

### Denied Hermes built-ins (6)

The plugin actively deregisters the following Hermes built-ins so the agent cannot bypass kars governance:

`web_search` · `web_fetch` · `code_interpreter` (Python sandbox) · `image_generation` · `file_search` (Hermes' own RAG) · `chat_completion` (direct provider call)

Each is replaced by its kars equivalent (`foundry_*` via MCP or `http_fetch`) that routes through the inference router and is therefore subject to Content Safety, the L7 egress allowlist, and AGT policy.

---

## Channels (4 first-class adapters today)

Hermes ships 20+ channel adapters; kars wires the four production-grade ones via CLI flag → env var → `entrypoint.sh` → `hermes config set channels.*` flow:

| Channel | Env var (set by CLI) | Hermes config key |
|---|---|---|
| **Telegram** | `TELEGRAM_BOT_TOKEN`, `TELEGRAM_ALLOWED_USERS` | `channels.telegram.{token,allowed_users,enabled}` |
| **Slack** | `SLACK_BOT_TOKEN` | `channels.slack.{token,enabled}` |
| **Discord** | `DISCORD_BOT_TOKEN` | `channels.discord.{token,enabled}` |
| **WhatsApp** | `WHATSAPP_TOKEN` | `channels.whatsapp.{token,enabled}` |

Credentials live in a Kubernetes secret named `<sandbox-name>-credentials` in namespace `kars-<sandbox-name>`, mounted via `envFrom: { secretRef: { optional: true } }` so a Hermes pod starts even before the secret is created. Add or rotate tokens with:

```bash
kars credentials update my-hermes-agent --telegram-token <bot-token>
kubectl rollout restart deployment/my-hermes-agent -n kars-my-hermes-agent
```

When no channels are configured the entrypoint logs `No channels — starting hermes gateway in idle daemon mode` and serves only mesh / spawn / hook traffic — perfect for sub-agents that talk only to other agents.

---

## Plugins (5 tool providers wired via env vars)

Hermes ships 70+ tool plugins; kars exposes five production search/scrape providers through the same auto-config pattern as channels:

| Plugin | Env var | Hermes config key |
|---|---|---|
| Brave Search | `BRAVE_API_KEY` | `tools.brave.api_key` |
| Tavily | `TAVILY_API_KEY` | `tools.tavily.api_key` |
| Exa | `EXA_API_KEY` | `tools.exa.api_key` |
| Firecrawl | `FIRECRAWL_API_KEY` | `tools.firecrawl.api_key` |
| Perplexity | `PERPLEXITY_API_KEY` | `tools.perplexity.api_key` |

When none are set the agent uses `foundry_web_search` (Foundry Bing Grounding) instead — that is the default path and requires no configuration.

---

## Identity, mesh, and Entra Verified ID

Hermes pods participate in the AGT mesh identically to OpenClaw — same registry, same relay, same Signal Protocol stack — through `kars-agt-mesh` (`runtimes/agt-mesh-python/`).

| Subsystem | Where |
|---|---|
| **Persistent identity** (Ed25519 + X25519, DID = `did:mesh:<sha256(pub)[:32]>`) | `$HERMES_HOME/.agt/identity.json` (emptyDir, 0600) |
| **Entra Verified tier upgrade** | Entrypoint exchanges the projected SA token for an Entra Agent App token (audience: `<app-id>/.default`) → POST `/agt/registry/v1/registry/verify` → the operator panel and `kars topology` show `tier=verified, verified_app_id=<guid>` |
| **Prekey-writer guard** | An exclusive `fcntl.flock` on `$HERMES_HOME/.agt/.mesh-prekeys.lock` — a second process trying to start a MeshClient for the same identity fails loud with `MeshTransportError`. See [the cross-runtime audit](internal/security-audits/2026-06-06-cross-runtime-mesh-aks.md) for why this matters. |

---

## Bringing your own agent code

By default the image ships a smoke-test agent at `/opt/kars-default-agent/main.py` that answers a single chat-completion. Real users supply their own via:

```yaml
spec:
  runtime:
    kind: Hermes
    hermes:
      agentCode:
        oci:
          image: myregistry.azurecr.io/my-hermes-agent:1.2.3
      # — or —
      agentCode:
        git:
          url: https://github.com/me/my-hermes-agent
          ref: v1.2.3
          path: src
```

The controller mounts the source at `/sandbox/agent` (the Hermes working directory) — no other changes required. Hermes auto-discovers any `*.py` modules in the working directory; kars-registered tools and hooks remain active for everything you load.

---

## See also

- **[Runtimes](runtimes.md)** — first-class adapter catalog (Hermes row included).
- **[CRD reference — `HermesConfig`](api/crd-reference.md#hermesconfig)** — the full `spec.runtime.hermes.*` schema.
- **[Channels & plugins](channels-plugins.md)** — credential / env-var contract for channels and tool plugins, OpenClaw and Hermes side-by-side.
- **[Mesh plugin](mesh-plugin.md)** — Hermes-as-mesh-peer story with `runtimes/agt-mesh-python/`.
- **[Hermes troubleshooting runbook](runbooks/hermes-troubleshooting.md)** — operator-facing diagnostics.
- **[Internal: cross-runtime mesh AKS audit](internal/security-audits/2026-06-06-cross-runtime-mesh-aks.md)** — what was proven and why specific defences exist.
