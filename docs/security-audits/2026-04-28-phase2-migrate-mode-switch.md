# Phase 2 — `phase2-migrate-mode-switch` security audit (2026-04-28)

## §0 Reuse map (§0.2 #11)

- **No new CRD field.** Targets the existing
  `spec.upstreamCompatibility.{sigsAgentSandbox, upstreamSandboxRef}`
  fields shipped in S8 (#57).
- **No controller change.** The OverlayMode reconciler logic — the
  pre-flight that requires `upstreamSandboxRef` when
  `sigsAgentSandbox == "overlay"` and the deployment-block skip — all
  landed in S8. This slice is the operator-facing tool that drives
  the existing reconciler path.
- **No new dependency.** `commander` + `chalk` + `execa` already in
  the CLI for every other command.
- **`kubectl patch --type=merge`** is the standard mutation primitive
  used elsewhere in the CLI (`add`, `credentials`); no new transport.
- **Mode literals** (`off / observe / translate / overlay`) and field
  names (`sigsAgentSandbox`, `upstreamSandboxRef`) sourced from
  `controller/src/crd.rs` `UpstreamCompatibilityConfig` doc comment
  (lines 104-134) — the single source of truth.

## §1 AGT boundary

Read+patch surface only on the `ClawSandbox` CR; AGT is not in the
loop. The patched fields drive the controller's reconcile path; the
controller itself emits AGT receipts (Phase 3). No AGT change in
this slice.

## §2 STRIDE

| Threat | Mitigation |
|---|---|
| **Spoofing** — operator patches a sandbox they don't own | Patch is gated by the caller's kubeconfig RBAC (same as `kubectl patch`). No bypass. |
| **Tampering** — operator strands a sandbox in an inconsistent state | Three guards: (1) `validateMode` rejects bad combinations client-side (e.g., `--upstream-ref` for non-overlay; missing `--upstream-ref` for overlay). (2) JSON merge patch always sets *both* `sigsAgentSandbox` and `upstreamSandboxRef` (the latter to `null` when leaving overlay) so a stale ref cannot survive a mode flip. (3) The controller's existing pre-flight (S8) `degrade!`s the sandbox if `overlay` is set without a ref — the CLI cannot create a half-applied state the controller would accept. |
| **Repudiation** | `kubectl patch` is auditable via the cluster's audit log + the SSA `managedFields` map. The next `azureclaw attest` call surfaces the new manager (`kubectl-patch` or whatever the operator's kubeconfig name is) under `fieldOwners`. |
| **Information disclosure** | No secret material printed. Patch contents printed only under `--dry-run` or `--format json` and contain only mode + upstream-ref name (operator-supplied input). |
| **Denial of service** | Single `kubectl get` + single `kubectl patch` per invocation. No loops, no informers, no watches. Bounded. |
| **Elevation of privilege** | The CLI cannot grant itself permissions it does not have. `kubectl patch` requires `patch` on `clawsandboxes.azureclaw.azure.com`; cluster admins gate this via RBAC. |

## §3 Out of scope (S9.2 / Phase 3)

- `azureclaw migrate from-kagent` — Solo.io kagent CR → ClawSandbox
  translator. Needs the upstream kagent CRD shape; separate PR.
- Real `azureclaw convert` — YAML translator from upstream
  `sigs.k8s.io/agent-sandbox` shapes. Currently a Phase 0 exit-3
  skeleton; S9.2 implements it.
- `azureclaw migrate verify <name>` — checks that an OverlayMode
  sandbox is consistent with its upstream Sandbox CR. Requires an
  upstream CRD informer; deferred to a future slice.
- Multi-cluster or fleet mode (`migrate --all` / `--cluster-config-dir`)
  — not in scope for Phase 2; the right primitive to layer on top is
  in place (the per-sandbox command is idempotent + JSON-output capable).

## §4 Implementation surface

`cli/src/commands/migrate.ts` (~330 LOC, single new file):

- `MIGRATE_MODES` — the four mode literals from the CRD doc.
- `validateMode(mode, upstreamRef)` — pure; returns array of human
  errors. Called before any `kubectl` invocation.
- `buildModePatch(mode, upstreamRef)` — pure; returns the JSON merge
  patch. Always emits both fields; uses `null` (RFC 7396 delete) for
  `upstreamSandboxRef` when leaving overlay.
- `readCurrentMode(spec)` — pure; reads the two fields off a
  ClawSandbox spec, defaulting missing/unknown to `off + null`.
- `summariseTransition(current, target)` — pure; returns
  `{message, noop}` so the orchestrator can skip a no-op patch.
- `modeDisplay(mode)` — pure; renders `off` as `native` for human
  output (matches product positioning).
- `runPatch / fetchCurrentSpec` — execa-only orchestration.
- `runMigrate(name, target, opts)` — top-level orchestrator.
- `migrateCommand()` — Commander factory with five subcommands
  (`to-overlay`, `from-overlay`, `to-translate`, `to-observe`,
  `to-native`). `to-overlay` uses `requiredOption` for
  `--upstream-ref` (Commander pre-validates).
- `__test` named export exposing the five pure helpers.

`cli/src/commands/migrate.test.ts` — 22 vitest cases.

`cli/src/cli.ts` — one import + one `addCommand` under the existing
"Interop" section (alongside `convert` and `a2a`).

## §5 Field semantics

The patch is always rooted at `spec.upstreamCompatibility` and always
contains both fields:

```
overlay:    { sigsAgentSandbox: "overlay",   upstreamSandboxRef: { name } }
translate:  { sigsAgentSandbox: "translate", upstreamSandboxRef: null }
observe:    { sigsAgentSandbox: "observe",   upstreamSandboxRef: null }
off:        { sigsAgentSandbox: "off",       upstreamSandboxRef: null }
```

JSON merge patch (RFC 7396) interprets `null` as "delete the field"
on the server side, which matches the controller's
`Option<LocalObjectRef>::skip_serializing_if = "Option::is_none"`
round-trip semantic. Asserted directly in tests
(`buildModePatch — uses null (not undefined) for upstreamSandboxRef
removal — RFC 7396`).

## §6 SSA + reconciler skip

Uses JSON merge patch (`--type=merge`), not server-side apply, for
two reasons:

1. **Field ownership semantics.** The CLI is invoked ad-hoc by an
   operator; we want the patch attributed to the operator's kubeconfig
   manager (e.g. `kubectl-patch` or the user's name), not to a
   long-lived field manager. The next `azureclaw attest` run
   (S11/S11.1) will surface this under `fieldOwners`, and an
   `--baseline` diff will flag it as `fieldOwnerAdded` — which is
   exactly the change-control signal we want.
2. **Atomicity.** Both fields ship in one patch object so the server
   either accepts both or rejects both; there is no window where a
   sandbox could be in `overlay` mode without a ref (which the
   controller would `degrade!` anyway, but client-side prevention is
   cheaper than server-side rejection).

The controller's existing reconcile path (S8) handles the new spec
without modification.

## §7 Failure modes

| Failure | Behaviour |
|---|---|
| `--upstream-ref` missing for `to-overlay` | Commander `requiredOption` rejects; exits non-zero before any kubectl call. |
| `--upstream-ref` passed to non-overlay subcommand | `validateMode` flags it; CLI prints red `✗` and exits **2**. |
| Empty-string `--upstream-ref` for `to-overlay` | `validateMode` flags it; exits **2**. |
| Sandbox CR not found at `kubectl get` time | execa rejects; CLI prints red error + exits **1**. |
| `kubectl patch` fails (RBAC, network) | execa rejects; CLI prints red error + exits **1**. |
| Sandbox already in target state | Pre-flight detects no-op; CLI prints `(already in target state)` + exits **0** without applying. JSON output sets `noop: true`. |
| `--dry-run` | Prints the JSON merge patch + advisory line; no kubectl call. Exits **0**. |
| Unknown mode value in current sandbox spec | `readCurrentMode` defaults to `off`; transition is summarised against the default (the controller would `degrade!` the original anyway, so the operator's mental model is "we're recovering from a bad state"). |

## §8 Test surface

`cli/src/commands/migrate.test.ts` — 22 vitest cases:

- 6 × validateMode (every known mode, overlay-needs-ref, ref-on-non-
  overlay rejected, empty-ref rejected, unknown-mode rejected, etc.)
- 4 × buildModePatch (native shape, overlay shape, translate/observe
  ignore-but-null, RFC 7396 null-not-undefined invariant)
- 4 × readCurrentMode (defaults, overlay round-trip, unknown-mode
  fallback, stale ref preserved for noop detection)
- 5 × summariseTransition (noop variants, ref-only change, the two
  canonical user stories `native → overlay` and `overlay → native`)
- 3 × modeDisplay (off → native, passthrough, null/undefined → native)

CLI workspace test count: 315 → **337** (+22). vitest + `tsc --noEmit`
+ oxlint all green.

End-to-end smoke verified manually:
- `node dist/index.js migrate --help` lists all five subcommands.
- `node dist/index.js migrate to-overlay demo --upstream-ref legacy
  --dry-run` prints the expected JSON merge patch.
- `node dist/index.js migrate to-overlay demo --dry-run` (without
  ref) is rejected by Commander before any code runs.

## §9 Verify-don't-guess (§0.2 #10)

- **Mode literals + field names** read from `controller/src/crd.rs:104-126`
  (`UpstreamCompatibilityConfig.sigs_agent_sandbox` doc comment +
  `upstream_sandbox_ref` field).
- **JSON merge patch semantics** verified against RFC 7396: `null`
  values delete the corresponding key. The Rust controller's
  `Option<...>::skip_serializing_if = "Option::is_none"` round-trip
  matches this — when the server applies a `null`, the field
  disappears from the persisted object, which deserialises back as
  `None` on the controller. Tested directly via the
  `'"upstreamSandboxRef":null'` assertion.
- **`requiredOption` ordering vs custom validation.** Commander.js
  validates `requiredOption` *before* the action handler runs; our
  `validateMode` runs inside the handler and is a defense-in-depth
  check (e.g. for an empty-string ref that satisfies `requiredOption`
  but is operationally invalid).

## §10 Ops surface

```
# Day-zero adoption: wrap an upstream sigs.k8s.io/agent-sandbox CR
azureclaw migrate to-overlay legacy --upstream-ref legacy-agent

# Inspect what would change without applying
azureclaw migrate to-overlay legacy --upstream-ref legacy-agent --dry-run

# Switch back to pure AzureClaw (controller resumes Pod ownership)
azureclaw migrate from-overlay legacy

# Status-only mirror (no overlay, no Pod ownership)
azureclaw migrate to-observe legacy

# JSON output for CI / scripts
azureclaw migrate to-overlay legacy --upstream-ref legacy-agent --format json
```

Pairs with `azureclaw attest <name> --baseline <approved.json>`
(S11.1) for change control: capture the baseline before the migrate,
diff after.

## §11 Sign-offs

- **Author / dev:** AzureClaw Phase 2 implementer (this PR).
- **Reviewer:** to be filled at PR review.
