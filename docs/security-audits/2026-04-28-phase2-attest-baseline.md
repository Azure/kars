# Phase 2 — `phase2-attest-baseline` security audit (2026-04-28)

## §0 Reuse map (§0.2 #11)

- **Diff logic** is a pure function (`diffAttestations`) over the
  S11 `AttestationReport` shape. No new transport, no new
  serialisation, no new storage. The deterministic spec-hash recipe
  shipped in S11 (canonical JSON + SHA-256) is what makes the diff
  byte-equal across runs.
- **Baseline file format** *is* the S11 attestation JSON envelope
  (`apiVersion: "azureclaw.azure.com/v1alpha1-attest"`,
  `kind: "Attestation"`). No second schema; users `>`-redirect today's
  `attest --format json` output and feed it back via `--baseline`
  tomorrow.
- **No new dependency.** `node:fs/promises` is already in use
  elsewhere in the CLI.
- **No controller change**, no CRD change, no new K8s object.
  `attest --baseline` is read-only from kubectl's perspective and
  read-only from the local filesystem.

## §1 AGT boundary

Read surface only. The diff exits non-zero on drift but never
mutates a cluster resource and never calls AGT. AGT receipt-id
emission stays Phase 3.

## §2 STRIDE

| Threat | Mitigation |
|---|---|
| **Tampering** — attacker edits the baseline file to hide drift | Out-of-scope mitigation today. The baseline file is plain JSON; an attacker with write access to the file can mask drift. Phase 3 lands controller-side cosign signatures inside the attestation envelope; once `signature` is non-null, `--baseline` will refuse to consume an unsigned or invalid-signed baseline. The CLI is designed so flipping that gate on is additive (envelope already carries the `signature` field). |
| **Spoofing** — attacker passes a fabricated baseline that "matches" the current sandbox | Same channel as above; relies on Phase 3 signature. Today the recommended workflow is "check approved.json into git, gate on it from CI" — the trust anchor is the git repo's review process, not the file itself. |
| **Information disclosure** — diff output leaks secrets | The diff inputs are the same fields S11 already emits (spec hash, version hash, manager names, phase). No new field categories surfaced. No secret material printed (no token, no key, no signing material). |
| **Denial of service** — pathological input | Diff is O(n + m) over field-owner sets and policy ref lists, both of which are bounded by the CRD shape (≤ 4 policy refs, ≤ tens of managedFields entries). `loadBaseline` reads a single JSON file; size of an attestation is sub-kilobyte. Bounded. |
| **Repudiation** — operator denies running the command | `attest` is read-only and has no side effect. Repudiation N/A. |
| **EoP** | Read-only — no `kubectl patch/apply/exec`, no filesystem write. |

## §3 Out of scope

- **Signed baseline verification** (Phase 3, gated on cosign-keyless
  controller emission).
- **`azureclaw verify <bundle>`** companion (Phase 3 follow-up; will
  reuse `diffAttestations` and a new `verifySignature` helper).
- **Time-travel mode** (`--at <ts>` against a controller-persisted
  history) — Phase 3, requires controller-side persistent receipt
  log.
- **Fleet mode** (`--all` / `--baseline-dir`) — separate slice; the
  current single-sandbox shape is the right primitive to build it on.
- **Per-field-count diff** — deliberately excluded (set-comparison
  only on managers); see §4.

## §4 Implementation surface

`cli/src/commands/attest.ts` (delta vs S11):

- `AttestationDelta` discriminated union with seven variants
  (`specHash`, `phase`, `policyVersionHash`, `policyAdded`,
  `policyRemoved`, `fieldOwnerAdded`, `fieldOwnerRemoved`). One
  variant per human-meaningful change category.
- `AttestationDiff` envelope `{ baseline, current, deltas, drift }`.
- `diffAttestations(baseline, current)` pure function — no IO, no
  time, no kubectl. Order-insensitive for policy refs (matched on
  `kind/name` key) and field owners (set comparison on manager).
- `loadBaseline(path)` — reads + validates the JSON envelope sentinel
  (`apiVersion === "azureclaw.azure.com/v1alpha1-attest"` and
  `kind === "Attestation"` and `sandbox.specHash` is a string).
  Returns `null` on `ENOENT` (CLI exits 3); throws on parse / shape
  errors (CLI surfaces and exits non-zero).
- `describeDelta(d)` — exhaustive switch over the union; emits one
  human sentence per variant. TS exhaustiveness check guarantees a
  new variant can never be silently dropped.
- `formatHuman` — appends a "Baseline diff:" section with `✓` /
  `✗` markers per delta and a final `DRIFT: N delta(s)` red banner.
- `formatJson` — unchanged; the `baselineDiff` field is part of the
  `AttestationReport` shape so the existing JSON serialiser picks it
  up for free.
- `attestCommand()` — adds `--baseline <path>` option, calls
  `loadBaseline` + `diffAttestations`, sets `report.baselineDiff`,
  exits with code 2 if `report.baselineDiff?.drift` is true.

`cli/src/commands/attest.test.ts`:

- 11 new cases covering every delta variant, the set-comparison /
  count-fluctuation invariant, missing baseline file, invalid
  baseline file (no envelope sentinel), exhaustive `describeDelta`.

## §5 Field semantics

The diff is order-insensitive on:

- **Policy refs** — matched on `${kind}/${name}` key. So a baseline
  with `[ToolPolicy/tp, InferencePolicy/ip]` and a current with
  `[InferencePolicy/ip, ToolPolicy/tp]` produces zero deltas.
- **Field owners** — compared as a `Set<manager>`. Per-manager field
  count fluctuation is intentionally ignored (every controller SSA
  write bumps the count by tens; flagging that as drift would page
  on every reconcile).

The diff is order-sensitive on:

- **Top-level scalars** (`specHash`, `phase`) — direct equality.

## §6 SSA + reconciler skip

Read-only command. No SSA, no reconcile.

## §7 Failure modes

| Failure | Behaviour |
|---|---|
| Baseline file missing | `loadBaseline` returns `null`; CLI prints red `✗ baseline file not found:` to stderr, exits **3**. |
| Baseline file malformed JSON | `JSON.parse` throws; execa/Node prints stack to stderr, exits non-zero (≠ 2 / 3). |
| Baseline file valid JSON but missing envelope sentinel | `loadBaseline` throws `not a valid AzureClaw attestation`; CLI surfaces and exits non-zero. |
| Sandbox CR not found at `attest` time | execa rejects (S11 path); CLI exits non-zero before diff runs. |
| Drift detected | CLI prints all deltas + red `DRIFT:` banner, exits **2**. |
| No drift | CLI prints green `✓ no drift detected`, exits **0**. |
| Drift only in Phase-3-placeholder fields (`signature`, `agtAuditReceiptId`, `reconcileTraceId`) | These fields are deliberately *not* part of the diff today. They will be added when Phase 3 lands real values; until then a `null → null` "drift" would be noise. |

## §8 Test surface

`cli/src/commands/attest.test.ts` — 11 new vitest cases:

- 1 × no-drift (matching reports → `drift: false`, empty deltas).
- 1 × spec-hash drift detected.
- 1 × phase drift detected.
- 1 × policy versionHash drift detected.
- 1 × policy added + removed (set comparison).
- 1 × new SSA manager surfaces as `fieldOwnerAdded`.
- 1 × ignored field-count fluctuation when manager set unchanged.
- 1 × `describeDelta` is exhaustive over all seven variants.
- 1 × `loadBaseline` returns null for missing file.
- 1 × `loadBaseline` happy-path round-trip via tmpdir.
- 1 × `loadBaseline` rejects file missing envelope sentinel.

CLI workspace test count: 304 → 315 (+11). vitest + `tsc --noEmit` +
oxlint all green.

## §9 Verify-don't-guess (§0.2 #10)

- **Set-vs-count tradeoff on field owners.** Verified by reading the
  K8s SSA reference: every server-side `apply` from a controller
  bumps the per-field count for that manager (each field write
  produces a fresh leaf in `fieldsV1`). Set comparison is the right
  granularity for "did a new actor touch this object?" without
  paging on every reconcile.
- **Policy ref matching key.** Phase 2 CRDs scope policy CRs to
  `azureclaw-<sandbox>` namespace, so `(kind, name)` is unique within
  one attestation report; namespace is identical for every entry.
- **JSON envelope sentinel.** `apiVersion` + `kind` come from S11
  unchanged; `loadBaseline` rejects anything else, so a fabricated or
  wrong-shape file fails fast instead of producing a partial diff.
- **Exit codes.** `0/2/3` chosen so `set -e` pipelines treat drift as
  an error (2) distinguishably from infrastructure problems (≠ 0/2/3,
  e.g., kubectl not found is whatever execa exits with).

## §10 Ops surface

```
azureclaw attest demo                                  # current state, exit 0
azureclaw attest demo --format json > approved.json    # capture baseline
azureclaw attest demo --baseline approved.json         # diff, exit 0/2/3
azureclaw attest demo --baseline approved.json --format json   # CI-friendly
```

Recommended workflow: commit `approved.json` to the repo alongside
the YAML manifests, gate every PR on `azureclaw attest <name>
--baseline approved.json`. The diff section in the human output is
the change reviewer's signal.

## §11 Sign-offs

- **Author / dev:** AzureClaw Phase 2 implementer (this PR).
- **Reviewer:** to be filled at PR review.
