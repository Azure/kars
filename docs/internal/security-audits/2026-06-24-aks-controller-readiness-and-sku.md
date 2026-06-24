# Security Audit — AKS OOTB: controller readiness ordering + subscription-aware VM SKU selection

PR: Azure/kars (branch `fix/aks-controller-readiness-and-sku`)

## Scope

Capability-path changes in `cli/src/commands/up.ts` and
`cli/src/commands/up/preflight.ts`. Non-gated supporting changes:
`controller/src/main.rs` (startup ordering), `cli/src/lib/vm-size.ts` (+ test),
`deploy/bicep/{main.bicep,main.json,modules/aks.bicep}` (parameterize the system
pool VM size).

Two field-reproduced OOTB failures of `kars up --release` against a fresh AKS
cluster, both upstream issues unrelated to any security control:

1. **Bicep preflight rejected the deploy** because the chart pinned `_v5`
   D-series SKUs (`Standard_D4s_v5` sandbox pool, hardcoded `Standard_D2as_v5`
   system pool) that the operator's subscription gates in `eastus2` (while
   `_v4`/`_v6`/`_v7` of the same families are allowed). No single hardcoded
   default works across subscriptions, and the system pool size was not even
   overridable.
2. **Helm `--wait` timed out** because the `kars-controller` Deployment
   (`replicas: 2`) was stuck at 1/2: the leader replica was healthy, but the
   standby crashlooped. Root cause: `controller/src/main.rs` spawned the
   `/healthz` server *after* the leader-election barrier (`ready_rx.await`), and
   `leader_election::acquire_and_hold` signals readiness only on the *acquire*
   branch — so the standby blocks forever, never serves `/healthz`, fails its
   readiness probe, and is killed by its liveness probe (CrashLoopBackOff).
   Latent since leader election (#223) but only fatal after `a7dddcb5`
   (2026-06-01) switched the controller probes from a PID-1 exec check to
   `httpGet /healthz`. Invisible in kind/dev because `values-local-dev.yaml`
   sets `replicas: 1` + `LEADER_ELECTION_ENABLED=false`.

## Changes

- **VM SKU selection (`cli/src/lib/vm-size.ts`, `up.ts`, `up/preflight.ts`):**
  before deploy, query `az vm list-skus --location <region>` for sizes actually
  available to the subscription and auto-pick the first usable from an ordered
  preference list (historical defaults first, so existing subs are unchanged), or
  honour `--node-vm-size` / `--system-vm-size`. Graceful fallback to defaults if
  the query can't run. Preflight now displays/validates the real resolved SKUs
  instead of a hardcoded `D4s_v5`.
- **Bicep:** parameterize the previously-hardcoded system-pool SKU
  (`systemVmSize`); regenerate `main.json`.
- **Controller (`main.rs`):** move the `/healthz`/metrics server to start
  *before* the leader-election barrier so both leader and hot-standby pass their
  readiness/liveness probes (a standby is healthy — ready to take over).
- **Helm timeout (`up.ts`):** raise the controller `--wait` from 5m → 10m as
  defense-in-depth for cold-cluster image pulls + Sigstore TUF fetch.

## Threat model

### T1: New asset source / attacker-reachable input? (NO)
`az vm list-skus` is a read-only ARM query scoped to the operator's own
subscription; its output only selects from a fixed, code-defined SKU preference
allow-list. `--node-vm-size` / `--system-vm-size` are operator-supplied strings
validated against that subscription's available set before use; an
unavailable/typo'd value is rejected with the available alternatives. No new
registry, credential, or network egress path.

### T2: Behaviour / security-control change? (NO)
No change to sandbox isolation, egress, RBAC, admission policies, mesh, or
credentials. The controller change only reorders when the (already-present)
health server starts; it does **not** change leader election — exactly one
replica still holds the Lease and reconciles, so there is no double-write. The
standby now reports Ready (correct: it is a viable hot standby) but still does
not reconcile until it wins the Lease. VM size is a capacity/availability
property, not a security boundary (confidential isolation still pins its CC SKU).

### T3: Fail-open risk? (NO)
If `az vm list-skus` cannot be queried, the CLI falls back to the historical
defaults and the Bicep preflight still hard-fails on a truly unavailable SKU —
no silent weakening. The controller readiness change can only make a
genuinely-healthy standby report Ready; a crashed/hung process still fails
`/healthz` and is restarted.

## Verdict

Accept. Both fixes address upstream availability/liveness bugs in the AKS deploy
path with no change to any runtime security control. Verified: `cargo build`
+ 846 controller tests pass; CLI typecheck + lint (0 errors) + 818 tests
(incl. 10 new `vm-size` tests) pass; `az bicep build` compiles; root cause
confirmed against live cluster logs (standby stuck at lease wait, `/healthz`
connection refused, CrashLoopBackOff from liveness).

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
