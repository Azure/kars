<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Built-in Hermes SRE

The canonical `sre` Sandbox diagnoses the cluster and creates **pending**
`KarsSREAction` proposals. It never approves or executes its own remedies.
Installation is opt-in; label or namespace occupancy alone is not authority.

## Install and authorize

First install the qualified Core controller/router and follow
[registered SRE authority](../how-to/sre-authority.md). Existing sources require
explicit UID/resourceVersion review, enrollment and migration. Do not bypass that
workflow with an ordinary Helm enable or a direct ServiceAccount grant.

For a genuinely fresh source, a registrar can run:

```sh
kars sre install --context <context> --namespace kars-system --release kars
```

This command upgrades the Core release, not just the SRE workload. Coordinate it
with other release changes. The chart creates `sre-inference`, `sre-tools` and
the Hermes `sre` source in the release namespace. The controller claims
`kars-sre`, then projects the registered private identity after authorization.

The Hermes client uses HTTPS **127.0.0.1:9446**, with the agent-safe token and CA
projected by the controller. The router, not the agent, holds Kubernetes
diagnostic authority. Both initial authorization and later requests can fail
closed when registration, namespace ownership or credential privacy changes.

For local-model qualification, configure the existing
[local inference route and egress](../local-inference.md) first. Changing
`--model` alone does not select a local provider. Never silently substitute a
paid provider for an unavailable local route.

## Proactive health sweeps

When `SRE_ENABLED` is enabled, the maintained Hermes entrypoint starts
`kars_runtime_hermes.plugin.sre_watcher`, even without channel credentials.
The watcher:

1. Acquires a process-lifetime file lock so duplicate supervisors do not overlap.
2. Checks the registered SRE HTTPS `/readyz`, the inference router's `/readyz`
   (including its existing finite-contract/calendar readiness), installed Kars
   plugin and loopback model configuration.
3. Reads existing proposals and invokes the installed Hermes oneshot command
   with the **`sre` toolset only**, the existing Kars governance hooks and the
   loopback inference provider. It asks the real agent to run `sre_diagnose`,
   investigate current failures, and propose only supported, evidence-backed
   remedies. Initial failures are not silently absorbed.
4. Records completion, failure, timeout or blocked authority, then waits before
   the next sweep. A timed-out process group is killed and reaped before another
   turn can start. SIGTERM also cleans up the active turn.

Every inference request still traverses router budget reservation and accounting.
There is no watcher-owned budget, grant, credential fallback or retry route.
Missing authority/contracts block the sweep rather than starting an ungoverned
turn. Existing interactive human sessions are not locked by the supervisor.
Blocked preflight checks retry after 30 seconds without invoking inference;
completed, failed or timed-out Hermes turns use the full configured cadence.

Environment settings on the existing Hermes `extraEnv` surface:

| Setting | Default | Meaning |
|---|---|---|
| `SRE_WATCHER_ENABLED` | `true` | Set `false`, `0`, `no` or `off` to disable |
| `SRE_HEALTH_INTERVAL_SECONDS` | `900` | Completion-to-next-start delay, 60–86400 seconds |
| `SRE_HEALTH_TIMEOUT_SECONDS` | `180` | Per-turn deadline, 10–900 seconds |
| `SRE_WATCHER_NOTIFY_TARGET` | unset | Optional explicit `telegram:recipient`, `slack:recipient` or `discord:recipient` |

The old deterministic event/phase watcher is replaced, not run alongside this
supervisor. Legacy `SRE_WATCHER_MODE`, 10-second event polling, dedupe and message
rate-limit settings no longer select a second behavior. This includes the old
chart's `phase-changes-only` value; it does not prevent real health sweeps.

### Reports and optional notifications

`$HERMES_HOME/sre-latest-report.json` is atomically replaced after each sweep.
`sre-latest-turn.log` retains at most **256 KiB** of combined stdout/stderr.
Overflow kills and reaps the full child process group and records a failed
turn, never a healthy result. Capture is bounded while the process runs, not
just when reading the completed log. Successful-turn JSON contains at most
64 KiB and marks truncation. A blocked sweep does not make that
older process log current. `finished` means the process exited successfully,
**not** that diagnostics or a live acceptance gate passed.

These local files have the lifetime of the existing Hermes home volume; they
are not durable across an `emptyDir` replacement. Successfully created
`KarsSREAction` objects are the persistent review surface in Kubernetes.

No channel is required. Telegram can infer a recipient only when its bot token
and `TELEGRAM_ALLOW_FROM` are both configured. Slack/Discord require their
matching token and an explicit notification target; they never default to
Telegram. Notification failures are recorded separately and do not discard the
health report. The watcher sends through the existing `hermes send` path.

## Diagnosis and proposals

`sre_diagnose` reports controller readiness, CRD presence, resource phases,
workload availability and reconcile freshness. Failed/malformed inventories,
failed workload reads, missing phases and absent/invalid freshness timestamps
produce explicit `coverage_errors`. A zero degradation count with incomplete
coverage **does not mean healthy**.

The tools also include `sre_describe_state`, `sre_logs`, `sre_explain_error`,
`sre_describe_resource`, `sre_what_changed`, `sre_endpoints_inspect`,
`sre_image_probe` and `sre_top`. Error hypotheses must be confirmed against
resource evidence. Metrics availability is optional.

`sre_propose_fix` supports `DeleteResourceQuota`, `DeletePod`,
`ScaleDeployment`, `PatchDeploymentImage` and `RolloutRestart`. It creates a
Pending proposal and returns the actual action name only after a successful
CREATE response, or reuses an identical Pending action (`cr_reused=true`,
`cr_created=false`) found in the live inventory without rewriting it. Unknown
proposal inventory blocks creation rather than risking repeated proposals.
`cr_error` means persistence was not confirmed. Unsupported
remedies require operator review, never a fabricated action ID.

```sh
kars sre actions --context <context>
kars sre approve <action-id> --context <context> --note "Reviewed evidence"
kars sre reject <action-id> --context <context> --reason "Not supported by evidence"
```

Approval is a separate human operation requiring the appropriate RBAC. The
controller subsequently validates and executes supported approved actions.
The current executor uses the controller client; the permissionless
`sre-writer` ServiceAccount's existence is **not proof** of pod-bound writer
TokenRequest execution.

## Live Kind acceptance

Package the coordinated Hermes changes once; do not independently deploy an
SRE-only alternative. With qualified Core/router authority and an explicitly
configured real local model, observe an **unprompted** sweep discovering actual
Kind state. Verify real Hermes tool calls, evidence-backed findings, explicit
unknown coverage, and any returned action ID against the actual Pending
KarsSREAction. Verify that no action was automatically approved or executed.

Unit tests, a ready pod, process exit zero, historical fixtures, a manual chat
or a deterministic watcher alert are not autonomous-agent acceptance.

For an interactive diagnostic smoke check use `kars connect sre --context
<context>`. Retire the reviewed registration before `kars sre uninstall`; see
the authority guide for exact sequencing.
