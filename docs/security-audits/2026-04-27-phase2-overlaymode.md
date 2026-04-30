# 2026-04-27 — Phase 2 S8 `phase2-overlaymode` security audit

> **Implementation plan reference:** §2.1 lines 263–275 (three-mode taxonomy: Native | Translate | Overlay), §8 Phase 2 entry "S8 `phase2-overlaymode` — sigs/agent-sandbox real Overlay", §15.2 #7 first half.
>
> **§14.6 destination:** Multi-runtime hosting (column 11) — partial. S8 contributes the Overlay half; S9 (`migrate to-overlay-mode`, `kubectl claw convert`) lands the CLI half.

## §0 — Existing implementation surveyed (§0.2 #11 anti-duplication)

Before any code was written, the implementer audited the existing reconciler surface to ensure S8 extends seams rather than parallel-implementing them. The surveyed surfaces and the reuse decisions:

| Existing surface | Path | Reuse decision |
|---|---|---|
| `LocalObjectRef { name: String }` | `controller/src/mcp_server.rs:157` | **Reused.** Fifth client now (signing/jwks ref, profile, agent-card, guardrail-profile, **upstream sandbox ref**). No second `ObjectReference`-shaped type introduced. |
| `UpstreamCompatibilityConfig` | `controller/src/crd.rs:104` | **Extended in place.** Added `upstream_sandbox_ref: Option<LocalObjectRef>`; the existing `sigs_agent_sandbox: Option<String>` field gains a fourth accepted value (`"overlay"`). Schema migration is purely additive — no field rename, no breaking change. |
| `crate::status::conditions::preserve_transition_time` | `controller/src/status/conditions.rs` | **Reused** unchanged for the new three-condition matrix (Ready / Progressing / Suspended). |
| `crate::status::stamp_degraded` + `degrade!` macro in reconciler | `controller/src/status/mod.rs:179` + `reconciler/mod.rs:232` | **Reused** for both new failure modes (overlay-without-ref, unknown sigs-agent-sandbox value). Errors flow through the same Degraded stamp + 60 s requeue path as every other spec-invalid case. |
| `build_running_status_patch` / `running_status_matches` | `controller/src/status/mod.rs:25,81` | **Mirrored**, not extended in place — overlay status has a distinct `phase` value (`"Overlay"`) and a different condition shape (Suspended=True), so it gets its own pair (`build_overlay_status_patch` / `overlay_status_matches`). The two pairs follow the same shape so a future S7 condition refactor can unify them. |
| `'deployment_block` labelled-block break | New (this slice) | The existing reconciler `reconcile()` is monolithic (~1.5 k lines). Wrapping the 520-line Step-4 Deployment block in a labelled block + early-`break 'deployment_block` is the minimum-diff way to gate it on `overlay_mode` without re-indenting the entire body. The block survives until S15 (`phase2-hotspot-pass3`) reduces `reconciler/mod.rs` below 800 LOC; at that point the deployment block becomes its own helper function and the labelled-block trick is removed. |
| Helm `crd.yaml` (ClawSandbox CRD) | `deploy/helm/azureclaw/templates/crd.yaml` | **Untouched.** `upstreamCompatibility` was schema-only in Phase 1 and the helm template never mirrored it. `kube-rs` registers the runtime schema from the Rust struct (which is the contract); the helm CRD is admission-only. There is no helm-drift test for ClawSandbox today (only McpServer / ToolPolicy / A2AAgent / InferencePolicy / ClawEval). Adding admission-side enforcement is deferred to a future slice that introduces `claw_sandbox_validations()`. |

**No duplicate seam introduced.** `ci/no-stubs.sh` and `ci/no-custom-crypto.sh` pass cleanly with `BASE_REF=origin/dev`.

## §1 — AGT boundary

S8 changes **no** AGT surface. AGT 3.3.0 is consumed unchanged; `governance_config` (the `Spec.governance` block) is hoisted out of the deployment block so it is computed regardless of mode, but no governance enforcement runs differently in overlay mode. Specifically:

- The router's AGT client is not instantiated in overlay mode (the router container does not exist — its Pod is upstream-owned).
- The governance ConfigMap (Step 4c) is still created so an operator who later flips back to Native mode does not lose policy state.
- AGT trust-store rebuild is not triggered by overlay-mode flips — the upstream Sandbox CR's lifecycle is independent.

S8 **does not** weaken any AGT-enforced invariant. The overlay is an *intentional governance hand-off* to the upstream operator, not a bypass.

## §2 — STRIDE threat model (S8-specific)

| Threat | Vector | Mitigation in S8 |
|---|---|---|
| **S — Spoofed upstream Sandbox CR ref** | Operator A's `ClawSandbox` references operator B's upstream `Sandbox` CR by name | `LocalObjectRef` is **namespace-local** by construction (no `namespace:` field). The reconciler resolves the upstream CR within `azureclaw-{name}` (the controller-owned namespace it just created). Cross-namespace ref is impossible without a schema change. |
| **T — Tampered overlay status** | An attacker patches `ClawSandbox.status.sandboxPod` to mislead `kubectl get` | Status sub-resource RBAC is unchanged; only the controller's ServiceAccount can patch `.status`. The `upstream/` prefix is computed by the controller from the spec — a bad spec produces Degraded, not a misleading status. |
| **R — Repudiation (Pod ownership ambiguity)** | Operator claims AzureClaw "started" the Pod when an upstream operator did | `phase: "Overlay"` + `Suspended=True / Reason=OverlayMode` + `sandboxPod: upstream/<ref>` make the hand-off explicit on the CR. `kubectl describe clawsandbox` shows the upstream CR name in the Suspended condition's message. |
| **I — Information disclosure (cross-mode leak)** | Switching from Native → Overlay leaves stale Deployments / blocklist CronJobs running | The controller never deletes resources it stops creating; reconcile is additive. Operators must `kubectl delete deployment/<name>` themselves on a mode flip. **Documented gap.** A future slice can add a delete-on-mode-change finalizer; for S8 we accept the manual-cleanup ergonomics in exchange for safety (never auto-delete an in-flight Pod). |
| **D — DoS (overlay loop)** | Hot reconcile loop bumps `resourceVersion` every reconcile | New `overlay_status_matches` idempotency guard mirrors `running_status_matches` — same byte-equality check on `phase`, `namespace`, `observedGeneration`, `sandboxPod`, and Ready=True. Verified by `overlay_status_matches_returns_true_for_settled_overlay_status` test. |
| **E — Privilege escalation (overlay-mode bypass)** | Attacker sets `sigsAgentSandbox: "overlay"` to skip the AzureClaw NetworkPolicy | NetworkPolicy creation (Step 3) runs **before** the deployment block and is **not** gated on overlay mode. The overlay namespace, SA, and NetworkPolicy are created identically in Native and Overlay. The upstream Pod inherits the same egress restrictions because NetworkPolicy is namespace-scoped, not Pod-scoped. |

## §3 — Out of scope (deferred)

- **Upstream `Sandbox` CR watcher / status mirroring.** Reading the upstream CR's `.status` and reflecting it onto `ClawSandbox.status.upstreamConditions` requires the upstream CRD to be installed and a discovery-based informer. Deferred to a future slice (post-S7 once the Conditions matrix lands).
- **CRD CEL admission for ClawSandbox.** No `claw_sandbox_validations()` exists yet — overlay-requires-ref is enforced at runtime via `degrade!`. A future slice can hoist into CEL once a ClawSandbox validations function lands.
- **`kubectl claw convert <upstream-Sandbox> → <ClawSandbox-overlay>`.** Lands in S9 (`phase2-migrate-cli`).
- **Overlay-mode `Suspend` actuator.** When `regressionAction: Suspend` lands in S6+S7's regression actuator, it must understand that overlay mode means the upstream operator owns suspension. Out of scope here.
- **Auto-cleanup on Native → Overlay mode flip.** See STRIDE I above.

## §4 — Implementation surface

| File | Δ | Description |
|---|---|---|
| `controller/src/crd.rs` | +~150 LOC, +5 unit tests | `LocalObjectRef` import; `UpstreamCompatibilityConfig` gains `upstream_sandbox_ref`; `is_overlay_mode()` + `overlay_target_name()` pure helpers; expanded field-level docs. |
| `controller/src/status/conditions.rs` | +5 LOC | New `TYPE_SUSPENDED` constant + new `reason::OVERLAY_MODE` constant. |
| `controller/src/status/mod.rs` | +~110 LOC + 6 unit tests | `build_overlay_status_patch` / `overlay_status_matches`. |
| `controller/src/reconciler/mod.rs` | +~70 LOC, ~3 LOC of indentation | Overlay pre-flight (Degrade on missing-ref / unknown-value); `'deployment_block` wrapper with early-`break`; blocklist gated; Step 5 dispatches on overlay target. `governance_config` + `blocklist_cm_name` hoisted out of deployment block. |
| `CHANGELOG.md` | +60 LOC | S8 entry inserted above S6 (chronological-newest-first within Phase 2). |

## §5 — Field semantics rationale

**Why string-with-magic-value `"overlay"` rather than an enum?** The `sigs_agent_sandbox` field is a string in Phase 1 (`Option<String>` in `crd.rs`). Phase 1 schema is in production on `dev`; rotating to an enum would force a `v1alpha1 → v1alpha2` schema change purely for one field. We accept the runtime-validated string for Phase 2 and revisit at the planned `v1alpha2` boundary in S13 (`phase2-v1alpha2-migration`). The CEL admission rule that lands with `claw_sandbox_validations()` will catch typos at admission time; until then the reconciler does the same job via `degrade!`.

**Why namespace-local upstream ref?** Cross-namespace references would let operator A's `ClawSandbox` claim governance over operator B's upstream `Sandbox`. STRIDE threat S above. The Phase 1 `LocalObjectRef` shape (no namespace field) is the right primitive.

## §6 — SSA + reconciler skip logic

The Deployment block uses `Patch::Apply` with `field-manager = "azureclaw-controller"` (Phase 1). Skipping the Apply in overlay mode does **not** strip ownership of an existing Deployment — `kube-rs` Apply only manages fields the controller owns; it does not delete the object. **Documented behaviour:** flipping a sandbox from Native to Overlay leaves the Deployment running, owned by the AzureClaw field-manager. Operators must `kubectl delete deployment/<name>` to retire it. See STRIDE I.

The blocklist CronJob (Step 4d) is similarly skipped, with the same flip-leaves-it-running semantics.

## §7 — Failure modes

| Trigger | Behaviour |
|---|---|
| `sigsAgentSandbox: "overlay"`, `upstreamSandboxRef: null` | `Degraded=True / Reason=SpecInvalid / Message="upstreamCompatibility.sigsAgentSandbox=\"overlay\" requires upstreamCompatibility.upstreamSandboxRef.name"`. Reconcile requeues every 60 s. |
| `sigsAgentSandbox: "overlay"`, `upstreamSandboxRef.name: ""` | Same as above. Empty string is treated as missing. |
| `sigsAgentSandbox: "Overlay"` (capitalised typo) | `Degraded=True / Reason=SpecInvalid / Message="upstreamCompatibility.sigsAgentSandbox: unknown value \`Overlay\` (expected off|observe|translate|overlay)"`. |
| `sigsAgentSandbox: "off" \| absent`, `upstreamSandboxRef: { name: "x" }` | Native mode runs unchanged; the ref is silently ignored. (Validated by `overlay_target_name_extracts_only_in_overlay_mode` test.) |
| Upstream `Sandbox` CR doesn't exist in the namespace | **Not detected by AzureClaw.** S8 does not watch the upstream CR. The overlay's NetworkPolicy + SA + ConfigMap simply target a Pod that doesn't exist; nothing fails. The operator sees no Pod for their `ClawSandbox`. Detection lands with the upstream-watcher slice (deferred). |

## §8 — Test surface

Controller workspace: 264 → 276 (+12).

- 5 tests on `UpstreamCompatibilityConfig` helpers (`is_overlay_mode_true_only_for_overlay_string`, `overlay_target_name_extracts_only_in_overlay_mode`, `defaults_are_native_mode`, `serde_round_trip_preserves_overlay_fields`, `serde_omits_upstream_ref_when_none`).
- 6 tests on overlay status helpers (emit-shape, matcher rejects-status-missing/wrong-phase/wrong-ref/stale-generation, settled-true, transition-time-preservation).

`cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` all green. CI gates (`no-stubs.sh`, `no-custom-crypto.sh`, `check-loc.sh`) all pass with `BASE_REF=origin/dev`.

## §9 — Verify-don't-guess citations (§0.2 #10)

- `sigs.k8s.io/agent-sandbox` repo (referenced in `docs/sigs-agent-sandbox-compat.md`) is the upstream contract this overlay mode integrates against. S8 adds **no** runtime call into the upstream CR — only the namespace-local `LocalObjectRef` pointer to it, surfaced in `status.sandboxPod`. The next slice that watches the upstream CR's `.status` will need `kube-rs` discovery + the upstream CRD installed; that is intentionally deferred.
- AGT 3.3.0 (`Cargo.toml` workspace pin) is unchanged; no AGT API used in this slice differs from S5/S6.

## §10 — Ops surface

- Operators flip a sandbox into overlay mode by patching `spec.upstreamCompatibility`:
  ```yaml
  spec:
    upstreamCompatibility:
      sigsAgentSandbox: overlay
      upstreamSandboxRef:
        name: my-upstream-sandbox
  ```
- `kubectl get clawsandbox` shows `phase: Overlay` and `sandboxPod: upstream/my-upstream-sandbox`.
- `kubectl wait --for=condition=Ready clawsandbox/<name>` works (overlay reconciles to `Ready=True`).
- `kubectl wait --for=condition=Suspended=True clawsandbox/<name>` is a new way to gate "AzureClaw stopped driving a Pod" — useful in CI / blueprints.
- `azureclaw connect <name>` is **expected to fail** in overlay mode (no AzureClaw Pod). A future CLI improvement can detect overlay mode and redirect to the upstream CR; not S8.

## §11 — Sign-offs

- **Reviewer 1 (controller / reconciler):** All changes are additive to the reconcile path; no existing-mode behaviour modified. Idempotency guard mirrored from `running_status_matches`. Failure modes route through the same `degrade!` macro as Phase 1 validation. NetworkPolicy + SA creation order unchanged — overlay mode does not bypass the egress policy. **Approved.**

- **Reviewer 2 (security / threat model):** STRIDE pass surfaced one documented gap (Native → Overlay flip leaves Deployments running). Acceptable for Phase 2 in exchange for never auto-deleting an in-flight Pod; explicit operator action required. Cross-namespace ref impossible by schema. Status-sub-resource RBAC unchanged. **Approved.**


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
