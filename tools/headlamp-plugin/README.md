# AzureClaw Headlamp Plugin

Adds an **AzureClaw** sidebar to the [Headlamp](https://headlamp.dev/) Kubernetes
dashboard with list + detail views for the 9 AzureClaw custom resources:

- ClawSandbox
- InferencePolicy
- ClawMemory
- McpServer
- A2AAgent
- ToolPolicy
- TrustGraph
- ClawPairing
- ClawEval

Detail panes show `.spec`, `.status`, and a typed Conditions table with
status colouring (Ready / Provisioned → green, Degraded / Failed → red,
everything else → amber).

> **Status chip semantics.** Chips are **reason-aware**: when the
> `Ready` condition carries a hard-failure reason
> (`SignatureMismatch`, `BundleVerifyFailed`, `AuthMisconfigured`,
> `MemoryStoreMissing`, `RuntimeAdapterMissing`, `ShapeInvalid`,
> `AllowlistDrift`, `PolicyCompileFailed`) the chip renders **red**;
> transient reasons (`AwaitingRouterEnforcement`,
> `AwaitingFoundryProvisioning`, `NoSandboxesReferencing`, `Pending`)
> render **amber**. The reason is appended as a secondary muted label.
> The ClawSandbox detail pane renders **all** servers referenced by
> `governance.mcpServerRefs[]` via `McpServerFleetCard` (per-server
> phase + reason + JWKS digest + tool count + `MISSING` chip for
> dangling refs).

## Build

```bash
npm install
npm run build       # → dist/main.js
```

The CLI's `azureclaw dev --target local-k8s` builds this
automatically and side-loads `dist/` into the Headlamp pod via
`kubectl cp` to `/headlamp/plugins/azureclaw/`.

## Standalone install (manual)

```bash
# Build
npm install && npm run build

# Copy into a running Headlamp pod
POD=$(kubectl get pod -n headlamp -l app.kubernetes.io/name=headlamp -o jsonpath='{.items[0].metadata.name}')
kubectl cp dist headlamp/$POD:/headlamp/plugins/azureclaw
kubectl rollout restart deployment/headlamp -n headlamp
```

## Adding a new CRD

The plugin is data-driven. Append one entry to `AZURECLAW_CRDS` in
`src/index.tsx` — list/detail routes and the sidebar entry are
generated automatically. No per-CRD boilerplate required.

## Mesh Topology + observability

The **Mesh Topology** sidebar entry renders a live SVG tree of the AGT mesh:

```
                  AGT Relay
                /     |     \
      controller   controller   controller
       / | \         |           / \
      sub sub sub   sub         sub sub
```

Layout is built from `ClawSandbox` CRs grouped by the
`azureclaw.azure.com/parent=<name>` label (the same label the
`azureclaw operator` CLI uses for its blessed-TUI topology).
Sandboxes without the label are controllers (top tier); sandboxes
with it are sub-agents fanned out under their parent.

### Data sources

| Datum | Prometheus query (5 s poll) |
|-------|-----------------------------|
| Per-sandbox mesh msgs sent (lifetime) | `azureclaw_mesh_messages_sent_total` |
| Per-sandbox mesh msgs received (lifetime) | `azureclaw_mesh_messages_received_total` |
| Per-sandbox mesh msgs sent (5 m increase) | `sum by (sandbox) (increase(azureclaw_mesh_messages_sent_total[5m]))` |
| Per-sandbox mesh msgs received (5 m increase) | `sum by (sandbox) (increase(azureclaw_mesh_messages_received_total[5m]))` |
| Local AGT trust-graph size | `azureclaw_agt_known_agents` |
| Relay connected agents | `sum(agentmesh_relay_connected_agents)` |
| Relay throughput (msg/s, 5 m) | `sum(rate(agentmesh_relay_messages_routed_total[5m]))` |
| Relay totals (routed / stored / delivered) | `sum(agentmesh_relay_messages_{routed,stored,delivered}_total)` |

The `sandbox=<name>` label on every per-sandbox metric is added at
**scrape time** by the `azureclaw-sandbox-router` PodMonitor in
`deploy/monitoring/podmonitor-sandbox-router.yaml` (relabel:
`__meta_kubernetes_pod_label_azureclaw_azure_com_sandbox` →
`sandbox`). The Rust router exports plain `IntCounter`s — no
in-process per-sandbox cardinality.

### What the counter actually counts

`azureclaw_mesh_messages_{sent,received}_total` ticks **once per
WebSocket frame** that the inference router proxies between OpenClaw
and the relay (in `inference-router/src/routes/mesh.rs`). Concretely:

- ✅ **Counted**: KNOCK (Signal-Protocol session establishment),
  X3DH bundle exchange, every encrypted `mesh_send` call, and the
  explicit `sendHeartbeat()` ticks `agt-transport.ts` schedules
  every 30 s (vanilla AGT MeshClient doesn't auto-heartbeat —
  see `vendor/` patch notes).
- ❌ **Not counted**: WebSocket `Ping` / `Pong` keepalives — the
  router short-circuits these with `continue` before the
  `fetch_add`. Registry HTTP calls (`/v1/agents/...`,
  `/v1/agents/{did}/heartbeat`) also don't count — those go over
  HTTP, not the WS relay.

**Why sent ≫ received early on:** a fresh sandbox emits ≥ 1 KNOCK
per peer it's aware of (e.g. execbrief with 3 sub-agents → ≥ 3
KNOCKs) plus a heartbeat every 30 s, but only receives back the
relay's KNOCK-ack (single inbound frame) until a real conversation
starts. The counters reset on pod restart — they live in the
router process, not the relay.

### Per-node decorations

For each controller / sub-agent circle the plugin renders:

- `↑<sent_lifetime> ↓<received_lifetime>` (counter values)
- `N children · M trust`, where:
  - **children** = sub-agent CRs labeled
    `azureclaw.azure.com/parent=<this>`. Deterministic; comes
    straight from the Kubernetes API.
  - **trust** = peers in *this* router's local AGT trust graph
    (`azureclaw_agt_known_agents`). Only populates after live
    traffic and resets to 0 on pod restart — so for a freshly
    restarted controller you'll see `3 children · 0 trust` until
    the first KNOCK round-trip completes.

Edge thickness ∝ traffic (5 m rate, sum of sent+received). Two
pulse colours per relay-edge:

- **Yellow** (sandbox → relay): outbound msgs, speed ∝ sent rate
- **Light blue** (relay → sandbox): inbound msgs, speed ∝ recv rate

Controller → sub-agent edges are dashed (logical hierarchy; the
actual frames still flow via the relay).

### Token Budget panels

The Overview page and each ClawSandbox detail page also render a
**💰 Token Budget (24 h)** card that joins:

- `InferencePolicy.spec.tokenBudget.{dailyTokens,perRequestTokens}`
  (read via Headlamp's Kubernetes client)
- `sum by (sandbox) (increase(azureclaw_tokens_total[24h]))` from
  Prometheus

The sandbox → policy link uses
`ClawSandbox.spec.inferenceRef.name`.

### Prometheus base URL

By default the plugin queries `http://127.0.0.1:19091`. Override at
runtime by setting `window.AZURECLAW_PROMETHEUS_URL = '...'` (e.g.
via a small Headlamp banner / browser DevTools).
