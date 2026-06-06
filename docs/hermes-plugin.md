# kars Hermes plugin (`runtimes/hermes/`)

The **kars Hermes plugin** is the agent-side runtime surface for kars on top of the [Hermes Agent](https://github.com/NousResearch/hermes-agent) (Nous Research, MIT) — a Python 3.11+ agent harness with **20+ messaging channels**, **18+ inference providers**, **70+ built-in tools**, and a native MCP client. When a Hermes sandbox boots, the Hermes gateway auto-discovers the kars plugin from `$HERMES_HOME/plugins/kars/` and loads it; from that point on the agent's tool surface is the **9 governance-aware kars tools** the plugin registers plus the 6 Hermes built-ins kars explicitly denies.

| Property | Value |
|---|---|
| **Plugin ID** | `kars` (manifest: `runtimes/hermes/src/kars_runtime_hermes/plugin/plugin.yaml`) |
| **Source** | `runtimes/hermes/src/kars_runtime_hermes/` (~3,500 LOC Python) |
| **Process / UID** | Loads into the agent container (UID 1000) as the `hermes gateway run --accept-hooks` daemon (PID 1) |
| **Network egress** | None directly — every outbound call goes via the inference router (UID 1001) on `127.0.0.1:8443` |
| **Mesh session ownership** | This plugin **owns** the Signal Protocol session (X3DH + Double Ratchet + KNOCK) via `runtimes/agt-mesh-python/` (kars-agt-mesh) — the Python AGT MeshClient at TS-SDK byte-for-byte parity. The router only WebSocket-bridges opaque ciphertext. |

For the conceptual split between plugin-owned mesh and router-owned governance/audit, see [Architecture → The mesh](architecture.md#the-mesh) and [AGT boundary](architecture/agt-boundary.md).

---

## Registered tools (9 total)

Authoritative source: `runtimes/hermes/src/kars_runtime_hermes/plugin/plugin.yaml`. The plugin registers exactly these tools via Hermes' `register_tool()` API; every Hermes built-in that overlapped (e.g. Hermes' own `web_search`) is deregistered in the same plugin load pass so the agent never sees two competing implementations.

### Mesh + sub-agents (4)

| Tool | What it does |
|---|---|
| `kars_discover` | Look up sibling agents on the AGT registry by display name or capability. |
| `kars_mesh_send` | Send a message to a sibling. Encryption is via Double Ratchet inside the plugin; the router sees ciphertext only. Returns `delivered_via_agt_relay` (fire-and-forget) or `delivered_and_replied` (sync round-trip when the peer auto-responder is enabled). |
| `kars_mesh_inbox` | Drain the local inbox (decrypted plugin-side) without blocking. |
| `kars_mesh_await` | Block until a message arrives from a specific sender (with timeout). |

The `mesh_worker` background loop (`KARS_MESH_AUTO_RESPONDER=1`, set by the controller on sub-agent containers) auto-decrypts every inbound and dispatches to the agent's LLM, publishing the resulting reply back through `kars_mesh_send` — giving you the synchronous request/response pattern needed for `parent → child` pipelines.

### Handoff (3)

| Tool | What it does |
|---|---|
| `kars_handoff_request` | Ask another sandbox to take over the current session (escalation pattern). |
| `kars_handoff_confirm` | Accept (or reject) an incoming handoff request. |
| `kars_handoff_status` | Inspect the state of an in-flight handoff. |

### Foundry data plane (1 native + 8 via MCP)

| Tool | What it does |
|---|---|
| `foundry_memory` (native) | Per-agent long-term memory backed by Azure AI Foundry Memory Store. Scoped via `agent:${CLUSTER_NAME}/${SANDBOX_NAME}` so memory survives pod restart and is per-sandbox-isolated. |
| `foundry_web_search`, `foundry_image_gen`, `foundry_code_execute`, `foundry_file_search`, `foundry_conversations`, `foundry_evaluations`, `foundry_deployments`, `foundry_agents` | Wired via the platform MCP server at `http://127.0.0.1:8443/platform/mcp` — Hermes' native MCP client connects on first use. |

### Network (1)

| Tool | What it does |
|---|---|
| `http_fetch` | Single outbound HTTP fetch, governance-gated. Subject to the L7 egress allowlist (`KarsSandbox.spec.networkPolicy.allowlistRef`) + the auto-refreshing OISD + URLhaus blocklist + any active `EgressApproval` overlay. Hermes' own `web_fetch` built-in is deregistered so this is the only path. |

### Hooks (governance + telemetry)

| Hook | What it does |
|---|---|
| `pre_tool_call` | AGT governance gate — every tool call is screened against the active policy profile (`developer` / `web` / `azure` / `minimal`) before the kernel executes it. Fail-closed with a 3-call grace window if the policy service is briefly unreachable. |
| `post_tool_call` | Telemetry — emits the standard kars OTel spans (`kars.tool.invocation`) so the operator-CLI topology and Headlamp mesh dashboard pick up Hermes-side tool activity identically to OpenClaw. |

### Denied Hermes built-ins (6)

The plugin actively deregisters the following Hermes built-ins so the agent cannot bypass kars governance:

`web_search` · `web_fetch` · `code_interpreter` (Python sandbox) · `image_generation` · `file_search` (Hermes' own RAG) · `chat_completion` (direct provider call)

Each is replaced by its kars equivalent (`foundry_*` / `http_fetch`) that routes through the inference router and is therefore subject to Content Safety, the L7 egress allowlist, and AGT policy.

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
