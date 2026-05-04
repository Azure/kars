# Manual E2E suite

This directory holds the **manually-runnable** end-to-end test matrix
for AzureClaw. It is **separate from CI**:

| Suite | Runs in | Path | Touches a real cluster? |
|---|---|---|---|
| CI smoke | GitHub Actions on every PR | `tests/e2e/run.sh` | Kind, ephemeral |
| **Manual matrix** | **operator on demand** | **`tests/e2e-manual/`** | **yes — yours** |

The manual matrix exists to cover the scenarios that the free-tier Kind
runner in CI cannot — full runtime fan-out, cross-runtime AgentMesh,
the governance lane (Content Safety, policy deny, rate-limit, trust),
failure modes (router crash, relay disconnect, OOM), and multi-tenant
isolation. Run it before each release tag.

> **The runner never creates or destroys a cluster.** Bring your own.

---

## Prerequisites

1. A reachable Kubernetes cluster (Kind, AKS, or any conformant K8s)
   with your kubeconfig context pointing at it.
2. AzureClaw installed in `azureclaw-system`:
   ```bash
   helm upgrade --install azureclaw deploy/helm/azureclaw \
     -n azureclaw-system --create-namespace
   ```
3. CRDs registered (`kubectl get crd clawsandboxes.azureclaw.io`).
4. `kubectl`, `bash`, and (for the cross-runtime mesh scenario) `yq`.
5. AgentMesh relay + registry deployed in the `agentmesh` namespace if
   you want the mesh + governance scenarios. They are auto-installed by
   the AzureClaw helm chart with `agentmesh.enabled=true`.

The runner refuses to start if any of these are missing.

---

## Quickstart

```bash
# Run every scenario:
bash tests/e2e-manual/run.sh

# Or via Make:
make test-e2e-manual

# Run a subset:
bash tests/e2e-manual/run.sh --scenario governance,isolation

# Limit the runtime-matrix scenario to a couple of runtimes:
bash tests/e2e-manual/run.sh --scenario runtime --runtime openclaw,oai-agents

# Keep namespaces around for triage when something fails:
bash tests/e2e-manual/run.sh --keep-ns
```

`bash tests/e2e-manual/run.sh --list` enumerates scenarios.

---

## Scenarios

| ID | Script | What it validates |
|---|---|---|
| `runtime` | `scenarios/runtime_matrix.sh` | Every first-class runtime (OpenClaw, OpenAI Agents Python, Anthropic, MAF Python, LangGraph Python, LangGraph TypeScript, Pydantic-AI) reaches `Ready` and includes the `inference-router` sidecar. |
| `mesh` | `scenarios/cross_runtime_mesh.sh` | Two sandboxes of different runtimes register with the AgentMesh registry, exchange a KNOCK, and round-trip an E2E-encrypted message via the relay. |
| `governance` | `scenarios/governance_lane.sh` | The router enforces Content Safety, PolicyEngine deny lists, RateLimiter budgets, and TrustManager thresholds. |
| `failures` | `scenarios/failure_modes.sh` | Router crash → pod restart, relay scale-to-zero → fail-closed mesh ops, oversize allocation → kubelet contains it. |
| `isolation` | `scenarios/multi_tenant_isolation.sh` | NetworkPolicy blocks cross-tenant TCP, ServiceAccounts are distinct per tenant, sandboxes cannot reach the kube API. |

Each scenario prints a per-scenario summary; `run.sh` prints an
aggregate at the end and exits non-zero if any scenario failed.

---

## Environment knobs

| Variable | Default | Purpose |
|---|---|---|
| `MANUAL_E2E_NAMESPACE_PREFIX` | `azureclaw-e2e-manual` | Prefix for the per-scenario namespaces. |
| `MANUAL_E2E_TIMEOUT` | `300` | Per-resource readiness timeout (seconds). |
| `MANUAL_E2E_KEEP_NS` | `0` | `1` → keep namespaces after each scenario for triage. |
| `MANUAL_E2E_VERBOSE` | `0` | `1` → dump `kubectl describe` on each failed assertion. |
| `AZURECLAW_E2E_RUNTIMES` | all aliases | Subset of runtimes for the `runtime` scenario. |
| `AZURECLAW_E2E_PEER_A` / `_B` | `openclaw` / `oai-agents` | Peer choice for the `mesh` scenario. |
| `AZURECLAW_E2E_GOV_RUNTIME` | `openclaw` | Sandbox runtime for the `governance` scenario. |
| `AZURECLAW_E2E_BURST` | `60` | Request count for the rate-limit probe. |

---

## Adding a scenario

1. Drop a script into `scenarios/`. Source `lib/common.sh` and (if you
   need ClawSandbox CRs) `lib/cr_factory.sh`. Use `scenario_header`
   first and `scenario_summary` last; emit individual results via
   `log_pass` / `log_fail` / `log_skip`.
2. Register it in the `SCENARIOS` array near the top of `run.sh` with
   the form `id|script_filename|short description`.
3. Document it in the table above.

The runner expects every scenario to clean up its own namespaces unless
`MANUAL_E2E_KEEP_NS=1`.

---

## Troubleshooting

**`require_azureclaw_installed: not found`** — the manual runner sees no
`azureclaw-system` namespace or no `clawsandboxes.azureclaw.io` CRD.
Install the chart (see prerequisites).

**`mesh` scenario says “agentmesh-relay/registry not installed”** — install
AzureClaw with mesh enabled (`agentmesh.enabled=true` in helm values).

**`failures` scenario hangs at relay scale-up** — the kubeconfig user
needs permission to scale deployments in the `agentmesh` namespace.

**Sandbox stuck in `Pending`** — usually image-pull or admission. Run
with `MANUAL_E2E_VERBOSE=1` and inspect the namespace events.

**Cluster cost** — every scenario is namespace-scoped and tears down on
exit. The `runtime` scenario peaks at ~7 sandboxes serially (one at a
time) so resource needs match a single sandbox plus the platform.

---

## What this suite intentionally does **not** do

- It does **not** edit, replace, or run anything in `tests/e2e/run.sh`
  (the CI suite). The CI Kind matrix is untouched.
- It does **not** create or destroy clusters. Use the cluster you already
  have.
- It does **not** publish any images, secrets, or telemetry off the
  cluster.
- It does **not** require Azure credentials for the smoke probes; the
  governance scenario only exercises the inference-router policy chain
  via loopback HTTP. Live model calls require the AzureClaw helm chart
  to be configured with Foundry credentials separately.
