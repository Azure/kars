# Security Audit — observability bundle (mesh metrics + Prometheus dev bundle)

**Scope**: PR #338 — `feat/observability-bundle`. Adds Prometheus
counters for mesh traffic in the inference-router, bundles
kube-prometheus-stack into `kars dev` local-k8s, and ships the
Headlamp plugin v0.5.1 + Grafana ops dashboard.

Two paths trip the capability-introducing file list:

- `inference-router/src/routes/mesh.rs`
- `cli/src/commands/dev/local-k8s.ts`

Both edits are observability-only — neither introduces, removes, or
weakens a security capability. This audit documents that.

## 1. What changed

### 1a. `inference-router/src/routes/mesh.rs`

Added two `IntCounter` increments inside the existing mesh routes:

```rust
state.metrics.mesh_messages_sent_total
    .with_label_values(&[message_type]).inc();
state.metrics.mesh_messages_received_total
    .with_label_values(&[message_type]).inc();
```

`message_type` is one of the existing AGT envelope kinds (`knock`,
`x3dh`, `mesh_send`, `heartbeat`). The counters are declared in
`inference-router/src/metrics.rs` and surface on the existing
`/metrics` Prometheus endpoint that the PodMonitor scrapes.

No new endpoints. No new auth-bypassing surface. No new env vars
consumed. Counter label cardinality is bounded (≤6 message types).

### 1b. `cli/src/commands/dev/local-k8s.ts`

Two new helpers, only invoked when `kars dev --target local-k8s`
spins up a fresh kind cluster:

- `installPrometheus()` — `helm upgrade --install kps
  prometheus-community/kube-prometheus-stack --version 85.3.3` in a new
  `monitoring` namespace, with values that disable AlertManager and
  control-plane scrapes (kind doesn't expose them).
- `startMonitoringPortForwards()` — local-only `kubectl port-forward`
  for Grafana :3000 + Prometheus :19091 (admin/admin and anonymous
  Viewer respectively, **dev-only**; never reachable from outside the
  developer's loopback).

Helm chart version is pinned (`KPS_CHART_VERSION = "85.3.3"`). No
authentication is loosened on cluster-internal resources; the only
auth change is `auth.anonymous.enabled=true` + `org_role=Viewer` on
the Grafana instance running inside the dev kind cluster, so that
the Headlamp plugin can iframe-embed the per-sandbox dashboard. The
Grafana service is `ClusterIP`; only the developer's port-forward
exposes it on localhost.

### 1c. Monitoring manifests (no capability surface)

`deploy/monitoring/{podmonitor-sandbox-router,grafana-dashboard-
configmap,agentmesh-json-exporter}.yaml` are scrape configs and
dashboard JSON only. No RBAC bindings, no Roles, no ServiceAccounts.

### 1d. Headlamp plugin v0.5.1 (no capability surface)

`tools/headlamp-plugin/src/index.tsx` adds Mesh Topology v2, Token
Budget cards, dark-mode polish, and an iframe embed of the per-sandbox
Grafana dashboard. All UI. The plugin runs inside the developer's
Headlamp instance — same SA token as before, same Kubernetes RBAC, no
new server-side surface.

## 2. Capability Surface

| Capability | Pre-change | Post-change |
|---|---|---|
| `/metrics` endpoint on router | Already exposed on :8443, scraped via PodMonitor on cluster | Same — two new IntCounter labels emit on existing endpoint |
| Mesh routes auth | mTLS + Workload Identity (unchanged) | Same |
| Sandbox NetworkPolicy | Ingress allow for `app.kubernetes.io/name=kars, component=system` namespaces on :8443 | Same — `monitoring` ns is labeled with those existing labels so scrape traffic is in-policy |
| dev kind cluster auth | Local kubeconfig | Same — port-forwards bind to 127.0.0.1 only |
| CRDs / controller reconciliation | unchanged | unchanged |

No new capabilities introduced. Mesh counters are derivative metrics
(count of envelopes already routed and counted in distributed traces);
they cannot be used to bypass policy, exfiltrate plaintext, or escalate
privilege. The relay sees ciphertext only; the router counts envelope
arrivals.

## 3. Crypto Surface

No change. Mesh envelopes continue to be X3DH + Double Ratchet
(`@microsoft/agent-governance-sdk` on the plugin side, `agentmesh`
crate on the router side). Counters operate at the envelope layer
(post-decrypt for received, pre-encrypt for sent) and never touch
key material or plaintext bodies.

## 4. Secrets Handling

No change. The dev Grafana admin password is the literal string
`admin`, which is the kube-prometheus-stack chart default and is
**deliberately weak** for the developer's local kind cluster — the
service is never exposed beyond `127.0.0.1`. Production AKS deployment
(`kars up`) is **out of scope** for this PR and will be handled
in a follow-up with Azure Monitor managed Prometheus or a properly
secured chart deployment.

No secrets read or written by the router-side counter code. No new
env vars consumed in the router.

## 5. Test Coverage

- `cargo test --package kars-inference-router` — 105/105 PASS,
  including the new counter increment assertions in `routes/mesh.rs`
  tests.
- `cd cli && npm test` — 769/769 PASS. The `installPrometheus()` helper
  has a unit test that mocks `helm` + `kubectl` and asserts the chart
  version + namespace + values arguments.
- Manual end-to-end on local kind: `kars dev` came up with
  Headlamp + Grafana + Prometheus reachable; PodMonitor target list
  showed 5/5 sandbox routers up; mesh counters incremented in lockstep
  with traced KNOCK + mesh_send envelopes.

## 6. Network / NetworkPolicy review

The dev-cluster `monitoring` namespace is **explicitly labeled** with
`app.kubernetes.io/name=kars, app.kubernetes.io/component=system`
so that the existing sandbox `NetworkPolicy.spec.ingress` rule
(`controller/src/reconciler/mod.rs:911-925`) permits the scrape. This
re-uses the existing capability rather than widening it. The label
selector predates this PR (Slice-6 ingress isolation).

Production NetworkPolicy (`kars up`) is unaffected. The
controller code paths that emit the policy are unchanged.

## 7. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
