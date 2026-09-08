# Security Audit — Governed inference budgets (v1)

Date: **2026-09-08 UTC**
Status: **Implementation/integration candidate — publication not approved**

Scope: `shared/inference_budget/`, `controller/src/inference_budget/`,
`controller/src/task_identity.rs`, `inference-router/src/inference_budget/`,
controller Task/Team/runtime wiring, router dispatch/route wiring, CLI and Helm.

Gated paths: controller CRDs, router providers/routes, CLI commands,
`deploy/helm/kars/files/`, `shared/inference_budget/`.

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

## Remaining release decisions/gates

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

## Signoffs

- Implementation author: changes under active development; not a signoff.
- Independent technical reviewer: **pending**.
- Security/privacy reviewer: **pending**.
- Financial-scope/model-contract owner: **pending**.
- Required human approval/signatures: **pending**.

No reviewer identity, email, approval, waiver, or signature is inferred or
fabricated. Prior feature waivers do not apply to this capability.

## Verdict

**Pending — not approved for publication or Ready.** Budget Rust/Clippy, actual
Kind enforcement, independent review and two genuine human signoffs remain
required. No `Signed-off-by` identity is supplied until those people actually
review and approve; the existing security-audit gate is expected to remain red
for missing signatures, without a waiver or altered rule.
