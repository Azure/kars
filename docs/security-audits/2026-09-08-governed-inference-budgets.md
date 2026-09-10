# Security Audit — Governed inference budgets (v1)

Date: **2026-09-08 UTC**
Status: **Source audit approved under explicit maintainer delegation**;
exact-head combined native qualification remains mandatory before publication.

Scope: `shared/inference_budget/`, `controller/src/inference_budget/`,
`controller/src/task_identity.rs`, `inference-router/src/inference_budget/`,
controller Task/Team/runtime wiring, router dispatch/route wiring, CLI and Helm.

Gated paths: controller CRDs, router providers/routes, CLI commands,
`deploy/helm/kars/files/`, `shared/inference_budget/`.

## Current delegated approval (2026-09-10)

The maintainer explicitly authorized publication sign-offs after additional
focused review rounds, recorded in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The AI attestation below is delegated review, not a claim that a second human
personally reviewed or signed this capability. Earlier, separately limited
signature waivers are not extended, and no technical check is waived.

A fresh focused review of `c4291b0f3808747d7fe221af99f47b37e17d16a9`
identified a recovery race: after capturing authority A, recovery could observe
that A was no longer live, then close replacement authority B and its new Pod
session inside a transaction against the latest ledger. The repair at
`e89040dfa76ae3e3b015f2ec7bc39b7c3b24ffc3` compares the captured authority
inside every store CAS attempt. Changed authority is deferred for a fresh
live check; genuinely invalid unchanged authority is still revoked.

Deterministic regressions cover replacement before the transaction, replacement
during a 409 CAS retry, and unchanged-A revocation. They check the replacement
Pod session and InFlight work, plus conservative maximum liabilities for old
accepted work. Independent re-review of the exact repair against `5ae7b99a`
reported no significant issues. This is bounded source-review closure, not
an independent native execution or financial certification.

Actual guarded qualification at `e89040df` passed:

- Controller binary: 82 budget cases, including the five selected recovery
  cases and all three new interleaving/revocation regressions. These selectors
  overlap and are not added together.
- Router library: 40 inference-budget cases.
- Strict paired controller/router all-target Clippy and workspace formatting.

The existing offline/locked shared target was used, with two jobs, incremental
compilation disabled and an 8.5 GiB stop floor; minimum observed free space was
9.38 GiB. No dependency installation or target cleanup was needed.

The current source `66c4ce3a674317d9329ba1d7dda7b8763d0834ae` additionally
merges services `920f9f0e` and its actual landed SRE ancestry. Every tracked
byte outside three CLI test-harness files is identical to qualified `e89040df`.
The forwarded 39 CLI cases and typecheck passed using the existing compatible
cache (Vitest 4.1.10 versus lockfile 4.1.8). The services head's actual hosted
CLI also passed; that result does not qualify this budget composition.

The same candidate retains the reviewed native fixture repairs: verified
same-image digest aliases, consistent named-provider routing, a proper
CA:false/serverAuth/SAN server leaf and negative TLS checks, and bounded
UID/generation/image/budget-binding checks when reconnecting a port-forward
after legitimate Pod replacement. A genuine 503 or identity/authorization
failure cannot become success. Spending/cancellation assertions and the
native readiness deadline remain unchanged.

**Exact-head hosted Rust/CLI/security and complete native budget/SRE lifecycle
qualification remain required.** Earlier image, TLS-profile and stale
port-forward failures remain failed historical results, not proof of current
spending or cancellation enforcement. Shared credential/MCP composition is a
separate gate. No main promotion, customer/H100 deployment, public image or
private Bridge publication is authorized by this source sign-off.

Signed-off-by: pallakatos (maintainer delegation recorded above) <lakatos.toth.pal@gmail.com>
Signed-off-by: GitHub Copilot (delegated AI audit, not an independent human) <223556219+Copilot@users.noreply.github.com>

## Summary

Durable token ceilings and operator-configured maximum-price caps for governed
inference only. No claim covers compute, GPU/VM, tool/MCP, storage, networking,
all-in task spend, invoice accuracy, taxes, or exchange rates.

## T1: New capability / attack surface? (YES)

- A private HTTPS budget broker and durable Kubernetes accounting CRD are new
  control-plane surfaces. Router identities, all Task ancestors and complete
  effective authorization must be checked before any dispatch grant.
- Budget-account UIDs, signed bootstrap ownership and replay fences prevent
  name reuse, arbitrary-object adoption, concurrent overspend and reset-to-zero
  recovery. Accounting stays separate from credential grants and tool costs.
- The shared Task identity helper captures live UID/RV evidence; it is not an
  immutable registry until a consumer verifies and persists authoritative pins.

## T2: Security-control change? (YES)

- Finite inference requires router-only projected Pod/audience tokens,
  exact TokenReview and live identity checks, current privacy proof, dedicated
  admission, a qualified immutable router image, and versioned model contracts.
- Legacy/operator opaque control tokens and the SRE API token are not broker
  authorization. Pending privacy qualification never authorizes RPCs or issuance.
- Unsupported generation, opaque tunnels, and unbounded mandatory moderation
  fail closed rather than bypass accounting or disable existing guardrails.

## T3: Availability / fail-open risk? (INCREASED for opted-in finite accounts)

- Store/API/CAS, privacy/admission, TLS, image, contract, expiry, capacity or
  lineage failures intentionally deny finite sends. Uncertain accepted work
  remains fully funded; conservative full-context reservations may strand
  otherwise unused quota. These are explicit limitations, not zero-cost claims.
- Unbounded standalone defaults remain unchanged. Missing accounting must never
  be treated as empty accounting. Pending-state rollout behavior and accepted
  cancellation/retry require the planned real Kubernetes qualification.

## Verification

### Historical native primary-workload repair (2026-09-10)

Standalone qualification found a real ReplicaSet admission failure:
`AdmissionRequest.subResource` is absent on primary operations. The shared
`kars-inference-budget-workloads` expression now uses
`request.?subResource.orValue('')` rather than a raw field access. The exact
controller usernames, core-only Deployment restriction, namespace selector,
ephemeral-container restriction, failure policy and all other predicates remain
unchanged. Helm and Rust consume the same JSON; no second policy copy was added.

The isolated repair and native regressions originate at
`63c9a06403e6ae9264fefd3c3074bd7c55338d99`. Actual Kubernetes qualification at
composed head `c8f7a143a3242484ea782d5c3170ae00295074f6` passed all **19 workload
records**, including the real Deployment/ReplicaSet/Pod owner-UID chain, allowed
primary controller creation, and exact intended tenant/Deployment/ephemeral
denials. The probe proves each principal has the necessary RBAC rather than
mistaking an unrelated authorization error for admission enforcement. The
tokenless, deliberately unscheduled fixtures do not claim runtime readiness.

Evidence: [native API job 102719417174](https://github.com/Azure/kars/actions/runs/34428691641/job/102719417174).
The same composed run passed hosted Rust, CLI, schema and benchmark jobs.
Its separate [standalone job](https://github.com/Azure/kars/actions/runs/34428691641/job/102725472891)
finished **23 passed / 1 failed**: budget Pods are created, but the router reports
`ErrImagePull` / `ImagePullBackOff`. Spending and cancellation assertions were
not reached. No image-resolution cause, full SRE or CNI enforcement is inferred.

Forwarding the exact repair to this budget branch passed 12 focused CLI/probe
tests, TypeScript checking, native JavaScript syntax, Helm lint and diff checks.
No local Cargo build was used for this forward. This is bounded admission
closure, not complete budget enforcement or new-head hosted qualification.
Required human signatures, exact composed-runtime qualification and publication
approval remain outstanding.

### Initial authoring snapshot (historical)

The table and initial local-build statements below record the earlier authoring
state. They are not the current result of the native workload repair above.

| Area | Evidence | Status |
|---|---|---|
| Shared arithmetic/state engine | UID ancestry, ancestor reservation, BeginDispatch, settlement, cancellation, replay and breach tests written | Rust execution pending |
| Kubernetes store | UID/RV PUT/CAS, concurrent sibling, lost acknowledgement, corruption/replacement, bootstrap tests written | Rust execution pending |
| Router dispatch | Buffered/stream actual-send integration and conservative usage tests written | Rust execution pending |
| Private authority | Shared exact admission bundle, Pod projection and audience checks; real core privacy helper referenced | Prerequisite merge and live qualification pending |
| Helm/default/reused values | 12 budget/schema cases plus 6 local-inference compatibility cases | **18 passed locally** |
| CLI | Scoped CREATE and pinned-account reporting | **7 passed locally** |
| CLI static validation | Existing TypeScript typecheck and targeted oxlint | **Passed locally** |
| Public API/CEL | Independent pinned Kind v0.24 / Kubernetes v1.31 preflight added, no Rust image dependency | Not executed |
| Complete broker Kind integration | Real loaded router digest, TLS broker, sibling token/price caps, route closure and cancellation scenario added to the existing E2E runner | Not executed |
| Shared Task UID identity | Seven readiness, ancestry, UID/RV, Team-owner and API failure tests authored | Rust execution pending |
| Static gates | Existing LOC, no-custom-crypto and no-stubs scripts against `068ae160` | **Passed locally** |
| Source checks | Rustfmt parsing, JavaScript/shell/YAML/TOML syntax and diff checks | **Passed locally** |
| Affected-crate strict Clippy | Required, no waivers | Pending |
| Launch/Team cadence gates | Explicit scope syntax, first-opt-in transition constraints, and mandatory asynchronous broker/account checks | Source implemented; Rust/API qualification pending |
| Router code identity | Finite mode requires an operator-qualified immutable router manifest digest | Helm configuration tested; runtime qualification pending |

No Cargo command, dependency installation, local Docker test, deployment, customer
mutation, H100 operation, main-branch change, image publication, or public push
was performed for this evidence. Authorized local checkpoint `eb26efd9` and
forward merge `0701baed` preserve the candidate and exact privacy parent
`7dc72810a2e3c87aa751cfa95d9152f8dcd10194`. Existing authorized cached CLI
dependencies were used after the local runner was found missing.

The subsequent parent-coordinated forward merge uses exact fixed privacy/SRE
ancestry `068ae16041ecf7bd2b8321dfeb22e381ebbd587b`, including `9d0f8e23` epoch
transition repairs and the shipped-schema Kubernetes compatibility repair.
Parent-reported prerequisite tests/Clippy and schema API successes do not
qualify this budget implementation. Full SRE Kind remains a separate open gate.
The existing `security-audit-required` script now discovers this correctly placed
record and fails specifically for **0 of 2 required genuine signer emails**.
That failure is intentional until human review, not a waived or fabricated pass.

## Original release checklist (historical)

The following records the initial requirements, not the current approval or
Rust execution status. The current delegated approval and still-open native
gates are stated above.

1. Qualify against the real, now-forwarded privacy issuer prerequisite; no
   fallback implementation. Parent re-review and full SRE Kind remain separate
   gates, not implied by this merge.
2. Qualify Team lifecycle/selective launch integration, first-opt-in transition
   constraints, and source/route/cancellation regressions, including the actual
   broker-in-Kind scenario and separate schema/identity preflight.
3. Run affected Rust tests, strict Clippy, schema/drift/LOC and complete disposable
   Kind enforcement tests, including actual API/CEL evidence.
4. Independently review the bootstrap signature, authoritative ancestor CAS,
   provider maximum-bound assumptions, every actual-send path, private token
   accessibility, and conservative uncertainty/capacity behavior.
5. Obtain genuine required human audit signoffs before protected publication.

## Earlier bounded reviewer repair candidate (historical)

The source-only independent review identified five blockers. This repair:

1. Moves integer validation and Pod decoration to their intended module scopes.
2. Restricts legacy launch suppression to unsupported budget scopes.
3. Separates new-work admission from actual revocation; pending/exhausted/API
   failures retain Task UIDs and already funded work. Explicit pause, removed
   policy/owner authority and UID changes still revoke normally.
4. Requires authoritative final Anthropic stream usage before any refund.
5. Preserves unscoped legacy planning readiness without allowing a pinned account
   to escape enforcement.

New Rust regressions cover full Team reconcile interleavings with real ledger
reserve/begin/settle transitions, UID stability, supported launch intent,
pause/revocation, Task-controller budget waits and legacy parent readiness, plus
16 malformed/incomplete Anthropic protocol variants through the actual stream
and settlement path. **These Rust tests have not run**: the shared Cargo lease
remains with the credential integration owner until the parent directly grants it.
Formatting/source checks do not establish compilation or technical closure.

## Original unsigned authoring state (historical)

- Implementation author: changes under active development; not a signoff.
- Independent technical reviewer: **pending**.
- Security/privacy reviewer: **pending**.
- Financial-scope/model-contract owner: **pending**.
- Required human approval/signatures: **pending**.

No reviewer identity, email, approval, waiver, or signature is inferred or
fabricated. Prior feature waivers do not apply to this capability.

## Verdict

Source approval is recorded above under the maintainer's explicit delegation.
Publication and Ready remain pending exact-head technical and native
qualification. Historical unsigned states and failed runs in this document
must not be represented as current blockers already repaired or as successful
runtime acceptance. The existing audit gate is unchanged.
