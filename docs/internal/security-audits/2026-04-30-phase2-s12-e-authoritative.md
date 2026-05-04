# Phase 2 — S12.e: Authoritative-ref mode (fail-closed)

**Date:** 2026-04-30
**Slice:** S12.e (`phase2-s12-e-authoritative`)
**Scope:** Controller-only PR. Lifts the `AZURECLAW_FEATURE_SIGNED_ALLOWLIST`
env gate (always-on); promotes
`spec.networkPolicy.allowlistRef` to **authoritative source** for
NetworkPolicy egress when set; adds an in-process last-known-good
(LKG) cache for fail-closed degradation; surfaces three new
`status.conditions` (`AllowlistVerified`,
`AllowlistAuthoritative`, `AllowlistDrift`).

## Existing implementation surveyed

Per plan §0.2 #8, every reused seam:

| Touchpoint | Reused / Extended |
|---|---|
| `controller/src/crd.rs` — `OciArtifactRef`, `NetworkPolicyConfig::{allowed_endpoints, allowlist_ref}` | Read-only consumer; no schema change. Doc updated to reflect new authoritative semantics. |
| `controller/src/policy_fetcher.rs` (S12.b/c/d) — `fetch_and_verify`, digest-keyed `cache`, `SignerPolicyConfig`, `SharedSignerPolicy` | Reused unchanged. New `resolve_allowlist[_with_handle]` and `AllowlistResolution` consume them. New separate `lkg_cache` (per-`(ns,name)`, no TTL). |
| `controller/src/status/mod.rs` — `build_running_status_patch_with_extras`, `running_status_matches_with_extras`, `build_degraded_status_patch` | Reused unchanged. Now flows up to three conditions instead of one. |
| `controller/src/status/conditions.rs` — `preserve_transition_time`, `reason::*`, `TYPE_ALLOWLIST_VERIFIED` | Added `TYPE_ALLOWLIST_AUTHORITATIVE`, `TYPE_ALLOWLIST_DRIFT`, and reasons `INLINE`, `STALE_LKG`, `FAILED_CLOSED`, `INLINE_DIFFERS_FROM_ARTIFACT`, `INLINE_CLEARED`. |
| `controller/src/reconciler/mod.rs` — NetworkPolicy build + running-status patch site | One call site replaced (resolver computed once, drives both NP egress and status). New early-return on `fail_closed_no_lkg` after NP write — pod is **not** deployed. |
| `deploy/helm/azureclaw/templates/crd.yaml` | New `Allowlist` printer column (priority 1). `allowlistRef` description updated. |

**Why no new module:** the resolution logic is a thin layer over the
existing fetcher (~250 LOC of state machine in `policy_fetcher.rs`);
splitting it out would mean exposing `LkgEntry` cross-module without
benefit. Keeping it co-located with `fetch_and_verify` makes the LKG
write-on-success invariant local to the function that produces the
verified result.

## Trust model — what changed vs. S12.b–d

| Aspect | S12.b–d (status-only) | S12.e (authoritative) |
|---|---|---|
| Source of truth for NP egress | inline `allowedEndpoints` only | verified canonical artifact when `allowlistRef` is set; inline only when ref is unset |
| Verify failure outcome | `AllowlistVerified=False/<reason>` Condition only; NP unaffected | LKG fallback if present, else fail-closed (no user egress + Degraded + early-return) |
| Inline + ref both set | both used independently | **artifact wins**; inline ignored; `AllowlistDrift=True/InlineDiffersFromArtifact` if they differ |
| Feature gate | `AZURECLAW_FEATURE_SIGNED_ALLOWLIST=1` required | none — always-on |

## Threat model

### Threats addressed

1. **Compromised reviewer / CR-author writes a permissive inline
   allowlist hoping verify will fail and inline is silently used as
   fallback.**
   *Mitigation:* on verify-fail-no-LKG the resolver returns
   `endpoints = None` — inline is **never** used as a fallback when a
   `allowlistRef` is set. Test: `resolve_with_ref_does_not_silently_use_inline_as_fallback`.

2. **Operator publishes a tampered artifact (digest replay, signer
   mismatch, canonical-form violation).**
   *Mitigation:* sigstore + SignerPolicy gate from S12.b/c/d still apply
   verbatim — `fetch_and_verify` is the same code path. The new layer
   only consumes its result.

3. **Verify-fail rides through across operator-visible controller
   events (restart, pod replacement, helm upgrade).**
   *Mitigation:* LKG is **in-process only**. Controller restart drops
   it deliberately so the first post-restart reconcile of a
   verify-failing sandbox cannot ride a stale allowlist across an
   operator-visible event. Test:
   `controller_restart_simulation_drops_lkg_fail_closed`.

4. **Drift between inline and signed artifact silently broadens
   egress (operator forgot to update inline after publishing a
   stricter artifact).**
   *Mitigation:* artifact wins on every reconcile;
   `AllowlistDrift=True/InlineDiffersFromArtifact` surfaces the
   condition for monitoring (`kubectl wait` /
   `kubectl get cs -o wide`). Drift detection is set-equal after
   normalization (host lowercase + default-port-443) so cosmetic
   reordering doesn't false-positive.

5. **Transient network blip flips `AllowlistVerified` to False and
   collapses egress.**
   *Mitigation:* `FetchError::Transient` is treated specially —
   prior conditions are preserved verbatim; LKG endpoints (if any)
   are programmed; no flip in status. Only the dropdown to fail-closed
   happens when LKG is also absent (so a fresh sandbox cannot ride a
   transient error into broad egress either).

### Threats NOT addressed (out of scope / accepted)

| Threat | Why accepted |
|---|---|
| Persisting LKG to disk / etcd | Persistence reintroduces threat #3 (stale allowlist surviving operator-visible events). The trade-off is documented: first reconcile after a restart of a verify-failing sandbox fails closed. |
| Cross-pod LKG sharing (HA replicas) | The leader-elected controller (S5.b) means at most one replica reconciles; the standby has no LKG. After a leader handoff, the new leader fails closed for one reconcile. Acceptable. |
| Reading `etcd` to recover LKG after restart | Same threat #3; explicitly avoided. |
| Allowlist artifact signed but operator's SignerPolicy is broader than expected | Out of scope of S12.e; addressed by S12.d SignerPolicy ConfigMap review and rotation. |

## Fail-closed semantics — concrete behavior

| Scenario | NP `sandbox-policy` | Pod | Status |
|---|---|---|---|
| `allowlistRef` unset, inline empty | baseline rules only | created | (no S12 conditions) |
| `allowlistRef` unset, inline set | baseline + non-443 inline | created | `AllowlistAuthoritative=False/Inline` |
| `allowlistRef` set, verify ok | baseline + non-443 from artifact | created | `Verified=True`, `Authoritative=True/Verified`, `Drift=…` |
| `allowlistRef` set, verify fails, LKG present | baseline + non-443 from LKG | created | `Verified=False/<reason>`, `Authoritative=False/StaleLKG` |
| `allowlistRef` set, verify fails, **no LKG** | **baseline only** (no user rules) | **NOT created** | Degraded + `Verified=False/<reason>`, `Authoritative=False/FailedClosed` |
| Transient error, LKG present | baseline + non-443 from LKG | created | prior conditions preserved |
| Transient error, no LKG | baseline only | **NOT created** | Degraded |

The "baseline" rules are the always-allowed egress that AzureClaw
itself depends on (DNS, IMDS for the inference-router only via
iptables UID-restriction, HTTPS port 443 for Workload Identity / AOAI
/ Foundry / Content Safety, and the AGT mesh + relay ports). These
are written **regardless** of allowlist resolution outcome — without
them the inference-router cannot acquire tokens, so the sandbox
would be dead-on-arrival.

> **Why deny-all is not a separate code path.** A K8s pod with no
> NetworkPolicy is **unrestricted**. Writing a NetworkPolicy with only
> the baseline rules (no user-defined egress) is what produces the
> "fail closed" outcome. We always write `sandbox-policy` so an
> operator who deletes inline `allowedEndpoints` and unsets the ref
> doesn't accidentally unrestrict the pod.

## Implementation invariants

1. **LKG is updated only on a successful verify** (Branch 2 of
   `resolve_allowlist_with_handle`). A successful LKG-fallback (Branch 3)
   does not refresh the LKG — that would mask a long-running verify
   outage from operators looking at status alone.
2. **Drift state is per-sandbox** — `LkgEntry { drift_active,
   drift_clear_counter }` lives alongside endpoints in the LKG map.
   Drift clears after ≤2 reconciles emitting `False/InlineCleared`,
   then drops out of status entirely (JSON-merge-patch semantics).
3. **`fail_closed_no_lkg` short-circuits pod deployment.** The
   reconciler returns `Action::requeue(60s + jitter)` after writing
   the (no-user-rules) NP and stamping Degraded. The pod is not
   created or restarted; on a successful next reconcile, `Step 4:
   Deploy sandbox pod` runs as normal.
4. **`Patch::Merge` semantics for `status.conditions`** — the array is
   replaced wholesale by each patch, so dropping a condition from the
   resolver's output drops it from status. This is by design for the
   drift-clear debouncing.
5. **`running_status_matches_with_extras` matches type+status+reason
   (not message)** — message updates do not churn `resourceVersion`.
   Borrowed from existing S12.b infrastructure unchanged.

## Operational signals

`kubectl get clawsandbox -o wide` shows the new `Allowlist` column:

```
NAME    PHASE     RUNTIME   MODEL      ISOLATION   ALLOWLIST   AGE
demo    Running   OpenClaw  gpt-4o     standard    True        3m
brittle Failed    OpenClaw  gpt-4o     standard    False       2m
```

For diagnosis:

```bash
kubectl get cs <name> -o jsonpath='{.status.conditions[?(@.type=="AllowlistAuthoritative")]}'
```

Reasons map directly to remediation:

- `Verified` — healthy (artifact wins)
- `StaleLKG` — verify is failing; check the `AllowlistVerified`
  condition's `reason` (e.g. `SignerPolicyMissing`, `Unauthorized`,
  `DigestMismatch`) and remediate; the LKG is keeping the sandbox up
- `FailedClosed` — first reconcile after restart with a still-broken
  artifact, OR a fresh sandbox whose first verify failed; same
  remediation as `StaleLKG`, but pod will not start until verify
  succeeds
- `Inline` — `allowlistRef` is unset; this is the legacy path

## Backwards compatibility / migration

- The `AZURECLAW_FEATURE_SIGNED_ALLOWLIST` env var is now a no-op.
  Setting or unsetting it has no effect. The Helm chart no longer
  references it. (Listed in CHANGELOG as a removal.)
- Existing CRs that do **not** set `spec.networkPolicy.allowlistRef`
  are unaffected — same NP shape, no new conditions emitted (the
  legacy `Inline` Authoritative=False condition is added only if
  `allowedEndpoints` is non-empty, so a sandbox with no networking
  config remains condition-clean).
- Existing CRs that **do** set `allowlistRef` but never had verify
  succeed (because the gate was off) will now run verify on the next
  reconcile. If a SignerPolicy is configured (cluster ConfigMap from
  S12.d, or env fallback), they should verify and proceed normally.
  If neither is configured, they will fail closed —
  operator-corrective action: install SignerPolicy ConfigMap or
  unset `allowlistRef`.
- No installed base; no migration ceremony.

## Tests

New (~16) in `controller/src/policy_fetcher.rs`:

- Resolution branches: `resolve_no_network_policy_returns_default`,
  `resolve_inline_only_emits_authoritative_inline`,
  `resolve_inline_empty_no_ref_emits_no_conditions`,
  `resolve_with_ref_no_signer_no_lkg_fails_closed`,
  `resolve_with_ref_no_signer_with_lkg_uses_lkg`,
  `resolve_with_ref_does_not_silently_use_inline_as_fallback`,
  `resolve_inline_with_ref_does_not_emit_authoritative_inline`,
  `resolve_fail_closed_emits_two_conditions`,
  `resolve_malformed_signer_policy_fails_closed_with_specific_reason`.
- LKG: `lkg_round_trip_get_put_clear`,
  `controller_restart_simulation_drops_lkg_fail_closed`.
- Helpers: `endpoint_lists_equivalent_normalizes_case_and_default_port`,
  `endpoint_lists_equivalent_distinguishes_ports`,
  `endpoint_lists_equivalent_distinguishes_hosts`,
  `canonical_to_endpoint_config_round_trips_host_port`,
  `allowlist_resolution_default_is_no_op`.

Removed: 5 tests covering the old feature-gate behavior
(`feature_disabled_by_default`, `feature_enabled_when_env_one`,
`feature_disabled_for_non_one_truthy_values`,
`fetch_returns_feature_disabled_when_gate_off`,
`with_handle_returns_none_when_feature_gate_off`).

Net: 401 → 412 controller tests passing.

## Out of scope (deferred)

- **Persistent LKG (etcd / file).** Deliberately rejected — see Threat #3.
- **Per-replica LKG sync in HA controllers.** Leader election
  (S5.b) means only one replica reconciles; standby holds no LKG.
- **Surfacing fail-closed-no-LKG as `Phase: Failed` instead of
  Degraded.** Phase is reserved for the runtime kind / pod
  lifecycle; allowlist resolution failure is an orthogonal axis
  exposed via `Conditions` + `Degraded`.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
