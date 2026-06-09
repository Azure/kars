# Act II — presenter runbook

Use this when the kars-sre agent isn't built yet (S1-S5 in progress)
and you need to walk Act II by hand. Once S4 lands, the kars-sre
agent runs every step here autonomously and the runbook becomes the
*expected* behaviour spec.

## Pre-flight (before going on stage)

```bash
# 1) Fresh local cluster + kars installed (from Act I demo intro)
kars dev

# 2) Apply Agent A
kubectl apply -f tools/demo/act2/agent-a-research.yaml
kubectl -n kars-research rollout status deploy/research --timeout=120s

# 3) Confirm Agent A is healthy
kubectl -n kars-research get pod
# Expect: research-<hash>   2/2   Running
```

## The break (Act II, scene 1 — "something is wrong")

```bash
bash tools/demo/act2/break.sh
```

The script:
1. Applies `platform-hardening-quota.yaml` to `kars-research`
2. Force-deletes the running pod (so the failure surfaces in seconds, not on the next natural restart)
3. Confirms `FailedCreate / exceeded quota` event on the ReplicaSet
4. Prints the current pod state, the ResourceQuota, and the most recent FailedCreate event

Expected wall-clock: ~5–10 s for break, then ~30 s for the audience to see the Pending pod settle.

## The diagnosis (Act II, scene 2 — "kars-sre takes over")

These are the steps the kars-sre agent should walk. Until S2 ships,
do them by hand — talking through what the agent would say:

```bash
# 1) "What's the cluster state?" — sre_describe_state
kubectl get karssandbox -A
# Expect: research is Degraded (or Available=False).

# 2) "What changed recently?" — sre_what_changed
kubectl -n kars-research get events --sort-by=.lastTimestamp | tail -10
# Expect: FailedCreate from the ReplicaSet, exceeded-quota message.

# 3) "Describe the failing pod" — sre_describe_resource
kubectl -n kars-research describe pod -l app.kubernetes.io/component=sandbox
# Expect: Pending; events show no obvious workload-config issue.

# 4) "List quotas in the namespace" — sre_describe_resource on ResourceQuota
kubectl -n kars-research get resourcequota
kubectl -n kars-research describe resourcequota platform-hardening-quota
# Expect: requests.memory: 50Mi  (vs. used: ~256Mi)

# 5) "Propose the fix" — sre_propose_fix
echo "Proposed: delete ResourceQuota platform-hardening-quota in ns kars-research"
echo "Rationale: the quota's requests.memory ceiling is below the sandbox's actual"
echo "request; pod cannot be admitted while the quota is in effect."
echo "Resource is NOT labeled kars.azure.com/managed-by — safe to delete."
```

## The approval + fix (Act II, scene 3 — "operator approves")

In the full Act II this is a Telegram approval ping from kars-sre.
For the runbook walk, simulate by hand:

```bash
# Operator nods. Apply the fix.
bash tools/demo/act2/reset.sh
```

Expected: ResourceQuota gone, controller schedules a new pod, pod
reaches Running 2/2 within ~15 s.

## Tear-down (after the demo)

```bash
kubectl delete karssandbox research -n kars-system
kubectl delete namespace kars-research --ignore-not-found
kubectl delete -f tools/demo/act2/platform-hardening-quota.yaml --ignore-not-found
```

## Why this scenario

Picked because it's the most pure-infrastructure incident shape on
the candidate list:

- **The break is a real-world GitOps mistake** (operators routinely
  add ResourceQuotas via their gitops pipeline; getting the values
  wrong is common).
- **The symptom is unmistakable in `kubectl`** (Pending pod +
  `exceeded quota` event — universally-recognised K8s incident).
- **The fix is a single delete** — fits the SRE agent's typed-action
  model cleanly, doesn't touch any kars governance state, doesn't
  need node-level privilege.
- **The diagnostic walk uses three different `sre_*` tools** in
  natural sequence (`sre_describe_state`, `sre_what_changed`,
  `sre_describe_resource`) — covers the demo's "show what the tools
  do" goal without contrivance.

See `docs/blueprints/07-kars-sre-proposal.md` §7.7.1 for the
`DeleteResourceQuota` typed-action definition + protected-resource
denylist that lets the SRE agent execute this fix safely.
