# `kars-runtime-hermes` — kars in-pod adapter for Hermes Agent

Implements the kars **v1 runtime contract** for [Hermes Agent](https://hermes-agent.nousresearch.com/) (Nous Research). When this package is installed inside a kars sandbox pod, it registers itself as a Hermes plugin and wires Hermes into kars' governance, mesh, and orchestration plane.

## Architecture

```
┌─── kars sandbox pod ────────────────────────────────────────┐
│                                                              │
│  Hermes (Python 3.11+)                                       │
│  └─ ~/.hermes/plugins/kars/   ← THIS PACKAGE                 │
│     ├─ register(ctx)          — Hermes plugin entry          │
│     ├─ governance pre_tool_call hook  ─┐                     │
│     ├─ kars_spawn family               │                     │
│     ├─ kars_handoff_*                  │  HTTP to            │
│     ├─ foundry_*  (9 tools)            ├─→ localhost:8443    │
│     ├─ kars_discover                   │                     │
│     ├─ http_fetch                      │                     │
│     └─ kars_mesh_* (Signal E2E)        │                     │
│                                        │                     │
│  inference-router (Rust, sidecar)  ◄───┘                     │
└──────────────────────────────────────────────────────────────┘
```

The plugin is installed two ways:

1. **Image-staged** (production / dev image build): the `Dockerfile` for `sandbox-images/hermes/` copies the plugin tree into `/opt/kars-hermes-stage/plugins/kars/`, and the entrypoint mirrors it to `$HERMES_HOME/plugins/kars/` on every container start. This is the recommended path — plugin updates ship with the image.

2. **`pip install` mode**: this package can also be `pip install`ed onto an existing Hermes installation. After install, run `cp -r $(python -c "import kars_runtime_hermes, os; print(os.path.dirname(kars_runtime_hermes.__file__))")/plugin ~/.hermes/plugins/kars` (or set `HERMES_PLUGINS_PATH` to include this package's `plugin/` subdir). Used for local development of the plugin itself.

## What it does

| Capability | How |
|---|---|
| **AGT policy gate** | `ctx.register_hook("pre_tool_call", ...)` — every tool call POSTs `/agt/evaluate`; denied calls return an error result to the LLM without executing |
| **Sub-agent spawn** | `kars_spawn`, `kars_spawn_status`, `kars_spawn_destroy`, `kars_spawn_list` — HTTP to `/sandbox/*` on the router |
| **Peer discovery** | `kars_discover` — `/agt/registry/v1/agents/{did}` lookup |
| **Mesh messaging** | `kars_mesh_send`, `_inbox`, `_await`, `_transfer_file` — end-to-end encrypted (Signal Protocol) via the Python AGT MeshClient (`runtimes/agt-mesh-python/`); the router bridges opaque ciphertext only. Exercised by `tests/e2e/interop/hermes_openclaw_bidi.sh` |
| **Handoff** | `kars_handoff_request`, `kars_handoff_confirm`, `kars_handoff_status` |
| **Foundry tools** | `foundry_code_execute`, `foundry_download_file`, `foundry_image_generation`, `foundry_web_search`, `foundry_file_search`, `foundry_memory`, `foundry_conversations`, `foundry_evaluations`, `foundry_deployments`, `foundry_agents` — all proxied through router |
| **Memory binding** | `foundry_memory` uses store name `memory-${SANDBOX_NAME}` per the KarsMemory convention |
| **HTTP fetch** | `http_fetch` — routes through `/egress/fetch` for egress allowlist enforcement |
| **Trust / signing telemetry** | After successful peer interactions: POST `/agt/trust` + `/agt/signing-counter` |
| **MCP** | Hermes' native MCP client consumes the `mcp_servers.*` block the entrypoint translates from `/etc/kars/mcp/<server>/meta.json` — no plugin code needed |

## Contract

The plugin honors [`docs/runtimes/CONTRACT.md`](../../docs/runtimes/CONTRACT.md) v1. Notable items:

- Reads `/etc/kars/secrets/admin-token` at plugin init for admin-scope endpoints
- Honors `KARS_DEV_PROFILE` for relaxed dev-mode behavior
- Skips Foundry tool registration when `KARS_PROVIDER` ∈ {`github-copilot`, `github-models`}
- Pre-seeds trust set from `AGT_TRUSTED_PEERS` (mesh trust bootstrap)
- Fail-closed grace period of 3 consecutive `/agt/evaluate` failures (`KARS_AGT_EVALUATE_FAIL_OPEN_GRACE` env override)

## Mesh parity

`kars_mesh_*` tools run on a Python AgentMesh `MeshClient` (`runtimes/agt-mesh-python/`) at byte-for-byte wire parity with the TypeScript SDK, so a Hermes agent and an OpenClaw agent are first-class encrypted-mesh peers (proven by `tests/e2e/interop/hermes_openclaw_bidi.sh`). The Signal session lives in the agent process; the router only bridges ciphertext.

## Development

```bash
cd runtimes/hermes
pip install -e ".[dev]"
pytest
```

To test against a running Hermes installation:
```bash
mkdir -p ~/.hermes/plugins
ln -s "$(pwd)/src/kars_runtime_hermes/plugin" ~/.hermes/plugins/kars
hermes plugins doctor
```
