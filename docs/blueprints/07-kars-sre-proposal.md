<!--
Copyright (c) Microsoft Corporation.
Licensed under the MIT License.
-->

# kars-sre — built-in AKS SRE agent (proposal)

**Status:** 🚧 proposal (not yet implemented)
**Filed:** 2026-06-08, from a debugging session that uncovered 12 OOTB blockers in the local-k8s flow that an in-cluster SRE agent could have auto-diagnosed.
**Target PR:** separate (this is design only; implementation tracked as
  `kars-sre-mvp` todo)

## Why

This session shipped 12 OOTB blockers on the Hermes-support branch
(`hermes/act1-docker-smoke-fixes`) — every single one was diagnosable
from cluster state + controller logs + chart source + image manifests:

1. AGT auto-clone missing
2. Sandbox-image curl had no retry
3. Copilot IDE-JWT cache ignored `expires_at`
4. Copilot had no fallback chain on 503
5. Egress proxy `TcpStream::connect` had no timeout
6. Operator `n`-spawn dialog hardcoded a runtime list that drifted
7. `kars-runtime-hermes` was never loaded into kind
8. `kars add` log-then-exit-0 on real errors
9. CRD-mismatch errors gave no actionable hint
10. `kars dev` didn't build the hermes runtime image
11. `KARS_DEV_PROFILE=true` only in CLI's dynamic overlay
12. Naked `helm template | kubectl apply` nuked the controller's
    inference creds

Every one of these required a human to manually run a `kubectl …` →
read the output → cross-reference source code → form a hypothesis →
test the fix. An in-cluster agent with read-only kube + helm + source
access could have walked the same diagnostic ladder autonomously and
either applied the fix (under AGT approval) or surfaced a one-shot
command.

## What it is

A single `KarsSandbox` of `runtime.kind: Hermes` deployed into the
dedicated `kars-sre` namespace as part of `kars up` (opt-in). You
talk to it via `kars connect kars-sre` (standard WebUI).

## Tool surface

Hermes-plugin extensions on top of the existing kars Hermes plugin:

| Tool | What it does | Approval |
|---|---|---|
| `sre_describe_state`  | Structured snapshot of all kars-owned CRs + pods + events across the cluster | none (read-only) |
| `sre_logs`            | Tail any pod's any container (capped 500 lines, redacts secrets) | none |
| `sre_explain_error`   | Takes an error string, queries a corpus of known kars failure modes + the controller source, returns root-cause hypothesis | none |
| `sre_diagnose`        | Walks the standard checklist: CRD freshness vs source, controller env, dev-profile, image-loaded-in-kind, network reachability, CR status | none |
| `sre_propose_fix`     | Generates a concrete kubectl/helm/kars command that would resolve the diagnosed issue (no side effects) | none |
| `sre_apply_fix`       | Actually runs the proposed fix command | **AGT approval** |
| `sre_run_ootb_smoke`  | Spawns one sandbox per `WIRED_KINDS` runtime against the live cluster and asserts each reaches Running 2/2 | **AGT approval** |

## Security posture

Inherits every isolation guarantee from the existing sandbox posture:

- kars-strict seccomp + iptables UID-1000 egress guard
- read-only root FS + `runAsNonRoot` + drop ALL caps
- Same dual-container layout (agent + inference-router sidecar)

### 6.1 Cluster access — Tier 1: local-k8s (kind) — MVP target

**Authentication:** in-cluster ServiceAccount token. The sandbox pod's
`ServiceAccountName: kars-sre` (in namespace `kars-sre`) is projected
to `/var/run/secrets/kubernetes.io/serviceaccount/token` by the kubelet,
auto-rotated on the standard k8s schedule (default 1h). `kubectl` /
`helm` inside the agent container use the in-cluster config path
(`KUBERNETES_SERVICE_HOST` / `KUBERNETES_SERVICE_PORT`) — no kubeconfig
file mounted, no static credential.

Why this is right for local-k8s first:
1. Works on a fresh kind cluster without any Entra / Azure dependency.
2. Same auth substrate kars already uses elsewhere (the controller's
   own ServiceAccount in `kars-system`, see
   `deploy/helm/kars/templates/controller-rbac.yaml`).
3. Single rotation point: `kubectl rollout restart deploy/kars-sre`
   forces a new SA token; no out-of-band cert/key/PAT to revoke.
4. RBAC is the ONLY authorization gate — the binding below is the
   complete blast-radius definition.

### 6.2 Cluster access — Tier 2: AKS (deferred, Phase 2)

When the user is on AKS, the same `kars-sre` ServiceAccount federates
to an Entra App via Workload Identity (the same pattern the kars
controller already uses — see `controller/src/auth/wi.rs`). The
Helm chart annotation set:

```yaml
serviceAccount:
  annotations:
    azure.workload.identity/client-id: <SRE-app-client-id>
```

`kars sre install` runs `az identity federated-credential create` to
wire the federation; otherwise everything else (RBAC, plugin code,
deployment shape) is byte-identical to local-k8s. This means the
MVP doesn't have to wait for AKS support to be useful — once it
works against kind, the AKS wiring is purely additive operator
glue, not a code-level change in the agent or its tools.

### 6.3 RBAC — the complete authorization gate

This ClusterRole IS the access model. There is no second authorization
layer (no admission webhook, no policy engine) — the agent's blast
radius is precisely what RBAC permits and nothing more:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: kars-sre-reader
rules:
  # Read kars-owned CRs (the only resources the agent reasons about).
  - apiGroups: ["kars.azure.com"]
    resources:
      - karssandboxes
      - inferencepolicies
      - toolpolicies
      - mcpservers
      - karsmemories
      - karsevals
      - trustgraphs
      - egressapprovals
      - karspairings
      - a2aagents
      - karsauthconfigs
    verbs: ["get","list","watch"]
  # Core workload state.
  - apiGroups: [""]
    resources: ["pods","services","configmaps","events","namespaces"]
    verbs: ["get","list","watch"]
  - apiGroups: ["apps"]
    resources: ["deployments","statefulsets","daemonsets","replicasets"]
    verbs: ["get","list","watch"]
  # Pod logs — NOT pods/exec, NOT pods/portforward.
  - apiGroups: [""]
    resources: ["pods/log"]
    verbs: ["get"]
  # Secret METADATA only. The agent process never sees secret data
  # because (a) the SA is granted only get/list on `secrets` itself
  # (apiserver returns secret data only on get-by-name, which the
  # router sidecar strips via field selector on forward — see §6.4),
  # and (b) the router proxy filter masks the .data field on any
  # /api/v1/.../secrets response before it reaches the agent
  # container. Belt + suspenders.
  - apiGroups: [""]
    resources: ["secrets"]
    verbs: ["get","list"]
  # CRD schema introspection so the agent can spot stale CRDs
  # (exactly the failure mode this session's debug arc hit).
  - apiGroups: ["apiextensions.k8s.io"]
    resources: ["customresourcedefinitions"]
    verbs: ["get","list"]
```

**Notably absent** (each is a deliberate ban, not an oversight):
- `create` / `update` / `delete` / `patch` on anything
- `pods/exec` — agent cannot shell into other sandbox pods
- `pods/portforward` — agent cannot relay traffic
- `secrets/data` field — see §6.4
- `tokenrequests` — agent cannot mint other SA tokens
- Anything outside `kars.azure.com`, core, apps, apiextensions

### 6.4 Secrets handling

The agent CAN list secrets and CAN call `kubectl get secret <name>`,
but it CANNOT see the `data` field. Two-layer enforcement:

1. **Router-side filter (primary):** the existing inference-router
   sidecar is the network choke point for the agent (UID 1000
   talks to UID 1001 over loopback; iptables blocks everything
   else). Extend its existing apiserver-proxy with a `secrets`
   filter that strips `.data` and `.stringData` from any response
   body whose kind is `Secret`. ~30 LOC in
   `inference-router/src/proxy.rs`.
2. **RBAC-side defense in depth (secondary):** the standard k8s
   `secrets` resource doesn't subdivide `data` from metadata, so
   we can't gate it via verb. The router filter is the real
   enforcement; RBAC `get` is the floor.

### 6.5 Write actions — Phase 2 short-lived token approval

`sre_apply_fix` and `sre_run_ootb_smoke` do NOT broaden the
`kars-sre-reader` ClusterRole. Instead, every write proposal generates
an action ID; the operator approves it in their TUI, at which point
the controller mints a SHORT-LIVED ServiceAccount token scoped to
JUST the verb+resource+namespace the agent proposed:

```
Agent → "Propose: kubectl rollout restart deploy/palhermes -n kars-palhermes"
         rationale: "agent container stuck in CrashLoopBackOff for 5 minutes"
  → action-id 'sre-action-7f3a' created in AGT trust store
  → Operator notified in `kars operator` TUI: "kars sre approve sre-action-7f3a"
  → Operator inspects proposed command + rationale; approves or rejects
  → On approve: TokenRequest API mints a token for SA `kars-sre-writer`
    bound to a one-shot ClusterRoleBinding `kars-sre-write-sre-action-7f3a`
    granting JUST `apps/deployments` `update` on `kars-palhermes/palhermes`,
    TTL 5 min
  → Agent executes via that token, single-use
  → ClusterRoleBinding + Secret torn down by the controller post-execution
  → Full audit chain: AGT approval entry, k8s audit log entry, router
    audit JSONL entry — all three correlated by action-id
```

This means the standing blast radius is ALWAYS read-only. Write
permission is materialized per-approved-action, scoped to one verb +
one resource, expires in 5 minutes, and is revoked immediately after
the call. No long-lived write token exists in the cluster at any
time.

### 6.6 Egress


## CLI integration

```bash
kars up                    # existing — prompt for --with-sre at install time
kars sre install           # explicit install for an existing cluster
kars sre talk              # opens `kars connect kars-sre` + helpful banner
kars sre diagnose [problem-text]
                           # one-shot CLI call, prints the agent's report
                           # without dropping into the WebUI
kars sre approve <action-id>
                           # operator approves a pending apply_fix /
                           # ootb_smoke proposal
kars sre uninstall         # clean removal
```

## Implementation surface

| Component | Effort | Notes |
|---|---|---|
| `runtimes/hermes/src/kars_runtime_hermes/plugin/sre.py` | M | New tool module registering the 7 `sre_*` tools |
| `runtimes/hermes/tests/test_sre.py` | M | Unit tests for each tool with mocked kubectl/helm/source-read |
| `deploy/helm/kars/templates/kars-sre-sandbox.yaml` | S | KarsSandbox + InferencePolicy + ToolPolicy + ClusterRoleBinding + ConfigMap with kars source snapshot |
| `cli/src/commands/sre.ts` | S | install / talk / diagnose / approve / uninstall |
| `controller/src/reconciler/sre.rs` | S | Optional: special-case the SRE sandbox to wire its kubeconfig as a `Secret` and mount it at `/etc/kars/kubeconfig` |
| `docs/sre.md` | S | Runbook: how to deploy, talk to, and govern the agent |

Total: ~2k LOC, ~3-4 dev days.

## Phasing

### MVP (`kars-sre-mvp` todo)

Read-only tools only: `sre_describe_state`, `sre_logs`,
`sre_diagnose`, `sre_explain_error`, `sre_propose_fix`. No
approval-gated tools. ~500 LOC, ~1 day. Validates the deployment
shape + tool calling pattern against a real cluster.

### Phase 2

Add `sre_apply_fix` + `sre_run_ootb_smoke` + AGT approval flow.
~800 LOC. Requires Hermes to surface the AGT approval protocol
from its plugin API (already exposed via the trust store, but the
approval-gating shape needs a per-tool wrapper).

### Phase 3

Add `sre_continuous` mode: agent watches cluster events, proactively
diagnoses pods that ImagePullBackOff or CrashLoopBackOff > 2x in
60s, posts a fix proposal to a Slack/Telegram channel without
human invocation. Requires the channel-token plumbing that already
ships with Hermes.

## Design open questions

1. **Multi-cluster?** Should one `kars-sre` agent on AKS see all
   blueprints (single-cluster) or be deployed per-cluster?
   MVP: per-cluster.
2. **Kubeconfig mount vs Workload Identity?** AKS + WI is cleaner
   long-term but kind needs a kubeconfig mount. MVP: kubeconfig
   mount, with WI as Phase 2 enhancement.
3. **Source-code access scope?** `api.github.com` read-only against
   `Azure/kars` only, or also against the AGT toolkit + sandbox-image
   dependencies? MVP: kars + AGT.
4. **History / corpus.** Should the agent ship with a pre-seeded
   knowledge base of known issues (e.g. exported from this
   session's commit messages)? MVP: no — agent reads the source
   live and reasons from there. Reduces drift.

## Validation gate

Before this lands as merged, it must be able to autonomously
diagnose and fix-propose **every one of the 12 OOTB blockers**
listed in the "Why" section above, given only the cluster state
that existed at the moment each was hit. That's a regression
test corpus.
