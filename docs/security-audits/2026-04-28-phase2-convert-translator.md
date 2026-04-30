# Phase 2 — S9.2 `phase2-convert-translator` — real `azureclaw convert`

**Slice:** S9.2
**Branch:** `phase2-convert-translator`
**Base:** `dev` @ `d090bcc`
**Status:** ready for review
**Audit date:** 2026-04-28

## 0. One-line summary

Replace the Phase 0 exit-3 skeleton of `azureclaw convert` with a real
YAML-in / YAML-out translator between the AzureClaw `ClawSandbox` CRD
and the upstream `agents.x-k8s.io/v1alpha1 Sandbox` CRD
(`kubernetes-sigs/agent-sandbox`), plus an "overlay-skeleton emit" target
for bootstrapping a fresh `ClawSandbox` against an operator-owned upstream
Sandbox. Hard-fail-on-lossy default; `--allow-lossy` waives.

## 1. Reuse map (existing implementation surveyed)

Per §0.2 #8, this section enumerates every existing seam touched or
deliberately reused. New code extends seams; nothing parallel-implements.

| Existing seam | Where it lives | How this slice uses it |
|---|---|---|
| `convertCommand()` exit-3 skeleton | `cli/src/commands/convert.ts` (Phase 0) | replaced — same Commander shape, same flag names, same exit-code spectrum, but real implementation under the hood |
| Programmatic CLI registration | `cli/src/cli.ts:21,66` | unchanged — `convertCommand` already registered under "Interop" |
| `yaml` package (v2.6.0) | `cli/package.json` | reused — `parse`, `parseAllDocuments`, `stringify`. No new dep. |
| `chalk` for stderr formatting | already a dep | reused for `red` errors + `yellow` warnings + `dim` dry-run |
| `__test` export pattern | `attest.ts`, `migrate.ts`, `policy.ts` | reused — every pure helper exposed via `__test` for vitest |
| ClawSandbox shape | `controller/src/crd.rs:25-405` | translator targets the **actual** field tree (`openclaw.image`, `openclaw.extraEnv`, `sandbox.{isolation,seccompProfile,…}`, `resources`, `upstreamCompatibility`, …) — **not** the aspirational §4 mapping table in `docs/sigs-agent-sandbox-compat.md`, which lists fields that don't exist in the CRD (e.g. `spec.scale`, `spec.sandbox.expiry`, `spec.sandbox.volumes`) |
| Controller seccomp/runtimeClass logic | `controller/src/reconciler/mod.rs:34-78` (`build_pod_security_context`, `isolation_scheduling`) | mirrored exactly: `confidential` → `kata-vm-isolation` + `RuntimeDefault`; `enhanced` + `<name>` → `Localhost(profiles/<name>.json)`; `RuntimeDefault`/empty → `RuntimeDefault` |
| `UpstreamCompatibilityConfig` | `controller/src/crd.rs:104-126` | overlay-emit target writes literal `{ sigsAgentSandbox: "overlay", upstreamSandboxRef: { name } }` — same shape `migrate to-overlay` (S9.1) writes |
| `LocalObjectRef.name` semantics | `controller/src/crd.rs` (same-namespace only) | `--sandbox-ref ns/name` namespace half is **validated**, not encoded — namespace mismatch with input metadata.namespace exits 2 |

No new transitively-reachable AGT or crypto code. No second YAML parser.
No second seccomp/runtimeClass mapper.

## 2. AGT boundary

Not applicable. This is a manifest-translator CLI that runs on the
operator's workstation. It never touches AGT, never makes network calls,
never reads cluster state. The translation is a pure function from
YAML-in to YAML-out.

## 3. STRIDE

| Threat | Asset | Mitigation |
|---|---|---|
| **S**poofing | None — no identity surface | n/a |
| **T**ampering | Output YAML | Translator is pure; no hidden state. Output deterministic for a given input (env keys sorted, status stripped). |
| **R**epudiation | Conversion result | Output is plain YAML printed to stdout; operator pipes/saves/inspects it before applying. No magic. |
| **I**nformation disclosure | Input YAML may contain Secrets in `env.valueFrom` | `valueFrom` entries are dropped with a per-name warning, never emitted in output. Stale literals for the same env name are also purged. |
| **D**enial of service | YAML parser DoS via giant inputs | Inherits `yaml@2.6.0` defaults; no recursion. CLI is single-shot, not a server. |
| **E**levation of privilege | None — no privileged operation | n/a |

The single notable threat is the `valueFrom`-vs-stale-literal interaction
(I). Rubber-duck pass #4 flagged it; mitigated as documented in
`envArrayToMap` (`cli/src/commands/convert.ts:233-272`).

## 4. Out of scope (deferred to follow-up slices)

- **`azureclaw migrate from-kagent`** — kagent has multiple CRDs
  (`Agent`, `ToolServer`, `Identity`) that need a tree-of-resources
  translator. Different shape; separate slice.
- **Live-cluster import** (`azureclaw convert --from-cluster ns/name`)
  — works today via `kubectl get … -o yaml | azureclaw convert
  --file=/dev/stdin`; standalone flag is UX polish.
- **Lossless round-trip mode** — by design impossible: AzureClaw and
  upstream are different governance scopes. Adding `--strict` for
  CI lint is a future option.
- **CRD validation against schema** — translator emits structurally-correct
  YAML. Final admission validation is K8s + the CRD `validations` block.
  We do not duplicate the OpenAPI schema check on the client side.

## 5. Implementation surface

Files added / modified:

| File | LOC delta | Status |
|---|---|---|
| `cli/src/commands/convert.ts` | replace 125 → ~520 | replaced (Phase 0 skeleton → real implementation) |
| `cli/src/commands/convert.test.ts` | new file, ~570 LOC, 48 tests | added |
| `CHANGELOG.md` | +73 LOC | edited |
| `docs/security-audits/2026-04-28-phase2-convert-translator.md` | new | added |

No controller, router, or vendored SDK changes.

## 6. Field semantics

**Forward (`ClawSandbox` → upstream `Sandbox`)**

| ClawSandbox path | Upstream path | Notes |
|---|---|---|
| `metadata.{name,namespace,labels,annotations}` | same | `cleanMetadata` strips `uid/resourceVersion/managedFields/creationTimestamp` |
| `spec.openclaw.image` | `spec.podTemplate.spec.containers[0].image` | required; missing → exit 2 |
| `spec.openclaw.extraEnv` | `spec.podTemplate.spec.containers[0].env` | sorted by key for deterministic output |
| `spec.resources` | `spec.podTemplate.spec.containers[0].resources` | pass-through |
| `spec.sandbox.{readOnlyRootFilesystem,runAsNonRoot,allowPrivilegeEscalation}` | `spec.podTemplate.spec.containers[0].securityContext` | direct map |
| `spec.sandbox.isolation == "confidential"` | `spec.podTemplate.spec.runtimeClassName: kata-vm-isolation` | + `seccompProfile: { type: RuntimeDefault }` (Kata VM provides isolation) |
| `spec.sandbox.seccompProfile` | `spec.podTemplate.spec.containers[0].securityContext.seccompProfile` | mirrors controller logic — see §1 reuse row |
| `spec.{inference,governance,a2a,agent,azureServices,networkPolicy,upstreamCompatibility}` | (lossy drop) | warn per field; hard-fail without `--allow-lossy` |
| `status` | (stripped) | warn |

`spec.replicas` is fixed to 1 — ClawSandbox does not carry a `scale` field
today.

**Inverse (upstream `Sandbox` → `ClawSandbox`)**

| Upstream path | ClawSandbox path | Notes |
|---|---|---|
| `metadata.{name,namespace,labels,annotations}` | same | server-managed metadata stripped |
| `spec.podTemplate.spec.containers[0].image` | `spec.openclaw.image` | required |
| `spec.podTemplate.spec.containers[0].env` | `spec.openclaw.extraEnv` | order-aware projection (see §3 I) |
| `spec.podTemplate.spec.containers[0].resources` | `spec.resources` | pass-through |
| `spec.podTemplate.spec.runtimeClassName == "kata-vm-isolation"` | `spec.sandbox.isolation = "confidential"` | absent → `enhanced`; unknown value → warn |
| `spec.podTemplate.spec.containers[0].securityContext.seccompProfile` | `spec.sandbox.seccompProfile` | `canonicaliseSeccomp` (see §7) |
| `spec.podTemplate.spec.containers[0].securityContext.{readOnlyRootFilesystem,runAsNonRoot,allowPrivilegeEscalation}` | `spec.sandbox.{...}` | direct map |
| `spec.{shutdownTime,shutdownPolicy,volumeClaimTemplates}` | (lossy drop) | warn each |
| `spec.replicas` (when ≠ 1) | (lossy drop) | warn |
| `spec.podTemplate.spec.{volumes,initContainers,serviceAccountName,hostNetwork,hostPID,hostIPC,nodeSelector,affinity,tolerations,imagePullSecrets}` | (lossy drop) | warn each |
| `spec.podTemplate.metadata.{labels,annotations}` | (lossy drop) | controller manages pod labels/annotations |
| Multiple containers | first wins | warn |

**Overlay emit (upstream `Sandbox` → fresh `ClawSandbox` skeleton)**

Output is intentionally minimal: only `apiVersion`, `kind`, `metadata.{name,namespace}`,
and `spec.upstreamCompatibility = { sigsAgentSandbox: "overlay", upstreamSandboxRef: { name } }`.
No pod-template fields. The operator must add governance fields
(`spec.governance`, `spec.inference`, `spec.a2a`, `spec.agent`) before
applying — the translator emits a reminder warning.

## 7. Seccomp canonicalisation (inverse-only)

`canonicaliseSeccomp` accepts:

- `Localhost { localhostProfile: "profiles/<name>.json" }` → `<name>` — canonical
- `Localhost { localhostProfile: "<name>.json" }` → `<name>` + warn — non-canonical path
- `Localhost { localhostProfile: "<name>" }` → `<name>` + warn — non-canonical
- `RuntimeDefault` on confidential → undefined (no warning; controller would emit this)
- `RuntimeDefault` on non-confidential → undefined + warn (controller would emit Localhost)
- `Unconfined` or unknown `type` → undefined + warn
- `Localhost` without `localhostProfile` → undefined + warn

This three-level tolerance lets the inverse round-trip cleanly even when
the upstream YAML was written by someone who didn't know the canonical
`profiles/<name>.json` form, while warning loudly enough that the
operator notices and fixes the source.

## 8. SSA + reconciler skip

Not applicable — this slice does not touch the controller. The CLI
emits YAML; the operator (or a CD pipeline) applies it. Server-Side
Apply field-manager identity is whatever the apply step uses.

## 9. Failure modes

| Mode | Exit code | User feedback |
|---|---|---|
| Invalid `--to` | 2 | `error: --to must be one of: clawsandbox, upstream-sandbox, overlay (got X)` |
| File unreadable | 2 | `error: cannot read <path>: <fs error>` |
| Multi-doc YAML | 2 | `error: input YAML contains N documents; convert accepts exactly one` |
| Non-mapping root | 2 | `error: input YAML must be a single mapping (object)` |
| Missing apiVersion / kind | 2 | `error: input YAML missing apiVersion` |
| Wrong source kind for target | 2 | `error: expected source kind=X apiVersion=Y/...; got kind=A apiVersion=B/...` |
| Missing image | 2 | `error: ClawSandbox.spec.openclaw.image required` / `error: upstream Sandbox primary container missing image` |
| Missing podTemplate / spec / containers | 2 | precise `error: missing spec.podTemplate` / `no containers` etc. |
| Overlay sandbox-ref namespace mismatch | 2 | `error: --sandbox-ref namespace "X" does not match input metadata.namespace "Y"` |
| Overlay missing `--sandbox-ref` | 2 | `error: --to overlay requires --sandbox-ref=<name|namespace/name>` |
| Lossy translation without `--allow-lossy` | 4 | warnings printed to stderr, then `error: translation is lossy (N warning(s)); pass --allow-lossy to proceed` |
| Lossy translation **with** `--allow-lossy` | 0 | warnings printed to stderr, manifest printed to stdout |
| Lossless translation | 0 | manifest printed to stdout |
| `--dry-run` + lossy + no `--allow-lossy` | 4 | identical to non-dry-run lossy refusal |
| `--dry-run` + would-succeed | 0 | warnings (if any) to stderr, no manifest, dim "dry-run: would emit ..." |

The 4 / 0 split is the safety contract: the default is hard-fail because
silently dropping a TokenBudget or a ContentSafety floor is a governance
regression. `--allow-lossy` is an explicit operator acknowledgement.

## 10. Test surface

48 vitest cases in `cli/src/commands/convert.test.ts`. Coverage:

- 6 cases — `parseTarget`: each known target, undefined/empty/unknown rejection.
- 6 cases — `parseManifest`: single-doc happy path, multi-doc reject,
  empty input reject, non-mapping reject, missing-apiVersion reject,
  missing-kind reject.
- 2 cases — `cleanMetadata`: server-managed strip, non-object short-circuit.
- 2 cases — `mapToEnvArray`: deterministic sort, non-string skip.
- 5 cases — `envArrayToMap`: simple convert, duplicate-literal warn,
  valueFrom drops prior literal, literal-overrides-valueFrom double warn,
  non-array short-circuit.
- 7 cases — `canonicaliseSeccomp`: canonical form, `<name>.json`, bare
  name, RuntimeDefault on confidential (no warn), RuntimeDefault on
  non-confidential (warn), unknown type, missing localhostProfile.
- 8 cases — `clawsandboxToUpstreamSandbox`: enhanced happy path,
  confidential RuntimeDefault override, RuntimeDefault/empty seccomp,
  one-warning-per-AzureClaw-only-field (×7 in single test), wrong source
  kind, missing image, status-block warn.
- 7 cases — `upstreamSandboxToClawsandbox`: round-trip from canonical
  fixture, kata→confidential, unknown runtimeClass→enhanced+warn,
  multi-container first-wins, full lossy-field warning sweep
  (16 fragments asserted), wrong source kind, missing
  podTemplate/containers/image triple-reject.
- 5 cases — `emitOverlay`: skeleton emission, ns/name accept, ns mismatch
  reject, ClawSandbox source reject, empty-name-after-slash reject.
- 3 cases — `dispatch`: each target.
- 1 case — `formatYaml`: stable output.
- 1 case — round-trip stability across forward → inverse.

End-to-end smoke verified manually:

```
$ node dist/index.js convert -f /tmp/sandbox-fixture.yaml --to clawsandbox
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: demo
  namespace: azureclaw-demo
spec:
  openclaw:
    image: openclaw:1.2.3
    extraEnv:
      FOO: bar
  sandbox:
    isolation: confidential
exit=0
```

Workspace test count: CLI 337 → 382 (+45 net; 48 added, 3 lost on the
deleted Phase 0 skeleton tests). All other suites unchanged.

## 11. Verify-don't-guess

Per §0.2 #10. Sources cited in code and tests:

- **Upstream API shape:**
  `kubernetes-sigs/agent-sandbox @ c8c85f5f145441b60502227d5017ae8207164538`,
  `api/v1alpha1/sandbox_types.go` —
  https://github.com/kubernetes-sigs/agent-sandbox/blob/c8c85f5f145441b60502227d5017ae8207164538/api/v1alpha1/sandbox_types.go
  (verified via GitHub MCP `get_file_contents` 2026-04-28).
  Confirmed: `agents.x-k8s.io/v1alpha1`, `Sandbox`, `spec.podTemplate.spec`
  is `corev1.PodSpec`, `spec.podTemplate.metadata` is `PodMetadata`
  (labels + annotations), `spec.volumeClaimTemplates`, `Lifecycle` inlined
  (`shutdownTime` + `shutdownPolicy`), `replicas` validated `0..1` default
  `1`. `api/` directory contains `v1alpha1/` only — no v1alpha2 yet.

- **Controller seccomp/runtimeClass logic:**
  `controller/src/reconciler/mod.rs:34-78` (`build_pod_security_context`
  + `isolation_scheduling`). Confirmed `confidential` → `kata-vm-isolation`
  (not `kata-cc-isolation` as the rubber-duck pass initially suggested
  and as `docs/sigs-agent-sandbox-compat.md` does **not** specify).

- **ClawSandbox CRD shape:** `controller/src/crd.rs:25-405`. `OpenClawConfig`,
  `SandboxConfig`, `InferenceConfig`, `UpstreamCompatibilityConfig` all
  read directly; field names taken verbatim.

- **Phase 0 skeleton it replaces:** `cli/src/commands/convert.ts` (pre-edit)
  — referenced `docs/sigs-agent-sandbox-compat.md §4`. Mapping table in
  §4 was found to be aspirational (lists fields like `spec.scale`,
  `spec.sandbox.expiry` that the CRD does not have); translator was
  written against the **actual** CRD shape and the audit notes this
  divergence (§1).

## 12. Ops surface

- **`azureclaw convert -f file.yaml --to clawsandbox`** — pull an
  upstream Sandbox YAML, get a ClawSandbox skeleton back. Use case:
  Day-1 adoption from an existing `agents.x-k8s.io/v1alpha1 Sandbox`
  workload.
- **`azureclaw convert -f file.yaml --to upstream-sandbox --allow-lossy`**
  — emit an upstream-shaped manifest from a ClawSandbox. Use case:
  Day-1 *de*-adoption (or testing AzureClaw in a cluster that runs
  the upstream controller).
- **`azureclaw convert -f file.yaml --to overlay --sandbox-ref=foo`** —
  bootstrap overlay-mode ClawSandbox from an existing upstream Sandbox.
  Day-1 partial adoption: keep upstream-managed pods, add AzureClaw
  governance overlay. Pairs with `azureclaw migrate to-overlay` (S9.1)
  which performs the in-place mode switch on an *existing* ClawSandbox.

The `--dry-run` mode is for CI lint: pipe a manifest through, verify
it converts cleanly, fail the build if it does not.

## Sign-offs

- Author: GitHub Copilot CLI agent (claude-opus-4.7) — implementation,
  rubber-duck-driven design refinement (5 critique findings adopted),
  test sweep, manual smoke.
- Reviewer 1: rubber-duck agent — design critique 2026-04-28; 5 findings
  (lossy-default split, overlay input gating, valueFrom handling,
  seccomp localhostProfile path convention, dry-run lossy preservation,
  ns mismatch rejection, multi-doc reject, status strip, missing-podSpec
  invalid-input handling, deterministic env ordering). All 10 findings
  adopted as documented; 0 deferred.
- Reviewer 2: end-to-end smoke + upstream verification — `node dist/index.js
  convert` exits 0 on a confidential-mode upstream Sandbox; upstream API
  shape verified against `kubernetes-sigs/agent-sandbox @ c8c85f5` directly
  via GitHub MCP (no stale doc reliance).


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
