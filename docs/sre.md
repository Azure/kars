<!--
Copyright (c) Microsoft Corporation.
Licensed under the MIT License.
-->

# kars-sre — the built-in SRE agent

A long-running, in-cluster agent that diagnoses Kubernetes incidents
on the same kars cluster that runs your other agents. Optional, opt-in.

Status: **Slice 1 (MVP)** — read-only diagnostic tools. See
[`docs/blueprints/07-kars-sre-proposal.md`](blueprints/07-kars-sre-proposal.md)
§7.1 for the full slice ladder.

---

## Install

```bash
kars sre install
```

Equivalent to `helm upgrade --reuse-values --set sre.enabled=true`.
Brings up:

| Resource | Where | What it is |
|---|---|---|
| `InferencePolicy/sre-inference` | `kars-system` | model preference + content-safety + token budget for the SRE agent |
| `KarsSandbox/sre` | `kars-system` | runtime = Hermes; `extraEnv: KARS_SRE_ENABLED=true` |
| `ToolPolicy/sre-tools` | `kars-sre` | gates the `sre_*` tool surface |
| `ClusterRole/kars-sre-reader` | cluster | read on kars CRs + apiextensions + core workloads in `kars-*` namespaces |
| `ClusterRoleBinding/kars-sre-reader` | cluster | binds the ClusterRole to `kars-sre/sandbox` SA — explicit subject (no group binding, no wildcard) per §7.8.3 |

The controller derives namespace `kars-sre` from the sandbox name
`sre` per the standard `kars-<name>` convention. The SA `sandbox`
inside that namespace is created by the controller on first reconcile.

## Talk to it

```bash
kars sre talk
# port-forwards the WebUI; visit http://localhost:18790
```

Try:

> *give me a cluster-wide health overview*

The agent will:
1. Call `sre_describe_state` → kars-CR snapshot
2. Call `sre_diagnose` → checklist walk
3. Summarise what it found

For more targeted questions:

> *tail logs from the research-agent pod in kars-research*
> *what does "exceeded quota" usually mean in kars?*
> *propose a fix for the broken research-agent*

## Tools available in Slice 1

All read-only — no approval gates yet.

| Tool | What it does |
|---|---|
| `sre_describe_state` | structured snapshot of every kars-owned CR (kind, name, namespace, phase, conditions, lastReconciled) |
| `sre_logs` | tail pod logs via apiserver (caps at 500 lines) |
| `sre_diagnose` | walk the kars-CR health checklist (controller Ready, CRDs installed, no Degraded sandboxes, no stale reconciles) |
| `sre_explain_error` | match an error string against the OOTB-blocker corpus, return root-cause hypothesis |
| `sre_propose_fix` | return a typed-action proposal (Slice 1 codifies `DeleteResourceQuota`; the rest of the typed-action set lands with `sre_apply_fix` in Slice 3) |

## What it CAN'T do (yet)

Per the slice ladder:

- **No K8s diag toolset yet** — `sre_image_probe`, `sre_endpoints_inspect`, `sre_what_changed`, `sre_top` land in Slice 2
- **No fix execution** — `sre_apply_fix` + TokenRequest mint + admission backstop land in Slice 3
- **No proactive notifications** — `sre_continuous` informer loop + `kars_notify_human` (Telegram/Slack) land in Slice 4
- **No source-code grounding** — GitHub MCP wiring lands in Slice 5

Until Slice 3 lands, fix execution is operator-driven: copy the
proposal output, apply manually. The Act II demo's runbook
(`tools/demo/act2/runbook.md`) walks this.

## Containment — what kars-sre is NOT allowed to do

The SRE agent is the only sandbox in the cluster with cluster-wide
read RBAC, and (in Slice 3+) the only sandbox that can request
short-lived writer tokens. These privileges are **uniquely held** —
see proposal §7.8 for the nine-layer containment design. In summary:

- The `sre_*` tools don't exist in any other pod's runtime image
  (Slice 1: env-gated; Slice 1.5: separate `kars/sre-sandbox` image)
- Only one `KarsSandbox` per cluster can carry `kars.azure.com/role=sre`
  (Slice 3 admission policy)
- The `kars-sre-reader` ClusterRoleBinding is pinned to a specific
  ServiceAccount (no group bindings; satisfies §7.8.3)
- The SRE sandbox cannot spawn sub-agents — the `kars_spawn` family
  is skipped during plugin registration (§7.8.5)
- The SRE sandbox is not on the mesh — `kars_mesh_*` family is
  skipped during plugin registration; the NetworkPolicy in
  `sre.yaml` blocks the `agentmesh` namespace; the agent has no
  DID and is not registered (§7.8.6)
- Future write actions (Slice 3) are typed (no shell exec), exclude
  governance state (RBAC, secrets, kars CRs, kube-system,
  validating webhooks), use short-lived TokenRequest tokens bound
  to the pod's UID with 5-min TTL (§7.7.1 + §7.8.4)

## Uninstall

```bash
kars sre uninstall
```

Sets `sre.enabled=false` via `helm upgrade --reuse-values`. The
controller garbage-collects the sandbox + namespace + RBAC via
ownerRefs.

## See also

- Full design: [`docs/blueprints/07-kars-sre-proposal.md`](blueprints/07-kars-sre-proposal.md)
- Demo Act II walkthrough: [`tools/demo/act2/runbook.md`](../tools/demo/act2/runbook.md)
