# CRD Schema-Drift Audit (post-S10/S13)

Scope: every code path in the repo that constructs, parses, or validates an
`azureclaw.azure.com/v1alpha1` CR. Canonical schema lives in
`controller/src/crd.rs`, sibling files (`inference_policy.rs`, `tool_policy.rs`,
`a2a_agent.rs`, `claw_eval.rs`, `claw_memory.rs`, `mcp_server.rs`,
`pairing.rs`, `trust_graph.rs`), and the Helm CRD YAML in
`deploy/helm/azureclaw/templates/crd*.yaml`.

---

## 1. Executive summary

1. **🔴 BLOCKER — Cross-cluster offload is broken on launch.**
   `controller/src/mesh_peer/offload.rs:104-150` still emits the
   **pre-S10/S13** `ClawSandbox` shape: it sets the removed `spec.inference`
   block, sets `spec.governance.toolPolicy: "offload"` (string profile, not
   `toolPolicyRef`), and never sets the now-required `spec.inferenceRef`.
   Identical class of bug to the spawn regression that already burned us;
   admission rejects with `Required value: spec.inferenceRef`.
2. **🔴 BLOCKER — `azureclaw handoff <name> --to cloud` is broken.**
   `cli/src/commands/handoff.ts:401-422` posts a CRD with the **wrong API
   group** (`azureclaw.io/v1alpha1` — there is no such group) **and** the
   pre-S10/S13 shape (`spec.model`, `spec.handoff` top-level,
   `spec.governance.toolPolicy: "default"`, no `runtime`, no `inferenceRef`).
   This is the operator-mode CLI handoff path; it cannot succeed against any
   cluster running the post-S10/S13 controller.
3. **🟡 LATENT — `azureclaw convert --to clawsandbox` produces invalid output.**
   `cli/src/commands/convert.ts:338-352` emits a `ClawSandbox` with
   `runtime` + `sandbox` only — no `inferenceRef`. Helm CRD line 21 declares
   `inferenceRef` `required`, so the converted manifest cannot apply
   without a manual edit. Documented as lossy (warns about dropped governance
   etc.) but the missing required field is not flagged.
4. **🔴 CI gap — no test wires *any* hand-built CRD payload through the
   typed Rust struct.** Every CRD-emitting path in the repo (sub-agent spawn,
   offload, CLI add/up/handoff/convert, tests/compat fixtures) uses raw
   `serde_json::json!{...}` or raw TS object literals. No round-trip test
   does `serde_json::from_value::<ClawSandbox>(payload)` to fail-loud at
   `cargo test` time. Both this audit's findings and the original S10/S13
   regression would have been caught instantly by such a check.
5. **🟡 CI gap — `helm_drift` covers every CRD *except* `ClawSandbox`.**
   `controller/src/helm_drift.rs:168-273` has drift tests for `mcpserver`,
   `toolpolicy`, `a2aagent`, `inferencepolicy`, `clawmemory`, `claweval`,
   `trustgraph` — but the central `ClawSandbox` CRD (handwritten
   `crd.yaml`, 798 LOC) is never compared against the Rust-derived schema.
   That is precisely where the recent breakage originated.

---

## 2. Inventory — every CRD builder/consumer

| # | File | Lines | Direction | Kind | Mechanism | Launch-critical? | Risk |
|---|------|-------|-----------|------|-----------|------------------|------|
| 1 | `inference-router/src/spawn/mod.rs` | 176-290 | write | ClawSandbox | raw `serde_json::json!` | **YES** (sub-agent spawn) | ✅ Post-fix; matches schema. **Untyped** — no compile-time guard |
| 2 | `inference-router/src/spawn/mod.rs` | 614-705 | read | ClawSandbox | `obj.data.get("spec")…` | **YES** (handoff snapshot) | ✅ Reads only camelCase keys present in schema |
| 3 | `controller/src/mesh_peer/offload.rs` | 104-200 | write | ClawSandbox | raw `serde_json::json!` | **YES** (offload spawn) | 🔴 **BLOCKER** — pre-S10/S13 shape (see Finding F1) |
| 4 | `controller/src/mesh_peer/offload.rs` | 568-580, 602+ | patch | ClawSandbox | annotation/finalizer patches | YES | ✅ touches `metadata` only |
| 5 | `cli/src/commands/up/sandbox_bringup.ts` | 351-387 | write | ClawSandbox | TS object literal | **YES** (`azureclaw up`) | ✅ Post-S10/S13 shape correct |
| 6 | `cli/src/commands/add.ts` | 140-194 | write | ClawSandbox | TS object literal | **YES** (`azureclaw add`) | ✅ Post-S10/S13 shape correct |
| 7 | `cli/src/commands/handoff.ts` | 401-422 | write | ClawSandbox | TS object literal (JSON.stringify) | **YES** (`azureclaw handoff --to cloud`) | 🔴 **BLOCKER** — wrong API group + pre-S10/S13 shape (Finding F2) |
| 8 | `cli/src/commands/convert.ts` | 338-352 (upstream→claw) | write | ClawSandbox | TS object literal | partly (operator import path) | 🟡 **LATENT** — missing required `inferenceRef` (Finding F3) |
| 9 | `cli/src/commands/convert.ts` | 515-525 (overlay) | write | ClawSandbox | TS object literal | partly | 🟡 documented skeleton; emits `runtime`/`sandbox`/`inferenceRef`-less spec |
| 10 | `cli/src/commands/pair.ts` | 115-130 | write | ClawPairing | TS object literal | YES (`azureclaw pair create`) | ✅ matches `controller/src/pairing.rs` |
| 11 | `cli/src/commands/inferencepolicy.ts` | 54-119 | write | InferencePolicy | TS object literal via `buildInferencePolicySpecFromFlags` | YES | ✅ matches `controller/src/inference_policy.rs` |
| 12 | `cli/src/commands/toolpolicy.ts` | (build helpers) | write | ToolPolicy | TS object literal | YES | ✅ matches `controller/src/tool_policy.rs` |
| 13 | `cli/src/commands/a2a.ts` | 154-182 | write | A2AAgent | TS object literal via `buildA2aAgentSpecFromFlags` | YES | ✅ matches `controller/src/a2a_agent.rs` (note: `policyRefs.toolPolicy` here is a *string*, intentionally — different shape from `ClawSandbox.governance.toolPolicyRef`) |
| 14 | `cli/src/commands/mcp.ts` / `cli/src/commands/eval.ts` | (build helpers) | write | McpServer / ClawEval | TS object literal | YES | ✅ matches Rust types |
| 15 | `cli/src/refs.ts` | 37-99 | write | InferencePolicy + ToolPolicy | TS object literal (helpers used by `up` / `add`) | **YES** | ✅ correct shape |
| 16 | `cli/src/commands/policy.ts` | 36-41 | patch | ClawSandbox | merge-patch on `spec.networkPolicy.allowedEndpoints` | YES | ✅ matches `NetworkPolicyConfig` |
| 17 | `cli/src/commands/migrate.ts` | 115-120, 130+ | read+write | ClawSandbox | `spec.upstreamCompatibility` patch | secondary | ✅ matches `UpstreamCompatibilityConfig` |
| 18 | `controller/src/reconciler/**` | various | read | ClawSandbox | typed `Arc<ClawSandbox>` from kube-rs | **YES** | ✅ typed — no risk |
| 19 | `controller/src/{tool_policy,a2a_agent,inference_policy,mcp_server,claw_memory,claw_eval,trust_graph}_reconciler.rs` | finalizer/status patches | patch | various | raw `json!` but only `metadata.finalizers` / `status` | YES | ✅ touches non-spec fields only |
| 20 | `tests/compat/fixtures/null-provider-{prod-denied,devonly-ok}.yaml` | full file | write | ClawSandbox | hand-written YAML | NO (admission test) | 🟡 uses `spec.agt`, `spec.sandbox.isolation: strict` — neither in `crd.rs`; intentionally crafted invalid manifest, but illustrates absence of fixture-vs-schema check |
| 21 | `examples/**/clawsandbox.yaml` (10 files) | full file | write | ClawSandbox | hand-written YAML | docs/quickstart | ✅ all on post-S10/S13 shape (verified by spot-check) |
| 22 | `inference-router/src/spawn/mod.rs` | 401-450 | write | core/v1 Secret | raw `json!` (not a CRD) | YES | not in audit scope |

**Counts:** 6 distinct paths produce a `ClawSandbox` payload from scratch
(spawn, offload, up, add, handoff, convert). Of these, **3 use raw
`serde_json::json!` / TS object literals with no typed cross-check** (spawn,
offload, handoff). The other 3 are TS but at least covered by `add.test.ts` /
`convert.test.ts` (rubber-stamp, see §4).

---

## 3. Findings

### F1 🔴 BLOCKER — `mesh_peer/offload.rs` emits pre-S10/S13 ClawSandbox

**File:** `controller/src/mesh_peer/offload.rs:104-150`

**Current shape (offending fragments):**
```rust
let spec = json!({
    "runtime": { "kind": "OpenClaw", "openclaw": { … } },
    "sandbox": { … },
    "inference": {                                      // ← REMOVED in S13
        "provider": "azure-ai-foundry",
        "model": model,
        "contentSafety": true,
        "promptShields": true,
        "tokenBudget": { "daily": …, "perRequest": 32000 }
    },
    "networkPolicy": { … },
    "governance": {
        "enabled": true,
        "toolPolicy": "offload",                        // ← string profile, REMOVED in S13
        "trustThreshold": 900,
        "trustedPeers": format!("offload-parent:{from_amid}"),
        "registryMode": "global"
    }
});
```

**Canonical (`controller/src/crd.rs:32-132` + `855-889`):**
- `spec.inferenceRef: LocalObjectRef` is **required** (`crd.rs:61`,
  `crd.yaml:21` `required: ["runtime", "sandbox", "inferenceRef"]`).
- `spec.inference` does not exist on the schema.
- `GovernanceConfig.tool_policy_ref: LocalObjectRef` is the only tool-policy
  field (`crd.rs:866`); `toolPolicy: <string>` does not exist.

**Failure mode:** `kube` admission returns
`Required value: spec.inferenceRef`; the offload sandbox never spawns; the
`OffloadStatus` reply to the requesting peer carries the create error.

**Fix sketch:** before the `json!` block, ensure an `InferencePolicy` CR
named e.g. `offload-{request_id}-inference` exists in `azureclaw-system`
(or reuse a cluster-default `pairing-default-inference` CR), drop the inline
`inference` block, and replace `governance.toolPolicy` with
`toolPolicyRef: { name: "offload-default-toolpolicy" }`. Same surgery the
spawn path already received.

### F2 🔴 BLOCKER — `cli/src/commands/handoff.ts` posts a non-existent API group

**File:** `cli/src/commands/handoff.ts:401-422`

**Current shape:**
```ts
const crdManifest = JSON.stringify({
  apiVersion: "azureclaw.io/v1alpha1",       // ← wrong group; canonical is azureclaw.azure.com
  kind: "ClawSandbox",
  metadata: { name: targetName, namespace: "azureclaw-system" },
  spec: {
    model: process.env.DEFAULT_MODEL || "gpt-5.4",       // ← not in schema (top-level)
    handoff: { mode: "restore", predecessor: name },     // ← not in schema (top-level)
    networkPolicy: { defaultDeny: true, approvalRequired: true, learnEgress: sourceLearnEgress },
    sandbox:    { isolation: sourceIsolation },
    governance: { enabled: true, toolPolicy: "default", trustThreshold: sourceTrustThreshold },
  },
});
```

**Canonical:** `apiVersion: "azureclaw.azure.com/v1alpha1"`; required
`spec.runtime` + `spec.inferenceRef`; `spec.governance.toolPolicyRef.name`.
There is no `spec.model` or `spec.handoff` at the top level — handoff
metadata is annotation/label-driven (see `inference-router/src/spawn/mod.rs:244-264`
for the canonical convention).

**Failure mode:** `kubectl apply` fails before admission with
`error: unable to recognize ".": no matches for kind "ClawSandbox" in version
"azureclaw.io/v1alpha1"`. `azureclaw handoff <name> --to cloud` reports
"Failed to create target sandbox CRD" and aborts. Because of the wrong group,
the rest of the schema delta is masked — the operator only sees the API-group
error first.

**Fix sketch:** mirror `spawn/mod.rs:163-287`:
- `apiVersion: "azureclaw.azure.com/v1alpha1"`
- `spec.runtime: { kind: "OpenClaw", openclaw: {} }`
- `spec.inferenceRef: { name: "<target>-inference" }` (and create that
  `InferencePolicy` CR alongside, like `up/sandbox_bringup.ts:338-345`)
- `spec.governance.toolPolicyRef: { name: "<target>-toolpolicy" }`
- handoff metadata via labels/annotations
  (`azureclaw.azure.com/spawned-by=handoff`,
  `azureclaw.azure.com/predecessor=<name>`).

### F3 🟡 LATENT — `convert.ts` upstream→clawsandbox output omits required `inferenceRef`

**File:** `cli/src/commands/convert.ts:338-352`

The upstream Sandbox → ClawSandbox conversion produces:
```ts
spec: {
  runtime:  { kind: "OpenClaw", openclaw },
  sandbox:  sandboxFields,
  // resources optionally added below
}
```
No `inferenceRef`. The Helm CRD declares it required
(`crd.yaml:21` `required: ["runtime", "sandbox", "inferenceRef"]`).
The companion warnings list at `convert.ts:218-226` enumerates dropped
**input** fields but never points out the missing **required output** field.

Severity: latent rather than blocker because `convert` writes to stdout —
admission only fires when the operator pipes to `kubectl apply`. But that
*is* the documented happy path, so the next operator who runs
`azureclaw convert --to clawsandbox … | kubectl apply -f -` gets a confusing
admission error.

**Fix sketch:** require `--inference-ref <name>` (or auto-mint a sibling
`InferencePolicy` CR like `add.ts` does) and stamp `spec.inferenceRef.name`.
Ditto for `governance.toolPolicyRef` if the operator wants AGT enforcement.

### F4 🟡 LATENT — `convert.ts` overlay skeleton + warning text mention legacy `spec.inference`

**File:** `cli/src/commands/convert.ts:515-532`

The overlay output is documented as a "skeleton — edit before applying" and
lacks `runtime`/`sandbox`/`inferenceRef`, which is acceptable. However the
warning text on line 530-531 reads:

> "overlay skeleton has no governance fields; add spec.governance / **spec.inference** / spec.a2a / spec.agent before applying"

`spec.inference` is the removed S13 field. The hint should read
`spec.inferenceRef`. Operators following this hint will reproduce the
S10/S13 regression by hand.

### F5 🟡 LATENT — `inference-router/src/spawn/mod.rs:480-498` reads `spec.governance.enabled` but ignores stale `toolPolicy` legacy key

**File:** `inference-router/src/spawn/mod.rs:644-678`

The list/snapshot reader only inspects `spec.governance.{enabled,trustThreshold,trustedPeers}`. That is correct — but if any *other* writer (e.g. F1, F2) ever lands a CR with the legacy `spec.governance.toolPolicy` string, this reader silently drops it. Combined with the controller's pruning behaviour, the legacy key is invisible end-to-end. Listed for completeness; no read regression here, but it's why F1/F2 escaped detection — a stale write is silently consumed by every downstream reader.

### F6 🟡 LATENT — `tests/compat/fixtures/null-provider-*.yaml` carries fields not in the Rust schema

**Files:** `tests/compat/fixtures/null-provider-prod-denied.yaml:50-55`,
`tests/compat/fixtures/null-provider-devonly-ok.yaml:40-…`

Use `spec.agt.providers.*` and `spec.sandbox.isolation: strict`.
`spec.agt` does not exist in `controller/src/crd.rs`; `isolation` doc-string
says `standard | enhanced | confidential` (no enum validation).
`deploy/helm/azureclaw/templates/admission-null-provider.yaml:47-52` references
`object.spec.agt.providers.*` via CEL — i.e. the VAP guards a field that is
pruned by the structural CRD before admission CEL even runs.

These fixtures are **intentionally** invalid (the comment block explains it),
but the situation is fragile: pruning vs. CEL-on-pruned-fields means the VAP
matches *nothing* in the post-pruning object. Worth confirming the fixture
test still asserts what it claims to assert.

### F7 🟢 CLEANUP — TS shape mismatch between two CRDs is correct but easy to mis-author

`A2AAgent.spec.policyRefs.toolPolicy` is a **string**
(`controller/src/a2a_agent.rs:206-211`) whereas
`ClawSandbox.spec.governance.toolPolicyRef` is `{ name: string }`
(`controller/src/crd.rs:866`). Both are correct against their own schemas,
but the two near-identical names make it easy to copy/paste-author a
`{ toolPolicy: { name: … } }` object onto `A2AAgent` (or a bare string onto
`ClawSandbox`) — exactly the class of mistake S13 was supposed to eliminate
by going to refs everywhere. Consider renaming the A2AAgent field to
`policyRefs.toolPolicyRef.name` in a future minor schema bump for symmetry.

---

## 4. CI gap analysis

### 4.1 No round-trip test from raw payload to typed Rust struct

The two regressions in the prompt (and F1, F2 above) all share the same
shape: a raw `json!{…}` / TS literal builds a payload that *looks* right but
diverges from `ClawSandboxSpec`. The single most powerful guard would be one
line of test code:

```rust
let parsed: ClawSandboxSpec = serde_json::from_value(crd["spec"].clone())
    .expect("emitted spec must deserialize as the canonical Rust type");
```

Today this guard exists in **zero** locations:
- `inference-router/src/spawn/mod.rs:718-784` has unit tests, but they only
  exercise `SpawnRequest` (the *input* DTO), never the constructed CRD output.
- `cli/src/commands/add.test.ts` re-implements the exact `buildSandboxManifest`
  helper that production uses (lines 55-167) and asserts against itself — a
  textbook rubber-stamp. The S13 regression would still pass these tests
  because both production and test author the same bad shape.
- `cli/src/commands/convert.test.ts` similarly tests its own builders.
- No vitest spec validates a TS-built object against a generated JSON Schema
  derived from the Rust types.

### 4.2 `helm_drift` covers six CRDs but not `ClawSandbox`

`controller/src/helm_drift.rs:168-273` has matched-pair tests for every
sibling CRD (`mcpserver`, `toolpolicy`, `a2aagent`, `inferencepolicy`,
`clawmemory`, `claweval`, `trustgraph`). The matching `*_crd()` helpers
exist in `crd_validations.rs:144-491`. **There is no `claw_sandbox_crd()`
helper and no drift test against `crd.yaml`.** The chart's hand-written
798-line `crd.yaml` can drift from `crd.rs` indefinitely without CI noticing.

The S10/S13 regression first manifested as Helm CRD requiring `inferenceRef`
while the spawn JSON did not provide it — i.e. Helm and Rust were *in sync*,
but the consumer code was not. So drift testing alone is not sufficient
(see 4.1), but its absence on the largest CRD is itself a hole.

### 4.3 No admission-validation test against any built payload

`tests/e2e/run.sh` brings up a Kind cluster and runs end-to-end traffic, but
its grep results show nothing exercising sub-agent spawn or offload via the
spawn endpoint (no `/sandbox/spawn`, `/agt/handoff/init`, or `--to cloud`
hits). The existing harness covers the controller + router but not the
CRD-emitting client paths (CLI handoff, router spawn, controller offload).

### 4.4 `additionalProperties: false` is not set on `crd.yaml`

I found no `additionalProperties: false` in `deploy/helm/azureclaw/templates/crd.yaml`.
Structural CRDs prune unknown fields by default — so legacy keys
(`spec.openclaw`, `spec.inference`, `spec.governance.toolPolicy`) are
**silently dropped**, not rejected. The user-reported error
`unknown field "spec.governance.toolPolicy"` was almost certainly produced
by `kubectl apply`'s client-side OpenAPI strictness (`--validate=true` is
the default in modern kubectl). That's defense-in-depth at best — the
moment a writer uses the kube-rs `DynamicObject` create path
(`spawn/mod.rs:289`, `offload.rs:200`), client-side strictness is bypassed
and pruning silently succeeds. **F1 will spawn an offload sandbox whose
`spec.governance.toolPolicy` is silently dropped** and whose missing
`spec.inferenceRef` is the only error the controller surfaces.

### 4.5 Summary

| Guard | Exists? | Catches the two recent regressions? |
|-------|---------|-------------------------------------|
| `cargo fmt`/`clippy` | ✅ | No — `json!` macro is well-formed |
| `cargo test --all` (Rust unit/integration) | ✅ | No — no test parses spawn output as `ClawSandboxSpec` |
| CLI `npm test` (vitest) | ✅ | No — `add.test.ts` rubber-stamps the same builder |
| `helm_drift` Rust↔YAML | ✅ for 6/7 CRDs | No — and missing entirely for `ClawSandbox` |
| `tests/e2e/run.sh` (Kind) | ✅ | No — never hits `/sandbox/spawn` or `handoff --to cloud` |
| Cluster admission with `additionalProperties: false` | ❌ | Partially (only `required` fields surface) |

---

## 5. Recommended CI guard (minimum-effort, prioritized)

### Priority 1 — Round-trip every CRD-emitting site through the typed struct

**Effort:** ~30 LOC across two files. Catches both reported regressions and
F1, F2, F3, F4 immediately.

**Location:** `inference-router/src/spawn/mod.rs` (new test in the existing
`#[cfg(test)] mod tests` block at line 718) and a new
`controller/src/mesh_peer/offload_tests.rs`.

Pseudocode (Rust side):
```rust
// inference-router/src/spawn/mod.rs — new test
#[test]
fn spawn_payload_matches_canonical_clawsandbox_spec() {
    use azureclaw_controller::crd::ClawSandboxSpec;     // re-export from controller crate
    let req = SpawnRequest {
        agent_id: "child".into(),
        model: Some("gpt-4.1".into()),
        governance: true,
        trust_threshold: Some(500),
        learn_egress: false,
        isolation: Some("enhanced".into()),
        token_budget_daily: None,
        token_budget_per_request: None,
        trusted_peers: None,
        handoff: None,
    };
    // Pure builder extracted from create_sandbox() — no kube/network
    let crd = build_clawsandbox_crd_value("parent", &req);
    let spec = crd.get("spec").cloned().expect("spec");
    let _: ClawSandboxSpec = serde_json::from_value(spec)
        .expect("spawn-emitted spec must round-trip ClawSandboxSpec");
    // Spot-check critical fields
    assert_eq!(crd["apiVersion"], "azureclaw.azure.com/v1alpha1");
    assert!(crd["spec"]["inferenceRef"]["name"].is_string());
    assert!(crd["spec"]["governance"]["toolPolicyRef"]["name"].is_string());
}
```

Refactor needed: extract the `let mut spec = json!({...}); ... let crd = json!({...})`
block from `create_sandbox` (mod.rs:163-287) into a pure
`fn build_clawsandbox_crd_value(parent: &str, req: &SpawnRequest) -> serde_json::Value`.
Same refactor for `mesh_peer/offload.rs:96-200` (separate `build_offload_clawsandbox_value()`)
— that test will fail today and surface F1.

For controller↔inference-router: either (a) make the controller crate
publish a `crd` cargo feature that re-exports `ClawSandboxSpec`, or (b)
move `crd.rs` into a tiny `azureclaw-crd-types` workspace crate that both
the controller and the inference-router depend on. (b) is the cleanest fit
with the workspace pattern documented in
`<project_instructions>` and avoids a circular dep.

### Priority 2 — Add `claw_sandbox_crd()` + `helm_drift` test for ClawSandbox

**Effort:** add one helper in `crd_validations.rs` modeled on
`mcp_server_crd()` (l. 144), and one test in `helm_drift.rs` modeled on
`helm_crd_matches_rust_schema()` (l. 168). ~40 LOC total.

```rust
// controller/src/crd_validations.rs — new helper
pub fn claw_sandbox_crd() -> CustomResourceDefinition {
    let mut crd = ClawSandbox::crd();
    apply_common_metadata(&mut crd);              // labels/annotations stripping
    add_x_kubernetes_validations(&mut crd, …);    // CEL rules from crd.yaml:22-46
    crd
}

// controller/src/helm_drift.rs — new test
const CLAWSANDBOX_HELM_CRD_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/azureclaw/templates/crd.yaml");

#[test]
fn helm_clawsandbox_crd_matches_rust_schema() {
    let want = claw_sandbox_crd();
    let have = parse_helm_yaml(CLAWSANDBOX_HELM_CRD_PATH, "clawsandboxes.azureclaw.azure.com");
    assert_eq!(strip_irrelevant(&want), strip_irrelevant(&have));
}
```

This catches one direction of drift (Helm vs Rust) but not consumer drift
(spawn vs Rust) — Priority 1 still required.

### Priority 3 — Generate TS types from JsonSchema; consume in TS builders

**Effort:** medium. Highest value-per-LOC for catching CLI regressions
(handoff.ts F2 would be a TS compile error).

Steps:
1. Add a `cargo` binary `controller/src/bin/dump_jsonschema.rs` that walks
   the `kube::CustomResource` derive of every CRD and emits OpenAPI v3
   JSON Schema to `cli/src/generated/clawsandbox.schema.json` etc.
2. Add `npm run gen-types` (using `json-schema-to-typescript` already in
   use elsewhere) to compile those into `cli/src/generated/types.ts`.
3. In `up/sandbox_bringup.ts`, `add.ts`, `handoff.ts`, `convert.ts` annotate
   the literal: `const sandboxManifest: ClawSandboxManifest = { … }`. TS
   strict mode then catches `apiVersion: "azureclaw.io/…"` (literal-type
   mismatch), missing `inferenceRef`, `toolPolicy` instead of `toolPolicyRef`,
   etc. at `npm run typecheck` time.
4. Wire into `.github/workflows/ci.yml` as part of the existing `cli-build`
   job; failing `gen-types` (drift between Rust schema and committed TS)
   becomes a hard CI failure.

### Priority 4 — Set `additionalProperties: false` on every block in `crd.yaml`

**Effort:** YAML editing only, but high blast radius (any tolerated typo
becomes a hard reject). Recommend doing this **after** P1+P3 land, so the
client side is clean before the server starts rejecting.

### Priority 5 — Tighten kind harness

**Effort:** small. Add to `tests/e2e/run.sh`:
- After `azureclaw add`/`up`, exec into the sandbox pod and call
  `POST /sandbox/spawn` against the inference-router; assert the resulting
  `ClawSandbox` exists and has `spec.runtime.kind=OpenClaw`,
  `spec.inferenceRef.name`, `spec.governance.toolPolicyRef.name`.
- Exercise `azureclaw handoff <name> --to cloud` against the e2e cluster
  with a stub destination.

This is end-to-end coverage; P1 is the unit-level guard that gives instant
PR signal.

### What would have caught each historical regression?

| Regression | P1 (round-trip) | P2 (helm-drift) | P3 (TS types) | P4 (additionalProperties) |
|------------|-----------------|-----------------|---------------|---------------------------|
| spawn emitted `spec.openclaw` (pre-S10) | ✅ | ❌ | n/a (Rust) | ✅ (post-P4) |
| spawn emitted `spec.inference` (pre-S13) | ✅ | ❌ | n/a (Rust) | ✅ (post-P4) |
| spawn emitted `governance.toolPolicy` (pre-S13) | ✅ | ❌ | n/a (Rust) | ✅ (post-P4) |
| F1 offload (this audit) | ✅ | ❌ | n/a (Rust) | partial — only `inferenceRef` missing surfaces |
| F2 handoff.ts (this audit) | n/a (TS) | ❌ | ✅ | ✅ (post-P4) |
| F3 convert.ts (this audit) | n/a (TS) | ❌ | ✅ | ✅ |

P1 is the cheapest guard with the highest hit-rate against the actual
incident class. **Recommend landing P1 first, P3 second, P2 in parallel.**
