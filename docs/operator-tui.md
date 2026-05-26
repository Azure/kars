# Operator TUI — modular panels

> Source: `cli/src/commands/operator/panels/`.

`kars operator` is the operator's terminal dashboard for live cluster
state. It surfaces every shipped CRD plus a per-sandbox provider-status panel.

## Quick start

```bash
kars operator                                 # full live TUI
kars operator --panels mcpserver,toolpolicy   # filter to two panels
kars operator --per-sandbox                   # group panels by sandbox
kars operator --snapshot                      # one-shot stdout render
```

In the live TUI, press **Shift-P** to toggle the modular-panels overlay.
Press **Esc** or **q** to close it.

## Panel inventory

| ID                  | CRD / source                             | What it shows |
|---------------------|------------------------------------------|---------------|
| `karssandbox`       | `KarsSandbox`                            | Name, health, model, isolation, age, role |
| `karspairing`       | `KarsPairing`                            | agentA ↔ agentB, trust state, Conditions |
| `mcpserver`         | `McpServer`                              | URL, productionMode, tool count, **JWKS Secret presence**, Conditions |
| `toolpolicy`        | `ToolPolicy`                             | `appliesTo`, commerce (mandates + floor), approval, rate-limit, Conditions |
| `inferencepolicy`   | `InferencePolicy`                        | budgets (daily/per-req), guardrail floor, ordered model preference, Conditions |
| `a2aagent`          | `A2AAgent`                               | endpoint URL, productionMode, AgentCard publication status, capabilities |
| `karsmemory`        | `KarsMemory`                             | sandboxRef, store, scope, retention, Foundry binding, RBAC scope summary |
| `karseval`          | `KarsEval`                               | sandboxRef, suite, schedule, lastRunAt, lastScore, nextScheduledAt |
| `provider_status`   | derived (router `/healthz`, kubectl events, SA annotations, Gateways) | per-sandbox + cluster-wide provider health |

## Flags

| Flag | Default | Effect |
|------|---------|--------|
| `--panels <a,b,c>` | all | Comma-separated panel ids to render. Unknown ids are silently dropped. |
| `--per-sandbox` | off | Group panels vertically per sandbox-name; each sandbox gets its own block (`══ Sandbox: NAME ══`). |
| `--snapshot` | off | Render once to stdout (blessed tags stripped) and exit. |
| `--context <ctx>` | current | Kubernetes context. |

## Layout (default)

```
┄ KarsSandbox ┄
NAME              HEALTH      MODEL          ISOLATION      AGE     ROLE
sb-1              healthy     gpt-4.1        enhanced       12m     controller
sb-2              healthy     gpt-4.1        enhanced       4m      controller
────────────────────────────────────────────────────────────────────────
┄ KarsPairing ┄
alice-bob (kars) 1h
  pair: alice ↔ bob   state=Active   trust=Verified
  ● Paired=True HandshakeComplete: X3DH ok
────────────────────────────────────────────────────────────────────────
┄ McpServer ┄
mcp-fs (kars-sb-1) 20m
  url: http://mcp-fs.kars-sb-1.svc:8080   production=yes   tools=4
  jwks-secret: <present>
  ● Ready=True Reachable
  ● Authenticated=True JWKSValidated
────────────────────────────────────────────────────────────────────────
┄ ToolPolicy ┄
tp-default (kars) 2h
  appliesTo: sb-1   rules=6   approval=yes   rate-limit=30/min
  commerce: mandates=yes   floor=$5.00
  ● Programmed=True PolicyCompiled
────────────────────────────────────────────────────────────────────────
┄ InferencePolicy ┄
ip-default (kars) 2h
  appliesTo: sb-1   tokens: daily=100000   per-req=4000
  guardrail-floor: high
  models: 1.gpt-4.1 → 2.gpt-4o
  ● Programmed=True PolicyCompiled
────────────────────────────────────────────────────────────────────────
┄ A2AAgent ┄
agent-card-1 (kars-sb-1) 10m
  endpoint: https://example.test/a2a   production=yes
  AgentCard: published
  capabilities: tasks, streaming
  ● CardPublished=True Signed
────────────────────────────────────────────────────────────────────────
┄ KarsMemory ┄
mem-1 (kars) 5h
  sandbox: sb-1   store=store-default   scope=user-123   retention=30d
  foundry-binding: bound
  rbac: project-MI: Azure AI User on RG
  ● Bound=True MemoryStoreReady
────────────────────────────────────────────────────────────────────────
┄ KarsEval ┄
eval-nightly (kars) 1d
  sandbox: sb-1   suite=rag-quality   schedule=0 2 * * *
  last-run: 2026-04-29T02:00:00Z   score: 0.91   next: 2026-04-30T02:00:00Z
  ● Scheduled=True CronProgrammed
────────────────────────────────────────────────────────────────────────
┄ Providers ┄
Per-sandbox: sb-1
  ● foundry            healthy
  ● agt                healthy
  ● acr                healthy
  ? identity           unknown — no workload-identity annotation

Cluster-wide
  ? agc                unknown — no Gateway objects (a2a-ingress not enabled)
────────────────────────────────────────────────────────────────────────
```

Per-sandbox layout (`--per-sandbox`):

```
══ Sandbox: sb-1 ══
┄ KarsSandbox  (sb-1) ┄
…panels filtered to sb-1…
══ Sandbox: sb-2 ══
┄ KarsSandbox  (sb-2) ┄
…
```

## Provider-status interpretation

The provider panel **never invents data** — every status is the verbatim outcome of a probe. Possible values:

| Status      | Meaning |
|-------------|---------|
| `healthy`   | Probe succeeded. |
| `degraded`  | Partial outage; see `reason`. |
| `down`      | Probe failed with a definitive negative answer. |
| `unknown`   | Probe couldn't observe the truth (RBAC denial, no Gateway objects, network unreachable, etc.). The `reason` is verbatim from the probe — do not interpret it as "broken". |

Per-sandbox providers (`Foundry`, `AGT`, `ACR pull-through`, `Identity (WI)`)
are all observed via the inference-router or kubectl. Cluster-wide providers
(today: `AGC ingress`) are observed via Gateway / HTTPRoute objects.

## Architecture

```
operator command  ───►  KubectlDataSource ──► ClusterState
                                                  │
                                                  ▼
                              renderDashboard(state, opts)
                                                  │
                                                  ▼
                                     ┌────────────┴────────────┐
                                     │  Panel.render(state)    │
                                     └─────────────────────────┘
```

- A **`Panel`** is a tiny pure function over `ClusterState`. Empty-cluster
  rendering is required; every panel test exercises it.
- A **`ClusterDataSource`** (`KubectlDataSource` for live, `FixtureDataSource`
  for tests) owns all kubectl traffic — no panel issues kubectl directly.
- The **layout helper** (`renderDashboard`) handles `--panels` filtering and
  `--per-sandbox` grouping. Panels are immutable; layout is a pure function.

## Security posture

- TUI is **read-only**. No destructive verbs are exposed.
- RBAC scope = the operator's current kubeconfig context. Any visibility
  failure surfaces as `unknown` with the verbatim error.
- **No secret bytes are rendered.** JWKS / token / API-key fields collapse
  to `<present>` / `<missing>` via `panels/redact.ts`.
- No telemetry is emitted from the TUI.
