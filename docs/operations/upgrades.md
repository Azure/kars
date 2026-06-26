# Upgrades & rollback

This runbook covers moving a running kars cluster from one release to the next,
and reverting if something looks wrong. The CLI does the heavy lifting; this page
explains what happens, how to verify, and how to roll back.

> **Scope.** Production AKS clusters running kars `v1alpha1`. Local `kars dev`
> stacks "upgrade" by recreating the ephemeral container/kind cluster from the
> newer images — there is nothing stateful to migrate.

---

## 1. Two ways to move forward

| Command | What it does | Use when |
|---|---|---|
| **`kars upgrade`** | Full, failsafe release migration: detect version → record rollback point → import the target release's signed images into your ACR → `helm upgrade --atomic` → rolling restart → verify. | You want to move to a **published GitHub release** (the normal path). |
| **`kars up --upgrade`** | Fast Helm-only re-run against an existing cluster. Assumes your ACR **already holds** the target images; re-applies the chart + RBAC. | You manage images yourself (custom ACR pipeline) and just need to re-apply the chart. |

For almost everyone, **`kars upgrade`** is the right command — it owns the image
import and the rollback point, so the cluster never lands half-migrated.

---

## 2. Upgrade to the latest release

```bash
# Preview first — shows from→to and exactly which images would be imported
kars upgrade --dry-run

# Apply
kars upgrade
```

What happens, step by step:

1. **Connect & sanity-check.** Pulls AKS credentials from the cached context
   (`~/.kars/context.json`) and confirms a `kars` Helm release exists.
2. **Resolve target.** Defaults to the latest GitHub release; override with
   `--to <tag>`.
3. **Version guard.** If you're already at the target, it exits cleanly (use
   `--force` to re-run). If the cluster is **newer** than the target, it refuses
   to downgrade unless you pass `--force`.
4. **Import images** from `ghcr.io/azure` into your ACR — both the immutable
   version tag (e.g. `:v0.1.18`) and `:latest`. If a **required** image fails to
   import, the command aborts **before** any cluster change is made.
5. **`helm upgrade --atomic`** — on any failure Helm auto-rolls-back the release,
   so you can't end up half-upgraded.
6. **Rolling restart** of the controller, router, and sandbox workloads onto the
   new images.
7. **Verify** workload health and print a from→to summary.

Pin a specific release for reproducibility:

```bash
kars upgrade --to v0.1.18
```

Skip the seven multi-runtime adapter images for a faster control-plane-only
upgrade (OpenClaw + BYO remain runnable):

```bash
kars upgrade --skip-runtime-images
```

---

## 3. Verify

```bash
kars status                     # control plane + sandbox health at a glance
kubectl get pods -n kars-system # controller + router rollout status
kubectl get karssandbox -A      # per-sandbox phase
```

The upgrade prints `Cluster healthy on the new release` when the controller,
router, and sandbox deployments report Ready. If it instead warns that some
workloads aren't Ready yet, give them a moment and re-check `kars status` — image
pulls on fresh nodes can lag the Helm completion.

---

## 4. Roll back

Every `kars upgrade` records the prior Helm revision, so reverting is one command:

```bash
kars upgrade --rollback
```

This runs `helm rollback` to the previous revision, restarts the workloads, and
re-verifies health. Use it when an upgrade changed **chart templates, Helm
values, or CRDs** and you need to undo that.

> ⚠️ **What rollback does and doesn't revert.** kars workloads pin the
> **`:latest`** image tag (a deliberate convention — the controller always
> tracks `:latest`). `kars upgrade` imports the new release over `:latest` in
> your ACR, so a `helm rollback` reverts the **release revision** (chart, values,
> CRDs) but the rolling restart still pulls `:latest`, which now points at the
> upgraded images. To revert the **running image bits** to an earlier version,
> re-import that version explicitly:
>
> ```bash
> kars upgrade --to v0.1.17   # re-imports v0.1.17 over :latest and rolls workloads to it
> ```
>
> `kars upgrade` imports each release's immutable version tag alongside `:latest`,
> so the exact prior bits remain available in your ACR to roll back (or forward)
> to by version.

> **Data note.** Rollback reverts the **control plane** (controller, router,
> CRDs, chart config). It does not undo data-plane side effects that
> already happened (audit-chain entries, Memory Store writes, mesh messages) —
> those are append-only by design.

---

## 5. CRD / schema changes across releases

The CRD surface is served at `v1alpha1` and may change between minor releases.
`kars upgrade` applies the chart's CRDs as part of the Helm upgrade. When a
release changes a CRD schema, the change is called out in
[`CHANGELOG.md`](../../CHANGELOG.md); additive fields are backward-compatible, and
any field with a breaking change is flagged there. Review the changelog entry for
the target release before upgrading a cluster with hand-authored CRs.

---

## 6. See also

- [`kars upgrade` CLI reference](../cli-reference.md#kars-upgrade)
- [Image versioning](image-versioning.md) — the `:latest` vs pinned-tag model
- [Helm packaging](helm-packaging.md) — chart layout and release
- [GitOps](gitops.md) — reconciling the same changes declaratively with Argo/Flux
