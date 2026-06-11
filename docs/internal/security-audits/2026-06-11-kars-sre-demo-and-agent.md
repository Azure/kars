# Security Audit — kars-sre demo + agent (Slices 0–4 + selective Telegram pager + late-recovery healer)

**Date:** 2026-06-11
**Branch:** `kars-sre/demo-and-agent`
**PR:** [#397](https://github.com/Azure/kars/pull/397)
**Commits under audit** (46 since `main`):

- Demo Act-II harness: `075ba1d`, `0a26db4`, `72bedb2`, `8e7cb73`
- SRE Slice 1 (MVP read-only kars-CR tools): `3af6b71`
- SRE Slice 1 hardening: `91efb4a`, `5718fc4`, `91accb0`, `226f303`, `7fd3aa8`, `c447aa7`, `96e70bb`, `f6e8d0d`, `b25f41b`, `ab866ed`, `c506c54`, `deff899`, `d956594`, `f93598a`
- SRE Slice 2 (K8s diagnostic toolset): `5bdd29f`
- SRE Slice 3 (typed apply-fix) + Slice 4 (proactive watcher + Telegram): `81da63d`
- SRE Slice 4 UX (Headlamp Console + Chat): `64cb040`, `349901b`, `b48da89`, `c3b935f`, `a5e001f`, `704c758`, `4fb8681`, `c8f9b74`, `b588a5f`, `8def50f`, `aee5a71`, `59f99ed`, `b91e4e1`
- Hermes mesh productization for SRE: `fcce016`, `163e1de`, `3865b1c`
- Demo polish: `5f1c2ee`, `043ea5e`, `94cab91`
- SRE-action workload-aware recovery observer: `2ee6c91`
- Plugin workload-aware Phase column: `02fb78d`
- Stop spam: `27802be`
- Phase-changes-only pager (selective Telegram alerting): `c3fc023`
- Workload-availability overlay on synthetic phase (watcher + sre_diagnose both): `cfce890`
- Recovery window 5m→10m + late-recovery healer (Failed → Recovered edge): `4bf1560`

**Reviewers:** Pal Lakatos, Copilot

---

## Scope

This slice ships the autonomous SRE agent for kars sandboxes from concept to a working demo:

1. **Diagnostic surface** — `sre_describe_state`, `sre_diagnose`, `sre_logs`, `sre_describe_resource`, `sre_what_changed`, `sre_endpoints`, `sre_image_probe`, `sre_top` — all read-only, scoped via the `kars-sre-reader` ClusterRoleBinding bound to the SRE pod's `sandbox` SA.
2. **Typed apply-fix** — `sre_propose_fix` creates a `KarsSREAction` CR (Slice 3); the controller-side `KarsSREAction` reconciler validates against §7.7.1 protected-resource denylist, mints a 5-min TokenRequest for the chart-shipped `kars-sre/sre-writer` SA, creates a one-shot ClusterRoleBinding scoped to EXACTLY `(verb, resource, namespace)` of the action, executes, tears the CRB down, observes recovery.
3. **Proactive watcher + selective Telegram pager** — Slice 4: `phase-changes-only` watch mode alerts ONLY on `KarsSandbox` state transitions (including workload availability overlay), not on the event firehose. Recovery observer is workload-aware (no false Recovered) and now has a Late-Recovery healer (Failed → Recovered if the workload heals within 30 min of `appliedAt`).
4. **Headlamp UX** — embedded Hermes PTY chat in the operator dashboard, real-time SRE Console, workload-aware Cluster Health card.
5. **Demo Act-II infra-incident harness** — `tools/demo/act2/break.sh` applies a tight `ResourceQuota`, forces a pod evict, the SRE agent detects → diagnoses → proposes `DeleteResourceQuota` → operator approves → controller mints token → executes → workload recovers.

No new code-execution path was introduced into the agent runtime. No new bypass was opened in the inference-router or egress-guard. No new network egress was unlocked except what the chart already declares for the SRE sandbox (apiserver + Telegram API).

---

## Threat model

### T1: SRE agent escalates to cluster admin via the writer SA (MITIGATED — short-lived token + scoped CRB)

**Threat.** The chart ships a `kars-sre/sre-writer` ServiceAccount with no static RBAC binding. If a compromised agent (prompt injection, malicious tool, supply-chain bug in OpenClaw plugin) could mint a token for that SA and create a wildcard ClusterRoleBinding granting `*/*` on `cluster-admin`, every namespace in the cluster falls.

**Mitigations (defence in depth).**

1. **Authority split** — only the controller's SA (`kars-system/kars-controller`) has `create` on `serviceaccounts/token` for `sre-writer`. The agent's SA (`kars-sre/sandbox`) has no `create-token` permission and cannot mint the token directly.
2. **Two-step approval** — typed actions go from `Proposed` → `Approved` only after an operator with the cluster-admin-bound `kars:sre-approver` role patches `spec.approval.state="Approved"`. The agent never approves itself.
3. **Validation at the reconciler** — `validate_action()` enforces a closed set of `SUPPORTED_ACTIONS` (`DeleteResourceQuota`, `PatchDeploymentImage`, `ScaleDeployment`, `RolloutRestart`, `DeletePod`) and a `DENYLISTED_NAMESPACES` list including `kube-system`, `kars-system`, `kars-sre`, `agentmesh`, etc. (§7.7.1 layer 2 of 3). Unsupported actions and protected namespaces are rejected with `phase=Failed` before any token is minted.
4. **Token TTL 5 min, audience bound to apiserver** — `WRITER_TOKEN_TTL_SECONDS = 300` (`controller/src/kars_sre_action_reconciler.rs:135`). A leaked token is dead within 5 minutes.
5. **One-shot CRB scoped to EXACTLY the action** — the CRB is created with `resourceNames` = the specific target (e.g. `[platform-hardening-quota]`) and a fixed deletion timestamp. The reconciler tears it down immediately after execution.
6. **Plugin compiler gate (layer 1)** — the agent-side `sre_propose_fix` tool refuses to construct actions targeting the same denylisted namespaces. Defence in depth: the controller is the authority, but failing earlier surfaces clearer errors.
7. **Future admission backstop (layer 3)** — a ValidatingAdmissionPolicy on KarsSREAction CRs targeting protected namespaces is tracked in the roadmap.

**Net.** Even if the agent is fully prompt-injected, the worst it can do is create a `KarsSREAction` CR pointing at a non-denylisted namespace, where it sits as `Proposed` until a human approves. The operator sees what's proposed before any change.

### T2: Recovery observer marks a successful action as `Failed`, eroding operator trust (MITIGATED — late-recovery healer, this PR)

**Threat.** The recovery observer waited 5 min after `Applied` for the affected Deployment to satisfy `available >= desired`. Real-world recovery on cold-cache clusters, image pulls, or RS back-offs routinely exceeds 5 min. The demo on 2026-06-11 hit exactly this: the operator-approved patch worked, but research came back at ~6 min and the CR was already stamped `Failed`. The operator's Headlamp + Telegram pager then claimed `Failed` while the cluster was healthy.

This isn't a security-criticality threat in the classic confidentiality/integrity/availability sense, but it directly undermines the operator's ability to trust the SRE agent — and a distrusted autonomous agent gets disabled, defeating the whole defence-in-depth value the slice provides.

**Mitigation (this PR).**

1. `RECOVERY_WINDOW_SECONDS = 300` → `600` (10 min) to cover realistic cold-cache + RS back-off cycles.
2. **New `Failed → Recovered` edge.** For CRs that DID reach `Apply` (`appliedAt` set), the terminal-phase handler keeps running `observe_recovery()` for `LATE_RECOVERY_WINDOW_SECONDS = 1800` (30 min) since `appliedAt`. If recovery is observed, the phase flips to `Recovered` with `reason=LateRecovery`. Polling cadence during this window is 60s (vs 300s terminal cadence) so latency is bounded.
3. **Genuinely-terminal Failed is preserved.** Pre-apply failures (validation, unsupported action, denylisted namespace, apply error) have no `appliedAt` and remain terminal. The healer is opt-in by virtue of having reached `Apply`.

**No new privilege.** The healer reuses the existing `observe_recovery()` function, which lists Events and Deployments in the target namespace — both already permitted by the SRE pod's existing read RBAC. No new RBAC, no new token, no new code path that mutates cluster state.

**Audit-trail preserved.** When a Failed CR is flipped to Recovered, `stamp_phase` writes a fresh `lastTransitionTime` + a `LateRecovery` reason on the `Available` condition. The original Failed transition is preserved in the conditions history, so the timeline is `Applied → Failed → Recovered (LateRecovery, at appliedAt+Ns)`. Operators can see exactly what happened.

### T3: Phase-changes-only Telegram pager misses real workload incidents (MITIGATED — workload-availability overlay, this PR)

**Threat.** The Slice 4 watcher fired on `KarsSandbox.status.phase` transitions only. The controller doesn't flip CR phase when downstream pods fail (evicted pod can't re-admit due to quota, image-pull failure, OOM-loop). Result: the operator gets NO Telegram alert while the agent is silently offline — worse than no pager, because the operator believes the system is silent on no news.

**Mitigation (this PR).**

1. `sre_watcher._workload_state()` cross-checks each `KarsSandbox`'s namespaced Deployment in `kars-<name>` and synthesizes `WorkloadDown(<avail>/<desired>)` when `available < desired`. Transitions on the overlay fire one Telegram message per real state change.
2. `sre._impl_sre_diagnose` also incorporates the overlay — when the operator asks the agent "what's wrong?", the agent describes workload-down sandboxes with affected ns + deploy name.

**No new privilege.** The overlay lists Deployments in `kars-*` namespaces — already covered by `kars-sre-reader` ClusterRole (`apps/v1 deployments: get|list|watch`).

**No new egress surface.** Telegram API is already in the SRE sandbox's `NetworkPolicy.allowedEndpoints` (`api.telegram.org:443`).

### T4: Hermes mesh pre-warm leaks credentials or extends attack surface (MITIGATED — same trust boundary)

**Threat.** The Hermes runtime now starts a persistent mesh-keepalive subprocess (`runtimes/hermes/src/kars_runtime_hermes/plugin/entrypoint.sh`) to keep the sandbox registered with the AGT registry even when no operator is chatting. A bug in this subprocess could leak the agent's long-term Ed25519 identity or expose the prekey writer lock to attackers.

**Mitigation.**

1. The keepalive subprocess runs the same Python module (`kars_runtime_hermes.plugin.mesh`) and the same `MeshClient` singleton that the foreground gateway uses. No new key material, no new keystore path.
2. The prekey writer lock guard (`runtimes/agt-mesh-python/src/kars_agt_mesh/client.py::_acquire_prekey_writer_lock`, audited in `2026-06-06-cross-runtime-mesh-aks.md` §T1) protects against the keepalive process clobbering the foreground's prekey bundle. The keepalive process inherits the same `HERMES_HOME` env and acquires the lock first; the gateway is a no-op subscriber.
3. The `KARS_MESH_AUTO_RESPONDER=1` env var (which makes the keepalive process auto-reply to inbound mesh messages) is set ONLY inline on the keepalive subprocess env — not exported into the agent's environment, not visible to the LLM, not loggable via `os.environ` introspection from the OpenClaw tool surface.

### T5: Headlamp PTY chat tunnel allows arbitrary apiserver-proxy abuse (MITIGATED — port-forward only, no new tunnel)

**Threat.** The Headlamp SRE Console embeds the Hermes dashboard via an iframe served from `localhost:19119`. If this used the apiserver-proxy path (`/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/.../proxy/...`), an XSS in the dashboard could pivot to apiserver-proxy abuse via the operator's bearer token.

**Mitigation.**

1. The Headlamp plugin's Chat tab uses `kubectl port-forward` to `localhost:19119`, **not** apiserver-proxy. The iframe loads from `http://localhost:19119`, which carries no apiserver credentials. (Switching from apiserver-proxy to port-forward was commit `4fb8681` after we discovered the proxy path doesn't authenticate iframe asset loads — see `b91e4e1` for the final architecture.)
2. The Hermes dashboard itself runs in the SRE sandbox pod and is reachable only via `svc/sre 19119:9119`. The service has a `NetworkPolicy` that allows ingress only from the operator-labeled monitoring/headlamp namespace.

### T6: Demo Act-II `break.sh` permanently degrades a running cluster (MITIGATED — namespace-scoped + idempotent + clearly labeled)

**Threat.** The demo script applies a tight `ResourceQuota` in `kars-research`. If run against a production cluster (operator confusion, demo materials shipped to wrong env), it would block all new pods in that namespace.

**Mitigations.**

1. The script targets a specific namespace (`kars-research`) and a specific Deployment (`research`) — not cluster-wide.
2. The quota object is named `platform-hardening-quota` and has explicit labels identifying it as a demo artifact.
3. Removing the quota is a single `kubectl delete resourcequota platform-hardening-quota -n kars-research`. The fix that the SRE proposes is exactly this action.
4. Demo materials live under `tools/demo/act2/` with the directory name clearly indicating intent.

---

## What this audit does NOT cover

- Telegram channel security (operator's responsibility to control bot ownership; bot token is a secret managed via `kars credentials update sre --telegram-token`).
- Cross-namespace SRE — this slice only supports same-namespace recovery actions targeting workloads in `kars-*` namespaces. Cross-account / cross-cluster SRE is out of scope.
- The OpenClaw plugin's tool registration path is unchanged from prior audits; no new toolset added in this slice beyond the read-only diagnostic tools and `sre_propose_fix`.

---

## Test posture

- 6 reconciler unit tests pass on Linux/arm64 (`cargo test --release --package kars-controller -- kars_sre_action`).
- End-to-end demo verified on kind: induce incident via `break.sh`, agent detects via workload-availability overlay, proposes `DeleteResourceQuota`, operator approves via `kars sre approve`, controller executes via short-lived token, workload recovers, Late-Recovery healer flips Failed → Recovered (verified after the demo).
- Telegram pager fires correctly on transitions (verified `research: WorkloadDown(0/1) -> Running`).
- SRE chat (`sre_diagnose`) correctly reports workload-down sandboxes by namespace + deploy name (verified via Hermes UI).

---

## Sign-offs

Signed-off-by: Pal Lakatos <plakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
