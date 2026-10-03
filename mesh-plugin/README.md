<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

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

## Received-message encryption evidence

`IMeshTransport.onMessage` handlers and `waitForMessage` / `sendWithAck`
predicates receive a third argument: `"encrypted"`, `"plaintext"`, or `"unknown"`.
Inbox records carry the same `security` field. Only the SDK's explicit successful
encrypted receive path yields `"encrypted"`; missing metadata is `"unknown"`.
Payload fields and a previous encrypted message from the same peer cannot upgrade
that evidence. Existing two-argument callbacks remain supported.

Consumers requiring encrypted delivery must check each message's evidence,
including acknowledgements. Explicit plaintext compatibility peers remain
plaintext. Encrypted sends establish an SDK session first and propagate handshake
failure without falling back to plaintext. This evidence describes transport,
not permission to execute an instruction or proof that work was completed.

## Gated mission receiver (not yet a complete dispatch path)

`dist/mission-protocol.js` defines the version-1 `mission:` protocol used by the
OpenClaw runtime receiver. Messages are limited to 192 KiB and bind the Task UID,
Sandbox UID, Pod UID, run nonce, agent DID and dispatcher DID. An encrypted probe
returns the current runtime boot ID; assignments additionally bind that boot ID
and an assignment ID. The receiver requires per-message encrypted evidence and
the pinned dispatcher, not names or payload assertions of trust.

The receiver is disabled unless `KARS_MISSION_DISPATCH_ENABLED=true`. Enabling it
requires `KARS_MISSION_TASK_NAME`, `KARS_MISSION_TASK_UID`,
`KARS_MISSION_SANDBOX_UID`, `KARS_MISSION_POD_UID`,
`KARS_MISSION_DISPATCHER_DID` and a 64-hex `KARS_MISSION_IDENTITY_ROOT`.
The enabled runtime derives its signing identity from this root, the `runtime`
role, Sandbox UID and Pod UID using HMAC-SHA256 over the JSON array
`["kars-mission-identity-v1", role, ownerUid, podUid]`. Receiver validation checks
that the supplied DID matches this derivation as well as the complete binding.
It cannot prove root entropy, Secret provenance or controller ownership.
`KARS_IDENTITY_SEED` and public application IDs cannot substitute for this root.
The current chart/controller do not yet provide these bindings or a mission
dispatcher; the enabled path remains source-only.

Reserved messages never fall through to legacy task handlers, even when the
receiver is disabled. Reassembled legacy chunks are rejected because they lack
aggregate encryption evidence. The existing process-wide SDK singleton remains.
Before deriving a mission identity or creating its SDK client, runtime bootstrap
also acquires an exclusive loopback listener on `127.0.0.1:19791`. Its acquisition
promise and successful listener are retained for the process lifetime, including
after validation or SDK failure. A second initialization fails closed and
requires a process restart. This is a cooperative same-Pod prekey-writer guard,
not an authorization boundary against malicious processes or cross-Pod fencing.
The disabled mission path keeps legacy identity behavior unchanged.

Transport reconnects serialize connection retirement and reuse one SDK client
and key manager. SDK automatic reconnect is disabled in favor of the wrapper's
bounded-backoff retry. A replacement socket cannot open before the old actual
socket closes; ambiguous registration, timeout or failed cleanup quarantines the
transport instead of creating another writer. Sends reject if their connection
changes during session establishment. Heartbeat reconnect status comes from the
actual connection, not a cached success flag. Already-running upstream SDK
asynchronous handlers are not fully generation-fenced by this wrapper.

The opt-in real-relay test uses fresh identities for six encrypted messages over
two reconnect cycles, checking exact payloads, unchanged DIDs and actual socket
closure. It proves neither Entra authentication nor useful mission delivery.

Execution uses the real runtime tool loop through the local inference router:

- Assignment and tool authorization fail closed on policy errors; only HTTP 200
  with boolean `allowed: true` authorizes the action. Shell commands require a
  separate policy decision, including in the legacy loop.
- Strict requests use `max_completion_tokens: 8192` (legacy requests keep 2048)
  and stop after at most 25 rounds. Unknown tools cannot fall back to shell.
- Success requires nonempty final output, `finish_reason: stop` and positive,
  validated provider usage. Missing/invalid usage or an unresolved later request
  makes whole-run usage unknown; earlier totals are not reported as complete.
- Accepted, running, succeeded, failed and rejected replies are distinct. Exact
  duplicate assignments replay cached replies without executing twice; changed
  assignments with the same nonce are rejected. Execution is serialized.

Replay protection is **in memory for one runtime boot**, capped at 128 accepted
assignment records without eviction. It is not durable recovery. The dispatcher
library below adds persistent send exclusion; neither library is currently wired
into a deployed execution path. Receiver tests do not qualify useful mission
delivery, persistent teams or either beta gate.

### Durable dispatch and result storage

`mission-dispatcher.ts` uses the official encrypted transport and
`mission-store.ts` persists attempts through `kubernetes-json.ts`, an in-cluster
HTTPS client with certificate verification, rotating service-account credentials,
request/response size limits and an overall request deadline.

- A readiness probe checks the exact encrypted sender, runtime boot and target.
  A Kubernetes ConfigMap CREATE claims `(Task UID, run nonce)` **before** the sole
  assignment-send attempt. Competing dispatchers and restarts cannot resend that
  claim. This is at-most-one send attempt, **not exactly-once execution**.
- Currentness checks require a launched, observed Running Task, matching objective,
  Task-owned runtime binding and Sandbox, and the exact Ready Pod UID. Revision
  objectives must match their nonce and SHA-256 digest. These checks depend on
  controller-owned bindings and appropriate write authority; owner references
  alone are not an authorization boundary.
- Authenticated replies append to a bounded, resourceVersion-CAS event journal.
  Acceptance precedes ACK; terminal evidence is persisted before publication.
  Lost write responses preserve the durable state rather than authorizing resend.
- Actual output and `response.md` are archived in immutable per-run ConfigMaps,
  with Task UID/run/assignment/agent attribution and measured usage. Unknown usage
  is omitted, never manufactured as zero. Failed runs do not produce an artifact.
  Task UID and run nonce annotations also identify artifact ConfigMaps.
- Task-name projections and completion use optimistic concurrency. They are not
  a transaction across resources: consumers must validate Task UID and requested
  nonce before treating output or artifacts as current. Historical evidence is
  separate from the current run. Terminal publication may be retried without
  contacting the runtime, including after a transport identity change.

Bridge's current output, artifact and download readers validate the Task owner,
UID and requested/completed nonce after reading the projection. Unbound legacy
records remain historical, not current delivery. Historical deduplication includes
Task identity as well as the nonce. Team harvesting applies the same binding
checks before accounting or writing shared memory, and keys memory entries by
Task UID plus run nonce so distinct revisions do not overwrite one another.
Retirement retains resourceVersion concurrency checks. These reads are not
multi-resource transactions, and unavailable reads still need distinct UI states;
this change does not repair live router telemetry or deploy the dispatcher.

Bridge requests a run using the authorized launched Task's UID/resourceVersion
preconditions. A pending nonce is reused; conflicting or ambiguous writes surface
an error and never trigger a direct model call or a second execution path. Waiting
is bounded across API reads and sleeps, and completion must match the requested
Task UID and nonce. Missing ACKs do not prove non-delivery. Pending HTTP responses
have no output, model, token usage or completion timestamp; `ok: true` there means
request accepted, not mission completed.

Feedback requires the exact currently completed run and a valid bound review
journal. It creates a distinct revision nonce and stores the objective with that
nonce and its SHA-256 digest in one Task CAS. Reviews use ConfigMap CREATE or
resourceVersion-guarded replacement: concurrent reviews cannot adopt or overwrite
a newer journal. Reading review state is side-effect-free, and only the requested
revision's completion clears derived pending state. Artifact/efficiency approval
attribution requires both nonempty Task UID and run nonce, including historical
records. Task revision and review journal writes are **not a transaction**: if the
revision commits but the journal fails, the request errors while preserving the
pending revision; replay cannot dispatch it again or undo it. Unbound legacy
journals must not be silently adopted. Retired Tasks require normal relaunch.

Nonterminal and uncertain claims are never automatically retried. A send timeout
cannot cancel an already-started transport send, and a crashed dispatcher's claim
may remain pending. The `ownerSession` field is not a leadership lease or process
fence. Controller binding/Secret producers, helper lifecycle and single-prekey-
writer enforcement, Helm/RBAC wiring, orphan handling, correlated Bridge readers
and live end-to-end execution remain integration work. These source-level tests
are not evidence of a delivered mission.

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
