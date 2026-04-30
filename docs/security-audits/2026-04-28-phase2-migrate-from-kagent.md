# Phase 2 S9.3 — `phase2-migrate-from-kagent` security audit

**Status:** Implemented · awaiting merge into `dev`
**Date:** 2026-04-28
**Branch:** `phase2-migrate-from-kagent`
**Scope:** CLI translator only — zero Rust / controller / router changes.

## 1. Slice summary

Adds `azureclaw migrate from-kagent <input>` — a one-shot, pure-CLI
YAML translator that converts a `kagent.dev/v1alpha2 Agent` CR into an
AzureClaw resource bundle:

1. `azureclaw.azure.com/v1alpha1 ClawSandbox` (always, one)
2. `azureclaw.azure.com/v1alpha1 InferencePolicy` (zero or one — only when
   `spec.declarative.modelConfig` is set; provenance-only mapping)
3. `azureclaw.azure.com/v1alpha1 ToolPolicy` (zero or more — one per
   `(McpServer, toolName)` pair)

Closes §15.2 #8 of the implementation plan ("kagent migration tool"). The
day-1 adoption story is: an operator running kagent declarative agents
runs `azureclaw migrate from-kagent agent.yaml --image my/runtime:v1
--allow-lossy | kubectl apply -f -` to produce an AzureClaw resource
bundle they can apply alongside hand-authored AzureClaw `McpServer` and
`InferencePolicy` objects.

## 2. Threat model (STRIDE)

| Category | Threat | Mitigation |
|---|---|---|
| **Spoofing** | A malicious YAML claims to be a kagent Agent with `kind: Agent` but a fabricated `apiVersion`. | We hard-gate on the exact pair `(apiVersion=kagent.dev/v1alpha2, kind=Agent)`; any deviation → exit 2. |
| **Spoofing** | A multi-doc YAML smuggles a malicious second resource. | Multi-doc input is rejected → exit 2. |
| **Tampering** | An operator pre-populates `metadata.labels[azureclaw.azure.com/sandbox]` with a value other than the agent name to silently misroute generated `ToolPolicy.spec.appliesTo.sandboxMatchLabels`. | We refuse a pre-existing conflicting value → exit 2. Test: `rejects pre-existing conflicting sandbox label`. |
| **Tampering** | A long `spec.description` is used to smuggle a payload that exceeds the K8s 256 KiB metadata budget at apply time, breaking the entire bundle. | `spec.description` is capped at 4 KiB; truncation produces `azureclaw.azure.com/kagent-description-truncated: "true"` and a lossy warning that is gated by `--allow-lossy`. |
| **Repudiation** | Operator denies that a sandbox was migrated from a kagent Agent. | Every emitted resource carries `azureclaw.azure.com/migrated-from` and `azureclaw.azure.com/kagent-agent` provenance annotations. |
| **Information disclosure** | An env var with `valueFrom` (Secret reference) is silently materialised into a literal `extraEnv` value, potentially exposing the secret in the migrated YAML. | `valueFrom` env entries are **dropped** with a `warn`. A prior literal entry with the same name is also dropped (no stale value resurrection). |
| **Information disclosure** | An env var redefinition is silently coalesced, hiding which entry won. | Redefinitions emit a `warn` per shadowed entry. |
| **Information disclosure** | A description annotation may carry sensitive free text. | Pass-through is bounded (4 KiB) and the field is opt-in (kagent-controlled). |
| **Denial of service** | An attacker provides a kagent CR with thousands of `toolNames` to inflate ToolPolicy count and exhaust the cluster's CRD budget on apply. | Translator is in-memory only; cluster-side rate-limiting is a `kubectl apply` concern. We deduplicate identical tool names. We bound generated ToolPolicy names at 63 chars (DNS-1123 label). |
| **Denial of service** | `--out-dir` writes files; an attacker-supplied tool name like `../etc/passwd` walks out of the directory. | All filenames are derived from sanitised, DNS-1123-safe `metadata.name` values; any path separator from input is replaced. Existing files are refused (no overwrite) unless `--force`. |
| **Elevation of privilege** | A migrated `ClawSandbox` accidentally inherits an over-permissive `securityContext` from the input. | Pod / container `securityContext` from `SharedDeploymentSpec` is **not** projected (controller-managed). Lossy warn surfaced. |
| **Elevation of privilege** | `serviceAccountName` from the input grants the migrated agent unintended cluster privileges. | `serviceAccountName` is **dropped** with a warn (controller manages the SA). |
| **Elevation of privilege** | Wildcard egress domain (`*.example.com`) is silently honoured by AzureClaw with surprising semantics. | Detected and warned; emission is allowed but gated by `--allow-lossy`. |

## 3. STRIDE non-issues

- **Tampering of the translator binary**: out of scope — handled by signed
  release artefacts (Phase 3 cosign-on-admission).
- **Tampering of the input YAML in flight**: out of scope — the operator
  is the trust boundary.
- **Authentication / authorisation of the operator**: this CLI never talks
  to a cluster (no `kubectl` invocation, no kube context read, no token
  exchange). It is a pure file → file translator.

## 4. Existing implementation surveyed

Per slice rule §0.2 #7 ("No duplication, no dead code"):

| Existing seam | Path | Reused? | Rationale |
|---|---|---|---|
| `convert.ts::cleanMetadata` | `cli/src/commands/convert.ts:104-115` | Concept reused, not imported | The S9.2 helper strips a different field set; this slice needed strict server-managed-key drop (uid, resourceVersion, generation, creationTimestamp, managedFields, ownerReferences, finalizers, …) and KAGENT-specific annotation filtering. Re-implementing in `from_kagent.ts` keeps the CLI commands loosely coupled and avoids over-narrowing the S9.2 helper. |
| `convert.ts::envArrayToMap` | `cli/src/commands/convert.ts:233-272` | Concept reused, not imported | S9.2 maps Kubernetes `corev1.EnvVar[]` for forward / inverse `Sandbox` translation; S9.3 needs the same semantics for `SharedDeploymentSpec.env`. The pure-helper signatures differ (different `Warning` shape — S9.3 uses `{severity, path, message}` for richer CLI surface), so direct reuse would have introduced an internal `Warning` adapter type. Keeping each translator self-contained minimises the dependency graph between Phase 2 slices. |
| Commander subcommand registration | `cli/src/commands/migrate.ts:289-` | Reused — `from-kagent` is added as a sibling of `to-overlay`, `from-overlay`, `to-translate`, `to-observe`, `to-native` | Same `migrate` umbrella command. No new top-level command added. |
| `yaml.parseAllDocuments` + multi-doc rejection idiom | `cli/src/commands/convert.ts:71-85` | Re-applied | Same parser, same `d.contents !== null` non-empty filter, same exit-code grammar. |
| `ClawSandbox`, `InferencePolicy`, `ToolPolicy` CRD field shapes | `controller/src/crd.rs`, `controller/src/inference_policy.rs`, `controller/src/tool_policy.rs` | Targeted directly — zero divergence | The aspirational mapping table in `docs/sigs-agent-sandbox-compat.md` was *not* used here; we target the *actual* schema, just like S9.2 did for the upstream `Sandbox` translator. |
| Phase 2 CI scripts (`ci/no-stubs.sh`, `ci/no-custom-crypto.sh`, `ci/check-loc.sh`) | repo root | Reused, ran clean against `origin/dev` | No new stubs, no new crypto (sha256 hash for deterministic name suffixing only — `node:crypto` per existing convention; not security-sensitive). |
| `chalk`, `commander`, `yaml@2.6.0`, `vitest@3.x` | `cli/package.json` | Reused | No new runtime deps; no new dev deps. |

**No new module created in lieu of extending an existing seam.** The pure
translator file `cli/src/migrate/from_kagent.ts` is a *peer* of
`cli/src/commands/convert.ts`, not a parallel implementation: it targets
a different *input* shape (kagent `Agent`) and a different *output cardinality*
(multi-resource bundle vs. single-resource translate).

## 5. Aspirational mappings explicitly rejected

| Plan reference | What was rejected | Why |
|---|---|---|
| `plan.md:210` mentions emitting `ClawAgentIdentity` | We emit nothing of that kind. | `ClawAgentIdentity` is **Phase 4** per `docs/internal/internal-boundaries.md:28` and `docs/competitive.md:805`. The CRD has no schema in the cluster; emitting it would produce a manifest that fails admission. Plan line 210 is overridden by current repo reality per slice rule §0.2 #7. |
| Auto-emit `McpServer` for every kagent `Tool.mcpServer` | Not done. | A kagent `TypedReference` has only `(apiGroup, kind, name)`; the upstream MCP server URL, transport, JWKS, and OAuth client are not in the input. We surface the original ref via `azureclaw.azure.com/kagent-tool-ref` annotation and a hard warning that an equivalent AzureClaw `McpServer` must already exist. |
| Map `spec.declarative.modelConfig` → `InferencePolicy.spec.tokenBudget`/`modelPreference` | Not done. | kagent `ModelConfig` is a separate CRD; we never see its content. The emitted `InferencePolicy` carries only the model-config name as a provenance annotation. The user must hand-author `spec.inference.{provider,endpoint,model}` on the migrated `ClawSandbox` separately. |

## 6. Failure-mode matrix

| Failure mode | Exit code | Surface |
|---|---|---|
| Input is not a YAML mapping | 2 | `error: input is not a YAML mapping` |
| Input is empty | 2 | `error: input contains no YAML documents` |
| Input has > 1 YAML document | 2 | `error: input contains N YAML documents; from-kagent expects exactly one Agent` |
| Wrong `apiVersion` | 2 | `error: apiVersion '<v>' is not 'kagent.dev/v1alpha2'` |
| Wrong `kind` | 2 | `error: kind '<k>' is not 'Agent'` |
| Missing `metadata.name` | 2 | `error: metadata.name is required` |
| Unknown `spec.type` | 2 | `error: spec.type must be 'Declarative' or 'BYO' (got <x>)` |
| BYO without image | 2 | `error: spec.byo.deployment.image is required for BYO agents` |
| McpServer tool with missing `name` | 2 | `error: spec.declarative.tools[i].mcpServer.name is required` |
| Tool with missing `mcpServer` (type=McpServer) | 2 | `error: spec.declarative.tools[i].mcpServer is required for type=McpServer` |
| Tool of type=Agent with missing `agent.name` | 2 | `error: spec.declarative.tools[i].agent: 'name' is required for type=Agent` |
| Conflicting pre-existing `azureclaw.azure.com/sandbox` label | 2 | `error: metadata.labels['azureclaw.azure.com/sandbox'] already set to '<v>', conflicts with sandbox name '<n>'` |
| Invalid `--isolation` value | 2 | `error: --isolation must be one of standard\|enhanced\|confidential` |
| Translation lossy without `--allow-lossy` | 4 | `error: translation is lossy (N warnings). Pass --allow-lossy to waive.` |
| `--out-dir` file collision | 2 | `error: <path> already exists (use --force to overwrite)` |
| Two emitted resources collide on filename | 2 | `error: duplicate output filename '<f>' (kind+name collision)` |
| Successful translation | 0 | resources written to stdout / `--out-dir` |

## 7. CRD round-trip validation

The emitted resources have been validated by inspection against the
target CRD schemas:

- `ClawSandboxSpec` — `controller/src/crd.rs:28-87`. Every emitted field
  (`openclaw.image`, `openclaw.extraEnv`, `sandbox.isolation`,
  `networkPolicy.{defaultDeny, approvalRequired, allowedEndpoints}`,
  `governance.enabled`, `resources.{requests,limits}`) maps to a
  documented field with the correct type. No emit references a field
  that does not exist on the CRD.
- `InferencePolicySpec` — `controller/src/inference_policy.rs:77-98`.
  We emit only `appliesTo.sandboxName`. We deliberately do **not**
  populate `tokenBudget`, `contentSafety`, or `modelPreference` —
  doing so would require ModelConfig content we don't have.
- `ToolPolicySpec` — `controller/src/tool_policy.rs:48-65`. We emit
  `appliesTo.{tool, mcpServer, sandboxMatchLabels}` and conditionally
  `approval.mode`. We do **not** emit `commerce` or `rateLimit` —
  kagent has no equivalent fields.

## 8. CI surface

| Gate | Result | Notes |
|---|---|---|
| `cd cli && npx tsc --noEmit` | ✓ | clean |
| `cd cli && npm run lint` (oxlint) | ✓ | clean (0 warnings, 0 errors after fixing 2 useless-fallback-in-spread warnings on `meta.labels`/`meta.annotations`) |
| `cd cli && npm test` (vitest) | ✓ | 435/437 (2 pre-existing skipped); +53 new cases |
| `BASE_REF=origin/dev ci/no-stubs.sh` | ✓ | clean |
| `BASE_REF=origin/dev ci/no-custom-crypto.sh` | ✓ | clean (sha256 hash via `node:crypto` for deterministic name suffixing — pre-existing convention; not a new crypto primitive) |
| `BASE_REF=origin/dev ci/check-loc.sh` | ✓ | translator 721 LOC; tests 714 LOC |
| Manual end-to-end smoke (`migrate from-kagent` against a Declarative agent with tools + network) | ✓ | exits 0 with `--allow-lossy --image …`; exits 4 without `--allow-lossy`; YAML round-trip is well-formed multi-doc |

## 9. Test coverage map

53 cases in `cli/src/migrate/from_kagent.test.ts`:

| Group | Cases | Notes |
|---|---|---|
| `sanitizeDnsName` | 5 | empty fallback, DNS char map, dash collapse, leading/trailing trim, alnum passthrough |
| `hashSuffix` | 3 | determinism, distinct tuples, hex-of-length-6 |
| `generateToolPolicyName` | 2 | ≤ 63 chars, hash distinguishes after-sanitize collisions |
| `cleanMetadata` | 3 | server-managed strip, kubectl-annotation filter, non-object input |
| `envArrayToMap` | 3 | last-wins-warn, valueFrom drop+warn, prior-literal-purge |
| `projectDescription` | 2 | passthrough, truncation+warn |
| `translate` input gating | 5 | apiVersion / kind / name / type / BYO image |
| ClawSandbox basics | 6 | label/annotation injection, isolation override, sandbox-label conflict, label preserve, ns mismatch warn, ns match no-warn |
| Declarative runnability | 4 | non-runnable warn, --image escape hatch, InferencePolicy emit-when-modelConfig, no-emit-when-absent |
| Tools | 9 | per-(McpServer,tool) fan-out, approval mapping, wildcard emit, missing-name reject, agent-without-name reject, agent-as-tool drop, deterministic order, dedupe, headersFrom + allowedHeaders warns |
| Lossy fields | 3 | spec.skills, spec.allowedNamespaces, all declarative-only fields |
| Lossy deployment fields (BYO) | 1 (×11 sub-paths) | replicas, tolerations, affinity, nodeSelector, imagePullPolicy, imagePullSecrets, volumes, volumeMounts, securityContext, podSecurityContext, serviceAccountName |
| Networking | 2 | passthrough, wildcard warn |
| Bundle ordering & shape | 3 | order, BYO clean happy-path, env preservation |
| Description | 2 | small passthrough, large truncation |

## 10. Operational impact

- **No controller change.** The output of this slice is plain YAML; the
  controller has no awareness of the migration.
- **No router change.** Same.
- **No new CRD.** Same.
- **No admission policy change.** Same.
- **No image rebuild required.** This is CLI-only; users get the new
  subcommand by upgrading the `@azure/azureclaw` npm package.

## 11. Open follow-ups (out of scope for this slice)

- **`McpServer` translator** (Phase 2 S1 follow-on). When the operator
  has the upstream MCP server URL out-of-band, an `azureclaw mcp import`
  flow could emit an AzureClaw `McpServer` from a kagent `RemoteMCPServer`
  CR. Tracked under §15.2 #8.
- **`InferencePolicy` enforcement from `ModelConfig`** (Phase 2 S4
  follow-on). Reading a kagent `ModelConfig` CR + provider creds and
  emitting a fully-wired AzureClaw `InferencePolicy.spec.modelPreference`
  block. Requires a separate `azureclaw migrate import-modelconfig` flow.
- **`ClawAgentIdentity` mapping** (Phase 4). Once `ClawAgentIdentity`
  schema lands, kagent `serviceAccountName` and `serviceAccountConfig`
  could feed a `ClawAgentIdentity` instead of being dropped.

## 12. Sign-off

| Role | Reviewer | Decision |
|---|---|---|
| Slice author | Copilot agent (this PR) | implemented, tests + CI green |
| Pre-implementation rubber-duck critique | rubber-duck agent | 14 findings adopted, 0 deferred |
| Phase 2 invariants check | this slice | LOC budget ✓, no Rust change ✓, no new CRD ✓, no aspirational emit ✓, audit doc ✓, CHANGELOG ✓, CI scripts ✓ |
| Final reviewer | _to be assigned on PR_ | pending |


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
