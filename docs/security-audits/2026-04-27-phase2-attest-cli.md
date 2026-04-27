# Phase 2 — `phase2-attest-cli` security audit (2026-04-27)

## §0 Reuse map (§0.2 #11)

- **Spec-hash recipe:** canonical JSON (recursive key-sort, no
  whitespace) + SHA-256, prefix `sha256:`. Matches the recipe used by
  `controller/src/tool_policy_compile.rs::version_hash` (S2),
  `controller/src/a2a_agent_compile.rs::version_hash` (S3),
  `controller/src/inference_policy_compile.rs::version_hash` (S4),
  `controller/src/claw_memory.rs::version_hash` (S5),
  `controller/src/claw_eval.rs::version_hash` (S6). CLI re-implements
  the recipe in TS using `node:crypto.createHash("sha256")` — no
  shared TS↔Rust hashing crate exists; determinism is asserted
  directly in `attest.test.ts` so the two sides cannot drift without
  a test breaking.
- **Status surfaces consumed:** every per-CRD `status.versionHash` +
  `status.bindingConfigMap` field shipped by S2/S3/S4/S5/S6. No CRD
  schema change required.
- **`kubectl` shell-out pattern:** mirrors `cli/src/commands/list.ts`
  + `cli/src/commands/status.ts` (execa `kubectl get … -o json`,
  parse, ignore stderr). No new transport.
- **Commander.js + chalk patterns:** mirror `cli/src/commands/a2a.ts`.
- **Test pattern:** `__test` named export + vitest, mirroring
  `cli/src/commands/convert.test.ts` (Phase 0).

No new module created where an existing one would do; no second JWS
verifier, no second hashing helper, no second informer, no second
SSA writer. The command is read-only from kubectl's perspective.

## §1 AGT boundary

Read surface only. AGT AuditLogger receipt IDs and signed reconcile
audit chain emission are **deferred to Phase 3** — the CLI prints
`(Phase 3)` for both fields today. No AGT change in this slice.

## §2 STRIDE

| Threat | Mitigation |
|---|---|
| Spoofing — fabricated attestation | Output is purely a function of `kubectl get` results executed under the caller's kubeconfig. No client-side state. The deterministic spec-hash recipe (canonical JSON + SHA-256) means an attacker who can write to the cluster can change the hash, but cannot forge a hash that matches a different spec. Phase 3 will add a controller-emitted signature to lift this to non-repudiation. |
| Tampering — modified attestation in transit | The CLI emits attestations to stdout for the caller's eyes; there is no transport. JSON envelope is versioned (`apiVersion: "azureclaw.azure.com/v1alpha1-attest"`) so a future `verify` command can detect schema changes. |
| Repudiation | Phase 3 lands signed receipts. Phase 2 surfaces `(Phase 3)` placeholders in both human + JSON output, making it explicit that the current attestation is unsigned. |
| Information disclosure | All fields read are gated by the caller's kubeconfig RBAC. No secret material printed (no token, no key, no signing material). The SSA field-owner map prints manager names + counts only — never the field contents. |
| Denial of service | Command shells out at most `1 + N` `kubectl get` calls (1 for the sandbox CR, N ≤ 4 for referenced policies + 1 deployment annotation). Bounded. No watch, no informer, no daemon. |
| Elevation of privilege | Read-only. No `kubectl patch`, `apply`, `create`, `delete`, `exec`, or `port-forward`. |

## §3 Out of scope

- Signed reconcile audit chain emission (Phase 3).
- AGT AuditLogger receipt-ID retrieval (Phase 3).
- `azureclaw.azure.com/last-trace-id` annotation **writing** by the
  controller (Phase 3) — the lookup scaffolding is in place.
- `kubectl claw verify <attestation.json>` companion command — the
  versioned JSON envelope makes this trivially additive when Phase 3
  emission lands.
- Offline / no-kubectl mode — current implementation requires a
  reachable cluster context; acceptable for an operator CLI.

## §4 Implementation surface

- `cli/src/commands/attest.ts` (~350 LOC, well under any cap):
  - `canonicalJson(value)` — recursive key-sort canonicalisation.
  - `specHash(spec)` — `sha256:` + hex digest over canonical JSON.
  - `summariseFieldOwners(managedFields)` — counts SSA fieldsV1
    leaves per manager; sorts alphabetically.
  - `extractPolicyRefs(spec)` — handles four ref shapes
    (`toolPolicyRef`, `inferencePolicyRef`, `a2aAgentRef`, legacy
    `governance.toolPolicy.ref`).
  - `buildReport(opts)` — orchestrates the kubectl shell-outs.
  - `formatHuman(report)` — chalk-coloured operator output.
  - `formatJson(report)` — deterministic versioned envelope.
  - `attestCommand()` — Commander factory; registered under a new
    "Attestation" section in `cli.ts`.
  - `__test` named export exposing the pure helpers + formatters.
- `cli/src/commands/attest.test.ts` — 19 vitest cases.
- `cli/src/cli.ts` — one import + one `program.addCommand(...)` call.

## §5 Field semantics

The JSON envelope:

```json
{
  "apiVersion": "azureclaw.azure.com/v1alpha1-attest",
  "kind": "Attestation",
  "generatedAt": "<ISO-8601 UTC>",
  "sandbox": {
    "name": "...", "namespace": "...",
    "generation": 3, "observedGeneration": 3, "phase": "Running",
    "specHash": "sha256:<hex>",
    "specHashAlgorithm": "sha256-canonical-json"
  },
  "fieldOwners": [{ "manager": "...", "fieldsOwned": 42 }, ...],
  "policyVersions": [
    { "kind": "ToolPolicy", "name": "...", "namespace": "...",
      "versionHash": "sha256:<hex>",
      "bindingConfigMap": "..." }
  ],
  "reconcileTraceId": null,
  "agtAuditReceiptId": null,
  "signature": null
}
```

`null` placeholders for the three Phase-3 fields keep the schema
forward-compatible — Phase 3 just fills them in.

## §6 SSA + reconciler skip

Read-only command. No SSA, no reconcile.

## §7 Failure modes

| Failure | Behaviour |
|---|---|
| Sandbox CR not found | execa rejects; CLI exits non-zero with kubectl's stderr. |
| kubectl missing / no cluster context | execa rejects; CLI exits non-zero. |
| Referenced policy CR missing | per-policy lookup is wrapped in try/catch; missing entries surface in human output as `<missing>`. JSON output omits the entry rather than failing the whole report — partial attestation is still a useful operator signal. |
| Deployment annotation absent (overlay mode, or pre-Phase-3 cluster) | `reconcileTraceId` is `null` / `(Phase 3)`. |
| Empty `managedFields` | `fieldOwners` is `[]`. |
| Spec missing entirely | `specHash` of `{}` returned (deterministic; asserted in tests). |

## §8 Test surface

`cli/src/commands/attest.test.ts` — 19 cases:

- 5 × canonicalJson (key-sort, array order, nested recursion,
  null/undefined, primitives).
- 4 × specHash (re-ordered → same, modified → different,
  prefix/length, null/undefined-treated-as-empty).
- 4 × summariseFieldOwners (empty, multi-manager aggregation +
  alphabetical sort, missing-manager-as-`(unknown)`, leaf count).
- 4 × extractPolicyRefs (three top-level refs, legacy governance
  shape, missing-name skipped, non-object spec).
- 2 × formatters (formatJson round-trips through JSON.parse,
  formatHuman names every field including `(Phase 3)` placeholders).

CLI workspace test count: 285 → 304 (+19). vitest + `tsc --noEmit` +
oxlint all green.

## §9 Verify-don't-guess (§0.2 #10)

- The spec-hash recipe was reproduced from
  `controller/src/tool_policy_compile.rs::version_hash` (canonical
  JSON via `serde_json::to_value` + recursive `BTreeMap` round-trip
  + SHA-256 via `sha2::Sha256`). The TS recipe matches: `JSON.stringify`
  with sorted-keys recursion + `node:crypto.createHash("sha256")`.
  Determinism asserted directly in tests; serde-json on the Rust side
  with a `BTreeMap<String, Value>` produces identical canonical bytes
  to the TS recursive sort over the same input shape.
- Status field names + locations cross-checked against
  `controller/src/crd.rs` (ToolPolicyStatus, A2AAgentStatus,
  InferencePolicyStatus, ClawMemoryStatus, ClawEvalStatus) shipped
  in S2-S6 — all carry `version_hash` + `bindingConfigMap` (where
  applicable).
- `metadata.managedFields` shape cross-checked against the K8s API
  reference (every CR object has `managedFields[]` once SSA is in
  use; each entry carries `manager` + `fieldsV1` tree).

## §10 Ops surface

- `azureclaw attest <sandbox-name>` — default human output.
- `azureclaw attest <sandbox-name> --format json` — for CI / scripts.
- `azureclaw attest <sandbox-name> --namespace <ns>` — overrides the
  default `azureclaw-system` lookup namespace for the ClawSandbox CR
  (per-policy lookups still use `azureclaw-<sandbox-name>`).

## §11 Sign-offs

- **Author / dev:** AzureClaw Phase 2 implementer (this PR).
- **Reviewer:** to be filled at PR review.
