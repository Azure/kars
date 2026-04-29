# Changelog

All notable changes to AzureClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — Phase 2

### S15.f.8 `phase2-hotspot-plugin-cli-f8` — Foundry + http_fetch tool registrations

#### Refactored

- `cli/src/plugin.ts` 5071 → **4323 LOC** (−748, cumulative S15.f
  −2816, **78% to §4.2 cap of 3000**). Ten `api.registerTool` blocks
  (the nine `foundry_*` Foundry-shim tools + `http_fetch`) extracted
  to a new `cli/src/core/agt-tools/` directory:
  - `core/agt-tools/foundry.ts` — `registerFoundryTools(api, deps)`.
    Nine tools: `foundry_code_execute`, `foundry_image_generation`,
    `foundry_web_search`, `foundry_file_search`, `foundry_memory`,
    `foundry_conversations`, `foundry_evaluations`,
    `foundry_deployments`, `foundry_agents`. The bodies are
    byte-identical to the previous inline registrations; only the
    closure capture is replaced. `FoundryToolsDeps` threads `log`,
    `config` (for `config.model`), and a late-bound
    `getFoundryProject()` accessor — late binding is required because
    `initFoundry()` runs concurrently with `register()` and may
    complete after tool registration.
  - `core/agt-tools/http-fetch.ts` —
    `registerHttpFetchTool(api)`. The single `http_fetch` tool that
    routes outbound HTTP through the inference router's egress
    proxy.
  - `core/safe-json.ts` — the small `safeJson(obj, maxLen)` helper
    previously defined inline in `register()` lifted to a
    module-level utility so cluster modules import it directly.
- The cluster of 10 tools (~750 LOC) now appears in `plugin.ts` as
  two function calls: `registerHttpFetchTool(api)` +
  `registerFoundryTools(api, { log, config, getFoundryProject: () => foundryProject })`.

#### Operational invariants

- Tool names, parameters, descriptions, and execute-body semantics
  are unchanged — vendored extension manifest in
  `~/.openclaw-data/extensions/azureclaw/` keeps surfacing the
  identical 10-tool list. Backward-compatible.
- No new mesh / spawn / handoff / OpenClaw-specific surface
  changes — those clusters remain in `plugin.ts` for S15.f.9.

#### Tests

- 454 pass / 2 skipped (unchanged); 30 lint warnings (unchanged).
  `tsc --noEmit`, `npm run lint`, `npm test`, `npm run build` all
  green on `dev` and on the new branch.
- Foundry tool integration paths exercised by existing
  `plugin.test.ts` suites; the extraction is a closure restructuring
  with no observable behaviour change.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-plugin-cli-f8.md`
  documents the tool-by-tool extraction and confirms zero attack
  surface change (same router endpoints, same parameters, same
  egress posture).



#### Refactored

- `cli/src/plugin.ts` 5598 → **5071 LOC** (−527, cumulative S15.f
  −2068, **69% to §4.2 cap**). The 531-LOC `_runHandoffOrchestration`
  background routine + its 7-LOC `_hp` progress-tracker helper
  extracted to `cli/src/core/agt-handoff.ts` (609 LOC).
- `HandoffDeps` bag threads the four pieces of plugin.ts state the
  function touches (`handoffProgress`, `agtInbox`, mesh client +
  identity accessors, the `meshSend` wrapper). `progress` and
  `inbox` pass by reference so mutations propagate identically to
  the original closure-captured behaviour.
- `_hp` becomes a closure inside `runHandoffOrchestration` that
  captures `progress` + `log` from `deps`; plugin.ts no longer needs
  its own `_hp`.
- No behavior change. Function body byte-identical apart from
  `agtMeshClient`/`agtIdentity` accessor calls and direct
  `nameToAmid`/`amidToName` imports from `core/amid-cache.ts`.

### S15.f.6 `phase2-hotspot-plugin-cli-f6` — plugin.ts in-process tool-loop extraction

#### Refactored

- `cli/src/plugin.ts` 6104 → **5598 LOC** (−506, cumulative S15.f
  −1541). The 521-LOC `processTaskWithTools` — the AGT sub-agent's
  in-process tool-calling loop — extracted to
  `cli/src/core/agt-task-loop.ts` (540 LOC).
- `TaskLoopDeps` bag threads the two pieces of plugin.ts state the
  loop reads / mutates (mesh client, handoff-interrupt flags); pure
  imports (`TASK_TOOLS`, `routerUrl`, `resolveAmidByName`,
  `sanitizeLog`) bind directly in the module.
- No behavior change. Single semantic shift: `resolveAmidByName`
  now called with the canonical 2-arg form `(name, routerUrl, opts?)`
  inside the new module since the plugin-scope wrapper is no longer
  reachable from `core/`.

### S15.f.5 `phase2-hotspot-plugin-cli-f5` — plugin.ts heartbeat + offload extraction

#### Refactored

- `cli/src/plugin.ts` 6488 → **6104 LOC** (−384, cumulative S15.f
  −1035). Two functional clusters extracted in one bundled PR
  (per the OSS-prep no-micro-PR mandate):
  - `core/agt-heartbeat.ts` — `recordMeshSession`, `agtReconnect`,
    `notifyInboxToMemory` (~156 LOC).
  - `core/agt-offload.ts` — `runOffloadTask`,
    `startProactiveOffloadIfNeeded` plus `OffloadDeps`/`RunOffloadOpts`
    interfaces (~339 LOC).
- plugin.ts keeps thin wrappers that capture the AGT singleton state
  via closure (`_offloadDeps()`); ~310 read/write sites elsewhere
  in plugin.ts are unchanged. Same dep-injection pattern as
  S15.f.3 mesh transport.
- No behavior change. `agtReconnect` mutates `agtConnected` via a
  `setConnected` callback (ES module live-binding constraint).

### S15.f.4 `phase2-hotspot-plugin-cli-f4` — plugin.ts task-tools array extraction

#### Refactored

- `cli/src/plugin.ts` 6648 → **6488 LOC** (−160, cumulative S15.f
  −651). The 11-tool OpenAI function-call descriptor array consumed
  by `processTaskWithTools` (offload / sub-agent LLM loop) extracted
  to `core/agt-task-tools.ts` as `TASK_TOOLS`. Pure data move; no
  closures, no captured variables.
- No behavior change.

### S15.f.3 `phase2-hotspot-plugin-cli-f3` — plugin.ts chunked mesh transport extraction

#### Refactored

- `cli/src/plugin.ts` 6890 → **6648 LOC** (−242, cumulative S15.f
  −491). The chunked-transport layer (`meshSend` + 
  `meshHandleTransportMessage` + `pendingTransfers` Map + TTL
  cleanup + `MESH_*` constants + `PendingMeshTransfer` interface,
  ~265 LOC) extracted to `core/mesh-transport.ts`. Signal Protocol
  crypto stays in plugin.ts; this is purely the JSON-splitting wire
  layer.
- `meshSend` keeps a thin wrapper that captures `agtIdentity` so all
  14+ call sites stay byte-identical.
- No behavior change.

### S15.f.2 `phase2-hotspot-plugin-cli-f2` — plugin.ts native-agent delegate extraction

#### Refactored

- `cli/src/plugin.ts` 6974 → **6890 LOC** (−84, cumulative S15.f
  −249). `delegateToNativeAgent` (the AGT task-request → native
  OpenClaw agent dispatcher) extracted to
  `core/agt-task-delegate.ts`. Pure stdlib helper; zero plugin-internal
  dependencies.
- No behavior change.

### S15.f.1 `phase2-hotspot-plugin-cli-f1` — plugin.ts redact + AMID-cache extraction

#### Refactored

- `cli/src/plugin.ts` 7139 → **6974 LOC** (−165, 2.3% of pre).
  First slice of the S15.f plugin.ts decomposition train. Lifts the
  shared utility primitives — log-redaction helpers and the AMID
  resolver/cache — into `core/log-redact.ts` (40 LOC) and
  `core/amid-cache.ts` (213 LOC). `redactSecrets` re-exported from
  plugin.ts to preserve the legacy import surface. Resolvers take
  `routerUrl` as a parameter to avoid a circular dep.
- No behavior change.

### S15.e.7 `phase2-hotspot-operator-cli-e7` — operator.ts delete + connect dialog extraction (closes S15.e)

#### Refactored

- `cli/src/commands/operator.ts` 1027 → **859 LOC** (cumulative
  S15.e: 2894 → 859, **−2035**, 70.3% reduction). The `x`-key
  destroy-confirm dialog and the `Enter`-key connect-to-agent PTY
  session extracted to `operator/dialogs/{delete,connect}.ts`
  (~104 + ~158 LOC). Closes the S15.e operator.ts decomposition
  train.
- §4.2 800-LOC cap accepted at 859 (59 over) — the residual is the
  dashboard shell (state decls, widget construction, refresh poll
  loop, render orchestrator, keymap bindings), each component being
  inherently coupled to the surrounding closure.
- No behavior change.

### S15.e.6 `phase2-hotspot-operator-cli-e6` — operator.ts spawn-dialog extraction

#### Refactored

- `cli/src/commands/operator.ts` 1279 → **1027 LOC** (cumulative
  S15.e: 2894 → 1027, **−1867**). The `n`-key spawn-agent dialog
  (`draw`/`close`/`startEdit`/`launch`/`onKey`) extracted to
  `operator/dialogs/spawn.ts` (~295 LOC) via `SpawnDialogContext`.
  Modal flag passes through a `setDialogOpen(open)` callback.
- No behavior change.

### S15.e.5c `phase2-hotspot-operator-cli-e5c` — operator.ts header render extraction

#### Refactored

- `cli/src/commands/operator.ts` 1318 → **1279 LOC** (cumulative
  S15.e: 2894 → 1279, **−1615**). `renderHeader` + `healthSummary`
  extracted to `operator/render/header.ts` (~98 LOC) via
  `HeaderRenderContext`.
- No behavior change.

### S15.e.5b `phase2-hotspot-operator-cli-e5b` — operator.ts security + AGT render extraction

#### Refactored

- `cli/src/commands/operator.ts` 1586 → **1318 LOC** (cumulative
  S15.e: 2894 → 1318, **−1576**). `renderSecurity`, `renderAGTFull`,
  `renderAGT` and the `ok(v)` color-dot helper extracted to
  `operator/render/security.ts` (~287 LOC) via `SecurityRenderContext`.
- `renderAGTFull` is pure (no widget side-effects) so it takes
  positional args rather than a context.
- No behavior change.

### S15.e.5 `phase2-hotspot-operator-cli-e5` — operator.ts cluster + topology render extraction

#### Refactored

- `cli/src/commands/operator.ts` 1880 → **1586 LOC** (cumulative
  S15.e: 2894 → 1586, **−1308**). `renderTopology` (with nested
  `makeBox`/`fitVis`/`visualLen`/`statusIcon`) → `operator/render/topology.ts`
  (~199 LOC); `renderCluster` + `makeBar` → `operator/render/cluster.ts`
  (~143 LOC).
- Closure-captured `sandboxes`, `securityStates`, `topologyBox`,
  `clusterData`, `clusterNodeBox`, `clusterInfoBox` now injected via
  `RenderContext` interfaces. Two thin wrappers in operator.ts keep
  call-sites unchanged.
- Removed now-unused imports `NodeInfo`, `sumPrometheusCounter`.
- No behavior change. Lint 21 → 20.

### S15.e.4 `phase2-hotspot-operator-cli-e4` — operator.ts action helpers extraction

#### Refactored

- `cli/src/commands/operator.ts` 1960 → **1880 LOC** (cumulative
  S15.e: 2894 → 1880, **−1014**). Four egress action helpers extracted:
  `approveDomain`, `denyDomain`, `enforceEgress`, `learnEgress` →
  `cli/src/commands/operator/actions.ts` (116 LOC) via
  `createActions(ctx)` factory.
- Closure-captured `sandboxes` (reassigned per refresh) is now
  injected as a `getSandboxes()` getter; `activityLog` and
  `kubeContext` are passed by reference. No behavior change; bodies
  byte-identical.

### S15.e.3 `phase2-hotspot-operator-cli-e3` — operator.ts security + cluster fetcher extraction

#### Refactored

- `cli/src/commands/operator.ts` 2483 → **1960 LOC** (cumulative
  S15.e: 2894 → 1960, **−934**). Five remaining fetchers extracted:
  `fetchEgressDomains`, `fetchSecurityState`, `fetchAgtQuick` →
  `cli/src/commands/operator/fetchers/security.ts` (351 LOC);
  `fetchMeshHealth`, `fetchClusterHealth` →
  `cli/src/commands/operator/fetchers/cluster.ts` (188 LOC).
- Closure-captured `kubeContext`, `devMode`, and (for `fetchAgtQuick`)
  the cached `SecurityState` are now explicit parameters; call sites
  in `refresh()` updated. Mutation semantics preserved.
- No behavior change.
- Lint warnings dropped from 27 → 21.

#### Tests

- All 454 CLI tests pass; tsc / lint / build clean.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-operator-cli-e3.md`

### S15.e.2 `phase2-hotspot-operator-cli-e2` — operator.ts sandbox-list fetcher extraction

#### Refactored

- `cli/src/commands/operator.ts` 2739 → **2483 LOC** (cumulative
  S15.e: 2894 → 2483, −411). Sandbox-list fetchers
  (`fetchSandboxes`, `fetchSandboxesAKS`, `fetchSandboxesDocker`)
  extracted to `cli/src/commands/operator/fetchers/sandboxes.ts`
  (287 LOC). Also moved `kctl(args, context?)` helper to
  `operator/helpers.ts` so the new fetcher module can reuse it.
- Closure-captured `kubeContext` now passed as explicit parameter
  to AKS variant; semantics identical.
- No behavior change; bodies byte-identical.

#### Tests

- All 454 CLI tests pass; tsc / lint (27 baseline) / build clean.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-operator-cli-e2.md`

### S15.e.1 `phase2-hotspot-operator-cli-e1` — operator.ts type + helper extraction

#### Refactored

- `cli/src/commands/operator.ts` 2894 → **2739 LOC** — first
  sub-slice of the S15.e operator.ts decomposition train.
  Module-level types (`HealthState`, `SandboxInfo`, `EgressDomain`,
  `SecurityState`, `NodeInfo`, `ClusterHealth`, `MeshHealth`)
  extracted to `cli/src/commands/operator/types.ts` (128 LOC);
  module-level pure helpers (`timeSince`, `sumPrometheusCounter`)
  extracted to `cli/src/commands/operator/helpers.ts` (65 LOC).
- No behavior change; all declarations are byte-identical to the
  originals. §4.2 cap (800) not yet met — multi-PR sub-train.

#### Tests

- All 454 CLI tests pass; tsc / lint (27 baseline) / build clean.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-operator-cli-e1.md`

### S15.d.4 `phase2-hotspot-up-cli-d4` — up.ts sandbox bring-up extraction (caps S15.d at 766 ✅)

#### Refactored

- `cli/src/commands/up.ts` 1182 → **766 LOC** — final sub-slice of
  the S15.d up.ts multi-PR sub-train. Sandbox bring-up phase
  (federated credentials + Foundry RBAC + ClawSandbox CR + wait
  for Running + WebUI port-forward + deployment summary +
  `saveContext()`) extracted to
  `cli/src/commands/up/sandbox_bringup.ts` (482 LOC). Caller is a
  9-line dispatch.
- **§4.2 cap achieved for `up.ts`** (1849 → 766 over d.1-d.4;
  cap = 800).

#### Tests

- All 454 CLI tests pass; 2 skipped pre-existing. `tsc --noEmit`,
  `lint` (27 warnings, baseline-matched), `build` clean. No
  behavioral change — body moved verbatim.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-up-cli-d4.md`
  (sign-offs: Core ✅, Security ✅).

### S15.d.3 `phase2-hotspot-up-cli-d3` — up.ts AgentMesh deploy extraction

#### Refactored

- `cli/src/commands/up.ts` 1296 → 1182 LOC (sub-slice d.3 of the
  S15.d up.ts multi-PR sub-train; d.4 to follow). AgentMesh
  infrastructure deploy phase (Inspektor Gadget eBPF, relay +
  registry deployment, external-registry shortcut, optional AGIC
  ingress) extracted to `cli/src/commands/up/agentmesh_deploy.ts`
  (174 LOC). The result triple `{ registryMode,
  globalRegistryUrl, globalRelayUrl }` flows forward unchanged to
  the ClawSandbox CR creation step and `saveContext()`.

#### Tests

- All 454 CLI tests pass; 2 skipped pre-existing. `tsc --noEmit`,
  `lint` (27 warnings, baseline-matched), `build` clean. No
  behavioral change — body moved verbatim.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-up-cli-d3.md`
  (sign-offs: Core ✅, Security ✅).

### S15.d.2 `phase2-hotspot-up-cli-d2` — up.ts preflight extraction

#### Refactored

- `cli/src/commands/up.ts` 1660 → 1296 LOC (sub-slice d.2 of the
  S15.d up.ts multi-PR sub-train; d.3 + d.4 to follow). Preflight
  phase (auto-detect dev mode, cached-context prefill, banner +
  tool checks, Azure auth + subscription, interactive prompts,
  RBAC + provider preflight, SKU availability check, dry-run plan
  print) extracted to `cli/src/commands/up/preflight.ts` (392 LOC).
  Caller is a 4-line dispatch returning `{ rg }` or `null`
  (dry-run). Helper `isValidAzureHost` moved to the new module
  and re-exported for the deploy section.

#### Tests

- All 454 CLI tests pass; 2 skipped pre-existing. `tsc --noEmit`,
  `lint` (27 warnings, baseline-matched), `build` clean. No
  behavioral change — body moved verbatim.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-up-cli-d2.md`
  (sign-offs: Core ✅, Security ✅).

### S15.d.1 `phase2-hotspot-up-cli` — up.ts fast-upgrade extraction

#### Refactored

- `cli/src/commands/up.ts` 1849 → 1660 LOC (sub-slice d.1 of the
  S15.d up.ts multi-PR sub-train; further sub-slices d.2-d.4 will
  continue toward the §15 800-LOC cap). Self-contained
  `--upgrade` fast-path (cached-context Helm rerun + sandbox
  fed-cred sync) extracted to `cli/src/commands/up/fast_upgrade.ts`.
  Caller invokes via `await import("./up/fast_upgrade.js")` and
  returns immediately afterwards.

#### Tests

- All 454 existing CLI tests pass; 2 skipped. `tsc --noEmit`,
  `lint` (27 warnings, baseline-matched), `build` clean. No
  behavioral change — body moved verbatim.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-up-cli.md`
  (sign-offs: Core ✅, Security ✅).

### S15.c `phase2-hotspot-inference-router-routes` — inference.rs hotspot decomposition

#### Refactored

- `inference-router/src/routes/inference.rs` 1359 → 776 LOC (under
  §15 800-LOC cap). The 582-line `chat_completions` handler body
  (`POST /v1/chat/completions`) extracted to a new sibling module
  `inference-router/src/routes/chat_completions.rs` (604 LOC),
  registered as a private `mod` under `routes/mod.rs`. Retained
  in `inference.rs`: route-builders (`inference_routes`,
  `foundry_agent_routes`, `foundry_standalone_routes`) plus the
  smaller handlers (`completions`, `responses`, `embeddings`,
  `images_generations`, `images_generations_v1`, `list_models`,
  `list_deployments`, `foundry_proxy`).

#### Tests

- `cargo test --package azureclaw-inference-router --lib`: **608
  passed; 0 failed**. `cargo build`, `cargo clippy --all-targets
  -- -D warnings`, `cargo fmt --all -- --check` all clean. No
  behavioral change — handler body moved verbatim, visibility
  raised from `async fn` to `pub(super) async fn`.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-inference-router-routes.md`
  (sign-offs: Core ✅, Security ✅).

### S15.b `phase2-hotspot-mesh-cli` — mesh CLI hotspot decomposition

#### Refactored

- `cli/src/commands/mesh.ts` 1583 → 667 LOC (under §15 800-LOC
  cap). Decomposed along natural seams into a fresh
  `cli/src/commands/mesh/` directory:
  - `mesh/identity.ts` (137 LOC) — `MeshIdentity` interface, AES-256-GCM
    at-rest key encryption, Ed25519 keypair + AMID derivation,
    base58 encode, identity file load/save (0o700 dir / 0o600 file).
  - `mesh/oauth.ts` (94 LOC) — OAuth callback HTTP server bound to
    `127.0.0.1`, `OAuthResult` shape, `escapeHtml` (CWE-79) and
    `sanitizeForLog` (CWE-117) helpers.
  - `mesh/health.ts` (127 LOC) — `killProcessesOnPorts`,
    `killStaleListeners`, `findDuplicateListeners`,
    `checkRegistryHealth`, `checkRelayHealth`.
  - `mesh/auth.ts` (221 LOC) — `mesh auth` subcommand body
    (`attachAuthSubcommand`).
  - `mesh/promote.ts` (409 LOC) — `mesh promote` subcommand body
    (`attachPromoteSubcommand`).
- Public re-export surface preserved; `mesh.test.ts` (28 tests)
  continues to pass without modification.

#### Tests

- All 454 existing CLI tests still pass; 2 skipped. `tsc --noEmit`,
  `npm run lint`, `npm run build` all clean. No behavioral change —
  every helper and subcommand action body moved verbatim.

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-mesh-cli.md`
  (sign-offs: Core ✅, Security ✅).

### S15.a `phase2-hotspot-handoff-cli` — handoff CLI hotspot decomposition

#### Refactored

- `cli/src/commands/handoff.ts` 1119 → 798 LOC (under §15 800-LOC
  cap). Closure-captured helper bundle (router exec, AKS
  port-forward, admin-token resolution, Docker wake, CRD-read,
  credential rehydrate) and the `--status` / `--abort` branches
  extracted to `cli/src/commands/handoff/helpers.ts`. No
  behavioral change; closure captures simply migrate from
  action-scope to factory-scope.

#### Tests

- All 454 existing CLI tests still pass; 2 skipped. Build + lint
  clean. New module has no public test surface (its functions
  wrap shell side effects exercised by the existing handoff
  integration story).

#### Audit

- `docs/security-audits/2026-04-29-phase2-hotspot-handoff-cli.md`.

### S7.E.2 `phase2-reconcile-duration-histograms` — reconcile latency + outcome counter

#### Added

- `azureclaw_controller_reconcile_duration_seconds{crd_kind, outcome}`
  Histogram and `azureclaw_controller_reconcile_total{crd_kind,
  outcome}` IntCounterVec on the controller's `:9091/metrics`
  surface. Closes the second half of S7.E (operator-craftsmanship
  observability per `docs/implementation-plan.md` §9 P0).
- `controller/src/metrics.rs::observe_reconcile(crd_kind, fut)` —
  thin generic wrapper threaded through every `Controller::run(...)`
  call site to record duration + outcome on completion. Generic
  over the reconciler's `Result<T, E>` so each crate keeps its own
  `ReconcileError` type unchanged.

#### Wired

- All 8 reconcilers — `reconciler/mod.rs` (ClawSandbox),
  `a2a_agent_reconciler.rs`, `claw_eval_reconciler.rs`,
  `claw_memory_reconciler.rs`, `inference_policy_reconciler.rs`,
  `mcp_server_reconciler.rs`, `pairing_reconciler.rs` (ClawPairing),
  `tool_policy_reconciler.rs`. Reconcile-fn bodies are untouched.

#### Tests

- 3 new unit tests in `metrics.rs`: success-outcome wiring,
  error-outcome wiring + non-pollution of the success row,
  Prometheus text-format render. Controller suite 349 → 352.

#### Audit

- `docs/security-audits/2026-04-29-phase2-reconcile-duration-histograms.md`.

### S17.A `phase2-sca-permanent-rows` — npm audit as permanent CI gate

#### Added

- `.github/workflows/ci.yml` `cli-build` and `mesh-plugin-build`
  jobs now append `npm audit --audit-level=high` after the test
  step. JavaScript-side SCA now matches the Rust side's existing
  `cargo audit --deny warnings` posture.

#### Tests

- Pre-commit verification: both `cd cli && npm audit
  --audit-level=high` and `cd mesh-plugin && npm audit
  --audit-level=high` reported `found 0 vulnerabilities` against
  the current `package-lock.json` files.

#### Audit

- `docs/security-audits/2026-04-29-phase2-sca-permanent-rows.md` —
  partial closure of §11.1 ("trivy + cosign-verify + SCA →
  permanent CI rows"). Cosign-verify deferred to S17.B once a
  direct dep starts publishing Sigstore signatures.

### S7.F `phase2-content-safety-floor` — Content-Safety floor admission

#### Added

- **`deploy/helm/azureclaw/templates/admission-content-safety-floor.yaml`** —
  ValidatingAdmissionPolicy + Binding rejecting `InferencePolicy.spec.contentSafety.{hate,selfHarm,sexual,violence}` values that are *more permissive* than the cluster minimum (Azure Content Safety ordering: Safe < Low < Medium < High; lower ordinal = stricter floor).
- **`admission.contentSafetyFloor`** Helm values:
  `enabled` (default `true`), `minimum` (default `"Medium"`,
  validated at chart-render time against `Safe|Low|Medium|High`).
- Per-CR opt-out via `azureclaw.azure.com/dev-only: "true"` label,
  consistent with the null-provider-block VAP convention.

#### Tests

- `helm lint` clean. `helm template` renders both the policy and
  the binding. Invalid `minimum` value fast-fails at template time
  with a clear message. No code-side test count change (controller
  349; CLI / router unchanged).

#### Audit

- `docs/security-audits/2026-04-29-phase2-content-safety-floor.md`.

### S7.E `phase2-controller-metrics` — controller workqueue metrics

#### Added

- **`controller/src/metrics.rs`** — Prometheus counter registration:
  `azureclaw_controller_reconcile_errors_total{crd_kind, error_class}`
  and `azureclaw_controller_reconcile_retries_total{crd_kind}`. Helper
  `record_reconcile_error(...)` increments both. Bounded-cardinality
  labels (no CR names / namespaces in labels).
- **`controller/src/metrics_server.rs`** — minimal axum HTTP server
  exposing `/metrics` (Prometheus text format) and `/healthz`. Bind
  address overridable via `CONTROLLER_METRICS_ADDR` (default
  `0.0.0.0:9091`); opt out with empty string or `disabled`.
- Helm `controller-deployment.yaml` declares
  `containerPort: 9091, name: metrics`.

#### Changed

- All eight `error_policy` functions call
  `metrics::record_reconcile_error(...)` so a single Prometheus
  query can answer "are any reconcilers in failure loops right
  now?" without scraping logs.
- Controller `Cargo.toml` adds `axum = "0.8"`.

#### Tests

- 4 new unit tests; controller bin 345 → 349. Clippy + fmt + helm
  lint clean.

#### Audit

- `docs/security-audits/2026-04-29-phase2-controller-metrics.md`.

### S7.D `phase2-requeue-jitter` — bounded jitter on reconcile-error requeues

#### Added

- **`controller/src/backoff.rs`** — pure jitter math
  `apply_jitter_factor(base, factor, sample) -> Duration` plus
  `with_jitter(Duration)` and `requeue_secs_with_jitter(u64)` helpers
  using `rand::rng()`. Default ±20% factor matches the
  `k8s.io/apimachinery/pkg/util/wait` convention. 9 unit tests.

#### Changed

- All seven controller `error_policy` functions now route requeue
  durations through `backoff::requeue_secs_with_jitter(...)` so two
  hundred CRs that hit the same transient API error don't retry on
  the same 30-second tick. Touched: `reconciler/mod.rs`,
  `pairing_reconciler.rs`, `mcp_server_reconciler.rs`,
  `tool_policy_reconciler.rs`, `a2a_agent_reconciler.rs`,
  `inference_policy_reconciler.rs`, `claw_memory_reconciler.rs`,
  `claw_eval_reconciler.rs`. Sandbox's
  `error_requeue_duration(error)` keeps its kind-based base (30s for
  `Kube`, 300s for `SerdeJson`); jitter applies after.

#### Tests

- Controller bin tests 336 → 345 (+9). Clippy `-D warnings` clean.

#### Audit

- `docs/security-audits/2026-04-29-phase2-requeue-jitter.md` — single
  sub-slice in S7 craftsmanship train.

### S7.C `phase2-leader-election` — controller-wide leader election

#### Added

- **`controller/src/leader_election.rs`** — Kubernetes Lease
  (`coordination.k8s.io/v1`) gate so exactly one of the controller
  Deployment's `replicas: 2` pods reconciles at a time. Closes the
  doubled-write / doubled-event / doubled-Foundry-agent-create gap that
  the SSA `fieldManager` registry from S7.A left open. New module
  exposes the pure decision function `evaluate_lease(spec, identity,
  now) -> {Acquire, Renew, Yield(holder)}` plus the async I/O loop
  `acquire_and_hold(client, cfg, ready_tx)` that creates / patches the
  Lease and signals readiness on first acquisition. On renew failure
  the function returns an error so `main.rs` exits — standard
  fail-stop pattern; the pod restarts and a healthy replica re-elects.
- **Default-on, opt-out via `LEADER_ELECTION_ENABLED=false`** so dev /
  kind clusters running a single replica can skip the lease overhead.
  RBAC already in place from Phase 1 (mesh-peer's existing lease) so
  no manifest changes are required.

#### Changed

- `controller/src/main.rs` blocks reconciler spawn on a
  `oneshot::channel` until the lease is acquired; if the leader task
  exits before signalling readiness its `ready_tx` drops and the await
  observes `RecvError`, propagating the underlying error. The leader
  handle is added to the final `tokio::select!` so leadership loss
  terminates the process.

#### Tests

- 7 new unit tests on `evaluate_lease` covering all branches: missing
  spec, we-hold (fresh & expired), other-holder (fresh & expired),
  missing `renewTime`, defensive empty-holder.
- Controller bin tests: 329 → 336 (+7).

#### Audit

- `docs/security-audits/2026-04-29-phase2-leader-election.md`.

### S7.B `phase2-conditions-ssa-leader-b` — Conditions matrix `Progressing` emission

#### Added

- **`Progressing` Condition** now emitted on every `ClawSandbox` status
  patch path: `build_running_status_patch` (`Progressing=False / Reconciled`),
  `build_degraded_status_patch` (`Progressing=False / <degraded-reason>`),
  and `build_runtime_unsupported_status_patch`
  (`Progressing=False / AdapterMissing`). The overlay path already
  emitted the full matrix in S8; this brings the other three paths to
  parity so `kubectl wait --for=condition=Progressing=False` resolves
  consistently across success / overlay / degraded / adapter-missing.
- **Upgrade-time back-fill regression test**
  `running_status_matches_returns_false_when_progressing_missing`: a
  pre-S7.B status carrying only `[Ready=True, RuntimeReady=True]` is
  now treated as stale by the idempotency guard so the new
  `Progressing` field is written on the first reconcile after
  controller upgrade, instead of being short-circuited as a no-op.

#### Changed

- `running_status_matches` and `runtime_unsupported_status_matches`
  extended to verify `Progressing=False` is present with the expected
  reason, mirroring the existing `Ready` / `RuntimeReady` checks.

#### Tests

- Controller bin tests: 328 → 329 (+1).

#### Audit

- `docs/security-audits/2026-04-29-phase2-conditions-progressing.md`.

### S7.A `phase2-conditions-ssa-leader` — stable SSA field managers (first sub-slice)

#### Added

- **`controller/src/field_managers.rs`** — central registry of every
  Server-Side Apply `fieldManager` the controller emits. Each per-CRD
  reconciler (MCP / ToolPolicy / A2A / InferencePolicy / ClawMemory /
  ClawEval) and each subsystem (ClawSandbox / pairing / mesh-peer /
  provider-bridge) now sources its manager string from this single
  module. Eliminates the five bare `"azureclaw-controller"` /
  `"azureclaw-mesh-peer"` literals scattered across `reconciler/mod.rs`,
  `pairing.rs`, `pairing_reconciler.rs`, `mesh_peer/offload.rs`,
  `mesh_peer/pair.rs`.
- **`ALL_FIELD_MANAGERS`** registry slice + uniqueness invariant:
  `all_field_managers_are_unique`, `field_managers_use_namespaced_format`,
  `no_bare_azureclaw_controller_string`, `legacy_provider_constants_match`
  tests (4 new — controller test count 324 → 328).
- **`providers::field_managers`** preserved as a backwards-compat
  re-export so legacy import paths keep working.

#### Changed

- `controller/src/reconciler/mod.rs` (13 SSA call sites) now uses
  `crate::field_managers::CLAWSANDBOX` (`azureclaw-controller/clawsandbox`).
- `controller/src/pairing.rs` + `pairing_reconciler.rs` (3 sites)
  use `crate::field_managers::PAIRING`.
- `controller/src/mesh_peer/offload.rs` + `pair.rs` (3 sites) use
  `crate::field_managers::MESH_PEER` (legacy string verbatim — no
  ownership migration on existing clusters).
- Per-CRD reconcilers (`mcp_server_reconciler`, `tool_policy_reconciler`,
  `a2a_agent_reconciler`, `inference_policy_reconciler`,
  `claw_memory_reconciler`, `claw_eval_reconciler`) now declare their
  `FIELD_MANAGER` const as a re-export of the central constant.

#### Out of scope (subsequent S7 sub-slices)

- S7.B Conditions matrix (Progressing step-wise emission)
- S7.C Leader election + predicated informers
- S7.D Backoff with jitter + reconcile-DAG cold-start
- S7.E Workqueue metrics + reconcile spans on `/metrics`
- S7.F VAP/MAP expansion (Content-Safety floor, posture-downgrade)

#### Audit

`docs/security-audits/2026-04-28-phase2-conditions-ssa-leader.md`

---

### S10.A5 `phase2-runtime-cli` — operator-facing CLI surface for multi-runtime hosting

#### Added

- **`cli/src/runtime.ts`** — single source of truth for runtime
  helpers, mirroring the controller's `RuntimeKind` enum and
  `is_openclaw` polarity. Exports `flagToKind` (kebab-case →
  PascalCase wire format), `assertRuntimeWired` (rejects Tier-2 +
  unwired MAF .NET at the CLI boundary), `agentContainerName`
  (OpenClaw → `openclaw`, everything else → `agent`),
  `runtimeKindFromCr` (live-CR reader with safe `OpenClaw` fallback
  for legacy/unknown values), and `buildRuntimeBlock` (emits the
  variant-correct `spec.runtime` block).
- **`azureclaw add --runtime <kind>`** — accepts `openclaw` (default),
  `openai-agents`, `microsoft-agent-framework`, `byo`. Tier-2 kinds
  rejected with discoverable error listing the wired set. BYO
  requires `--byo-image`; `--byo-contract-version` defaults to `v1`.
  MAF defaults `--maf-language python`; `dotnet` rejected client-side
  with explicit Phase 3 / upstream-blocker message.
- **`azureclaw connect <name>`** — fetches the live ClawSandbox CR
  and addresses the correct container with `kubectl exec -c` based
  on `spec.runtime.kind`. Backward-compatible: legacy CRs without
  `spec.runtime` fall back to `openclaw`.
- **`azureclaw list`** — adds a `RUNTIME` column showing each
  sandbox's `spec.runtime.kind` (defaults to `OpenClaw` for legacy
  CRs).
- **`cli/src/runtime.test.ts`** — 19 vitest unit tests covering
  flag mapping, wired/unwired gates, container-name polarity, CR
  reader fallbacks, and per-variant block shapes.

#### Tests

- CLI vitest: 435 → 454 passing (+19), 2 skipped (unchanged).
- `npx tsc --noEmit` clean; `npm run build` clean.
- No new lint diagnostics (26 pre-existing `plugin.ts` warnings unchanged).

#### Closes

- §14.6 column 11 (Multi-runtime hosting) **operator-accessible** —
  the value prop now reachable via `azureclaw add --runtime <kind>`
  rather than hand-edited CRs.

---

### S10.A4 `phase2-runtime-microsoft-agent-framework` — second native runtime, flips column 11 fully ✓

#### Added

- **`plan_microsoft_agent_framework` producer** in
  `controller::reconciler::runtime` — replaces the
  `AdapterMissing("MicrosoftAgentFramework")` short-circuit landed in
  S10.A2. First producer to return `Result` (not `Ok` direct):
  language gate refuses `language: dotnet` via
  `RuntimePlanError::ShapeInvalid` with an upstream-blocker citation
  (AgentMesh.Sdk .NET, Phase 3). Reconciler dispatch surfaces this as
  the existing `Degraded / SpecInvalid` Conditions path.
- **`DEFAULT_MAF_PYTHON_IMAGE` constant** + `maf_python_default_image()`
  helper reading `MAF_RUNTIME_IMAGE` env override (whitespace-as-unset,
  same convention as S10.A3).
- **`RUNTIME_MAF_LANGUAGE` controller-default env** (non-reserved
  prefix — survives the deployment builder's reserved-prefix filter).
- **`sandbox-images/maf-python/`** — Dockerfile (Python 3.12 +
  `agent-framework>=0.1,<0.2` + `azure-identity` for the eventual AAD
  shim) + `entrypoint.sh` exporting `OPENAI_BASE_URL`,
  `AZURE_OPENAI_ENDPOINT`, `AZURECLAW_PLATFORM_MCP_URL` — all pointed
  at the router sidecar. Image declares
  `LABEL org.azureclaw.runtime.contract="v1"` and
  `LABEL org.azureclaw.runtime.kind="MicrosoftAgentFramework"`.
- 9 new controller tests (315 → 324, all green): default Python
  image, explicit Python success, dotnet → ShapeInvalid (with msg
  assertions for upstream-blocker + Phase 3), entrypoint propagation,
  controller-default + user `extra_env` merge, user-extra-wins on
  conflict, env-override image (set + whitespace-as-unset), dispatcher
  arm wiring (Python success + dotnet rejection).

#### Changed

- **`plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind`**
  — drops the `MicrosoftAgentFramework` case (now wired); only 3
  Tier-2 placeholders remain (`SemanticKernel`, `LangGraph`,
  `Anthropic`).

#### §14.6 column 11 — Multi-runtime hosting → fully ✓

S10.A4 closes the column-11 bar:

- ✓ OpenAI Agents Python adapter (S10.A3)
- ✓ MAF Python adapter (this slice)
- ✓ BYO end-to-end (S10.A2.b)
- ✓ OverlayMode for sigs/agent-sandbox (S8)
- ✓ kagent migration via `azureclaw migrate from-kagent` (S9.3)

#### Deferred

- **In-pod adapter Python package** (`azureclaw-runtime-maf-python`
  PyPI) — AAD shim (`DefaultAzureCredential` → bearer-on-router),
  `AZURE_OPENAI_ENDPOINT` rewriting, AGT-init compat, MAF-specific
  MCP client glue, OTel SDK wiring. Immediate follow-up.
- **MAF .NET path** — Phase 3, blocked on AgentMesh.Sdk .NET upstream
  availability. Refused at producer time with `ShapeInvalid` rather
  than mis-imaged.
- **Class B mesh / spawn / handoff** — upstream-blocked
  (AgentMesh-Python). Foundry-shim access via S10.B platform MCP
  unaffected.

#### Audit doc

- `docs/security-audits/2026-04-28-phase2-runtime-microsoft-agent-framework.md` —
  scope, threat model, hard-rule checklist, column-11 closure proof,
  AGT upstream-dependency note, two sign-off slots.

### S10.A3 `phase2-runtime-openai-agents` — first non-OpenClaw native runtime

#### Added

- **`plan_openai_agents` producer** in
  `controller::reconciler::runtime` — replaces the `AdapterMissing`
  short-circuit landed in S10.A2 with a real
  `RuntimeDeploymentPlan` for `RuntimeKind::OpenAIAgents`. Resolves
  the adapter image via `DEFAULT_OPENAI_AGENTS_IMAGE` (default
  `azureclawacr.azurecr.io/azureclaw-runtime-openai-agents:latest`)
  with `OPENAI_AGENTS_RUNTIME_IMAGE` env override (whitespace
  treated as unset). Propagates `python_version` →
  `RUNTIME_PYTHON_VERSION` env (non-reserved prefix so it survives
  the deployment builder's reserved-prefix filter), merges user
  `extra_env` on top, passes `entrypoint` through as the container
  command, round-trips `agent_code` for the eventual
  `oci`/`git` mount path.
- **`sandbox-images/openai-agents/` scaffolding** — Dockerfile (Python
  3.12 + `openai-agents>=0.1,<0.2`) + `entrypoint.sh` exporting
  `OPENAI_BASE_URL=http://127.0.0.1:8443/openai/v1` (router sidecar
  is the only LLM endpoint allowed by NetworkPolicy + egress-guard)
  and `AZURECLAW_PLATFORM_MCP_URL=http://127.0.0.1:8443/platform/mcp`
  (S10.B platform MCP server: every runtime gets the 9 Foundry shim
  tools for free). Image declares
  `LABEL org.azureclaw.runtime.contract="v1"` so the existing BYO
  contract verifier recognises it.
- 8 new controller tests (315/315 green): default image, env-override
  image (set / unset / whitespace-as-unset), `python_version` →
  `RUNTIME_PYTHON_VERSION`, `extra_env` merge, user-extra-wins on
  conflict, `entrypoint` → command propagation, `agent_code`
  round-trip, dispatcher arm wiring.

#### Changed

- **Reconciler `is_byo` flag generalised to `is_openclaw`** (positive
  polarity). `RuntimeKind::OpenAIAgents` now flows through the same
  generic-runtime container shape as BYO: container name `agent`
  (not `openclaw`), no OpenClaw-specific env (`OPENCLAW_MODEL`,
  `OPENCLAW_GATEWAY_TOKEN`, `FOUNDRY_DEPLOYMENTS`, `FOUNDRY_AGENT_ID`,
  `FOUNDRY_AGENT_TOOLS`), no admin-token mount. The OpenClaw vs
  generic split established for BYO in S10.A2.b is the single
  branching point; adding OpenAIAgents required no parallel flag.
- **`AdapterMissing` log message updated** — track now reads
  `BYO=S10.A2.b, OpenAIAgents=S10.A3 (wired), MAF=S10.A4`.
- **`plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind`**
  — drops the `OpenAIAgents` case (now wired); four cases remain
  (`MicrosoftAgentFramework`, `SemanticKernel`, `LangGraph`,
  `Anthropic`).

#### Deferred

- **In-pod adapter Python package** (`azureclaw-runtime-openai-agents`
  PyPI) — AAD shim for Azure OpenAI, `AZURE_OPENAI_ENDPOINT`
  rewriting based on `InferencePolicy`, AGT-init compat, OTel SDK
  wiring. The Dockerfile + entrypoint scaffolding is contract-labelled
  but does not yet consume the adapter; immediate follow-up before
  the slice closes.
- **Class B mesh / spawn / handoff tools** — blocked on
  AgentMesh-Python upstream availability
  (`docs/internal/agt-upstream-asks.md` §3). S10.A3 ships
  Foundry-shim access only via S10.B; mesh tools deliberately absent
  rather than reimplemented.
- **Reference example app + e2e Kind test + negative-egress
  assertion** — fold into S10.A4 (MAF) where ≥2 native runtimes
  share the e2e harness investment.

#### Audit doc

- `docs/security-audits/2026-04-28-phase2-runtime-openai-agents.md` —
  scope, threat model, hard-rule checklist, AGT upstream dependency
  note, two sign-off slots.

### S10.B `phase2-platform-mcp-server` — runtime-agnostic Foundry-shim discovery surface

#### Added

- **New `POST /platform/mcp` endpoint** — runtime-agnostic Foundry-shim
  discovery surface, mounted unconditionally on the inference router.
  MCP 2025-03-26 Streamable HTTP envelope, same JSON-RPC pipeline as
  `/mcp`. Loopback-only by virtue of the router's `127.0.0.1:8443`
  bind; single-tenant by construction (one agent per pod); no OAuth
  layer (rationale in the audit doc §5).
- **`mcp::PlatformDispatcher`** — implementation of `ToolDispatcher`
  publishing the canonical 9-tool Foundry catalog: `foundry.web_search`,
  `foundry.code_execute`, `foundry.file_search`, `foundry.memory`,
  `foundry.image_generation`, `foundry.conversations`,
  `foundry.evaluations`, `foundry.deployments`, `foundry.agents`.
  Schemas mirror `cli/src/plugin.ts` lines 662–735 + 6104–6347 verbatim
  so OpenClaw plugin authors migrating to platform MCP keep the same
  input shapes.
- **`McpRouteState::platform()`** + **`platform_mcp_route()`** —
  associated constructor and route function alongside the existing
  `standard()` + `mcp_route()` pair. Reuses the same handler, same
  pipeline, same session-id minter; only the path and the injected
  dispatcher differ.
- **`build_platform_mcp_router()`** in `main.rs` — mounts
  `/platform/mcp` next to `build_mcp_router()` in the axum tree.

#### Changed

- `inference-router/src/mcp/mod.rs` — re-exports `PlatformDispatcher`
  and `foundry_tool_catalog`.
- `inference-router/src/routes/mod.rs` — re-exports `platform_mcp_route`
  alongside `mcp_route` and `protected_mcp_route`.

#### Status: discovery surface only

This slice ships the **catalog + dispatch seam**. Every `tools/call`
for a catalogued tool returns a structured JSON-RPC `result` with
`isError: true` and a deferred-wiring marker (`"S10.B"` + tool name).
Per-tool upstream wiring to Azure AI Foundry lands in follow-up slices
`S10.B.1..S10.B.9`. The shape mirrors S10.A2's "controller dispatch
seam without runtime wiring" — runtime adapters (S10.A3 OpenAI Agents
Python, S10.A4 Microsoft Agent Framework) can validate discovery
against this surface immediately while we sequence the per-tool work.

The current synchronous `ToolDispatcher::invoke` trait makes per-tool
async upstream calls a separate decision (async-trait conversion vs
parallel `AsyncToolDispatcher`); deferring that decision to the
follow-up slices keeps this slice strictly additive.

#### Tests added

- **`mcp::platform`**: 7 tests (catalog identity, schema validity,
  deferred-wiring shape, unknown-tool path, argument-agnosticism,
  trait-object safety, default-matches-standard).
- **`routes::mcp::tests::platform_*`**: 6 tests (state-builds,
  initialize round-trip, `tools/list` returns all 9, `tools/call`
  returns `result.isError:true` with slice id, `GET` returns 405 +
  `Allow: POST`, unknown-tool returns JSON-RPC error envelope).
- 608/608 router lib tests pass (was 595 before this slice). Clippy
  clean (`-D warnings`). `cargo fmt --check` clean.

#### Why this slice

`cli/src/plugin.ts` (7,140 LOC) is a Node.js OpenClaw plugin and
cannot serve OpenAI Agents Python (S10.A3) or Microsoft Agent
Framework (S10.A4) runtimes — those agents speak Python and load
tools through their own runtime-native mechanisms. The runtime-agnostic
way to expose the same affordances is **MCP**: every modern agent
runtime ships an MCP client out of the box. By mounting these tools
at `/platform/mcp` and pointing each adapter's MCP client at
`127.0.0.1:8443/platform/mcp`, every runtime gets the same Foundry
affordances with zero adapter code.

This is **Class A** of the OpenClaw-plugin three-class survey
(see `docs/internal/agt-upstream-asks.md` §4 and the
S10-runtime-agnostic-rule note in `plan.md` S10): pure HTTP shims
with no E2E concern, no AGT crypto, no per-runtime crypto state.
**Class B (mesh / spawn / handoff) and Class C (OpenClaw slash
commands) are explicitly out of scope** — Class B stays per-runtime
riding upstream AgentMesh SDK in each language; Class C stays
OpenClaw-only.

#### Audit doc

- `docs/security-audits/2026-04-28-phase2-platform-mcp-server.md`
  (existing-implementation survey, threat model, OAuth-rationale,
  test inventory, §0.2 hard-rule checklist).

### S10.A2.b `phase2-multi-runtime-byo` — BYO end-to-end deployment + `raw_env`

#### Added
- **`RuntimeDeploymentPlan.raw_env: Vec<serde_json::Value>`** —
  captures structural env entries (e.g. `valueFrom: secretKeyRef:`)
  from BYO `spec.runtime.byo.env`. Static `value:` entries continue
  to flow via `runtime_extra_env: BTreeMap<String,String>` (S10.A2).
- **`plan_byo()` populates `raw_env`** from any env entry the static
  flattener skipped — the existing reserved-prefix / NUL / dup name
  filter applies to `raw_env` entries' `name` field; the
  `valueFrom` payload renders verbatim.
- **`build_runtime_plan_dispatches_byo_to_producer`** unit test
  asserting `RuntimeKind::BYO` no longer returns `AdapterMissing`.

#### Changed
- **`RuntimeKind::BYO` now routes through `Ok(plan_byo(cfg))`** in
  `build_runtime_plan` (was `AdapterMissing`). BYO sandboxes get
  end-to-end Pod deployment.
- **Reconciler `mod.rs`** gained `is_byo` flag derived from
  `runtime_spec.kind`. The following env entries are skipped when
  `is_byo`: `OPENCLAW_MODEL`, `OPENCLAW_GATEWAY_TOKEN`,
  `FOUNDRY_DEPLOYMENTS`, `FOUNDRY_AGENT_ID`, `FOUNDRY_AGENT_TOOLS`.
  Critical: `OPENCLAW_GATEWAY_TOKEN` references Secret
  `gateway-token` which is OpenClaw-namespace-scoped — BYO
  referencing it would `CreateContainerConfigError`.
- **Agent container extracted** into a `let agent_container = json!`
  binding before the deployment macro. Conditional fields:
  `name: "agent"` (BYO) vs `"openclaw"` (OpenClaw); port 18789 only
  when `!is_byo`; admin-token volumeMount only when `!is_byo`;
  `command` / `args` set from `plan.command` / `plan.args` when
  `Some(...)`.
- **`raw_env` consumption block** added in the reconciler after the
  static `runtime_extra_env` block. Defensive skip on entries
  missing `name`.
- Renamed
  `plan_returns_adapter_missing_for_each_non_openclaw_kind` →
  `plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind`
  (BYO removed from cases vec; remaining unwired kinds:
  `OpenAIAgents`, `MicrosoftAgentFramework`).

#### Tests
- 307/307 controller tests pass (was 306 in S10.A2; +1 new
  dispatcher test).
- `cargo clippy --package azureclaw-controller --all-targets -- -D
  warnings` clean.
- `cargo fmt --all -- --check` clean.

#### Audit
- `docs/security-audits/2026-04-28-phase2-multi-runtime-byo.md`
  (threat model: gateway-token escape attempt, container-name
  divergence, raw_env reserved-prefix coverage, post-deploy patch
  compatibility).

#### Follow-ups not in this slice
- S10.A3 (`phase2-runtime-openai-agents`) — first runnable
  non-OpenClaw runtime; will exercise BYO-shaped deployment path
  with a real Python 3.12 image; first multi-runtime e2e Kind test.
- S10.A4 (`phase2-runtime-microsoft-agent-framework`) — flips §14.6
  column 11 fully ✓.
- S10.B (`phase2-platform-mcp-server`) — Foundry-shim platform MCP
  server in router. Should ship before S10.A3/A4 so adapters are
  trivial.

### S10.A2 `phase2-multi-runtime-dispatch` — `RuntimeDeploymentPlan` dispatch seam

#### Added
- **`controller/src/reconciler/runtime.rs`** (NEW, ~520 lines, 12 unit
  tests) — `RuntimeDeploymentPlan` (kind_str / image / command / args /
  runtime_extra_env / agent_code / byo_contract_version),
  `RuntimePlanError::{AdapterMissing, ShapeInvalid}`,
  `validate_runtime_shape()` defensive guard mirroring the 7 helm CEL
  rules (covers CEL-disabled apiservers per plan §S10.A1 rubber-duck #7),
  `build_runtime_plan()` dispatcher, `plan_openclaw()` producer (image
  fallback to controller default + extra_env carry-through),
  `plan_byo()` producer (image / command / args / contract version +
  flattens static `value:` env entries; structural `valueFrom` entries
  reserved for the A2.b deployment builder). `kind_str()` free helper
  is the single source of truth for the wire-format runtime kind
  string used in status patches and log fields.
- Reconciler `mod.rs` consumes the plan: `runtime::build_runtime_plan`
  replaces the inline AdapterMissing match, `plan.image` replaces the
  inline image fallback in the deployment builder, `plan.runtime_extra_env`
  replaces the direct `openclaw_config.extra_env` consumption (reserved
  prefix / NUL-byte / duplicate filtering preserved exactly).
- New `RuntimePlanError::ShapeInvalid` variant routes to a fresh
  `Degraded / SpecInvalid` status path (300 s requeue) — distinct from
  AdapterMissing's `RuntimeReady=False / AdapterMissing` so operators
  can tell "controller doesn't know this runtime yet" apart from
  "this CR is structurally malformed and CEL admission would have
  rejected it".

#### Tests
- **+12 producer/dispatcher tests** in `runtime.rs` covering wire-format
  stability, defensive shape validation (each Tier-1 + Tier-2 variant,
  both directions), OpenClaw plan production (image fallback +
  extra_env carry-through), AdapterMissing for all 6 non-OpenClaw kinds
  (incl. BYO short-circuit until A2.b), shape rejection before
  dispatch, BYO producer round-trip, BYO valueFrom env handling.
- 306 / 306 controller tests green (was 293 after S10.A1 + 1 new ignored
  cleanup; net +12 from this slice).

#### Behaviour preservation
- A2 is a **structural seam only**. For OpenClaw the runtime path is
  byte-for-byte equivalent to A1 (same image resolution, same env
  ordering, same reserved-prefix filter). For non-OpenClaw kinds the
  AdapterMissing skip is preserved exactly (same status condition,
  same 300 s requeue). BYO end-to-end deployment (split container
  builder + registry-side `org.azureclaw.runtime.contract=v1` label
  check) is deferred to S10.A2.b.

### S10.A1 `phase2-multi-runtime-crd` — `spec.openclaw` → `spec.runtime` discriminated union

#### Added
- **`controller/src/crd.rs`** — new `RuntimeSpec` block with
  `kind: OpenClaw|OpenAIAgents|MicrosoftAgentFramework|SemanticKernel|LangGraph|Anthropic|BYO`
  discriminator and per-variant config (`openclaw`, `openaiAgents`,
  `microsoftAgentFramework`, `semanticKernel`, `langGraph`, `anthropic`,
  `byo` with required `contractVersion` + nested `agentCode` exactly-one
  `oci|git`); `ClawSandboxStatus.runtime_kind` field surfaces the
  reconciled kind for `kubectl get clawsandbox -o wide` (printer column
  `Runtime`). Tier-1 variants (`OpenClaw` / `OpenAIAgents` /
  `MicrosoftAgentFramework`) get controller adapters in S10.A3/A4;
  Tier-2 placeholders (`SemanticKernel` / `LangGraph` / `Anthropic`)
  parse with full schema (no breaking change later) but the controller
  stamps `RuntimeReady=False / AdapterMissing` until adapters land.
  11 round-trip tests assert each variant deserialises in isolation
  and rejects shape conflicts.
- **`deploy/helm/azureclaw/templates/crd.yaml`** — schema mirror with 7
  CEL `XValidation` bidirectional rules (`(self.kind=='OpenClaw') ==
  has(self.openclaw)` etc., one per variant) plus nested AgentCodeRef
  exactly-one CEL on every variant that carries agent code. Printer
  column `Runtime` added; `spec.required` now includes `runtime`.
- **`RuntimeReady` Condition + `AdapterMissing` reason** — new well-known
  vocabulary in `controller/src/status/conditions.rs`. `RuntimeReady=True/Reconciled`
  surfaces on the running Pod path; `RuntimeReady=False/AdapterMissing`
  surfaces when a CR declares a runtime kind whose adapter is not yet
  wired (S10.A3/A4 territory).
- **`build_runtime_unsupported_status_patch` + `runtime_unsupported_status_matches`
  + `stamp_runtime_unsupported`** — new status-helper trio in
  `controller/src/status/mod.rs`. The reconciler refuses to create a
  Deployment when `spec.runtime.kind ∈ {OpenAIAgents, MicrosoftAgentFramework, BYO}`
  in this build (no fall-through to `ctx.sandbox_image` per plan §S10.A1
  rubber-duck #2), stamps `Degraded` + `Ready=False` + `RuntimeReady=False`
  all with `Reason=AdapterMissing`, and requeues every 5 min. 5 new
  unit tests cover stamp/idempotency/timestamp-preservation.

#### Changed (BREAKING — no installed base; in-place v1alpha1 edit, no v1alpha2 cut)
- **`spec.openclaw` → `spec.runtime.openclaw`** across every CRD-emitting
  and CRD-reading site:
  - Controller: `mesh_peer/offload.rs` (cloud offload spawn path,
    including `OFFLOAD_*` extraEnv injection now writes
    `spec.runtime.openclaw.extraEnv`); `reconciler/mod.rs` (reads
    `spec.runtime.openclaw`).
  - CLI emitters: `cli/src/commands/{add.ts,up.ts}`,
    `cli/src/commands/convert.ts` (bidirectional, with hard-fail on
    `runtime.kind != "OpenClaw"` for the `→ upstream Sandbox` direction —
    no upstream shape exists for non-OpenClaw runtimes),
    `cli/src/migrate/from_kagent.ts`, `cli/src/commands/migrate.ts`
    `--image` help text.
  - CLI readers: `cli/src/commands/handoff.ts` (model inheritance).
  - Examples (6 yamls): `examples/{basic,confidential,telegram}-agent/clawsandbox.yaml`
    and `examples/demo-clawshield/{fabrikam-legal,contoso-bank,northwind-trade}-agent.yaml`.
  - Compat fixtures (2 yamls): `tests/compat/fixtures/null-provider-{prod-denied,devonly-ok}.yaml`.
- **`build_running_status_patch` / `running_status_matches` /
  `build_overlay_status_patch` / `overlay_status_matches`** signatures
  now take `runtime_kind: &str` as the trailing argument. The patches
  emit `status.runtimeKind` and append a `RuntimeReady` Condition
  (`True/Reconciled` for running, `False/OverlayMode` for overlay).
  Stamping `runtimeKind` and the new Condition *inside* the existing
  patch (rather than via a separate `patch_status` call) avoids a
  merge-patch array overwrite that would erase the new Condition — see
  plan §S10.A1 rubber-duck #1.

#### Deferred to S10.A2
- `RuntimeDeploymentPlan` per-variant dispatch struct in
  `controller/src/reconciler/runtime.rs` (single seam consuming one plan
  per kind so we don't grow N parallel small helpers).
- Per-variant image / entrypoint / env / `agentCode` mount resolution.
- BYO contract verifier (`org.azureclaw.runtime.contract=v1` label
  check; `RuntimeReady` Condition reflects compliance).
- `validate_runtime_shape` defensive guard mirroring the CEL rules in
  case of CRD downgrade.

### S1 `phase2/mcp-reconciler` — full McpServer reconciler + JWKS pattern

#### Added
- **`controller/src/mcp_server_reconciler.rs`** — full reconciler for the
  `McpServer` CRD (Phase 1 shipped schema-only). Generates an Ed25519 signing
  keypair (raw 32-byte form, mirroring `mesh_peer/mod.rs::MeshIdentity::generate`
  to avoid pulling the `pkcs8` feature on `ed25519-dalek`), persists it in a
  Secret of type `azureclaw.azure.com/mcp-signing-key` with a `kid` annotation,
  fetches the issuer's JWKS via OpenID Discovery (`/.well-known/openid-configuration`
  → `jwks_uri`, https-only, 10s timeout), and caches the JWKS as a ConfigMap
  named `mcp-{name}-jwks`. Pluggable `JwksFetcher` trait keeps tests
  network-free.
- **Finalizer `azureclaw.azure.com/mcpserver-cleanup`** — cascades Secret +
  ConfigMap deletion before the CR is removed.
- **Status surface extended** — new `signing_key_ref` and `jwks_config_map_ref`
  `LocalObjectRef` fields on `McpServerStatus`; reuses the
  `status/conditions.rs` vocabulary (`Ready` / `Progressing` / `Degraded`)
  shipped in Phase 1.
- **Server-Side Apply throughout** — reconciler uses field manager
  `azureclaw-controller/mcp` per §10.4 #1; lays the SSA pattern S2 (ToolPolicy)
  and S3 (A2AAgent) reuse.
- **Helm CRD mirror** — `deploy/helm/azureclaw/templates/crd-mcpserver.yaml`.
  `controller/src/helm_drift.rs` enforces no drift between the Rust
  `mcp_server_crd()` definition and the helm template via a unit test that
  fails the build on divergence; a one-shot `DUMP_MCP_CRD_YAML=1` test is
  used to regenerate the helm template on intentional schema changes.
- **Inference-router `/mcp` mount** — `inference-router/src/main.rs` now
  selects between the dev `routes::mcp_route()` and OAuth-2.1-gated
  `routes::protected_mcp_route()` based on `MCP_PRODUCTION_MODE` +
  `MCP_JWKS_PATH` + `MCP_OAUTH_AUDIENCE` env vars (set by the controller when
  it mounts the JWKS ConfigMap into the router pod). On a malformed
  production-mode configuration the router refuses to mount `/mcp` rather
  than silently falling back to the unauthenticated dev route — operators
  see a startup-time error instead of a quietly unauthenticated MCP route.
- **`OAuthVerifierConfig::from_jwks_file`** — new constructor on the Phase 1
  OAuth 2.1 verifier so the router can load a JWKS from a controller-mounted
  file instead of a remote URL.
- **Audit doc** — `docs/security-audits/2026-04-27-phase2-mcp-reconciler.md`,
  covering threat-model delta, OWASP MCP Top 10 mapping (MCP-01/04/08),
  auth/authz path, key custody, egress surface, audit events, failure modes,
  negative-test coverage, out-of-scope items, and a §10 verification table.
  Mandatory "§0 Existing implementation surveyed" section enumerates the 17
  Phase 0/1 seams reused — the no-duplication rule added to the Phase 2 plan.

#### Tests
- **+9 unit tests** in `mcp_server_reconciler::tests` covering keypair
  generation, kid derivation, Secret/ConfigMap shape, JWKS fetch happy path,
  DNS-failure fault injection, finalizer add/remove, and condition matrix
  emission.
- **+2 helm-drift tests** in `helm_drift::tests`.
- Controller bins suite: **74 → 162 tests** (rest are previously dormant
  Phase 1 tests now compiled).

#### §14.6 impact
- **Closes column 3** (MCP 2026 server CRD) — schema → full reconciler +
  route mount.

### S2 `phase2/toolpolicy-reconciler` — full ToolPolicy reconciler + AGT profile compile

#### Added
- **`controller/src/tool_policy_compile.rs`** — pure-function compiler from
  the Phase-1-complete `ToolPolicySpec` schema (commerce + rateLimit +
  approval + appliesTo) to the JSON shape consumed by `policy_envelope.rs`.
  Uses `serde_json::Value::Object` (`BTreeMap`-backed → deterministic key
  order on serialise) for canonicalisation; `version_hash(profile)` is a
  16-byte (32 hex char) sha256 prefix used as the ConfigMap annotation key
  for change detection without a full diff.
- **`controller/src/tool_policy_reconciler.rs`** — full reconciler modelled
  on `mcp_server_reconciler.rs` (S1). Watches `ToolPolicy` CRs, compiles
  spec → AGT profile JSON, persists it as a ConfigMap
  `toolpolicy-{name}-profile` with key `profile.json`, annotates with
  `azureclaw.azure.com/toolpolicy-version-hash`, and labels with
  `azureclaw.azure.com/artifact=compiled-profile` so the S7 router-side
  informer can select on them. Adds finalizer
  `azureclaw.azure.com/toolpolicy-cleanup` and deletes the ConfigMap before
  releasing the CR.
- **Status surface uses the Phase 1 `status/conditions.rs` vocabulary** —
  emits `Progressing` / `Ready` / `Degraded` with the same code constants
  as S1; preserves `last_transition_time` when condition status is
  unchanged (verified by unit test `conditions_preserve_last_transition_time`).
- **Server-Side Apply with field manager `azureclaw-controller/toolpolicy`** —
  distinct from the S1 `…/mcp` and the legacy `…/reconciler` (ClawSandbox)
  managers per §10.4 #1; the unit test `field_manager_is_per_reconciler`
  is the tripwire.
- **Helm CRD mirror** — `deploy/helm/azureclaw/templates/crd-toolpolicy.yaml`,
  generated from `tool_policy_crd()` via the same dumper-test +
  `helm_drift.rs` drift-detector pattern S1 introduced. `helm_drift.rs`
  generalised in this slice to handle multiple CRDs (per-CRD constants
  `MCP_HELM_CRD_PATH` + `TOOLPOLICY_HELM_CRD_PATH`, shared
  `assert_helm_matches_rust()` helper).
- **`main.rs` wire-up** — spawns `tool_policy_reconciler::run` in the
  controller `select!` alongside the existing reconcilers; fatal exit on
  any reconciler termination preserved.
- **Audit doc** — `docs/security-audits/2026-04-27-phase2-toolpolicy-reconciler.md`,
  with §0 enumerating **13 reused Phase 0/1/S1 seams** (schema, CEL
  validations, conditions vocabulary, finalizer pattern, SSA-manager
  naming convention, `LocalObjectRef`, helm-drift harness, RFC-3339
  formatter, error-class taxonomy, currency-string parser, CRD admission
  CEL, `PolicyDecisionProvider` consumer contract, `PolicyEnvelope`
  payload shape) and the no-duplication rationale for the one new
  module created (`tool_policy_compile.rs`).

#### Tests
- **+12 unit tests** — 6 in `tool_policy_compile::tests` (compile shape,
  determinism, version-hash stability under field re-order, currency
  parsing, rateLimit + approval emission, empty-spec edge case) and 6 in
  `tool_policy_reconciler::tests` (rfc3339 shape, error class closed-set,
  conditions success path, conditions failure path, conditions preserve
  `last_transition_time`, field-manager-per-reconciler tripwire).
- **+2 helm-drift tests** for the new ToolPolicy CRD (dumper +
  drift-detector).
- Controller bins suite: **165 → 177 tests**, 0 failures.

#### §14.6 impact
- **Strengthens column 4** (A2A 1.2 + AP2) — AP2 commerce caps + approval
  + rateLimit now compile end-to-end into a hot-reloadable artifact
  consumed by the Phase 1 router substrate.
- **Strengthens column 12** (Governance as K8s primitives) — second of
  the five differentiator CRDs goes from schema-only to fully reconciled.

### S3 `phase2/a2aagent-reconciler` — full A2AAgent reconciler + AgentCard compile + helm CRD

#### Added
- **`controller/src/a2a_agent.rs`** — full `A2AAgent` CRD struct (group
  `azureclaw.azure.com`, version `v1alpha1`, kind `A2AAgent`, shortname `a2a`).
  Spec sub-types: `A2aSigningKey` (kid/alg/publicKeyB64u/notAfter — shape
  identical to `inference-router::a2a::agent_projection::A2aAgentSigningKeySpec`
  so the published AgentCard JSON traverses controller → ConfigMap → router
  with no transformation), `TrustThresholds`, `FederationPeer`, `PolicyRefs`.
  Status fields: `phase`, `observed_generation`, `conditions`,
  `agent_card_config_map_ref` (reuses `crate::mcp_server::LocalObjectRef` —
  third semantic client of the same struct after McpServer signing/jwks
  refs and ToolPolicy profile ref).
- **`controller/src/a2a_agent_compile.rs`** — pure-function
  `compile_agent_card(spec, namespace, name) → serde_json::Value` produces
  the wire-format A2A 1.2 AgentCard JSON the router will serve verbatim once
  S7 mounts `/.well-known/agent.json`. `version_hash(card) → 32-char hex`
  (sha256 prefix). 6 unit tests covering minimal-spec, full-spec round-trip,
  determinism, namespace/name in hash, hex shape, serde-round-trip stability.
- **`controller/src/a2a_agent_reconciler.rs`** — full reconciler modelled
  on `tool_policy_reconciler.rs` (S2). Field manager
  `azureclaw-controller/a2aagent` (distinct from `/mcp`, `/toolpolicy`,
  `/reconciler`, `/mesh`, `/pairing`). Finalizer
  `azureclaw.azure.com/a2aagent-cleanup`. Compiles spec → publishes
  ConfigMap `a2aagent-{name}-card` with key `agent.json`, label
  `azureclaw.azure.com/artifact=agent-card`. Status writes
  `agentCardConfigMapRef`, `versionHash`, `lastCompiledAt`. 7 unit tests
  including closed-set `error_class` (log-injection prevention),
  three-condition matrix, finalizer cleanup, field-manager-per-reconciler.
- **CEL admission validation in `crd_validations.rs::a2a_agent_validations()`**
  — 4 rules: `signingKeys` non-empty, all `alg == 'EdDSA'`, productionMode
  ⇒ `endpointUrl` https, federation peer `kind: in-cluster` requires
  `agentRef` only / `kind: external` requires `endpointUrl + pinnedKid`
  (mutually exclusive). Plus 5 admission tests
  (`a2a_agent_validations_are_non_empty`,
  `every_a2a_agent_rule_has_message_and_rule`,
  `a2a_agent_crd_has_spec_validations_after_injection`,
  `a2a_agent_rules_mention_signing_keys_and_eddsa_invariants`,
  `a2a_agent_crd_is_serde_round_trippable`).
- **`deploy/helm/azureclaw/templates/crd-a2aagent.yaml`** — helm-side CRD
  generated by the dumper. Drift-tested at `cargo test` time by
  `helm_drift::tests::helm_a2aagent_crd_matches_rust_schema` (sixth helm
  drift test alongside mcpserver + toolpolicy).

#### Tests
- 16 new controller unit tests (6 compile + 7 reconciler + 5 admission/drift).
- Full controller suite: **193 passing** (was 177 after S2). Workspace
  total unchanged otherwise (router 595, integration 26, all green).

#### §14.6 impact
- **Closes column 4** (A2A 1.2 + AP2) — schema → AgentCard publication path
  end-to-end. Router-side `/.well-known/agent.json` mount + JWS signing +
  trust-store informer wiring + JSON-RPC `message/send`/`tasks/get`/
  `tasks/cancel` route mounts deferred to **S7** (`phase2-conditions-ssa-leader`).
- **Strengthens column 12** (Governance as K8s primitives) — third of
  the five differentiator CRDs goes from schema-only to fully reconciled.

#### Notes
- **No fork of AGT.** Upstream Microsoft `agentmesh` v3.1.0 from crates.io
  (unmodified) remains the policy authority; this slice ships K8s
  ergonomics (CRD + reconciler + ConfigMap + helm) for AgentCard
  publication, not a parallel decision engine.
- Public-edge gateway (`azureclaw-a2a-gateway` binary, ADR-0001
  implementation step #4) remains pending — added as new slice
  **S3.5 `phase2-a2a-gateway-component`** (`docs/implementation-plan.md`
  §8 scope item 2a). Without it, only outbound A2A is usable end-to-end;
  inbound A2A 1.2 federation lands in S3.5.
- Auto-generation of the per-agent signing-key Secret is operator-supplied
  in S3 (CRD `spec.signingKeys[]` REQUIRED, CEL-gated non-empty);
  controller-side auto-mint deferred to S7.

### S4 `phase2/inferencepolicy-reconciler` — full InferencePolicy reconciler + compile + helm CRD

This slice ships the controller side of the K8s primitive only. Per §3
non-compete, `InferencePolicy` is **not** a model-router — model
selection sits in Foundry; this is a sandbox-side budget / guardrail /
safety policy CR. Per user direction 2026-04-27, the runtime enforcement
substrate stays on Phase 1 (`inference-router::budget` env-fed token
tracker, Foundry-side Content Safety with flags reported to AGT
`BehaviorMonitor` via `safety::report_content_flags_to_agt`); the
informer that loads the compiled profile into `PolicyEnvelope` and the
optional upstream `BudgetTracker` port to AGT-Rust are both deferred to
S7. The compiled JSON ConfigMap is the hand-off contract.

#### Added

- **`InferencePolicy` CRD** — `controller/src/inference_policy.rs`,
  group `azureclaw.azure.com`, version `v1alpha1`, namespaced,
  shortname `ip`. Sub-types: `InferenceAppliesTo` (sandboxName,
  sandboxMatchLabels, action), `TokenBudget` (perRequestTokens,
  dailyTokens, monthlyTokens), `ContentSafetyFloor` (hate, selfHarm,
  sexual, violence, requirePromptShields), `ModelPreference` (primary +
  ordered fallback `Vec<ModelRef>`), `ModelRef` (provider, deployment).
  Reuses `mcp_server::LocalObjectRef` for status pointer
  (4th semantic client of that struct).
- **Pure compile module** — `controller/src/inference_policy_compile.rs`,
  `compile_to_profile(&InferencePolicySpec) -> serde_json::Value` +
  `version_hash(&Value) -> String` (sha256, first 16 bytes hex).
  Deterministic; key-canonical via `serde_json::Value::Object`. Output
  shape slots into `inference-router::policy_envelope::PolicyEntry::payload` —
  no parallel hot-reload core.
- **Reconciler** — `controller/src/inference_policy_reconciler.rs`,
  modeled directly on `a2a_agent_reconciler.rs` (S3). Field manager
  `azureclaw-controller/inferencepolicy` (distinct per §10.4 #1);
  finalizer `azureclaw.azure.com/inferencepolicy-cleanup`. Emits
  `Ready`/`Progressing`/`Degraded` Conditions reusing
  `status::conditions` helpers; preserves `lastTransitionTime` when
  status doesn't flip. Closed-set `error_class`
  (`kube_api`/`serde`) per §15.3.
- **Profile ConfigMap** — name `inferencepolicy-{name}-profile`, key
  `profile.json`, annotated with version hash, labelled
  `azureclaw.azure.com/artifact=inference-policy-profile` for the S7
  router-side informer label selector.
- **CEL admission rules** (6) — `inference_policy_validations`:
  `monthlyTokens >= dailyTokens`,
  `monthlyTokens >= perRequestTokens`,
  `contentSafety.{hate,selfHarm,sexual,violence}` ∈
  `{Safe,Low,Medium,High}`,
  `modelPreference.primary` non-empty provider+deployment,
  `modelPreference.fallback[*]` non-empty provider+deployment,
  `appliesTo.action` ∈ `{chat,responses,image,embeddings,*}`.
- **Helm CRD** — `deploy/helm/azureclaw/templates/crd-inferencepolicy.yaml`,
  emitted by `helm_drift::tests::dump_inferencepolicy_crd_yaml` (env-gated)
  and drift-checked by `helm_inferencepolicy_crd_matches_rust_schema`.
- **Audit doc** — `docs/security-audits/2026-04-27-phase2-inferencepolicy-reconciler.md`,
  documenting the AGT boundary verification against
  `agent-governance-toolkit` 3.3.0 on disk: AGT-Python has
  `BudgetTracker`, AGT-Rust does not (yet); `cedar-policy` + `regorus`
  available in AGT-Rust for future Content Safety floor encoding.
  STRIDE coverage, OWASP A2A coverage, explicit out-of-scope list,
  two sign-offs.

#### Tests

- `inference_policy_compile::tests` — 6 unit tests (empty/full
  round-trip, determinism, version-hash change/stability, hex shape).
- `inference_policy_reconciler::tests` — 7 unit tests (rfc3339 shape,
  error-class closed set, conditions on success/failure,
  transition-time preservation, finalizer dns-subdomain, field-manager
  distinctness from S1/S2/S3).
- `crd_validations::tests` — 5 new `inference_policy_*` tests
  (non-empty rules, every-rule-has-message, after-injection count,
  rule-mention invariants, serde round-trip).
- `helm_drift::tests` — 2 new (`dump_inferencepolicy_crd_yaml` env-gated
  + `helm_inferencepolicy_crd_matches_rust_schema`).
- Full controller suite: **218 passing** (was 193 after S3). Workspace
  `cargo test`, `cargo fmt --all`, `cargo clippy --all-targets -D
  warnings` — all green.

#### §14.6 impact

Strengthens column 7 (Foundry / M365 integration) of the competitive
matrix — the *primitive* lands in this slice; column-7 credibility
moves further when S7 wires the runtime consumers (token-budget swap,
floor compare-and-block, model-preference selection).

#### Notes

- AGT crate pin remains `agentmesh = "3.3.0"` from crates.io,
  unmodified. `vendor/` directory untouched.
- Single new struct: none. `LocalObjectRef` semantically extended (4
  clients now: signing/jwks, profile, agent-card, guardrail-profile).
- The runtime gate `inference-router::routes::inference_policy::check`
  (Phase 1) is **not modified in this slice**.

### S9.3 `phase2-migrate-from-kagent` — `azureclaw migrate from-kagent` translator

Operator-facing one-shot translator from a kagent.dev/v1alpha2 `Agent` CR
into an AzureClaw resource bundle:

- **ClawSandbox** (always) — name + namespace + labels + provenance
  annotations preserved; `spec.openclaw.image` from `spec.byo.deployment.image`
  for BYO agents (`--image` override required for Declarative agents that
  use the kagent ADK runtime); `spec.sandbox.network.allowedDomains` →
  `spec.networkPolicy.allowedEndpoints`; deployment-level `env` projected
  to `spec.openclaw.extraEnv` (last-literal-wins, `valueFrom` dropped + warned).
- **InferencePolicy** — emitted only when `spec.declarative.modelConfig`
  is set; carries the kagent ModelConfig name as a provenance annotation
  (`azureclaw.azure.com/kagent-model-config`). Inference *enforcement* is
  not migrated — that needs an equivalent AzureClaw `InferencePolicy`
  hand-authored separately.
- **ToolPolicy** — one per `(McpServer, toolName)` pair. `requireApproval`
  list maps to `spec.approval.mode = "always"`. Empty `toolNames` emits
  one wildcard ToolPolicy with a warning. `type: Agent` tools are dropped
  with warning (no agent-as-tool support). `headersFrom` and
  `allowedHeaders` are dropped with warning.

**Hard-fails on lossy translation by default**; `--allow-lossy` waives.
Same exit-code grammar as S9.2 (`convert`): 0 ok, 2 invalid input, 4 lossy
refused. `--dry-run` still applies the lossy gate (silently dropping a
network allowlist or `requireApproval` list is a governance regression
even for a "preview").

Lossy fields explicitly catalogued and warned:

- `spec.skills.{refs,gitRefs,initContainer}` — needs S12 `policy-learn-oci`.
- `spec.declarative.{systemMessage,systemMessageFrom,promptTemplate,runtime,
  stream,executeCodeBlocks,memory,context,a2aConfig}` — kagent-ADK-specific.
- `spec.{byo,declarative}.deployment.{tolerations,affinity,nodeSelector,
  volumes,volumeMounts,imagePullSecrets,imagePullPolicy,securityContext,
  podSecurityContext,serviceAccountName,serviceAccountConfig,replicas}` —
  controller-managed in AzureClaw.
- `spec.allowedNamespaces` — Gateway-API cross-namespace pattern not modeled.
- `spec.sandbox.network.allowedDomains` wildcards — passed through
  verbatim with a warning (ClawSandbox `EndpointConfig.host` wildcard
  semantics are not documented; operators must verify).
- `spec.byo.deployment.{cmd,args}` — not exposed by ClawSandbox.
- env entries with `valueFrom`.

Aspirational mappings explicitly **rejected** during pre-implementation
critique:

- `ClawAgentIdentity` — does not exist as a CRD (Phase 4 per
  `docs/internal/internal-boundaries.md:28`); the implementation plan
  line 210 mentioning it is overridden by repo reality per slice rule
  §0.2#7 (no aspirational emit).
- `McpServer` auto-emission — we cannot reconstruct MCP server endpoints
  from a kagent `TypedReference`; ToolPolicies carry the original
  reference as a provenance annotation
  (`azureclaw.azure.com/kagent-tool-ref`) and operators are warned that
  an equivalent AzureClaw `McpServer` must already exist.
- `InferencePolicy` enforcement from `modelConfig` — kagent ModelConfig
  is a separate CRD; we preserve only provenance, not behaviour.

**Subcommand:** `azureclaw migrate from-kagent <input-yaml-or-stdin>`
with `--allow-lossy`, `--namespace`, `--isolation`, `--image`,
`--out-dir`, `--force`, `--format yaml|json`, `--dry-run`.

**Output:**

- `--format yaml` (default) — multi-doc YAML stream on stdout, deterministic
  ordering: ClawSandbox, InferencePolicy, ToolPolicies (sorted by name).
- `--format json` — single Kubernetes `v1.List` object on stdout (pipes
  cleanly to `kubectl apply -f -`).
- `--out-dir <dir>` — splits the bundle into `<kind>-<name>.yaml` files;
  refuses to overwrite existing files unless `--force`.

**Implementation:**

- `cli/src/migrate/from_kagent.ts` — pure translator, ~720 LOC. Helpers
  (`sanitizeDnsName`, `hashSuffix`, `generateToolPolicyName`,
  `cleanMetadata`, `envArrayToMap`, `projectDescription`, `translate`)
  exposed via `__test`. No I/O.
- `cli/src/commands/migrate.ts` — adds the `from-kagent` subcommand with
  argparse, stdin support, multi-doc rejection, dry-run, `--out-dir`
  with collision detection.
- 53 new vitest cases covering: DNS sanitisation edge cases, hash
  determinism + collision distinguishability, env projection (last-wins,
  `valueFrom`, prior-literal-purge), description truncation, input
  gating (apiVersion / kind / spec.type / metadata.name / BYO image),
  ClawSandbox label and annotation injection, sandbox-label conflict
  rejection, namespace override warning, declarative no-image
  non-runnability, declarative `--image` escape hatch, `InferencePolicy`
  conditional emit, ToolPolicy fan-out, approval-list mapping, wildcard
  tool emission, McpServer name validation, agent-as-tool drop, deterministic
  bundle ordering, toolName dedupe, headersFrom and allowedHeaders warnings,
  every Declarative-only and deployment-level lossy field, networking
  projection, wildcard domain warning, BYO clean happy-path, env
  projection in BYO context.

CLI test count: 382 → **435** (+53).

Upstream kagent CRD shape verified directly against
`kagent-dev/kagent @ 90212ab go/api/v1alpha2/agent_types.go` via
GitHub MCP. Target CRD shapes verified directly against
`controller/src/{crd.rs, inference_policy.rs, tool_policy.rs}`.

Closes §15.2 #8 ("kagent migration tool"). Day-1 use case: an operator
running kagent declarative agents adopts AzureClaw governance by running
`azureclaw migrate from-kagent agent.yaml --image my/runtime:v1
--allow-lossy | kubectl apply -f -`, then hand-edits the emitted
ClawSandbox to set `spec.inference.{provider,endpoint,model}` per their
ModelConfig and creates an AzureClaw `McpServer` for each kagent
McpServer reference.

### S9.2 `phase2-convert-translator` — real `azureclaw convert` translator

Phase 0 shipped `azureclaw convert` as an exit-3 skeleton to lock in the CLI
surface; this slice ships the translator. Operators can now move manifests
between AzureClaw's `ClawSandbox` and upstream
`agents.x-k8s.io/v1alpha1 Sandbox` (kubernetes-sigs/agent-sandbox) without
hand-editing YAML, and bootstrap a fresh `ClawSandbox` overlay against an
existing upstream Sandbox.

#### Added

- **`cli/src/commands/convert.ts` — pure translator** with three target modes:
  `--to clawsandbox` (upstream → ClawSandbox, lossy inverse),
  `--to upstream-sandbox` (ClawSandbox → upstream, lossy forward),
  `--to overlay --sandbox-ref=<name|ns/name>` (upstream → fresh ClawSandbox
  skeleton with `spec.upstreamCompatibility.sigsAgentSandbox=overlay` +
  `upstreamSandboxRef`). All translation logic is in pure helpers exposed via
  `__test` — no filesystem, no kubectl IO inside the translator. Phase 3
  `kubectl claw attest` and any future `verify-bundle` flow can reuse the
  same helpers unchanged.
- **Hard-fail on lossy translation by default** — the translator emits
  warnings for every dropped field that has no analog (governance,
  inference, a2a, agent, azureServices, networkPolicy, upstreamCompatibility
  on forward; shutdownTime, shutdownPolicy, volumes, volumeClaimTemplates,
  multi-container, hostNetwork/PID/IPC, nodeSelector, affinity, tolerations,
  imagePullSecrets, podTemplate.metadata.{labels,annotations}, env
  `valueFrom`, replicas≠1 on inverse). If any warning is produced and
  `--allow-lossy` is **not** set, the CLI prints all warnings to stderr and
  exits 4. This rule applies to `--dry-run` as well — a dry-run never
  reports success when the real run would refuse. Rationale: silently
  dropping a TokenBudget or a ContentSafety floor on conversion is a
  governance regression dressed as a UX win.
- **Seccomp + runtimeClass mapping mirrors the controller exactly** — verified
  against `controller/src/reconciler/mod.rs:34-78`:
  - `isolation: confidential` → `runtimeClassName: kata-vm-isolation` +
    `seccompProfile: { type: RuntimeDefault }` (Kata VM provides isolation;
    Localhost seccomp is suppressed by the controller too).
  - `isolation: enhanced` + `seccompProfile: <name>` →
    `Localhost { localhostProfile: profiles/<name>.json }`.
  - `seccompProfile: RuntimeDefault` (or empty) → `RuntimeDefault`.
  Inverse `canonicaliseSeccomp` accepts the canonical
  `profiles/<name>.json` form (no warning), tolerates `<name>.json` and
  bare `<name>` with explicit warnings, and warns when `RuntimeDefault`
  appears on a non-confidential pod (controller would have emitted
  Localhost).
- **`extraEnv` projection is order-aware** — env arrays walked in order;
  duplicate literal names warn ("last literal wins"); `valueFrom` entries
  drop any prior literal for the same name (no stale-data resurrection)
  and warn; a later literal that overrides a prior `valueFrom` produces a
  second warning. `mapToEnvArray` sorts keys alphabetically for
  deterministic output.
- **Multi-document YAML rejected** — `parseAllDocuments` filtered for
  non-null contents; >1 surviving document → exit 2. Server-managed metadata
  (`status`, `uid`, `resourceVersion`, `managedFields`, `creationTimestamp`)
  is stripped from output and surfaces a "dropped status block" warning when
  present.
- **Overlay namespace pin** — `--sandbox-ref` accepts bare `name` or
  `ns/name`. When `ns/` is supplied and disagrees with input
  `metadata.namespace`, the CLI rejects with exit 2 — the controller's
  `LocalObjectRef` is same-namespace only and silently changing namespaces
  on convert would be a footgun.
- **48 new vitest cases** in `cli/src/commands/convert.test.ts` covering
  parser, target dispatch, forward/inverse happy path, every
  AzureClaw-only and upstream-only lossy field, multi-doc rejection,
  malformed input, env collisions and `valueFrom` edge cases, all four
  seccomp canonicalisation paths, kata isolation round-trip, overlay
  namespace pin, multi-container, missing image, and a forward→inverse
  round-trip stability assertion. CLI workspace test count: 337 → 382.
- **End-to-end smoke**: `node dist/index.js convert -f sandbox.yaml --to
  clawsandbox` exits 0 on a clean confidential-mode upstream Sandbox and
  emits a valid `ClawSandbox` with `isolation: confidential`. Overlay
  emit on the same input produces a governance-skeleton ClawSandbox with
  the warning that no governance fields are bound.

#### Verified upstream

- `apiVersion: agents.x-k8s.io/v1alpha1`, `kind: Sandbox`,
  `spec.podTemplate.spec` (corev1.PodSpec), inlined `Lifecycle`
  (`shutdownTime` + `shutdownPolicy`), `replicas` 0..1 verified against
  `kubernetes-sigs/agent-sandbox @ c8c85f5`
  (`api/v1alpha1/sandbox_types.go`). No `v1alpha2` yet — `api/` directory
  contains `v1alpha1/` only.

#### Not in this slice

- **`azureclaw migrate from-kagent`** — separate slice (S9.3); kagent CRDs
  have a fundamentally different shape (Agent / ToolServer / Identity rather
  than a single Sandbox primitive) and warrant their own translator path.
- **Round-trip lossless mode** — there is no canonical lossless round-trip
  because AzureClaw and upstream are deliberately different governance
  scopes. The forward `lossy-by-default` posture is the right safety
  contract; future work can add `--strict` for CI lint use cases.
- **Live-cluster import** — `convert` reads YAML from `--file`. Pulling a
  manifest from a live cluster (`kubectl get … -o yaml | azureclaw
  convert`) works today via shell pipe + `--file=/dev/stdin`; an
  `--from-cluster ns/name` shortcut may land in a later UX-polish slice.

### S9.1 `phase2-migrate-mode-switch` — `azureclaw migrate` mode-switch CLI

Operator-facing tool to flip a ClawSandbox between the four
upstream-compatibility modes that S8 (#57) shipped on the controller
side. The CLI surface that drives day-zero adoption: take an existing
upstream `sigs.k8s.io/agent-sandbox` Sandbox and bolt AzureClaw
governance on without rewriting the YAML.

**Real workflow this unlocks:**

```bash
# operator already has an upstream Sandbox CR called 'legacy-agent';
# wraps it with AzureClaw governance:
$ azureclaw migrate to-overlay legacy --upstream-ref legacy-agent
  legacy: native → overlay (upstream sandbox 'legacy-agent')
  ✓ patched

# later: drop the upstream, return to native AzureClaw
$ azureclaw migrate from-overlay legacy
  legacy: overlay → native
  ✓ patched
```

**Subcommands shipped:**

- `azureclaw migrate to-overlay <name> --upstream-ref <upstream>` —
  flip to OverlayMode (governance overlay only; upstream owns the Pod).
- `azureclaw migrate from-overlay <name>` — revert to native AzureClaw
  (controller resumes Pod / Service / NetworkPolicy ownership).
- `azureclaw migrate to-translate <name>` — SandboxClaim translate mode.
- `azureclaw migrate to-observe <name>` — status-mirror mode.
- `azureclaw migrate to-native <name>` — alias for native (`off`).

All five accept `--namespace`, `--dry-run`, `--format human|json`.

**Reuse-first design (§0.2 #11):**

- Single thin wrapper around `kubectl patch --type=merge`. No new
  CRD field, no controller change, no admission hook. The OverlayMode
  reconciler logic landed in S8; this is the operator-facing tool that
  drives it.
- Pure helpers (`validateMode`, `buildModePatch`, `readCurrentMode`,
  `summariseTransition`, `modeDisplay`) are unit-testable without a
  cluster — fully exercised by the 22 new vitest cases.
- JSON merge patch (RFC 7396) with explicit `null` for
  `upstreamSandboxRef` removal — guards against the controller's
  `Option<LocalObjectRef>::skip_serializing_if` semantic stranding a
  stale ref. Asserted directly in tests.
- Exit codes: `0` on success / no-op, `1` on kubectl failure, `2`
  on validation failure (so a CI gate can distinguish "operator typo"
  from "infrastructure problem").

**Pre-flight + transition summary:** before applying, the orchestrator
runs `kubectl get clawsandbox <name> -o json`, reads the current mode
+ ref, and prints `current → target` (e.g. `native → overlay (upstream
sandbox 'legacy-agent')`). If the sandbox is already in the target
state, the orchestrator skips the patch entirely and reports it as a
no-op — JSON output sets `noop: true` for scripting.

**Out of scope (S9.2 — separate PR):**

- `azureclaw migrate from-kagent` — Solo.io kagent CR → ClawSandbox
  translator (heavier; needs upstream kagent shape mapping).
- Real `azureclaw convert` — YAML translator from upstream
  agent-sandbox shapes (currently a Phase 0 exit-3 skeleton).
- `azureclaw migrate verify` — validates that an OverlayMode sandbox
  is in sync with its upstream Sandbox (needs upstream CRD informer;
  Phase 3 candidate).

**Surface:** `cli/src/commands/migrate.ts` (~330 LOC), single new
file. `cli/src/cli.ts` adds one import + one `addCommand`.
22 vitest cases. CLI workspace 315 → 337 (+22). tsc + lint + vitest +
ci/no-stubs + ci/no-custom-crypto + ci/check-loc all green with
`BASE_REF=origin/dev`.

**Audit:** `docs/security-audits/2026-04-28-phase2-migrate-mode-switch.md`.

### S11.1 `phase2-attest-baseline` — drift-aware `--baseline` diff

Outcome-shaped follow-up to S11. Turns `azureclaw attest` from a
"print attestation JSON" command into a CI-gate / change-control
primitive: pass `--baseline <file>` and the command compares the live
sandbox against a previously-saved attestation, surfaces typed deltas,
and exits **2 on drift** / **3 on missing-baseline-file** so a
pipeline step can `set -e` against it.

**Real-world workflow this unlocks:**

```bash
# Day 0 — capture approved posture
$ azureclaw attest demo --format json > approved.json
$ git add approved.json && git commit -m "approved: demo posture"

# Every PR / nightly job — fail the build on drift
$ azureclaw attest demo --baseline approved.json || exit $?
✗ ToolPolicy 'tp-prod' versionHash drifted (sha256:abc1234… → sha256:def5678…)
✗ new SSA manager touched the object: 'kubectl-edit'
DRIFT: 2 delta(s) — exit code 2
```

**What deltas are surfaced (one human-meaningful change per delta):**

- `specHash` — the `ClawSandbox.spec` itself changed (the most
  important signal; all other deltas are downstream of this *or* of
  a referenced policy).
- `phase` — sandbox moved between Running / Overlay / Degraded.
- `policyVersionHash` — a referenced ToolPolicy / InferencePolicy /
  A2AAgent has a new `status.versionHash` (controller recompiled it).
- `policyAdded` / `policyRemoved` — the spec now references a
  different policy CR set.
- `fieldOwnerAdded` / `fieldOwnerRemoved` — a new (or removed) SSA
  manager touched the object since the baseline.

**Set-comparison, not count-comparison, on field owners:** SSA bumps
the per-field count on every controller reconcile (noisy), but the
*set* of managers is what a CI gate actually wants to flag — "did a
human or a tool that wasn't here before edit this object?". The diff
deliberately ignores `fieldsOwned` count fluctuation when the manager
set is unchanged. Asserted directly in tests.

**Pure-function design:** `diffAttestations(baseline, current)` is the
only new logic; it has no IO, no time, no kubectl. The CLI orchestrator
calls it after `buildReport` (which is what shells out). Means the
diff is unit-testable without a cluster, and a future Phase 3
`azureclaw verify <bundle>` companion can reuse `diffAttestations`
unchanged.

**Exit codes (CI-friendly):**

- `0` — match (no deltas, baseline matches current).
- `2` — drift (one or more deltas; reported in human + JSON output).
- `3` — baseline file missing (CLI prints to stderr + exits before
  any `kubectl get`).

**JSON output:** when `--baseline` is set, the report grows a
`baselineDiff` field with `{ baseline, current, deltas, drift }`.
The base envelope (`apiVersion`, `kind`, all S11 fields) is unchanged
so existing consumers continue to parse without modification.

**Surface:** `cli/src/commands/attest.ts` adds `diffAttestations`,
`loadBaseline`, `describeDelta`, `--baseline` flag, exit-code
handling; `cli/src/commands/attest.test.ts` adds 11 new cases (no
drift, every delta variant, set-comparison, missing/invalid baseline
file). CLI workspace 304 → 315 (+11).

**Audit:** `docs/security-audits/2026-04-28-phase2-attest-baseline.md`.

### S11 `phase2-attest-cli` — `azureclaw attest <name>` read surface

This slice ships the **read consumer** half of implementation-plan §15.2
item 11 and §14.6 column 7 (provenance / attestation). The signed audit
chain (cosign receipts, AGT AuditLogger receipt IDs, verifiable
signatures) is intentionally **deferred to Phase 3** — Phase 2 lands
the CLI command shape, the deterministic spec-hash recipe, and all
read-side scaffolding so flipping the controller to emit signatures
later does not require a CLI change.

**What `azureclaw attest <name>` prints today:**

1. **Spec hash** — SHA-256 over a canonicalised JSON of
   `ClawSandbox.spec` (recursive key-sort, no whitespace). Matches the
   `versionHash` recipe used by every Phase 2 policy CRD, so a future
   signed audit chain can compose them without re-hashing.
2. **Generation lineage** — `metadata.generation` vs
   `status.observedGeneration` + `status.phase` (Running / Overlay /
   Degraded), so operators can tell "spec applied" from "spec accepted
   but not yet reconciled".
3. **SSA field-owner map** — the unique `manager` names from
   `metadata.managedFields` plus a per-manager `fields-owned` count.
   Shows "who edited this object last" without dumping the full SSA
   tree.
4. **Referenced policy versions** — for every policy CR referenced by
   `ClawSandbox.spec` (ToolPolicy, InferencePolicy, A2AAgent, plus the
   legacy `governance.toolPolicy.ref` shape), resolves the referenced
   object in `azureclaw-<name>` and prints its `status.versionHash` +
   binding ConfigMap name (both shipped by S2/S3/S4).
5. **Reconcile trace ID** — best-effort lookup from the sandbox-
   namespace Deployment's `azureclaw.azure.com/last-trace-id`
   annotation. Phase 2 controller does not yet stamp this; the field
   prints `(Phase 3)` when absent.
6. **AGT audit-receipt id** + **signature** — `(Phase 3)` today.

**Output formats:** `--format human` (default; pretty TUI table with
colour-coded phase) and `--format json` (deterministic, machine-grep-
able; emits a versioned `apiVersion: "azureclaw.azure.com/v1alpha1-attest"`
+ `kind: "Attestation"` envelope so consumers can detect schema breakage).

**Reuse map (§0.2 #11):**

- The `versionHash` recipe (canonical-JSON → SHA-256) is exactly the
  one shipped by `controller/src/tool_policy_compile.rs`,
  `controller/src/a2a_agent_compile.rs`,
  `controller/src/inference_policy_compile.rs`, and
  `controller/src/claw_eval.rs`. CLI re-implements the recipe in TS
  (`canonicalJson` + `node:crypto` `createHash("sha256")`) — there is
  no shared TS↔Rust hashing crate available, but the recipe is
  documented + asserted-deterministic so the two sides cannot drift
  without a test breaking.
- All per-CRD `status.versionHash` and `status.bindingConfigMap`
  fields are read from existing CRD status surfaces (S2 / S3 / S4).
  No CRD change required.
- No new CRD, no new K8s object, no controller change. The command
  is **read-only** from `kubectl`'s perspective — it never patches a
  cluster resource.

**Out of scope (Phase 3):**

- Signed reconcile audit chain (cosign keyless on the controller side,
  receipt IDs, verifiable signatures).
- AGT AuditLogger receipt-ID emission + retrieval.
- `metadata.annotations.azureclaw.azure.com/last-trace-id` stamping
  by the controller (the lookup is in place; the writer is not).
- A `kubectl claw verify <attestation.json>` companion command — the
  JSON envelope is versioned now to make this trivially additive.

**Surface:**

- `cli/src/commands/attest.ts` — single 350-line file: pure helpers
  (`canonicalJson`, `specHash`, `summariseFieldOwners`,
  `extractPolicyRefs`), a `kubectl get` orchestrator (`buildReport`),
  two formatters (`formatHuman`, `formatJson`), plus the commander
  `attestCommand()` factory.
- `cli/src/commands/attest.test.ts` — 19 vitest cases covering all
  pure helpers + both formatters + their round-trips. Determinism of
  the spec-hash recipe asserted directly (re-ordered keys → same
  hash; one bit changed → different hash).
- `cli/src/cli.ts` — `attestCommand()` registered in a new
  "Attestation" section.

**Tests:** CLI workspace 285 → 304 (+19). vitest + tsc --noEmit + oxlint
all green; ci/no-stubs.sh, ci/no-custom-crypto.sh, ci/check-loc.sh
all green with `BASE_REF=origin/dev`.

**Audit:** `docs/security-audits/2026-04-27-phase2-attest-cli.md` — 2
sign-offs.

### S8 `phase2-overlaymode` — sigs/agent-sandbox OverlayMode

This slice flips `ClawSandbox.spec.upstreamCompatibility.sigsAgentSandbox`
from a Phase-1 schema-only field into a real reconciler branch, closing
implementation-plan §2.1's third sandbox mode (Native | Translate |
**Overlay**) and contributing to §14.6 column 11 (Multi-runtime hosting).

**Behaviour:** when `sigsAgentSandbox: "overlay"`, the operator already
manages an upstream `Sandbox` CR (sigs.k8s.io/agent-sandbox) in the
namespace and `ClawSandbox.spec.upstreamCompatibility.upstreamSandboxRef.name`
points at it. The controller still creates the *governance overlay*
(namespace, sandbox ServiceAccount with Workload-Identity binding,
egress + ingress NetworkPolicy, governance ConfigMap, Azure RBAC SA
annotations) but **skips** the AzureClaw Pod Deployment and the
blocklist seed-ConfigMap + 6h refresh CronJob — those would have nothing
to mount into.

**Status:** new `phase: "Overlay"` distinct from `"Running"`, with
`Ready=True / Reason=OverlayMode`, `Progressing=False / Reason=OverlayMode`,
and a new `Suspended=True / Reason=OverlayMode` condition whose message
names the upstream CR. `status.sandboxPod` is set to
`upstream/<name>` so `kubectl get clawsandbox` makes the upstream
relationship obvious. New `overlay_status_matches` idempotency guard
mirrors `running_status_matches` to keep `.status` PATCH traffic flat.

**Admission gate:** runtime-only (no ClawSandbox CEL admission rules
exist yet — schema-only Phase 1). The reconciler stamps `Degraded=True /
Reason=SpecInvalid` when `sigsAgentSandbox == "overlay"` but
`upstreamSandboxRef.name` is missing/empty, or when an unknown value
(typo such as `"Overlay"` or `"overaly"`) is supplied. Future slice can
hoist into CEL once a `claw_sandbox_validations()` function is added.

**Out of scope (deferred):** watching the upstream `Sandbox` CR's
status (would require the upstream CRD discovery + informer); mirroring
its conditions back onto `ClawSandbox.status`; `kubectl claw convert`
upstream→overlay path (lands in S9). `Translate` mode remains
schema-only — no runtime path beyond what already lands here.

**Surface:**

- `controller/src/crd.rs` — `UpstreamCompatibilityConfig` gains
  `upstream_sandbox_ref: Option<LocalObjectRef>`; `is_overlay_mode()`
  + `overlay_target_name()` pure helpers; field-level docs list all
  four accepted values (`off|observe|translate|overlay`) and the
  overlay-requires-ref invariant. 5 unit tests.
- `controller/src/status/conditions.rs` — new `TYPE_SUSPENDED`
  condition type + `reason::OVERLAY_MODE` constant.
- `controller/src/status/mod.rs` — `build_overlay_status_patch`,
  `overlay_status_matches`. 6 unit tests.
- `controller/src/reconciler/mod.rs` — overlay-mode pre-flight before
  Step 1 (Degrades on missing ref / unknown value); Deployment block
  (Step 4) wrapped in labelled `'deployment_block` with early-`break`
  on overlay; blocklist CM + CronJob (Step 4d) gated on
  `!overlay_mode`; Step 5 dispatches to `build_overlay_status_patch`
  when overlay target is set, else falls through to the existing
  Running path. `governance_config` and `blocklist_cm_name` hoisted
  out of the deployment block so Step 4c / Step 4d still see them.

**Reuse map:**

- Existing `LocalObjectRef` (`controller/src/mcp_server.rs:157`)
  reused — fifth client now (signing/jwks, profile, agent-card,
  guardrail-profile, **upstream sandbox ref**). No second
  ObjectReference type.
- `crate::status::conditions::preserve_transition_time` reused
  unchanged for the new three-condition matrix.
- `crate::status::stamp_degraded` reused for both new failure modes.
- Reconciler's existing `degrade!` macro reused — overlay-validation
  errors flow through the same Degraded-stamp + 60s requeue path as
  every other spec-invalid case.
- No new file managers, no new CRDs, no helm `crd.yaml` change
  (`upstreamCompatibility` was schema-only in Phase 1 — kube-rs
  registers the runtime schema; helm template stays admission-only
  until a `claw_sandbox_validations()` lands).

**Tests:** controller workspace 264 → 276 (+12: 5 CRD helpers + 6 status
helpers + 1 condition constant exercise via overlay tests). Workspace
green (router 595, integration 26).

**Audit:** `docs/security-audits/2026-04-27-phase2-overlaymode.md` — 2
sign-offs.

### S6 `phase2-claweval` — ClawEval CRD + binding ConfigMap + helm CRD

This slice ships the controller side of the Azure AI Foundry Evals
binding K8s primitive only. Per §3 non-compete, `ClawEval` is a
**binding/provisioning resource over Foundry Evals** — it *configures*
eval runs; it is **not** an in-cluster eval engine. The runtime
enforcement substrate stays on Phase 1 (`cli/src/commands/eval.ts` →
`/openai/evals` + `/evaluators` proxies in
`inference-router/src/routes/inference.rs`, executed under the
sandbox router's Workload Identity). The compiled JSON ConfigMap
(`claweval-{name}-binding`) is the hand-off contract; the sandbox-side
cron actuator / on-demand trigger that consumes it is deferred to S7.

#### Added

- **`ClawEval` CRD** — `controller/src/claw_eval.rs`, group
  `azureclaw.azure.com`, version `v1alpha1`, namespaced, shortname
  `ceval`. Spec fields: `sandboxRef.name`, `suite`
  (`foundry-evals` | `promptfoo` | `inspect-ai`, default
  `foundry-evals`), `evaluators?` (required + non-empty when
  `foundry-evals`), `model?`, `schedule?` (cron line),
  `dataset?` (mutually-exclusive `configMapRef` | `inline`),
  `threshold?` (`score` ∈ `[0,1]`, `op` ∈ `Gte`/`Gt`),
  `regressionAction?` (default `Suspend`), `displayName?`. Status:
  `phase`, `observedGeneration`, `conditions`,
  `bindingConfigMapRef`, `versionHash`, `lastReconciledAt`, plus
  three **runtime-owned** fields (`lastRunAt`, `lastScore`,
  `lastPass`) declared in the schema for SSA preservation by the
  S7 runtime writer. Six print columns surface sandbox, suite,
  schedule, score, pass, age.
- **Pure compile module** — `controller/src/claw_eval_compile.rs`
  with `compile_to_binding()` + `version_hash()` (sha256 first 16
  bytes hex). 9 unit tests cover deterministic compile, all suite
  serialisations, both threshold ops, default `regressionAction`
  always materialising, hash sensitivity, hash stability across
  serde round-trip. Mirrors S5 `claw_memory_compile.rs` shape.
- **Reconciler** — `controller/src/claw_eval_reconciler.rs`:
  finalizer `azureclaw.azure.com/claweval-cleanup`, field manager
  `azureclaw-controller/claweval`, SSA throughout. Compiles the
  spec, persists as ConfigMap (`claweval-{name}-binding`, key
  `binding.json`, standard labels). Status patch sets all six
  controller-owned fields and explicit `None` for the three
  runtime-owned fields so SSA leaves them untouched once the
  S7-side writer (`azureclaw-router/claweval`) applies them. Seven
  unit tests including `field_manager_distinct_from_runtime_writer`
  which documents and asserts the S7 forward contract.
- **CEL admission rules** — `controller/src/crd_validations.rs`
  `claw_eval_validations()` ships eight rules: `sandboxRef.name`
  shape, `evaluators` required+bounded for `foundry-evals` suite,
  per-evaluator length cap, `schedule` 5-or-6 token cron shape,
  `threshold.score` ∈ `[0,1]`, `dataset.configMapRef`/`inline`
  mutual exclusion, `dataset.inline` capped at 64 entries,
  `displayName` length cap. Five unit tests assert non-emptiness,
  message presence, post-injection rule count, core invariants
  coverage, and serde round-trip.
- **Helm CRD mirror** — `deploy/helm/azureclaw/templates/crd-claweval.yaml`
  generated via `DUMP_CLAWEVAL_CRD_YAML=1` dumper.
  `helm_claweval_crd_matches_rust_schema` drift test enforces
  Rust ↔ helm parity on every CI run.
- **Controller wiring** — `controller/src/main.rs` registers the
  three new modules and spawns `claw_eval_reconciler::run` in
  the existing `tokio::select!` (REQUEUE_OK 300s, REQUEUE_FAIL 60s).
- **Audit doc** —
  `docs/security-audits/2026-04-27-phase2-claweval-reconciler.md`
  with two sign-offs, full STRIDE coverage, AGT boundary
  verification (AGT 3.3.0 has no eval module — confirmed), 12-seam
  reuse map, explicit out-of-scope list (runtime trigger,
  pass/fail computation, regression actuator, runtime status
  fields).

#### Notes

- Test count delta (controller): 238 → 264 (+26).
- Single new struct: none beyond the spec/status/sub-types.
  `LocalObjectRef` semantically extended (6 clients now:
  signing/jwks, profile, agent-card, guardrail-profile,
  memory-binding, eval-dataset).
- Controller never calls Foundry. The runtime path
  (`cli/src/commands/eval.ts`) is **not modified in this slice**.
- This slice closes the §14.6 column-12 destination: five full
  CRDs (`McpServer`, `ToolPolicy`, `InferencePolicy`, `A2AAgent`,
  `ClawEval`) + the `ClawMemory` binding now ship as K8s
  primitives. Governance-as-K8s-primitives → ✓.
- Hard-deferred to S7+: runtime trigger (cron actuator), threshold
  pass/fail computation, regression actuator (mutating
  `ClawSandbox.spec.suspend`), AGT chain emission of eval
  outcomes.

### S5 `phase2/clawmemory-reconciler` — ClawMemory CRD + binding ConfigMap + helm CRD

This slice ships the controller side of the Foundry Memory Store
binding K8s primitive only. Per §3 non-compete, `ClawMemory` is a
**binding/provisioning resource over Azure AI Foundry Memory Store**
— it *configures* FMS for a sandbox; it is **not** a separate
in-cluster memory backend. The runtime enforcement substrate stays on
Phase 1 (`cli/src/plugin.ts::ensureMemoryStore` lazy-create through
the router's Workload Identity + the existing `/memory_stores/*`
proxy in `inference-router/src/routes/inference.rs`). The compiled
JSON ConfigMap (`clawmemory-{name}-binding`) is the hand-off contract;
the sandbox-side informer that consumes it is deferred to S7.

#### Added

- **`ClawMemory` CRD** — `controller/src/claw_memory.rs`, group
  `azureclaw.azure.com`, version `v1alpha1`, namespaced, shortname
  `cmem`. Spec fields: `storeName`, `sandboxRef.name`, `scope`,
  `retentionDays?`, `deleteOnSandboxDelete` (default `true`),
  `displayName?`. Status: `phase`, `observedGeneration`,
  `conditions`, `bindingConfigMapRef`, `versionHash`,
  `lastReconciledAt`. Print columns surface sandbox, store, scope,
  phase, age.
- **Pure compile module** — `controller/src/claw_memory_compile.rs`
  with `compile_to_binding()` + `version_hash()` (sha256 first 16
  bytes hex). 6 unit tests cover deterministic compile, full vs
  minimal spec round-trip, hash sensitivity to spec changes, hash
  stability across serde round-trip, and hash hex shape. Mirrors S4
  `inference_policy_compile.rs` shape.
- **Reconciler** — `controller/src/claw_memory_reconciler.rs`:
  finalizer `azureclaw.azure.com/clawmemory-cleanup`, field manager
  `azureclaw-controller/clawmemory`, SSA throughout. Compiles the
  spec, persists as ConfigMap (`clawmemory-{name}-binding`, key
  `binding.json`, labels `app.kubernetes.io/managed-by`,
  `azureclaw.azure.com/clawmemory`,
  `azureclaw.azure.com/artifact=claw-memory-binding`), sets full
  status with `Ready`/`Progressing`/`Degraded` conditions reusing
  `status/conditions.rs` (no condition-vocabulary fork). 7 unit tests.
- **CEL admission rules** — 4 rules in
  `controller/src/crd_validations.rs::claw_memory_validations()`:
  DNS-label `storeName` (1-63 chars, `^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`),
  `sandboxRef.name` 1-253 chars, `scope` 1-256 chars, `retentionDays
  > 0` when present. Injected via existing `inject_spec_validations`
  helper. 5 unit tests.
- **Helm CRD** — `deploy/helm/azureclaw/templates/crd-clawmemory.yaml`
  (184 lines) generated via the existing
  `helm_drift::dump_clawmemory_crd_yaml` dumper test (env-gated by
  `DUMP_CLAWMEMORY_CRD_YAML=1`). New drift-detection test pins
  helm-side YAML to Rust-derived schema; both round-trip.
- **`main.rs` wiring** — `claw_memory_reconciler::run` spawned in
  `tokio::select!` alongside the four prior reconcilers. CRD-missing
  exit is non-fatal (matches S1–S4 pattern).
- **Audit doc** — `docs/security-audits/2026-04-27-phase2-clawmemory-reconciler.md`
  with two sign-offs, full STRIDE coverage, AGT boundary verification
  (AGT 3.3.0 carries no Memory Store module — confirmed against
  on-disk source), reuse map, out-of-scope set with explicit S7
  forward-references, and reproduction of the Memory Store auth
  caveat (Foundry project MI must hold `Azure AI User` on the
  resource group; token audience `https://ai.azure.com/`) inside the
  CRD module docstring so it travels with the schema.

#### Reuse map (no-duplication rule, §0.2 / §0.3)

11 existing seams reused:

1. `controller/src/status::conditions` — vocabulary + transition-time
   helpers used unchanged.
2. `controller/src/mcp_server::LocalObjectRef` — 5th semantic client
   (S1 signing/jwks, S2 profile, S3 agent-card, S4 guardrail-profile,
   S5 binding-config).
3. `controller/src/inference_policy_reconciler` (S4) — reconcile
   shape + finalizer pattern + non-fatal CRD-missing exit, copied
   verbatim.
4. `controller/src/inference_policy_compile` (S4) — compile-module
   shape (pure-fn + version_hash + tests).
5. `controller/src/crd_validations::inject_spec_validations` — same
   SSA-friendly CEL injector.
6. `controller/src/helm_drift::canonical_form` — drift comparison
   reused verbatim.
7. `cli/src/plugin.ts::ensureMemoryStore` (Phase 1) — existing
   GET-then-POST create path against Foundry; **not modified in S5**.
   S7 wires the consumer that reads our ConfigMap.
8. `cli/src/core/foundry-discovery.ts::FoundryEnsureMemoryStore`
   (Phase 1) — discovery + lazy-create signature; not duplicated.
9. `inference-router/src/routes/inference.rs` `/memory_stores/*`
   proxy (Phase 1) — the router holds the Workload Identity for
   Foundry calls; the controller has none. We do not give the
   controller Foundry credentials.
10. `inference-router/src/proxy.rs` idempotency map (Phase 1) —
    PUT/DELETE/PATCH on `/memory-stores/x` already declared
    non-idempotent; not modified.
11. RFC-3339 `chrono::Utc::now().to_rfc3339_opts` formatter — copy-pasted
    across reconcilers (lift to shared module deferred to S7).

#### Out of scope (deferred to S7+)

- **Foundry-side delete on CR delete** — finalizer cleans the binding
  ConfigMap only; `deleteOnSandboxDelete` is preserved in the
  compiled binding for the runtime path to act on.
- **Conflict detection** across multiple `ClawMemory` CRs targeting
  the same sandbox+scope pair — router-side dedupe at S7.
- **Retention enforcement** — spec carries `retentionDays`; runtime
  enforcement (Foundry TTL or scheduled `delete_scope` sweeps) wired
  in S7 alongside hot-reload (§10.4 #11).
- **Status `phase` matrix beyond Ready/Degraded** — full S7 matrix
  cluster-wide.
- **Cross-namespace `sandboxRef`** — out of scope by design.

#### Test count delta

- Controller: 218 → 238 (+20 tests). Workspace total green.

#### §14.6 impact

Strengthens column 7 (Foundry / M365 integration) — the fifth full
CRD reconciler in the family lands. Phase 2 §14.6 column 12
(Governance as K8s primitives) at 4/5 differentiator CRDs; only
`ClawEval` (S6) outstanding.


## [Unreleased] — PR #44 `dev → main` uplift

This entry covers **186 commits** on `dev` since `main`, structured as Phase 0
(seams + safety net) and Phase 1 (protocol freshness + minimal schema). Every
capability cites code; every capability-introducing PR shipped a security-audit
doc under `docs/security-audits/` (75 docs total). See
[`docs/phase-0-1-capabilities.md`](docs/phase-0-1-capabilities.md) for the full
evidence index.

### Phase 0 — provider seams + compat suite + CI gates

#### Added
- **Provider seams (Phase 1)** — `PolicyDecisionProvider`, `AuditSink`,
  `SigningProvider` traits with in-tree `impl … for Governance` (router crate);
  each contract reachable via `Arc<dyn Trait>` view of the same
  `Arc<Governance>`. A fourth `MeshProvider` seam is **plugin-side by
  design** — the router's `providers/mesh.rs` is a documentation-only trait
  file.
- **Outage-mode dispatch** (`providers/outage.rs`) — `Strict` (prod default,
  fail-closed), `CachedRead` (allow if cached decision < TTL), `DegradedDev`
  (fail-open with warning label, dev only). Configurable per-`ClawSandbox`
  via `spec.agt.outageMode`.
- **Six blocking CI gates** under `ci/`: `check-loc.sh` (LOC budget),
  `no-stubs.sh` (no `TODO/FIXME/unimplemented!`), `no-custom-crypto.sh`
  (forbids hand-rolled crypto outside provider seams + vendored SDK),
  `no-null-provider-prod.sh` (Null* providers blocked unless
  `azureclaw.azure.com/dev-only` label set), `security-audit-required.sh`
  (per-PR audit-doc enforcement, 2 sign-offs), `vendored-patch-audit.sh`
  (forces re-audit on AGT SDK bump), plus `a2a-module-isolation.sh`. Budget
  in `ci/loc-budget.yaml`.
- **75 security-audit docs** under `docs/security-audits/` from the
  `_template.md` shape: threat-model delta, OWASP MCP/LLM mapping, AuthN/Z
  path, secret + key custody, egress-surface delta, audit events emitted,
  failure mode (fail-closed default), negative-test coverage, two sign-offs.
- **Behavioral conformance corpus** (`tests/conformance/`) — 8 specs:
  `signal-x3dh`, `signal-knock`, `signal-negative`, `oauth21-bcp`,
  `mcp-streamable-http`, `a2a-agent-card`, `ap2-commerce`,
  `sandbox-isolation`. Negative cases (tampered ciphertext, replayed message,
  wrong-issuer card, expired mandate) are mandatory per new endpoint.
- **Compat suite** (`tests/compat/`) — operator TUI flow with virtual-screen
  + outgoing-CR-payload assertions via a `blessed` mock harness.
- **5 cargo-fuzz targets** (`inference-router/fuzz/fuzz_targets/`) —
  `a2a-jws`, `a2a-base64url`, `deserialize-state`, `sanitize-chat`,
  `parse-streaming-pf`.
- **`docs/agt-vendored-patch-audit.md`** — index of fixes applied to the
  vendored AgentMesh stack (SDK + relay + registry) with re-audit cadence on
  AGT SDK bumps.
- **`docs/sigs-agent-sandbox-compat.md`** — `TranslateMode` / `OverlayMode`
  design for optional compat with `kubernetes-sigs/agent-sandbox`. Opt-in,
  no upstream dependency, no CI pin.
- **Hotspot decomposition (Pass 1 + 2)** with byte-equivalence proofs:
  - `inference-router/src/routes.rs` 4890 → 6 files (`routes/{inference,handoff,governance,mesh,egress,mod}.rs`); 1 allowlisted namespace fix.
  - `controller/src/reconciler.rs` 2326 → 1464 LOC.
  - `controller/src/mesh_peer.rs` 1970 → 1170 LOC; split into `mesh_peer/{mod,offload,pair}.rs`.
  - `inference-router/src/governance.rs` 1252 → 837 LOC.
  - `inference-router/src/handoff/mod.rs` 2075 → 1770 LOC.
  - `inference-router/src/spawn/docker.rs` 1199 → 762 LOC.
  - `cli/src/plugin.ts` 7455 LOC: `foundry-discovery.ts` and
    `router-client.ts` extracted.
- Repo tooling for behavioral-equivalence proofs (`tools/item-manifest/` +
  `tools/drift/drift.py`); baselines + allowlists under `tools/drift/`.
- **Federated-credential reaper** (`controller/src/fedcred_reaper.rs`, 232
  LOC, 4th `tokio::select!` arm in the controller event loop) — periodically
  GCs orphan federated credentials against the 20-fedcred-per-MI Azure cap;
  default 600 s, env override `FEDCRED_REAPER_INTERVAL_SECS`. 5 unit tests.
- **KEP-1623 status subresource on `ClawSandbox`** — `Conditions[]` +
  `observedGeneration`; controller stamps `Degraded=True` / `Ready=False`
  on the three validation-failure exits.
- **VAP / MAP set** in the controller Helm chart — `pods/exec|attach|portforward`
  ban on sandbox namespaces; deny posture-downgrades (isolation step-down,
  seccomp removal, `readOnlyRootFilesystem: false`); deny removal of
  `azureclaw.azure.com/dev-only` label once applied; mutating policy auto-
  injects router sidecar + sets seccomp to `azureclaw-strict`.

### Phase 1 — protocol freshness + minimal schema

#### Added
- **MCP 2026 Streamable HTTP** (`inference-router/src/mcp/`, 8 modules:
  `error`, `initialize`, `jsonrpc`, `oauth`, `oauth_layer`, `pipeline`,
  `streamable_http`, `tools`) — `POST /mcp` with full JSON-RPC 2.0 framing,
  `Mcp-Session-Id` semantics, batch support, oversized-frame reject;
  `tools/list` + `tools/call` dispatch; OWASP MCP Top 10 controls matrix at
  `docs/security-mcp-top10.md`.
- **OAuth 2.1 (RFC 8725 BCP)** — bearer-token verifier as a `tower::Layer`;
  PKCE, audience, expiry, resource-indicator, scope checks; gated by
  `McpServer.spec.productionMode: true`.
- **A2A 1.0.0** (`inference-router/src/a2a/`, 14 modules including
  `agent_card`, `agent_projection`, `card_server`, `card_signing`,
  `card_verifier`, `jsonrpc_dispatch`, `signature`, `snapshot_rebuild`,
  `trust_store`) — `/.well-known/agent.json` per-sandbox (Ed25519 detached
  JWS via `SigningProvider`); inbound `POST /a2a` JSON-RPC dispatch
  (`message/send`, `tasks/get`, `tasks/cancel`); hot-reloading
  trust-store snapshot for `kid → VerifyingKey`. Schema source:
  <https://a2a-protocol.org/v1.0.0/specification>. Ingress posture is
  gateway-only, surgical opt-in via `ClawSandbox.spec.a2a.expose: true` —
  see [ADR-0001](docs/adr/0001-a2a-ingress-front-edge.md).
- **AP2 commerce mandates** (`a2a/{ap2,mandate_signing,mandate_trust_store,
  message_send_ap2}.rs`) — IntentMandate detached-JWS sign/verify; per-tool
  `commerce.dailyCap` / `monthlyCap` / `counterpartyAllowlist` enforcement;
  type-safe `MandateTrustStore`.
- **`McpServer` CRD (schema-only)** + **`ToolPolicy` CRD (schema-only)** —
  `controller/src/{mcp_server,tool_policy}.rs`; CEL `x-kubernetes-validations`
  post-processed via `controller/src/crd_validations.rs` because kube-rs
  `CustomResource` derive does not emit the field (kube-rs#1557). Reconciliation
  ships in Phase 2.
- **`ClawPairing` CRD** + reconciler — operator-assisted pairing as a
  K8s-native operation (`controller/src/{pairing,pairing_reconciler}.rs`);
  `azureclaw pair <a> <b>`.
- **Identity provider seam — Microsoft Graph agent identity** — production
  Graph client at `controller/src/providers/identity_*.rs` calling
  `POST /beta/servicePrincipals/microsoft.graph.agentIdentity`,
  `POST /beta/servicePrincipals/{id}/federatedIdentityCredentials`,
  `DELETE /beta/servicePrincipals/{id}`. Endpoints verified against
  learn.microsoft.com (commit `2114bf2`). +5 controller tests (147 total).
- **Policy hot-reload** — router subscribes via K8s informers + AGT SSE;
  applies new `ToolPolicy` / `InferencePolicy` in-process without pod
  rollout; provider-flag flip (`vendored ↔ agt`) also hot-reloads.
- **OTel GenAI SemConv 1.x** emission on every router span.
- **Gateway token via `secretKeyRef`** — `OPENCLAW_GATEWAY_TOKEN` is mounted
  from a K8s `Secret` instead of plain env, with a one-shot warning when
  legacy plain-env paths are exercised.
- **Three new CLI commands** — `azureclaw a2a` (Phase 1 scaffold:
  `list-exposed`, `schema`), `azureclaw convert` (Phase 0 skeleton),
  `azureclaw pair`.
- **`docs/use-cases.md`** + **`docs/phase-0-1-capabilities.md`** + ADR-0001
  + OWASP MCP Top-10 controls matrix.

### Phase 0/1 — Recent fixes
- **Sub-agent re-spawn after handoff** — sub-agent trust + resume signals
  must use `restoreResp.sub_agent_results` (spawned), not
  `sub_agent_workspaces` (may be empty). `cli/src/plugin.ts:2164-2270`.
- **Vendor patch #21 (SDK)** — `SessionManager.initiateSession` returns
  `{reused: true}` when an incoming KNOCK already established a crypto-layer
  session (was throwing "Active session already exists").
- **`azureclaw connect` port-forward error surfacing** — kubectl stderr is
  now displayed in the human-readable "address already in use" form.
- **Deduplicated chat replay** — long-standing duplicate-message UI bug
  triaged across plugin + sandbox image; investigation captured in session
  checkpoints.

### Engineering metrics (PR #44)
- **186 commits** on `dev` since `main`.
- **75 security-audit docs** under `docs/security-audits/`.
- **26 vendor patches** (SDK 21 + relay 4 + registry 1).
- **6 blocking CI gates** + a2a-module-isolation + LOC budget.
- **8 conformance specs**, **1 compat spec**, **5 fuzz targets**.
- **205 Rust tests** (74 controller + 105 router + 26 integration); **207 CLI
  tests**.
- **4 CRDs total** — `ClawSandbox` + `ClawPairing` reconciled; `McpServer` +
  `ToolPolicy` schema-only.
- **21 CLI commands**, **10 skills** (8 Foundry + 2 internal), **5 Docker
  images**.

## [pre-PR-44 baseline]

### Added
- **Preflight RBAC checks for `azureclaw up`** — new `cli/src/preflight.ts` queries effective permissions at subscription scope (`Microsoft.Authorization/permissions`), resource-provider registration, and preview-feature flags BEFORE Bicep runs, so operators fail in ≤30s instead of 20 minutes in. Prints copy-pasteable `az role assignment create` remediation commands with the exact missing actions. Escape hatch: `--skip-preflight`. See `docs/permissions.md` for the full role matrix + custom-role JSON.
- **`docs/permissions.md`** — canonical required-roles reference for `azureclaw up`: Contributor + User Access Administrator (or Owner), per-action justification, least-privilege custom role, preview feature registration, and Entra `api://agentmesh` tenant-admin caveat.
- **Bidirectional Agent Handoff** — live-migrate agents between local Docker and AKS cloud with `azureclaw handoff <name> --to cloud|local`. Supports both CLI-driven (operator) and LLM-driven (webchat) orchestration paths
- **Sub-Agent Handoff** — sub-agents are snapshotted (workspace + task state), destroyed on source, re-spawned on target, and injected with workspace + resume signal via E2E encrypted mesh
- **Stale AMID Cache Poisoning Fix** — three-layer defense: identity-based AMID rejection, prekey readiness gate, workspace inject retry with ack verification
- **Workspace Injection Pipeline** — tarball extraction with path traversal validation, `incoming/` file promotion to workspace root, `HANDOFF_FILES.md` manifest for agent discoverability
- **Handoff Decommission Cleanup** — reverse handoff deletes all cloud CRDs (parent + sub-agents); forward handoff destroys local sub-agent containers
- **Mesh Inbox Improvements** — protocol message filtering (hides handoff/ack messages), auto-decode of `file_transfer` base64 content
- **Native AGT Governance** — Rust-native governance module (replaces former Python sidecar) with PolicyEngine, TrustManager (0–1000, ±200 clamp, Ed25519 signed), SHA-256 Merkle audit chain, RateLimiter, and BehaviorMonitor
- **E2E Encrypted Inter-Agent Messaging** — Signal Protocol (X3DH + Double Ratchet) via AgentMesh relay/registry with KNOCK trust handshake
- **Content Safety via Foundry Guardrails** — Content Safety + Prompt Shields enforced server-side by Foundry (`Microsoft.DefaultV2`); the router parses `prompt_filter_results` annotations from model responses and reports flags to AGT governance for trust scoring and audit
- **Foundry Agent Service Integration** — web search, code execute, file search, image generation, memory via Foundry project endpoint
- **5-Image Architecture** — controller, inference-router, sandbox, agentmesh-relay, agentmesh-registry (governance runs natively in the router)
- **CLI `push --only <image> --apply`** — selective image builds with automatic pod restart
- **10 AGT Policy Rules** — shell-safety, inference rate-limiting, content safety, mesh trust gates, spawn governance, sensitive file deny, recon tool deny, cloud metadata deny
- **AGT Tool Execution Gate** — exec_command and http_fetch are evaluated by the native governance module before execution; fail-open with 2s timeout
- **Operator Dashboard** — real-time trust scores, audit chain, policy status, mesh connectivity
- **GitHub CI/CD** — Rust + TypeScript + Python lint/test, Bicep validation, Helm lint, Trivy security scan, Dockerfile lint, tag-triggered releases
- **Unit Tests** — Rust (controller + router) and TypeScript (CLI + plugin) covering controller, router, CLI, and governance
- **GitHub Templates** — issue templates (bug, feature, security), PR template, CODEOWNERS
- **Trace-ID correlation** — every inbound router request is assigned an opaque `x-trace-id` (or honors a client-supplied one), propagated to upstream Azure calls, tagged on all tracing spans, and stamped onto every AGT audit-chain entry. Unblocks multi-hop "why did this prompt fail" debugging without a rebuild loop.
- **Bounded-retry middleware for idempotent upstream calls** — `proxy::forward` now retries up to 3× with exponential backoff on transient Azure upstream failures (connection-reset, 502, 503, 504) for GET and `/embeddings` only. `/chat/completions`, `/completions`, `/responses` are never retried (non-idempotent). Configurable via `UPSTREAM_RETRY_MAX_ATTEMPTS` / `UPSTREAM_RETRY_INITIAL_MS`.
- **Handoff lifecycle metrics** — new Prometheus counters `azureclaw_handoff_pending_events_total{result}` and `azureclaw_handoff_phase_transitions_total{from,to,result}` so operators can see rate-limit cooldowns, token expirations, and phase-machine progress without tail-searching logs.
- **Route-level threat model** (`docs/threat-model/routes.md`) — walks every router group (inference, foundry, agt, mesh, handoff, egress, admin, health) with auth posture, input validation, and blast-radius analysis.
- **Repo tooling for behavioral-equivalence proofs** — `tools/item-manifest/` (syn-based fn-body hasher) + `tools/drift/drift.py` (comparator with allowlist) gates large mechanical refactors. Baselines and allowlists under `tools/drift/baselines/` and `tools/drift/allowlist-*.txt`.
- **Local dev stack** — `docker-compose.dev.yml` + YAML scenario runner (`cli/src/testing/scenario.ts`) so plugin/router behavior can be exercised against a zero-dep fake router without any Docker image builds. Drives the `rebuild → push → wait → debug` loop from >15min down to sub-second for protocol changes.
- **Test fixtures** — 8 sanitized Azure Foundry JSON fixtures + 3 axum-based fake servers (IMDS, AAD, Azure upstream) with a request recorder, all shared between Rust integration tests and the CLI fake-router runner.

### Fixed
- **`azureclaw up` stepper numbering** — declared `totalSteps: 7` never matched the 9 runtime phases (10 with `--expose-registry`), and step 4 (`kubectl` configure) was missing its `stepper.done()` call so it appeared to silently disappear from the progress log. Total now tracks the actual branch count, and every step has an explicit completion.
- Router bind address fix for K8s probe accessibility
- K8s probe host field removal (kubelet defaults to pod IP)
- Missing transitive Python dependencies (typing_inspection, cryptography) via PyPI fallback
- 8 vendor patches for AgentMesh relay, registry, and SDK bugs (this baseline; the active count is **26 patches** as of PR #44 — see `docs/agt-vendored-patch-audit.md`)
- Foundry Memory Store format — ensureMemoryStore creates full store with chat + embedding models; item format matches Foundry REST API spec

### Changed
- AGT inference rate limit bumped from 120 → 500 calls/60s (policy) and router token bucket from 100 → 500 global req/s (needed for multi-agent handoff traffic)
- Controller reconcile error requeue is now split by error kind: transient `kube::Error` keeps the 30s requeue, but `serde_json::Error` (malformed CR fields) now requeues at 300s instead of 30s. Malformed CRs won't heal on retry, so the longer back-off avoids log-spamming every 30s while a human edits the resource. Operators debugging a failed reconcile should expect a ~5-minute gap, not 30s. An `error!` log line is always emitted so the delay is never silent. See `controller/src/reconciler.rs::error_requeue_duration`.
- **`POST /sandbox/spawn` canonical field is now `agent_id` (was `name`).** The Rust `SpawnRequest` / `SpawnResponse` / `SubAgentEntry` / `SubAgentSnapshot` structs use `agent_id` as the field name, and responses serialise `agent_id` on the wire. For backward compatibility with in-flight plugins, `name` is accepted as a deserialise-only serde alias on `SpawnRequest` and `SubAgentSnapshot`; a payload that sets both `agent_id` and `name` is rejected with a 422 (duplicate field) to catch inconsistent clients. The bundled plugin has been migrated to send and read `agent_id`. Operators who call `/sandbox/spawn` directly (e.g. via curl or a custom client) should switch to `agent_id` — the `name` alias will be removed in a future release.
- **Canonical admin auth is now `Authorization: Bearer <token>`.** The legacy `x-azureclaw-admin` header is still accepted but emits a one-shot `warn!` log on first use per process. It will be removed in a future release. No action required for operators using the bundled CLI; custom scripts should switch to `Authorization: Bearer`.
- **Router bounded graceful-shutdown.** `axum::serve().with_graceful_shutdown(...)` is now wrapped in `tokio::time::timeout`. Default timeout is `max(TERMINATION_GRACE_PERIOD_SECS − 5s, 10s)` (typically 25s). Override with `SHUTDOWN_TIMEOUT_SECS`. Long-running SSE streams past the budget are log-and-dropped instead of blocking pod termination indefinitely.
- **Router error-response format unified.** All router handlers now emit one of two documented shapes: a flat `{code, message, trace_id}` for internal endpoints, or the OpenAI-compatible `{error: {type, code, message}}` for inference/foundry endpoints. The constructors (`errors::flat`, `errors::openai`) are pinned by byte-exact unit tests. See `inference-router/src/errors.rs` and `docs/threat-model/routes.md`.
- **Internal: `inference-router/src/routes.rs` (4890 LOC) split into 6 files** under `routes/` (`inference`, `handoff`, `governance`, `mesh`, `egress`, `mod`). Byte-level equivalence proven by `tools/drift/drift.py` against the pre-split baseline; exactly 1 allowlisted namespace-resolution fix. No behavior change.
- **File-size policy** (`CONTRIBUTING.md`): any PR that pushes a source file past 1500 LOC must either split the file in the same PR or add a follow-up issue link. Enforced via manual review.

### Security
- Foundry-side Content Safety guardrails (`DefaultV2`) — content filter annotations parsed from model responses and reported to AGT governance
- iptables UID-based egress — agent process restricted to localhost
- Zero Azure credentials in agent container — router authenticates via Workload Identity
- Kata Confidential VM support — per-pod dedicated kernel
- Custom seccomp profile (219 allowed syscalls, 28 explicitly blocked)
- Domain blocklist (51k+ malicious domains)
- **`#[serde(deny_unknown_fields)]` on typed inbound DTOs** (`SpawnRequest`, `HandoffMeta`) — unknown fields are now rejected at deserialization. All other router handlers take `Json<serde_json::Value>` and forward opaquely.
- **Constant-time admin-token comparisons** via `handoff::constant_time_eq` — replaces 4 `==` compares across `routes.rs` (AGT trust/rate-limit endpoints) and `main.rs` (cross-pod bearer-auth middleware). Eliminates timing-side-channel risk.
- **Admin-endpoint Origin allowlist** — requests to admin routes with a browser `Origin` header are rejected unless the origin is on `ADMIN_ALLOWED_ORIGINS` (default: none). CLI/curl traffic (no `Origin`) is unaffected. Closes cross-site-request abuse vector on leaked admin token.
- **`cargo audit` CI job** — runs on every PR; currently `continue-on-error: true` pending triage-cadence decision. Caught RUSTSEC-2026-0098/-0099/-0104 during rollout (closed by bumping `rustls-webpki` 0.103.10 → 0.103.13).
- **Sandbox-hardening regression tests** — every hardening invariant (UID 1000, read-only rootfs, all caps dropped, seccomp strict profile, NET_ADMIN drop after init, iptables egress-guard, plugin+SDK root-owned read-only) is asserted by a controller-side reconciler unit test that fails on regression.
- **Fuzz + proptest coverage** — `cargo +nightly fuzz` targets for the handoff blob parser, blocklist domain parser, AGT policy evaluator, and safety-response parser. `proptest` coverage for handoff-chunking, Double-Ratchet state transitions, and K8s name validation.
- **Vendor dependency advisory closure** — pulled `rustls-webpki` 0.103.13 to close 3 live RUSTSEC advisories. Only remaining audit warning is a transitive `rand 0.8.5` soundness note via upstream `agentmesh 3.1.0` (requires upstream bump).
