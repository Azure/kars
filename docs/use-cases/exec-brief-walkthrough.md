# Exec-brief walkthrough — a four-agent showcase

This page walks a real, reproducible end-to-end scenario: **one parent agent orchestrates three sub-agents to produce a two-page executive brief on the 2026 state of agentic AI runtimes.** It exists for one reason: when somebody asks "what does AzureClaw actually do, and what is it enforcing for me?", this is the answer you can point at, run, and observe.

The scenario lives at [`tools/e2e-harness/scenarios/exec-brief/`](https://github.com/Azure/azureclaw/tree/main/tools/e2e-harness/scenarios/exec-brief). It currently runs on AKS. The platform matrix below is honest about what works where today.

## Scenario in one sentence

A `parent` agent receives a prompt, spawns three sub-agents (`analyst`, `viz`, `writer`), each does its slice (web search + JSON build, chart + hero image, two-page markdown brief), files flow over the encrypted mesh, and the writer's output is sent back to the parent which delivers it via Telegram.

The pipeline exercises, on purpose, every enforcement layer worth talking about: signed CRDs, the iptables egress-guard, the router L7 allow-list, content safety, mesh E2E encryption, Foundry hosted-tool calls, MCP, the seccomp profile, and the per-sandbox NetworkPolicy.

## What you see at the top

```mermaid
flowchart LR
  U([Operator]) -->|telegram message| TG[Telegram Bot API]
  TG --> P
  subgraph AKS["AKS cluster"]
    direction LR
    P[parent sandbox] -->|mesh KNOCK + dispatch| A[analyst sandbox]
    P -->|mesh KNOCK + dispatch| V[viz sandbox]
    P -->|mesh KNOCK + dispatch| W[writer sandbox]
    A -.->|analyst.json via mesh| V
    A -.->|analyst.json via mesh| W
    V -.->|scorecard.png + hero.png via mesh| W
    W -.->|brief.md via mesh| P
  end
  P -->|telegram reply| TG --> U
  A -.->|web_search| F[(Azure AI Foundry)]
  V -.->|code_execute + image_generation| F
  P -.->|MCP ask_question| MCP[(mcp.deepwiki.com)]
```

Every arrow that leaves a sandbox box is enforced by the runtime. Section "Per-layer proof" below shows the artefacts.

## The four agents and what each one calls

| Agent | Tools used | Output |
|---|---|---|
| `parent` | `azureclaw_discover`, `azureclaw_spawn`, `azureclaw_mesh_send`, `azureclaw_mesh_await`, MCP `ask_question` (DeepWiki), `telegram_send_message` | Dispatch + final delivery |
| `analyst` | `foundry_web_search` × ≥3 queries, `file_write`, `azureclaw_mesh_transfer_file` × 2 | `analyst.json` (≤4 KB) with trends, control categories, runtimes, metrics |
| `viz` | `azureclaw_mesh_await`, `file_read`, `foundry_code_execute` (matplotlib), `foundry_image_generation`, `azureclaw_mesh_transfer_file` × 3 | `scorecard.png` (1024×640 grouped bar chart), `hero.png` (1024×1024 generated image) |
| `writer` | `azureclaw_mesh_await`, `file_read`, `file_write`, `azureclaw_mesh_transfer_file` × 1 | `brief.md` (~700–800 words, two pages) |

The choice of tools is deliberate: the scenario is meant to make at least one of each category fire (MCP, web search, sandboxed code execution, hosted image generation, sandbox FS, encrypted mesh, channel egress). The harness's [`verify.py`](https://github.com/Azure/azureclaw/blob/main/tools/e2e-harness/verify.py) checks all nine acceptance conditions and exits non-zero if any layer is silent.

## The 4-agent sequence

```mermaid
sequenceDiagram
  autonumber
  participant U as Operator (Telegram)
  participant P as parent
  participant DW as MCP DeepWiki
  participant A as analyst
  participant V as viz
  participant W as writer
  participant F as Foundry

  U->>P: telegram_send: "produce exec brief"
  P->>P: azureclaw_discover('*')
  P->>DW: ask_question (governance posture)
  DW-->>P: answer
  P->>A: spawn + mesh_send(brief + deepwiki excerpt)
  P->>V: spawn + mesh_send(viz brief)
  P->>W: spawn + mesh_send(writer brief)
  A->>F: foundry_web_search × ≥3
  F-->>A: ≥6 distinct https URLs
  A->>A: file_write analyst.json
  A->>V: mesh_transfer_file analyst.json
  A->>W: mesh_transfer_file analyst.json
  V->>F: foundry_code_execute (matplotlib scorecard)
  F-->>V: scorecard.png in sandbox FS
  V->>F: foundry_image_generation hero.png
  F-->>V: hero.png in sandbox FS
  V->>W: mesh_transfer_file scorecard.png
  V->>W: mesh_transfer_file hero.png
  V->>W: mesh_transfer_file analyst.json (re-forward)
  W->>W: file_write brief.md (one call, ~5 KB)
  W->>P: mesh_transfer_file brief.md
  P->>U: telegram_send: brief.md
```

Each `mesh_send` and `mesh_transfer_file` is an X3DH KNOCK + Double-Ratchet message via the AgentMesh relay; the relay only sees encrypted blobs. The parent is the only agent with Telegram channel access; the sub-agents have none.

## Per-layer proof

The point of the showcase is not "look, the agents talked to each other". The point is: **every claim AzureClaw makes about defence in depth shows up as an artefact you can read.** For each enforcement layer below, the proof is a concrete command you run and what you see.

### 1. Signed CRDs — what's enforced is what's signed

```bash
# The four sub-agents are each governed by their own ToolPolicy / InferencePolicy / EgressApproval.
# Pick any one, verify the controller's compiled digest equals the router's loaded digest.
for ns in azureclaw-execbrief-parent azureclaw-execbrief-analyst \
          azureclaw-execbrief-viz   azureclaw-execbrief-writer; do
  echo "=== $ns ==="
  kubectl get inferencepolicy -n "$ns" -o json \
    | jq '.items[] | {ready: (.status.conditions[]?|select(.type=="Ready")|.status),
                      digest: .status.bundleRefDigest}'
done
```

`status.bundleRefDigest` equals `policies.inference.loaded_digest` on each sandbox's router — see **[CRD trust model](../security/crd-trust-model.md)** for the full verification loop and negative tests. If those drift, the CR will not be `Ready`.

### 2. iptables egress-guard — kernel-level outbound firewall

Every sandbox pod has an `egress-guard` init container that installs iptables rules restricting UID 1000 (the agent process) to loopback + DNS + a NAT-redirect of TCP/80,443 → `127.0.0.1:8444`. The agent process **cannot** open a direct TCP connection to anything else — the kernel drops it. Proof:

```bash
kubectl exec deploy/execbrief-analyst -n azureclaw-execbrief-analyst \
  -c openclaw -- iptables -t nat -L OUTPUT -n 2>/dev/null || true
# (privileged inspection from the inference-router container, which has CAP_NET_ADMIN)
```

This layer is independent of the K8s NetworkPolicy and the L7 allow-list. The agent runs at UID 1000; no amount of in-process trickery can route around iptables.

### 3. Router L7 allow-list — by-hostname forward proxy

The redirected traffic lands at the inference-router on `127.0.0.1:8444`. The router checks the destination hostname against a signed allow-list (the same OCI + cosign pipeline as everything else):

```bash
kubectl get configmap egress-allowlist -n azureclaw-execbrief-analyst -o yaml \
  | yq '.data["allowlist.json"]' | jq
# → {"endpoints":[{"host":"api.azureml.ms","port":443},{"host":"api.search.bing.com","port":443}, …]}
```

The `analyst`'s allow-list does **not** include `api.telegram.org` — only `parent`'s does. So even if `analyst` were compromised, it could not directly post to Telegram; it would have to route the message through `parent` via the mesh, and `parent`'s `ToolPolicy` decides whether `telegram_send_message` is exposed. This is a textbook capability split.

### 4. K8s NetworkPolicy — ingress isolation

```bash
kubectl get netpol sandbox-policy -n azureclaw-execbrief-analyst -o yaml \
  | yq '.spec | {policyTypes, ingress, egress}'
```

The policy restricts ingress to peer sandbox namespaces (8443 / 18789 / 18791) and the operator namespace (8443 for the `/internal/policy-status` echo). It is enforced by the Cilium dataplane on AKS clusters labelled `kubernetes.azure.com/network-policy: cilium`. *(Egress was historically dropped in field-ownership drift; the [NP-egress fix](https://github.com/Azure/azureclaw/pull/336) consolidated this into a single SSA.)*

### 5. Mesh E2E encryption — relay sees ciphertext only

The four sandboxes register with the AgentMesh registry, exchange X3DH key bundles, and run a Double Ratchet for every message. The relay only forwards encrypted blobs. Proof:

```bash
kubectl logs -n agentmesh -l app=agentmesh-relay --tail=50 | grep route
# → "routed 412 bytes from <did:agt:analyst…> to <did:agt:viz…>" (no plaintext, just sizes + DIDs)
```

The KNOCK handler on the receiver decides whether to accept based on the `TrustGraph` projection and the sandbox's `governance.trustThreshold`. Sub-agents inherit the parent's spawn relationship as a baseline affinity boost; siblings are not auto-trusted.

### 6. Foundry hosted tools — workload-identity, no API keys

`foundry_web_search`, `foundry_code_execute`, and `foundry_image_generation` are dispatched by the router using the per-sandbox Workload Identity. The agent process never sees a Foundry key. Proof:

```bash
kubectl exec deploy/execbrief-viz -n azureclaw-execbrief-viz \
  -c openclaw -- env | grep -i foundry || echo "(none — correct)"
```

The router signs every Foundry call with an IMDS-acquired token whose audience is `https://ai.azure.com/`. The Memory Store + Content Safety floor + per-request token budget all fire on the router side, before the bytes leave the cluster.

### 7. seccomp profile — syscall blast-radius

Each agent container runs under a `RuntimeDefault` seccomp profile with additional hardening (no `ptrace`, no `mount`, no `unshare`, no `bpf`, no `module_*`). Proof:

```bash
kubectl get pod -n azureclaw-execbrief-analyst -l azureclaw.azure.com/component=sandbox \
  -o jsonpath='{.items[0].spec.securityContext.seccompProfile.type}'
# → RuntimeDefault
```

### 8. MCP — only what's declared

`parent`'s `McpServer/execbrief-deepwiki` declares the DeepWiki endpoint and OAuth issuer. The router exposes its tools as `execbrief_deepwiki.ask_question` etc.; any other MCP host the agent tries to dial is denied at the router. The sub-agents have **no** `mcpServerRefs` — they cannot call MCP at all. (`analyst`'s prompt explicitly says "Do NOT attempt any MCP tools — they are not available in your sandbox; web_search is sufficient.")

```bash
kubectl get clawsandbox execbrief-analyst -n azureclaw-execbrief-analyst \
  -o jsonpath='{.spec.governance.mcpServerRefs}'
# → (empty)
```

### 9. Telegram channel — only the parent

`parent` has the `telegram-credentials` Secret mounted; the sub-agents do not. The `telegram_send_message` tool is registered only in `parent`'s plugin set. This is enforced by the controller's pod-spec generation (Secret `envFrom` is conditional on the channel flag), not just by convention.

```bash
kubectl get secret -n azureclaw-execbrief-parent telegram-credentials -o jsonpath='{.metadata.name}'
# → telegram-credentials
kubectl get secret -n azureclaw-execbrief-analyst telegram-credentials 2>&1 | head -1
# → Error from server (NotFound)
```

## Platform support — what runs where today

This scenario has been validated **9/9 PASS on both AKS and local-k8s**. The platform matrix below is honest about what already works on each platform and what's pending.

| Layer | AKS | `local-k8s` (kind + controller) | `docker` (single-host) |
|---|---|---|---|
| Signed-CRD verification (controller + router echo) | ✅ | ✅ (same controller image, same cosign chain) | n/a (no CRDs; router still loads signed bundles from disk) |
| iptables egress-guard (init container) | ✅ | ✅ | ✅ (requires NET_ADMIN on the container — granted by `azureclaw dev`) |
| Router L7 allow-list | ✅ | ✅ | ✅ (mounted from disk, same allow-list shape) |
| K8s NetworkPolicy ingress | ✅ (Cilium dataplane) | ✅ (kindnet enforces NetworkPolicy — verified via PodMonitor allow-list) | n/a |
| Mesh E2E encryption | ✅ (cluster-internal relay) | ✅ (local relay + registry in the kind cluster) | ✅ (local relay + registry as docker containers) |
| Foundry hosted tools | ✅ (Workload Identity) | ⚠ (works if you wire an Azure connection string; no WI inside kind) | ⚠ (same — works with an env-var key for dev only) |
| seccomp profile | ✅ | ✅ | ⚠ (depends on docker's default seccomp; matches AKS for `RuntimeDefault`) |
| Telegram + other channels | ✅ | ✅ | ✅ |
| Observability (Prometheus + Grafana + Headlamp plugin) | ⚠ (Azure Monitor managed Prometheus — wiring pending) | ✅ (bundled with `azureclaw dev`) | n/a |

The reproducible end-to-end harness now runs on **AKS** and **local-k8s** (kind + controller). The `docker` platform is scaffolded in `tools/e2e-harness/platforms/docker.sh` and pending its first 9/9 validation run.

## What you can see while it runs (Headlamp + Grafana)

The four sub-agents and their inter-agent traffic are observable end-to-end without any extra setup on local-k8s — `azureclaw dev` installs Prometheus + Grafana + the AzureClaw Headlamp plugin on first run.

| View | URL (after `azureclaw dev`) | Shows |
|---|---|---|
| Headlamp → AzureClaw → **Overview** | `http://localhost:4466/` | Cluster-wide rollup: sandbox count, aggregate token budget vs spend, AGT decisions over time. |
| Headlamp → AzureClaw → **Mesh Topology** | same | Parent → sub-agent hierarchy as a live SVG. Edge thickness ∝ mesh-message rate; two-direction animated pulses (yellow=sent, light-blue=received) show real KNOCK + X3DH + `mesh_send` + heartbeat traffic; node labels show lifetime `↑sent ↓recv` counts. Controllers are decorated with `N children · M trust` from the AGT trust graph. |
| Headlamp → any **ClawSandbox** | same | Per-sandbox detail page with the embedded Grafana ops dashboard filtered to that sandbox, plus a Token Budget card backed by `azureclaw_tokens_total` and `TOKEN_BUDGET_DAILY`. Dark-mode aware. |
| Grafana — "AzureClaw — Agent Fleet Operations" | `http://localhost:3000/d/azureclaw-ops` | Enterprise NOC layout: fleet health (req/sec, error rate, P95, 24h tokens, est. cost), token & cost economy, latency SLO heatmap, AGT decisions over time with color-coded allow/deny/approval/rate-limit, bundle health matrix. |
| Grafana — "AzureClaw — Sandbox Fleet Overview" | `http://localhost:3000/d/azureclaw-fleet` | Simpler 10-panel quick fleet view. |
| Prometheus | `http://localhost:19091/` | Ad-hoc PromQL — `azureclaw_tokens_total`, `azureclaw_mesh_messages_{sent,received}_total`, `azureclaw_agt_policy_evaluations_total{decision}`, `agentmesh_relay_*`. |

The mesh traffic counters (`azureclaw_mesh_messages_sent_total` / `azureclaw_mesh_messages_received_total`) are emitted by the router and count KNOCK + X3DH + `mesh_send` + 30s heartbeats. They exclude WS Ping/Pong by design — see [`.github/skills/agt-e2e-encryption/SKILL.md`](../../.github/skills/agt-e2e-encryption/SKILL.md) for the full counter semantics. On AKS the same metrics flow via Azure Monitor managed Prometheus (wiring pending).

## Running it yourself

`azureclaw dev` on **local-k8s** brings up the whole stack — controller + sandbox + AGT relay + Headlamp + Prometheus + Grafana — and the exec-brief harness can then be pointed at the running cluster:

```bash
# from repo root
azureclaw dev --target local-k8s        # ~3-4 min on first run (kube-prometheus-stack image pulls)
# observe at http://localhost:4466/ (Headlamp), http://localhost:3000/ (Grafana)

cd tools/e2e-harness
SCENARIO=exec-brief PLATFORM=local-k8s ./run.sh
```

For **AKS**, prerequisites: an AKS cluster with AzureClaw installed (`make install`), a Telegram bot token, and an Azure AI Foundry project with web-search + code-execute + image-generation enabled. Then:

```bash
cd tools/e2e-harness
SCENARIO=exec-brief PLATFORM=aks ./run.sh
# (run.sh chains monitor + drive + verify)
```

A passing run looks like `9/9 PASS` on stdout and `verify.json` with each check's evidence. The full transcript, JSONL trace, and any artifacts the agents produced are in `out/<timestamp>/`. While the run is in progress, the Headlamp Mesh Topology view animates the parent→sub-agent traffic in real time.

## See also

* **[CRD reference](../api/crd-reference.md)** — the schema for every CRD this scenario uses.
* **[CRD trust model](../security/crd-trust-model.md)** — the threat model and verification proof for the signed CRDs above.
* **[Architecture](../architecture.md)** — the prose explanation of how the controller, router, and runtime fit together.
* **[AGT boundary](../architecture/agt-boundary.md)** — what the runtime delegates to the Agent Governance Toolkit and what stays in AzureClaw.
* **[Security overview](../security.md)** — the catalog of layered controls this scenario exercises.
