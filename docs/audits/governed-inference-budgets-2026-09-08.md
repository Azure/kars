# Governed inference budget capability audit

Date: **2026-09-08 UTC**
Status: **Implementation/integration candidate — publication not approved**

## Claim being evaluated

Durable token ceilings and operator-configured maximum-price caps for governed
inference only. No claim covers compute, GPU/VM, tool/MCP, storage, networking,
all-in task spend, invoice accuracy, taxes, or exchange rates.

## Current evidence

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
| Affected-crate strict Clippy/static guards | Required, no waivers | Pending |
| Launch/Team cadence gates | Explicit scope syntax, first-opt-in transition constraints, and mandatory asynchronous broker/account checks | Source implemented; Rust/API qualification pending |
| Router code identity | Finite mode requires an operator-qualified immutable router manifest digest | Helm configuration tested; runtime qualification pending |

No Cargo command, dependency installation, local Docker test, deployment, customer
mutation, H100 operation, main-branch change, image publication, or budget commit/
push was performed for this evidence. Existing authorized cached CLI dependencies
were used after the local runner was found missing.

## Remaining release decisions/gates

1. Integrate the real privacy issuer prerequisite; no fallback implementation.
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
