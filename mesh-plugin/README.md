# @kars/mesh — OpenClaw Federation Plugin

> **Status — build from source (not yet published).** This plugin is **not yet
> published to npm**; `@kars-runtime/cli` is currently the only kars package on
> npm. Install it by building from source (below). The `npm install -g @kars/mesh`
> command is shown for when the package is published — track this on the
> [roadmap](../docs/roadmap.md).

Connect any OpenClaw agent to a kars cluster for secure cloud offload and inter-agent communication via E2E encrypted AgentMesh.

**No Docker, no Rust, no kars CLI required on the client side.**

## What it does

| Capability | Description |
|-----------|-------------|
| **Cloud offload** | Delegate compute-heavy tasks to governed AKS sandboxes with GPU/inference |
| **Task results** | Automatic result relay — the cloud sandbox runs your task and pushes results back to you |
| **Inter-agent messaging** | Send/receive E2E encrypted messages to any agent on the mesh |
| **Agent discovery** | Find specialist agents by capability (security-auditor, code-reviewer, etc.) |

## Prerequisites

- **Node.js 20+** (22 recommended)
- **OpenClaw** installed and working locally
- An kars cluster admin who can generate a pairing token for you

## Install

```bash
# From npm (when published — not yet available)
npm install -g @kars/mesh

# From source (the supported path today)
git clone https://github.com/Azure/kars.git
cd kars/mesh-plugin
npm install && npm run build
```

### Register with OpenClaw

Add to your OpenClaw config (`~/.openclaw/openclaw.json`):

```json
{
  "plugins": {
    "allow": ["kars-mesh"],
    "entries": [
      {
        "name": "kars-mesh",
        "enabled": true,
        "path": "/path/to/mesh-plugin/dist/index.js"
      }
    ]
  }
}
```

Or if installed globally via npm:

```json
{
  "plugins": {
    "allow": ["kars-mesh"],
    "entries": [
      {
        "name": "kars-mesh",
        "enabled": true,
        "module": "@kars/mesh"
      }
    ]
  }
}
```

## Quick start

### 1. Get a pairing token

Ask your kars cluster admin to generate one:

```bash
# On the kars cluster (admin runs this)
kars pair generate --name alice-laptop --budget 500000 --expires 30d
```

They'll give you a token like: `azcp_1_eyJjb250cm9sbGVyX2FtaWQiOi...`

### 2. Pair your agent

In your OpenClaw agent session, say:

> "Pair with kars using this token: azcp_1_eyJ..."

Or directly invoke the tool:

```
mesh_pair(token: "azcp_1_eyJ...")
```

Pairing is **one-time**. Your identity is saved at `~/.kars/identity.json`.

### 3. Offload a task

> "Offload this task to the cloud: Analyze this codebase for OWASP Top 10 vulnerabilities and generate a markdown report"

Or:

```
cloud_offload(task: "Analyze codebase for OWASP Top 10 vulnerabilities", model: "gpt-4.1", timeout_minutes: 15)
```

### 4. Check status

> "What's the status of my offload?"

The plugin automatically receives status updates and the final result via the mesh.

## Available tools

| Tool | Description |
|------|-------------|
| `mesh_pair` | One-time pairing with a kars cluster |
| `cloud_offload` | Delegate a task to a governed cloud sandbox |
| `offload_status` | Check progress of an active offload |
| `mesh_send` | Send an E2E encrypted message to another agent |
| `mesh_inbox` | Read incoming messages from the mesh |
| `discover` | Find agents by capability or name |

## How it works

```
┌─────────────────┐     WebSocket      ┌──────────────┐     K8s API     ┌──────────────────┐
│  Your OpenClaw   │◄──── AgentMesh ───►│  kars   │───────────────►│  Offload Sandbox │
│  + mesh plugin   │     Relay (E2E)    │  Controller  │                │  (AKS pod)       │
└─────────────────┘                     └──────────────┘                └──────────────────┘
   ~/.kars/                          K8s Secret                     OFFLOAD_MODE=true
   identity.json                          mesh identity                  runs task → result
   pairings.json                          KarsPairing CRD                relayed back via mesh
```

1. **Pairing**: Your plugin connects to the relay, sends a `pair_request` with the token. The controller validates it, binds your AMID (Agent Mesh ID), and responds.

2. **Offload**: You send an `offload_request`. The controller validates your pairing/budget, creates a KarsSandbox CRD → pod runs your task → controller watches pod completion → reads result from pod logs → sends `offload_done` back to you via the relay.

3. **Security**: All relay messages are opaque base64 payloads. The pairing token is never stored (only its SHA-256 hash). Your Ed25519 identity provides authentication. The sandbox runs with full kars security (seccomp, NetworkPolicy, read-only rootfs, Content Safety).

## Files created

| Path | Purpose |
|------|---------|
| `~/.kars/identity.json` | Ed25519 keypair (AES-256-GCM encrypted) + AMID |
| `~/.kars/pairings.json` | Stored pairing metadata (relay URL, cluster name, budget) |

## NemoClaw / OpenShell sandbox setup

NemoClaw sandboxes enforce deny-by-default networking. The plugin needs
an egress policy preset to reach the kars relay (WebSocket) and
registry (REST). A ready-made preset is included in `nemoclaw/policies/presets/`.

### 1. Copy the preset into your NemoClaw blueprint

The preset uses `host.docker.internal` which resolves to different IPs
per platform. The setup script resolves the DNS and renders the preset
automatically:

```bash
cd mesh-plugin/nemoclaw
./setup.sh --install          # resolves host IP, copies preset to NemoClaw blueprint
./setup.sh --install --apply  # also applies preset to a running sandbox
./setup.sh                    # just prints the rendered preset to stdout
```

Or copy manually (you'll need to replace `__HOST_IP__` yourself):

```bash
cp mesh-plugin/nemoclaw/policies/presets/kars-mesh.yaml \
   ~/.nemoclaw/source/nemoclaw-blueprint/policies/presets/
```

### 2. Bake the plugin into the sandbox image

Copy the compiled plugin into the NemoClaw source tree so it's included
in the next image build:

```bash
mkdir -p ~/.nemoclaw/source/scripts/kars-mesh
cp -r mesh-plugin/dist/ ~/.nemoclaw/source/scripts/kars-mesh/dist/
cp mesh-plugin/openclaw.plugin.json ~/.nemoclaw/source/scripts/kars-mesh/
cp mesh-plugin/package.json ~/.nemoclaw/source/scripts/kars-mesh/
```

Then add this `COPY` to your NemoClaw `Dockerfile` (before the
entrypoint):

```dockerfile
COPY scripts/kars-mesh/ /sandbox/.openclaw-data/extensions/kars-mesh/
```

Rebuild the sandbox image.

### 3. Apply the egress preset

After the sandbox is running:

```bash
nemoclaw <sandbox-name> policy-add kars-mesh
```

The preset includes `allowed_ips` for SSRF override, so
`host.docker.internal` (private IP) is authorized without manual TUI
approval.

### 4. Pair and use

Inside the sandbox agent session:

> "Pair with kars using this token: azcp_1_eyJ..."

### How the proxy tunnel works

NemoClaw sandboxes route all egress through an HTTP CONNECT proxy.
The plugin automatically detects `HTTPS_PROXY` / `HTTP_PROXY` and:

1. Opens a raw TCP connection to the proxy (bypasses Node 22's undici interception)
2. Sends `CONNECT host:port` and waits for `200 Connection Established`
3. Runs the WebSocket upgrade inside the tunnel
4. Falls back to direct connection when no proxy is detected

No iptables changes or network hacks required — it works through
OpenShell's standard policy controls.

### Customising for production

The default preset uses `host.docker.internal` for local development.
For production deployments, edit the preset to use your public endpoints:

```yaml
endpoints:
  - host: relay.yourdomain.com
    port: 443
    access: full
    binaries:
      - { path: /usr/local/bin/node }
  - host: registry.yourdomain.com
    port: 443
    protocol: rest
    enforcement: enforce
    rules:
      - allow: { method: GET, path: "/**" }
      - allow: { method: POST, path: "/**" }
      - allow: { method: PUT, path: "/**" }
    binaries:
      - { path: /usr/local/bin/node }
```

Public hostnames don't need `allowed_ips` (no SSRF override required).

## Testing with OpenClaw (no sandbox)

1. Install OpenClaw on your machine
2. Install this plugin (see [Install](#install))
3. Register the plugin in your OpenClaw config
4. Start your agent: `openclaw agent --local`
5. Pair with your kars cluster (get token from admin)
6. Try: "Offload a task to analyze a simple math problem"

## Cluster admin setup

To enable federation on your kars cluster:

```yaml
# In your Helm values (deploy/helm/kars/values.yaml)
meshPeer:
  enabled: true
  relayUrl: "wss://relay.agentmesh.online/v1/connect"  # or your own relay
  clusterName: "my-kars-cluster"
```

Then upgrade:

```bash
helm upgrade kars deploy/helm/kars -n kars-system
```

Generate pairing tokens:

```bash
kars pair generate --name alice-laptop --budget 500000 --expires 30d
kars pair list
kars pair revoke alice-laptop
```

## Troubleshooting

| Problem | Solution |
|---------|----------|
| "Not paired" | Run `mesh_pair` with a token from your cluster admin |
| "Pairing expired" | Ask admin for a new token (`kars pair generate`) |
| "Connection lost" | Plugin auto-reconnects. If persistent, check relay URL |
| "No available slots" | Wait for current offload to finish, or ask admin to increase slots |
| "Budget exceeded" | Ask admin to create a new pairing with higher budget |
| Tools not showing | Verify plugin is in `plugins.allow` AND `plugins.entries` in OpenClaw config |
| ECONNREFUSED in sandbox | Apply the `kars-mesh` preset (`nemoclaw <name> policy-add kars-mesh`) |
| Proxy CONNECT denied | Check `allowed_ips` in preset matches the resolved IP. Run `nemoclaw <name> policy-list` to verify preset is applied |
| `engine:ssrf` in proxy log | The host resolves to a private IP. Add `allowed_ips` to the preset endpoint (see `nemoclaw/policies/presets/kars-mesh.yaml`) |

## Development

```bash
cd mesh-plugin
npm install
npm run build       # TypeScript → dist/
npm test            # vitest
npm run typecheck   # tsc --noEmit
npm run lint        # oxlint
```

## License

MIT
