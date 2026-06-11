# Operator UX — Headlamp plugin, mesh inspector, dashboards

Post 7 in the [kars blog series](README.md).

---

## The premise

A platform is only as good as the day-2 experience. Kars ships:

- A **Headlamp plugin** as the primary operator UI — agent overview, sandbox details, mesh peers panel, embedded chat with the SRE agent, action approval surface.
- **Grafana dashboards** for fleet-wide telemetry (token spend, mesh frame counts, recovery observer health, model latency).
- **A small CLI** (`kars sre`, `kars connect`, `kars mesh`) for the things that aren't worth a UI.

We deliberately did not write a bespoke web app. Headlamp gave us auth + RBAC + cluster-switching + namespace selection + multi-cluster federation for free.

---

## Why Headlamp

The options for "operator UI on top of Kubernetes" are:

1. **Lens** — closed-source UI from Mirantis, plugin model is OK but not first-class.
2. **K9s** — terminal UI, great for power users, no place for chat or dashboards.
3. **Bespoke React app** — full control, but you re-implement auth + kubeconfig handling + apiserver-proxy + RBAC presentation from scratch.
4. **Headlamp** — Kinvolk/Microsoft OSS, first-class plugin model, ships its own bearer-token-aware apiserver-proxy, multi-cluster support, themes, integrates with K8s RBAC. Plugins are React components that can mount custom pages, sidebars, and resource detail panels.

We picked Headlamp. The kars plugin is at `headlamp-plugin/`, packaged as a Headlamp extension, signed and published to the Headlamp plugin registry.

---

## What the plugin shows

### Overview page

- Cluster health summary (controller pod ready, every kars CRD installed, every InferencePolicy reconciled).
- Per-sandbox row with workload-aware Phase column. A `KarsSandbox` with `status.phase: Running` but Deployment `0/1` shows up as `Workload down` in red. (This is the same overlay the SRE agent uses — see [post 4](04-autonomous-sre.md).)
- Active incidents (pending `KarsSREAction` proposals awaiting approval).
- Token budget rollup (today / week / month).

### Sandboxes list

- One row per `KarsSandbox`, sorted by namespace.
- Columns: name, runtime, phase, workload availability, inference policy, isolation tier, age.
- Click-through to the sandbox detail page.

### Sandbox detail

- The CRD spec, rendered (and editable for non-spec fields via apiserver-proxy patch).
- Status conditions chain with timestamps — the operator-facing source of truth.
- Linked policy CRDs (`InferencePolicy`, `ToolPolicy`, `KarsMemory`, `McpServer`).
- Recent reconcile events.
- Quick links to pod logs / shell / dashboard.

### Chat tab (embedded Hermes PTY)

This is the surprise feature. For Hermes-runtime sandboxes (which the SRE agent uses), the plugin opens an iframe to `localhost:19119` (the operator's `kubectl port-forward` to `svc/<sandbox> 19119:9119`). Inside the iframe is the Hermes TUI — full chat, tool calls, session memory — running in the sandbox pod. The operator can ask the SRE agent "what's wrong?" and get a structured answer based on `sre_diagnose` results, in-place.

We landed on the port-forward + iframe pattern after fighting with apiserver-proxy for an afternoon. The apiserver-proxy doesn't apply bearer-token auth to iframe asset loads (browser security boundary), so subresources 401'd. Port-forward avoids the apiserver-proxy entirely; the iframe loads from `localhost`, which carries no apiserver credentials. The trade-off is the operator has to start the port-forward separately, but Headlamp's UI surfaces the exact command.

### Mesh peers panel

- One row per peer pair (sender DID → receiver DID), with the current trust score and interaction count.
- Last KNOCK outcome.
- Scrollback of recent envelope counts (sent/received over the last hour).

The data comes from the `kars_mesh_messages_{sent,received}_total` Prometheus counters that the router emits, plus the in-cluster `TrustGraph` CR projections.

### SRE Console

- Pending action proposals — pretty-printed `KarsSREAction` CRs awaiting approval.
- One-click approve / reject (POSTs the appropriate patch against the apiserver-proxy with the operator's bearer token, so the action is audited under the operator's identity).
- Action history — recent `Recovered` / `Failed` actions with the operator who approved them and the time-to-recover.

---

## The Grafana dashboards

We ship two dashboards in the Helm chart (`deploy/monitoring/`):

1. **`kars-fleet`** — fleet-wide view. Token spend per sandbox per day, model latency p50/p99, error rates, mesh frame volume, governance denials, content-safety blocks.
2. **`kars-ops`** — operator's pager view. SRE action funnel (Proposed → Approved → Applied → Recovered), recovery-window violations (the late-recovery healer firings), workload-down sandboxes, controller reconcile error rate.

The PodMonitor scrape rule labels each scrape with `sandbox=<name>` + `sandbox_namespace=<ns>` via relabeling. This is what lets the fleet dashboard split everything by sandbox without each pod knowing its own name.

If the dashboards aren't showing up in your Grafana, it's almost always the sidecar configmap discovery. We had this break twice in PR review — the fix is in `043ee6` if you want the exact incantation.

---

## The CLI

`kars` is a Node 22 TypeScript CLI with these subcommands relevant to day-2:

- `kars sre install` — installs the SRE agent into the cluster. Handles 3 cluster shapes: helm-release-managed, `kars dev --target local-k8s` (which `helm template | kubectl apply`s without a release record), and brand-new no-chart-at-all. Idempotent.
- `kars sre approve <action-id>` — patches a `KarsSREAction` to `Approved`.
- `kars sre list` / `kars sre show <action-id>` — list/inspect actions.
- `kars connect <sandbox>` — port-forward to a sandbox's chat/dashboard endpoint.
- `kars mesh status` — show the mesh peer graph for the cluster.
- `kars credentials update <sandbox> --telegram-token <...> --brave-key <...>` — rotate channel/plugin credentials without restarting pods (until the next reconcile, anyway).
- `kars push` / `kars up` / `kars dev` — build, push, deploy.

The CLI is intentionally small. Things that change cluster state are CRDs you `kubectl apply`; things that need an interactive UX are the Headlamp plugin; the CLI is for the gaps between those.

---

## What's NOT in the operator surface

- **No PR review workflow.** Approvals happen in Headlamp (UI) or via `kars sre approve` (CLI). No GitHub-PR-style review threading.
- **No multi-cluster fleet view.** Headlamp's own cluster-switcher handles multi-cluster. We don't synthesize a cross-cluster aggregated view; each cluster is its own Headlamp tab.
- **No bespoke alerting backend.** Telegram is wired in for the SRE pager (configurable). Beyond that, the OpenTelemetry telemetry can feed Alertmanager / App Insights / your alerting tool of choice.
- **No agent IDE.** Kars is the runtime + governance + ops surface, not the agent-authoring environment. Use whatever your runtime's framework provides (Hermes has a TUI, OpenClaw has its own author surface, MAF integrates with VS Code).

---

## Where to look

- **Headlamp plugin source:** `headlamp-plugin/` (TypeScript + React).
- **Plugin entry:** `headlamp-plugin/src/index.tsx`. Each registered component is a separate file under `src/pages/`.
- **Grafana dashboards:** `deploy/monitoring/grafana-dashboards/`.
- **PodMonitor:** `deploy/monitoring/podmonitor-sandbox-router.yaml`.
- **CLI sources:** `cli/src/commands/`.

---

## Up next

You've reached the end of the kars blog series. The full list, in case you want to revisit:

1. [Kars in 10 minutes](01-kars-in-10-minutes.md) — the lead post.
2. [AgentMesh deep-dive](02-agentmesh-deep-dive.md) — Signal Protocol between agents.
3. [Governance plane](03-governance-plane.md) — nine CRDs that compose into a policy.
4. [The autonomous SRE agent](04-autonomous-sre.md) — five minutes of trust per fix.
5. [Multi-runtime](05-multi-runtime.md) — eight agent frameworks, one trust boundary.
6. [Sandbox anatomy](06-sandbox-anatomy.md) — what's inside one agent pod.
7. [Operator UX](07-operator-ux.md) — this post.

If you found gaps, errors, or topics worth their own follow-up post: open an issue against `Azure/kars` with the `blog` label, or just amend the post in question. The series is meant to evolve.
