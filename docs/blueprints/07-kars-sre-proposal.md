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

The SRE sandbox inherits the standard kars egress posture, with
explicit dev-vs-production lifecycle:

- Default-deny NetworkPolicy
- **Dev / demo installs**: may start in `egressMode: Learn` (records
  every host the agent reaches into the next allowlist proposal),
  then operator promotes to `Strict` with a signed allowlist
- **Production installs**: default `egressMode: Strict` from day 1,
  with a pre-declared allowlist (or layered `EgressApproval` for
  ad-hoc grants)
- Allowed by default: in-cluster `kubernetes.default.svc` (apiserver),
  IMDS (Workload Identity), and the cluster DNS resolver. **Mesh
  endpoints (`agentmesh-relay.agentmesh.svc`,
  `agentmesh-registry.agentmesh.svc`) are intentionally NOT in the
  default allowlist** — the SRE agent does not use the mesh
  (see §7.8.6).
- Optional grants (operator opt-in): `api.github.com` (source-code
  grounding), `api.telegram.org` / `slack.com` (channel posts),
  `<registry>` for the `sre_image_probe` tool
- No outbound to user-workload registries or third-party services
  unless the operator explicitly extends the allowlist


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

---

## 7 — Re-slicing for production (2026-06-09 addendum)

The "MVP / Phase 2 / Phase 3" carving above describes the *implementation
order*. For productisation, we additionally need a slicing that maps
each capability to (a) one independently shippable PR, (b) one
observable user-facing capability, and (c) the demo flow that requires
it. This section supersedes the Phasing block above as the planning
breakdown; the implementation tables in §§4-6 still apply.

### 7.1 Slice catalog

Each slice is a **mergeable PR slice** with explicit upstream
dependencies (not "independently shippable from cold" — there is
ordering). Each adds one observable user-facing capability.

| Slice | Ships | Depends on | Demoable as | Effort |
|---|---|---|---|---|
| **S0 · Demo harness**     | `tools/demo/act2/` — broken `webshop` deployment (nginx:1.27-typo) · expected-fix script · reset script · presenter runbook · idempotent re-run | — | Presenter can walk Act II by hand end-to-end before any SRE code exists | ~0.5d |
| **S1 · MVP**              | Helm template (KarsSandbox + sandbox-name `sre` ⇒ namespace `kars-sre` + SA `sandbox` — see §7.9) · 5 read-only kars-CR tools (`sre_describe_state`, `sre_logs`, `sre_diagnose`, `sre_explain_error`, `sre_propose_fix`) · regression corpus | — | `kars sre install && kars connect sre → "health overview"` returns a kars-CR snapshot | ~1d |
| **S2 · K8s diag toolset** | Cluster-wide read RBAC (opt-in, three install modes — see §7.5 Q3) · workload-owner-graph tool (`sre_describe_resource`) · `sre_image_probe` · `sre_endpoints_inspect` · `sre_what_changed` (informer-cache backed) · `sre_top` (graceful degrade) · per-tool RBAC manifest | S1 | Operator describes a broken user workload by name, agent returns root cause | ~1d |
| **S3 · Typed apply-fix** | Typed action store (no shell exec) · protected-resource policy (§7.7) · SelfSubjectAccessReview + server-side dry-run pre-flight · TokenRequest mint with one-shot ClusterRoleBinding · admission policy backstop · operator approve UI (`kars sre approve`) | S1 + action store | `kars sre approve <id>` mints token, agent applies one typed action, CRB torn down | ~1.5d |
| **S4 · Proactive watcher** | `sre_continuous` informer loop · governed `kars_notify_human` tool (NOT Hermes' deregistered `send_message` — see §7.3) · per-symptom dedupe + 10-min throttle · prompt-injection containment for events/logs/annotations (§7.7.2) | S2; uses S3 only when posting fix proposals that need approval | Agent posts to Telegram/Slack on its own when a workload misbehaves | ~1d |
| **S5 · Source-code grounding** | Optional `--gh-token` install flag · GitHub MCP wiring · cached source-snapshot ConfigMap (size-bounded) · `api.github.com` egress grant | S1 (egress story) | "Why is the controller doing X?" → agent quotes file:line | ~0.5d |
| **S6 · Sovereign / hardening variant** | Install variant: AGT-only approval (no channel dep) · `sre_image_probe` external probe disabled · all egress denied except in-cluster | S1-S4 (it's a hardening pass, not a new capability) | Works in Blueprint 06 deployments | ~0.5d |

**For the showcase demo (Act II — ImagePullBackOff)** the demo flow
requires S0 + S1 + S2 + S3 + S4. S5 is a multiplier on diagnostic
quality but not required for the demo. S6 is post-launch hardening.

### 7.2 K8s diagnostic toolset (Slice 2 detail)

The MVP tool surface is kars-CR-centric (the agent's first job was
diagnosing kars OOTB blockers). For Slice 2 we add the tools needed
for arbitrary Kubernetes workload triage. **All write capabilities
remain in S3 — every tool here is read-only.**

| Tool | What it does | RBAC required | Notes |
|---|---|---|---|
| `sre_describe_resource`  | Structured equivalent of `kubectl describe`. Returns spec + status + recent events as one JSON document. For a workload (Deployment / StatefulSet / DaemonSet), automatically walks the **owner graph**: workload → ReplicaSet → Pods → Events → ServiceAccount + imagePullSecret names (metadata only) → Node summary. This is the single tool that handles ImagePullBackOff *and* DNS failures *and* admission denials *and* rollout stalls — one call, one document. | cluster-wide read on the requested kind + cascade (see §7.2.1) | The agent's first call after `sre_describe_state` |
| `sre_image_probe`        | Given an image reference, probe the registry (HEAD on the manifest endpoint) and return: exists / not, digest, age, closest tags by edit distance, **closest tag in use on THIS cluster** (de-duplicated across all workloads). | (out-of-cluster: `<registry>:443`) + cluster-wide read on workloads | Disabled by §7.1 S6 in sovereign mode |
| `sre_endpoints_inspect`  | For a given Service, walks selector → matching pods → `EndpointSlice` subset → endpoint-not-ready reasons. Returns the missing labels / failing readiness probes that drop pods from the EndpointSlice. | cluster-wide read on `services`, `discovery.k8s.io/endpointslices`, `pods` | The "0 endpoints" detective tool |
| `sre_what_changed`       | Incident-framing tool. Returns: (a) events of `reason∈{Failed, FailedScheduling, BackOff, FailedCreate, FailedKillPod, Unhealthy, ScalingReplicaSet, SuccessfulCreate, SuccessfulDelete, Killing}` in last N min from BOTH `core/events` AND `events.k8s.io/events` (the new API has different retention); (b) workload `metadata.generation` jumps observed by the SRE agent's informer cache since startup. The informer cache (~50MB max) is what makes "since N minutes ago" diff-able — Events alone are lossy. | cluster-wide read on `events`, `events.k8s.io/events`, `deployments`, `replicasets`, `statefulsets` | Informer cache is S2's only persistent state |
| `sre_top`                | Wrapper around `metrics.k8s.io/v1beta1` — CPU/memory per pod and per node. If metrics-server is absent (no API registration), returns `{"unavailable": "metrics-server not installed"}` and the agent's planner routes around it (§7.5 Q4). | `metrics.k8s.io/v1beta1` get/list on `pods` and `nodes`; `core` get on `nodes` | OOMKilled / pressure triage |

#### 7.2.1 RBAC manifest (exact)

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: kars-sre-reader
rules:
  # Core workloads (read-only, cluster-wide when --with-cluster-wide-read)
  - apiGroups: [""]
    resources: ["pods", "pods/log", "services", "configmaps", "namespaces", "events", "serviceaccounts", "nodes", "endpoints"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["apps"]
    resources: ["deployments", "statefulsets", "daemonsets", "replicasets"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["discovery.k8s.io"]
    resources: ["endpointslices"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["events.k8s.io"]
    resources: ["events"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["metrics.k8s.io"]
    resources: ["pods", "nodes"]
    verbs: ["get", "list"]
  # Secrets METADATA only — data field redacted by the inference-router proxy filter (§6.4)
  - apiGroups: [""]
    resources: ["secrets"]
    verbs: ["get", "list"]
  # CRD introspection
  - apiGroups: ["apiextensions.k8s.io"]
    resources: ["customresourcedefinitions"]
    verbs: ["get", "list"]
  # kars-owned CRs
  - apiGroups: ["kars.azure.com"]
    resources: ["*"]
    verbs: ["get", "list", "watch"]
```

**Notably absent** (each is a deliberate ban — see §7.7 for the write-action threat model):
`create/update/delete/patch on anything`, `pods/exec`, `pods/portforward`,
`tokenrequests`, RBAC kinds (`roles`, `rolebindings`, `clusterroles`,
`clusterrolebindings`), `serviceaccounts/token`, `validatingwebhookconfigurations`,
`mutatingwebhookconfigurations`.

#### 7.2.2 The agent's reasoning loop for an incident

1. `sre_describe_state` — kars CRs (keep our own house in order first)
2. `sre_what_changed` (15-min window) — frame the incident in time
3. `sre_describe_resource` — the unhealthy thing, with full owner graph
4. **specialised tool for the symptom:** `sre_image_probe` for `ImagePullBackOff`, `sre_endpoints_inspect` for "0 endpoints", `sre_top` for OOMKilled
5. `sre_propose_fix` — single typed action (§7.7), with rationale
6. (if S3 enabled) `sre_apply_fix` — via short-lived TokenRequest, on approval


### 7.3 Notification — the `kars_notify_human` tool (Slice 4 detail)

Hermes' built-in `send_message` is explicitly deregistered by the
kars Hermes plugin because it bypasses kars governance / egress /
audit. The SRE agent's notification path therefore uses a
**governed wrapper**:

- **Tool name:** `kars_notify_human`
- **Implementation:** new tool in `runtimes/hermes/.../plugin/sre.py`
  that goes through the inference-router (so policy + egress + audit
  apply uniformly), then dispatches to the configured channel
  adapter (Telegram / Slack / Discord / Teams) via the existing
  Hermes channel plumbing.
- **ToolPolicy gate:** `ToolPolicy.approval.channel` is honored for
  the *approval* prompt; the *notification itself* is unapproved
  (it's outbound, low-risk). Approval applies only to the inner
  `sre_apply_fix` that the notification announces.
- **Credentials:** standard kars convention — `--telegram-token`
  /  `--slack-token` install flags write to `<sandbox>-credentials`
  Secret in `kars-sre`; the Hermes entrypoint reads from `envFrom`.
  No new credential infrastructure.
- **Throttle:** at most one notification per affected workload per
  10 minutes; per-symptom dedupe (don't re-post the same
  `ImagePullBackOff` on the same `deployment/<ns>/<name>` twice).
- **Air-gap (S6):** if no channel is configured, `sre_continuous`
  posts to the AGT trust store only; operators check the agent's
  inbox via `kars sre inbox` (a tiny new CLI command).

### 7.4 Productisation gaps closed by this addendum

Items hinted at in the original proposal that this section makes
concrete:

| Gap in original | Closed by |
|---|---|
| "reads source live" — but how? | §7.1 S5: GitHub MCP wiring + optional `--gh-token`, with a fallback cached-snapshot ConfigMap |
| Approval channel pluggability | §7.3 + §7.5 Q6: explicit reuse of `ToolPolicy.approval.channel`; install-mode UX |
| K8s-native debugging beyond kars CRs | §7.2: workload owner graph + ImagePullBackOff/endpoint/OOMKilled patterns + exact RBAC |
| Privilege escalation via "approved kubectl command" | **§7.7 (new): typed action store + protected-resource policy + admission backstop** |
| Channel notification path | §7.3: `kars_notify_human` (NOT Hermes' deregistered `send_message`) |
| Demo dependency on Phase 3 | §7.1: S4 (proactive watcher) elevated; S0 (demo harness) added |
| Air-gap (Blueprint 06) compatibility | §7.1 S6: AGT-only approval mode; sre_image_probe disabled |
| Observability of the agent itself | §7.10 (new): expanded metric shape + latency histograms + audit correlation |
| Naming inconsistency (`kars-kars-sre`) | §7.9 (new): sandbox name `sre`, namespace `kars-sre`, SA `sandbox` |
| **Privilege containment — no inheritance to other agents** | **§7.8 (new): 9 boundaries — plugin packaging, sandbox uniqueness VAP, RBAC subject pinning VAP, writer-SA token containment (boundObjectRef + 5-min TTL + no auto-mount), spawn disabled in SRE image, mesh disabled at the source (no relay socket, no DID, not in registry), NetworkPolicy scoped admin port, audit on every privilege use** |
| Prompt injection via cluster events/logs | §7.7.2 (new): containment for untrusted strings |

### 7.5 Open decisions (must answer before any code lands)

Five (now six) decisions extended from §6:

**Q1.** **Top-level command vs CRD?** S1 ships as a chart sub-action
(idempotent helm upgrade) that creates standard first-class kars
resources: `KarsSandbox`, `ToolPolicy`, `InferencePolicy`, RBAC,
credentials secret. A dedicated `runtime.kind: SRE` is the **wrong**
abstraction (SRE isn't a runtime; it's a governed Hermes profile
with special tools/RBAC). If lifecycle/config grows, the right shape
is a future `KarsSREProfile` CRD that *wraps* a `KarsSandbox`, not a
new runtime kind.
- **Resolution:** chart sub-action for S1. Re-evaluate `KarsSREProfile`
  CRD only if config sprawl demands it. No new runtime kind.

**Q2.** **In-cluster SA vs mounted kubeconfig?** In-cluster SA
everywhere. Kind exposes the same in-cluster SA path AKS does
(it's a kubelet feature, not a cloud feature). WI on AKS is a
Phase 2 enhancement that adds federated-credential annotations on
top of the same SA — not a different code path.
- **Resolution:** in-cluster SA everywhere; drop kubeconfig mount
  from the plan entirely.

**Q3.** **Cluster-wide read blast radius — opt-in?** Yes, with three
install modes (a useful middle ground beyond binary kars-scoped /
cluster-wide):
- `kars sre install` — kars-scoped only (just `kars.azure.com/*` + its
  own ns). Demoable as kars-CR-only diagnose.
- `kars sre install --watch-namespace <ns>` — adds the K8s diag toolset
  scoped to one namespace.
- `kars sre install --with-cluster-wide-read` — full S2 RBAC (the
  Act II demo install).
- **Resolution:** ship all three modes in S2. Default is the most
  restrictive; the operator opts in to wider scope.

**Q4.** **What happens if metrics-server is absent?** Tool returns
`{"unavailable": "metrics-server not installed"}` and the agent's
planner routes around it. No hard error.
- **Resolution:** graceful degrade.

**Q5.** **How do we measure the SRE agent's own quality?** Multi-
dimensional metric — see §7.10 for the full shape. Single counter
isn't enough; we need outcome × symptom × resource_kind plus four
latency histograms (detection → proposal → approval → apply →
recovery).
- **Resolution:** see §7.10.

**Q6.** **Install-mode UX clarity** (raised by critique). When the
operator runs `kars sre install` without flags and then asks the
agent to diagnose `webshop/webshop`, the agent gets `forbidden`.
- **Resolution:** the agent's tool layer SelfSubjectAccessReview-
  checks at startup and surfaces "I can only diagnose resources in
  `kars-*` namespaces — re-run install with `--with-cluster-wide-read`
  or `--watch-namespace <ns>` to broaden". No silent failures.

### 7.6 Mapping to existing kars subsystems

Per first-class status, the SRE agent should reuse — not parallel —
the kars subsystems already in production:

| Concern | Reuse / extend |
|---|---|
| Sandbox primitive | `KarsSandbox` (one CR for the SRE sandbox; runtime.kind = Hermes) |
| Policy gating | `ToolPolicy` per `sre_*` tool name |
| Approval channel | `ToolPolicy.approval.channel` (Telegram/Slack/Discord/Teams) |
| Egress | `KarsSandbox.spec.networkPolicy` + signed allowlist. Production default `Strict`; dev/demo installs may start in `Learn` (see §6.6 lifecycle clarification) |
| Audit | inference-router's hash-chained JSONL audit (existing) |
| Status | `.status.phase` taxonomy (Pending → Compiled → Ready → Running → Degraded) |
| Identity | controller-issued FedCred (AKS) / in-cluster SA (kind) — see §7.9 |
| Notification | `kars_notify_human` governed tool (§7.3) — NOT Hermes' raw `send_message` |
| Mesh | **never** — the SRE agent is not on the mesh (§7.8.6); its only counterparties are the operator (channels + CLI) and the apiserver |

The SRE agent is, intentionally, just another kars sandbox with a
specialised tool surface and a wider RBAC. It does not introduce new
governance primitives; it uses the existing ones.

### 7.7 Threat model — write actions, protected resources, prompt injection

The original proposal said "agent proposes a kubectl command; operator
approves; controller mints a short-lived token". That model has two
fatal gaps:

1. **Privilege escalation via a single approved patch.** A single
   approved `patch` on RBAC, KarsSandbox, ToolPolicy, NetworkPolicy,
   ServiceAccount, Secret, CRD, or webhook config can escalate or
   persist privileges. Approving "kubectl patch" is the wrong unit.
2. **Prompt injection via cluster state.** Pod annotations, event
   messages, and pod logs are operator-untrusted strings that flow
   into the LLM context. An attacker (or an unwitting workload owner)
   can embed instructions in them.

This section closes both.

#### 7.7.1 Typed actions, not shell commands

`sre_apply_fix` does **not** take a kubectl/helm command string. It
takes a **typed action** from a closed set, schema-validated, with
a hard protected-resource denylist enforced in three places (defense
in depth):

| Typed action | Schema | Protected against |
|---|---|---|
| `PatchDeploymentImage` | `{namespace, name, container, image}` | image must NOT reference RBAC/SA/Secret-mount paths; namespace ∉ denylist |
| `RolloutRestart` | `{namespace, kind∈{Deployment,StatefulSet,DaemonSet}, name}` | namespace ∉ denylist |
| `ScaleDeployment` | `{namespace, name, replicas ∈ [0, 50]}` | namespace ∉ denylist; replicas clamped |
| `DeletePod` (= forced restart of one pod) | `{namespace, name}` | namespace ∉ denylist |
| `PatchConfigMapKey` | `{namespace, name, key, value}` | name ∉ kars-controlled CMs (allowlist of OPERATOR-managed CMs only) |

**Protected-resource denylist** (enforced at all three layers below):
RBAC kinds (`roles`, `rolebindings`, `clusterroles`, `clusterrolebindings`),
`serviceaccounts`, `serviceaccounts/token`, `secrets`,
`customresourcedefinitions`, `validatingwebhookconfigurations`,
`mutatingwebhookconfigurations`, `validatingadmissionpolicies`,
`validatingadmissionpolicybindings`, anything in
namespaces `kars-system`, `kars-sre`, `kube-system`, `kube-public`,
`kube-node-lease`, `agentmesh`, and any `kars.azure.com/*` CR
(`KarsSandbox`, `ToolPolicy`, `InferencePolicy`, `EgressApproval`,
`NetworkPolicy` of kars sandboxes, etc.). The SRE agent cannot
mutate kars governance state via this path.

**Three enforcement layers** (any one rejects → action denied):
1. **Plugin-side compiler** — `sre_apply_fix` rejects anything not
   in the typed-action set; rejects any action whose target hits
   the denylist.
2. **Controller pre-flight** — before minting the token, the
   controller runs `SelfSubjectAccessReview` (would the writer SA
   even be allowed?) AND **server-side dry-run** (does the action
   parse and pass admission?). Both must succeed.
3. **Admission backstop** — a `ValidatingAdmissionPolicy` that
   targets the `kars-sre-writer-*` user (kubelet client identity
   prefix is stable) and denies any verb on the denylist kinds /
   namespaces. So even a controller bug (e.g., wrong CRB scope)
   cannot let the SRE writer touch governance state.

#### 7.7.2 Prompt-injection containment for untrusted cluster strings

Pod logs, event messages, annotations, ConfigMap values are operator-
untrusted strings. Two defenses:

- **Quote-and-fence:** the SRE plugin wraps every block of cluster-
  sourced text in `<<<UNTRUSTED_CLUSTER_DATA>>> ... <<<END>>>` with a
  system-prompt directive that any instructions inside the fence are
  data, not commands.
- **Action-must-target-the-described-resource:** `sre_apply_fix` will
  reject any typed action whose `{kind, namespace, name}` does not
  match a resource the agent inspected in this turn (via
  `sre_describe_resource`). An attacker who injects "patch
  deployment/elevated-thing in kube-system" through a log line can't
  succeed because the agent never inspected that resource → action
  is rejected at the plugin layer.

The combination is not a full prompt-injection defense (no defense is),
but it makes the obvious exploits non-functional without operator
explicit complicity.

### 7.8 Privilege containment — no inheritance, no other consumers

**Requirement:** the elevated capabilities granted to `kars-sre`
(cluster-wide read RBAC, the writer SA, TokenRequest minting, the
typed-action store, the `sre_*` tool surface, the channel notification
path) must be **uniquely held by the one nominated SRE sandbox**.
No other agent, sub-agent, or workload in the cluster can inherit,
re-use, or fall heir to any of these capabilities.

Six containment boundaries, each defended at the layer where it
naturally enforces:

#### 7.8.1 Plugin packaging — the `sre_*` tools don't exist outside the SRE image

The SRE tool module (`sre_describe_state`, `sre_apply_fix`, etc.)
**does not ship** in the standard Hermes runtime image. It lives in
a **separate Python package** (`kars-sre-plugin`) that is installed
**only** in a dedicated `kars/sre-sandbox:<tag>` image. Standard
`kars/hermes-sandbox:<tag>` images have no awareness of the SRE
tool names, no code to invoke them, and no `register_tool` calls
that would expose them.

```
sandbox-images/
  hermes/             # ships kars-hermes-plugin (no SRE tools)
  sre/                # ships kars-hermes-plugin + kars-sre-plugin
```

Even if an attacker convinces a user-facing Hermes sandbox to ask
for `sre_apply_fix`, the LLM can ask but the tool is not registered,
so the call returns "tool not found" at the runtime, not at the policy
layer. **The tools simply do not exist in any other pod.**

#### 7.8.2 Sandbox uniqueness — only one KarsSandbox cluster-wide may be the SRE agent

A `ValidatingAdmissionPolicy` enforces that at most one `KarsSandbox`
in the cluster can carry the label
`kars.azure.com/role: sre`. Any subsequent CR with that label is
admission-rejected. The label is the only thing the controller's
SRE-aware reconciler looks at when deciding "is this the SRE
sandbox?" — so an operator cannot bypass uniqueness by naming
the second sandbox differently.

```yaml
# deploy/helm/kars/templates/admission/vap-sre-uniqueness.yaml
apiVersion: admissionregistration.k8s.io/v1
kind: ValidatingAdmissionPolicy
metadata:
  name: kars-sre-uniqueness
spec:
  matchConstraints:
    resourceRules:
      - apiGroups: ["kars.azure.com"]
        resources: ["karssandboxes"]
        operations: ["CREATE","UPDATE"]
  validations:
    - expression: |
        !(object.metadata.labels["kars.azure.com/role"] == "sre") ||
        size(variables.existing_sre) == 0 ||
        (size(variables.existing_sre) == 1 && variables.existing_sre[0].metadata.name == object.metadata.name)
      message: "Only one KarsSandbox per cluster may carry the kars.azure.com/role=sre label."
```

#### 7.8.3 RBAC subject pinning — no wildcard, no group binding

Every `ClusterRoleBinding` and `RoleBinding` that references
`kars-sre-reader`, `kars-sre-writer`, or any `kars-sre-write-*`
ClusterRole MUST name an explicit `(kind: ServiceAccount, namespace,
name)` subject. No subjects of kind `Group`, no `system:serviceaccounts`
group binding, no `*` name. A second VAP enforces this:

```yaml
# deploy/helm/kars/templates/admission/vap-sre-rbac-pinning.yaml
apiVersion: admissionregistration.k8s.io/v1
kind: ValidatingAdmissionPolicy
metadata:
  name: kars-sre-rbac-pinning
spec:
  matchConstraints:
    resourceRules:
      - apiGroups: ["rbac.authorization.k8s.io"]
        resources: ["clusterrolebindings","rolebindings"]
        operations: ["CREATE","UPDATE"]
  validations:
    - expression: |
        !object.roleRef.name.startsWith("kars-sre-") ||
        object.subjects.all(s,
          s.kind == "ServiceAccount" &&
          s.namespace == "kars-sre" &&
          (s.name == "sandbox" || s.name == "sre-writer"))
      message: |
        Bindings to kars-sre-* ClusterRoles may target only the
        ServiceAccounts 'sandbox' or 'sre-writer' in namespace
        'kars-sre'. Group bindings and wildcard subjects are denied.
```

#### 7.8.4 Writer SA token containment

The `sre-writer` ServiceAccount is **NOT auto-mounted**
(`automountServiceAccountToken: false` on the SA and on every pod
in `kars-sre`). The agent's primary pod uses the `sandbox` SA
(read-only). The writer SA exists only as an identity that the
**controller** mints TokenRequest tokens against — the agent's
container never sees a long-lived `sre-writer` token.

Each per-action token has:
- `audiences: ["api"]` (default Kubernetes audience; rejected by any
  non-apiserver consumer)
- `expirationSeconds: 300` (5-min TTL — cannot be extended without a
  new TokenRequest)
- `boundObjectRef: {kind: Pod, name: <sre-pod>, uid: <current pod UID>}`
  — the token is invalidated the moment the SRE pod is deleted or
  restarted, so a stolen token from a crashed pod is dead
- A one-shot `ClusterRoleBinding` of the form `kars-sre-write-<action-id>`
  that grants only the typed action's (verb, resource, namespace) —
  torn down by the controller after the action completes or after
  the 5-min TTL, whichever first

The token is passed to the agent process via an **in-memory env var**
on the typed-action subprocess, never written to disk, never logged.
The kars-strict seccomp profile blocks `ptrace`, so a hostile
sidecar cannot read the env from `/proc/<pid>/environ`.

#### 7.8.5 Spawn isolation — SRE cannot create sub-agents

The SRE sandbox **disables `kars_spawn`** entirely (the spawn plugin
is not registered in the SRE image). This prevents an attacker who
compromises the agent from spawning a sub-sandbox in `kars-sre` that
would inherit the namespace's RBAC. As an additional defence, the
controller's spawn reconciler rejects any sub-sandbox whose parent
namespace is `kars-sre`.

#### 7.8.6 Mesh disabled at the source — there is no relay path to abuse

**The SRE agent does not connect to the mesh.** Mesh is for inter-agent
collaboration; the SRE agent's only counterparties are (a) the operator
(via Telegram/Slack channel + the `kars sre` CLI + WebUI) and (b) the
Kubernetes apiserver. There is no use case that calls for sending or
receiving KNOCK frames, and disabling mesh removes a substantial
attack surface (Signal Protocol handshake parser, X3DH state, Double
Ratchet session state, the relay WebSocket connection, the mesh
authorization layer).

Enforced at three layers:

1. **KarsSandbox spec:** `spec.mesh.enabled: false` (new field) — the
   controller does not provision mesh credentials, does not mint a
   mesh identity, does not annotate the pod for the relay-connect
   init, and does not open the egress allowance to
   `agentmesh-relay.agentmesh.svc`.
2. **Image-level:** the dedicated `kars/sre-sandbox` image (§7.8.1)
   does not include the mesh Python package and does not register any
   `kars_mesh_*` tools. The mesh plugin module is absent from
   `runtimes/hermes/.../plugin/` in this image build.
3. **NetworkPolicy:** the SRE sandbox's egress NetworkPolicy does not
   include the `agentmesh` namespace in its allowlist. Even if a
   future bug accidentally tried to dial the relay, the network path
   does not exist.

If a remote agent on the cluster mesh somehow assembled a KNOCK frame
addressed to the SRE agent, it has nowhere to deliver it — the SRE
agent is not registered in the AGT registry, has no DID, and is not
holding a relay socket. The frame is dropped at the registry lookup.

#### 7.8.7 NetworkPolicy — admin port only reachable from cluster operators

The SRE sandbox's NetworkPolicy ingress on `:8443` (router admin) is
restricted to:
- Pods in `kars-system` with label `app.kubernetes.io/name=kars-controller`
- Pods in `monitoring` with label `app.kubernetes.io/component=prometheus`
- (NOT other sandbox namespaces, NOT `default`, NOT `kube-system`
  beyond the standard probes)

So even if another sandbox somehow obtained a kars-sre admin token,
the network path to use it does not exist.

#### 7.8.8 Audit — every SRE privilege use is observable

All eight containment boundaries above are **observable**:
- The VAPs in §7.8.2 and §7.8.3 emit Warning Events on every
  rejected admission attempt (visible in `kubectl events`)
- The controller-side TokenRequest in §7.8.4 emits a structured
  log line `"sre.token.minted action=<id> binding=<crb-name> ttl=300s"`
- The k8s audit log captures every TokenRequest call and every
  one-shot CRB creation/deletion
- The inference-router audit JSONL captures every `sre_apply_fix`
  invocation with its action_id

If any of these surfaces shows activity not correlated with an
operator-initiated `kars sre approve` flow, the cluster is
compromised — operator triage can immediately revoke the SRE
sandbox by `kubectl delete karssandbox sre`, which the controller's
ownerRef tree garbage-collects atomically (writer SA, all CRBs,
namespace, the lot).

#### 7.8.9 What an attacker who fully compromises the SRE agent CANNOT do

Stacking the above:
- Cannot create or delegate the `sre_*` tools to any other agent
  (§7.8.1 — they don't exist outside this image)
- Cannot install a second SRE agent (§7.8.2)
- Cannot bind the kars-sre ClusterRoles to any other SA, user, or
  group (§7.8.3)
- Cannot persist a writer token beyond 5 min, beyond pod lifetime,
  or across a controller restart (§7.8.4)
- Cannot spawn a sub-sandbox that inherits namespace RBAC (§7.8.5)
- Cannot be commanded by any remote agent over the mesh (§7.8.6 —
  the SRE agent is not on the mesh at all)
- Cannot be reached on its admin port by user-facing sandbox pods
  (§7.8.7)
- Cannot escape audit (§7.8.8)
- Cannot mutate kars governance state, RBAC, secrets, webhooks, or
  CRDs (§7.7 typed-action denylist + admission backstop)

The maximal blast radius of a fully-compromised SRE agent is:
**read-only inspection of cluster state, plus one typed action against
non-governance workload state per operator approval, plus channel
posts to the configured Telegram/Slack channel**. Nothing else.

### 7.9 Identity & naming

Resolves the `kars-kars-sre` gotcha raised in critique:

- **Sandbox name:** `sre` (NOT `kars-sre`). The controller derives the
  namespace as `kars-<name>` per the standard convention, so:
  - Namespace: `kars-sre`
  - ServiceAccount: `sandbox` (the controller-created default — NOT a
    custom name)
  - SA token mount: `/var/run/secrets/kubernetes.io/serviceaccount/token`
    (standard projected SA volume)
- **Writer SA (S3 only):** `sre-writer`, lives in `kars-sre`, no
  permissions by default. One-shot ClusterRoleBindings of the form
  `kars-sre-write-<action-id>` are created by the controller on
  approval and TTL'd by the controller's grant-lifecycle reconciler
  after 5 min OR after the action completes (whichever first).
- **Controller RBAC delta for S3 (must add):**
  ```yaml
  - apiGroups: [""]
    resources: ["serviceaccounts/token"]
    verbs: ["create"]
  - apiGroups: ["rbac.authorization.k8s.io"]
    resources: ["clusterrolebindings"]
    verbs: ["create", "delete"]
    resourceNames: ["kars-sre-write-*"]
  ```
  The controller's existing RBAC does not include `serviceaccounts/token`;
  this is required for the S3 TokenRequest path.

### 7.10 Observability — metrics, audit, recovery confirmation

The single counter from the original §7.5 Q5 is too coarse. The
production shape:

**Counters:**
```
kars_sre_proposals_total{
  outcome ∈ {proposed, rejected, expired, amended, apply_failed,
             applied_recovered, applied_no_recovery},
  symptom ∈ {ImagePullBackOff, CrashLoopBackOff, OOMKilled,
             ZeroEndpoints, RolloutStalled, FailedScheduling, Other},
  proposal_type ∈ {PatchDeploymentImage, RolloutRestart, ScaleDeployment,
                   DeletePod, PatchConfigMapKey},
  resource_kind ∈ {Deployment, StatefulSet, DaemonSet, ConfigMap, Pod},
  cluster_scope ∈ {kars-only, namespace, cluster-wide},
}
```

(Avoid `namespace`, `name`, image tag, action_id as Prometheus labels —
they go in the audit row.)

**Histograms:**
```
kars_sre_detection_to_proposal_seconds   # informer event → sre_propose_fix call
kars_sre_proposal_to_approval_seconds    # propose → operator approve (or reject/expire)
kars_sre_approval_to_apply_seconds       # approve → typed action executed
kars_sre_apply_to_recovery_seconds       # apply → observed recovery (or timeout)
```

**Audit correlation:** every proposal carries a stable `action_id`
that links five rows across the audit stack:
1. AGT trust-store proposal entry
2. K8s audit log event (controller minted the writer token)
3. K8s audit log event (sre-writer SA executed the action)
4. inference-router audit JSONL row (the agent's tool call)
5. recovery-or-timeout entry (informer observed the workload back to
   healthy, OR timed out)

`kars sre report --last 7d` aggregates these for human review.

**Recovery semantics:** "recovered" means the observed workload condition
transitions from the symptom that triggered the proposal back to
`Available=True` AND `Progressing=True` (workload-kind-appropriate)
within a configurable window (default 5 min). Otherwise → `applied_no_recovery`.

### 7.11 What's NOT yet specified (work needed before PR slicing)

These are the design artefacts a PR-slicing kickoff should produce.
None blocks the showcase demo, but all block "kars-sre is GA":

1. **Threat model document** — prompt injection vectors enumerated;
   protected-resource policy formalised; failure modes for the typed-
   action compiler.
2. **Tool JSON schemas** — exact JSON Schema for every `sre_*` tool's
   input + output. The agent's tool-calling reliability depends on this.
3. **Action / incident persistent data model** — where does an
   `action_id` live? Suggestion: a new `KarsSREAction` CRD (Phase 2)
   with `.status.phase ∈ {Proposed, Approved, Applied, Recovered,
   Failed, Expired, Rejected}`. Until then, AGT trust store + audit JSONL.
4. **e2e test matrix:**
   - kind (default substrate)
   - AKS (Workload Identity path)
   - metrics-server absent (graceful degrade)
   - private registry (image_probe failure modes)
   - egress Strict (no `api.github.com` available)
   - RBAC negative tests (cluster-wide read off, --watch-namespace
     wrong ns)
   - prompt-injection corpus (annotations / events containing the
     "ignore previous instructions" classic, plus k8s-flavoured
     variants like "patch deployment kube-system/kube-proxy …")
5. **Source-grounding privacy + ConfigMap size** — what's the upper
   bound on the cached source snapshot? Strategy: cap at 1MB; if the
   repo is larger, snapshot only the controller + plugin + chart
   templates (the parts the agent actually quotes).
6. **Upgrade / uninstall / GC behavior** — `kars sre uninstall` must
   tear down: the sandbox + its namespace, the ClusterRole + binding,
   the controller-side action store, any pending action grants, the
   credentials secret. Verify no orphan CRBs after kill-and-reapply.
7. **Backpressure on the informer loop** — when 50 workloads break
   simultaneously (cluster-wide outage), we cannot post 50 Telegram
   messages. Symptom-aggregation: batch by ns within 60 s; one
   notification per ns per symptom.


## Design open questions (carried over from §6 — answered in §7.5)

The original design open questions are answered in **§7.5** above.
For traceability they are echoed here with their resolutions:

1. **Multi-cluster?** Per-cluster install. MVP unchanged.
2. **Kubeconfig mount vs Workload Identity?** **Changed — see §7.5 Q2.** In-cluster SA everywhere; kubeconfig mount dropped from the plan.
3. **Source-code access scope?** kars + AGT, gated by optional `--gh-token` install flag (see §7.1 S5).
4. **History / corpus.** Agent reads source live (no pre-seeded corpus). Drift-free.

Additional decisions raised in §7.5: top-level command vs CRD (Q1),
cluster-wide read scope as opt-in (Q3), metrics-server graceful
degrade (Q4), agent quality metric (Q5).

## Validation gate

Before this lands as merged, it must be able to autonomously
diagnose and fix-propose **every one of the 12 OOTB blockers**
listed in the "Why" section above, given only the cluster state
that existed at the moment each was hit. That's a regression
test corpus.
