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

Plus SRE-specific scoping:

- **ClusterRole `kars-sre-reader`** — read-only on `karssandboxes`,
  `inferencepolicies`, `toolpolicies`, `mcpservers`, `karsmemories`,
  `pods`, `deployments`, `services`, `events`, `configmaps`,
  `secrets/metadata` (NOT `secrets/data` — agent never sees secret
  values), `customresourcedefinitions`.
- **No write access by default.** `sre_apply_fix` and
  `sre_run_ootb_smoke` route through AGT approval: operator gets
  a `kars sre approve <action-id>` prompt in their TUI before the
  agent's proposed kubectl/helm call executes.
- **Egress allowlist** (NetworkPolicy + router blocklist):
  `kubernetes.default.svc.cluster.local`,
  the configured inference endpoint (Foundry / Copilot),
  `api.github.com` (read-only) for source lookups.
  Nothing else.

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
