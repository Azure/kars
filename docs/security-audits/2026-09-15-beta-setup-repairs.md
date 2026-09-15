<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Beta setup repairs - bounded delegated source review

Base: `3bc7ff58d8d2994e474be39fd2af201d750fc5c2`.
Reviewed assembled source: `b24daef5b85a2dccf1269c520619aafe44528694`.

Status: **Scoped source-approved; combined-head hosted execution is still required.**
This record does not approve deployment, certify a smooth installation, or waive
any required check.

## Scope

- AKS private-root admitted-Pod verification: CLI admission, activation and
  continuity helpers, their regression tests, and enrollment documentation.
- Copilot device authorization: BFF handlers and tests, shared fixtures and wire
  contracts, browser polling/actions/sign-in rendering, and recovery guidance.
- Managed MCP diagnostics: current-generation BFF projection, typed client
  contract, actual capabilities-page rendering, and installation documentation.
- Public beta Helm guide and the two added regression-registration names in the
  existing Bridge CI job. Existing checks and registration requirements remain.

The independent core review covered `3bc7ff58` through `948e334e`; those files
remain unchanged in the assembled source. The independent application review
covered the MCP slice `a9a35bba`, Copilot slice `f7a75835`, corrective commit
`f6881877`, and the shared-type composition and CI guard at `b24daef5`.

## T1: New capability or attack surface?

The CLI sends a synthetic Pod through strict server-side dry-run to verify
supported root admission changes. This is not permission to persist that Pod,
accept arbitrary webhook mutations, or normalize unrelated workloads. The live
execution must match both a constrained derivation and replayed admission.

MCP reason/message/currentness fields are additive read projections, not
readiness actuators. Controller explanations are rendered as text, not HTML.
The Copilot routes retain the real device-flow upstreams; no token broker,
escrow, new persistent credential type, or additional credential authority is
introduced.

## T2: Security-control change?

The root admission repair preserves reviewed Deployment/ReplicaSet/Pod ancestry,
namespace and ServiceAccount identity, frozen template and retirement binding,
epoch, UID/resourceVersion rereads, and independently verified publication
markers. It applies before protection writes and during restored-root continuity.
No real-CREATE fallback or cross-namespace Secret-copy privilege is added.

Copilot start and token exchange now preflight the existing credential-store
requirements. The actual write still checks authority, namespace/grant identity,
generation, Secret type and compare-and-swap conditions. Preflight is not a
reservation and cannot eliminate a later write failure. Redirects, malformed
responses and unknown upstream errors do not become pending or authorized
successes; public error text is restricted.

MCP success diagnostics require current generation evidence. Stale observations
and older BFFs are explicitly distinguished from verified readiness. This does
not alter the controller's installation or protocol-discovery requirements.

## T3: Availability and ambiguous outcomes

Polling is serialized, GitHub slowdown increases are cumulative, and the
original expiry deadline is preserved. Cancellation/replacement and late
completion guards prevent stale UI updates. They cannot undo a successful
in-flight credential write.

The first independent application review found two defects: local expiry could
hide an unresolved storage outcome, and two Rust tests were nested rather than
registered. Both were repaired in `f6881877` and independently re-reviewed.
Expiry during an unresolved poll now reports an unconfirmed outcome and directs
the operator to inspect provider state before retrying. Explicit upstream expiry
and expiry without an in-flight poll remain distinct. Both former nested tests
are module-level; CI now requires their exact names in the actual test inventory.

The specific user's earlier device attempt was not observed or replayed. These
source defects are not asserted to be its proven cause. Consumed tokens have no
guaranteed resume path.

## Verification and limits

The parent executed 157 targeted CLI cases on the composition before the
unchanged CLI slice was frozen, plus its typecheck/build/focused lint. The final
assembled web source passed all 90 tests, full TypeScript checking and focused
lint. Rust formatting and whitespace checks passed.

Local dependency caches are provisional: CLI YAML was 2.9.0 rather than locked
2.8.3, and web Next was 16.2.9 rather than locked 16.3.3. No local Rust
compilation, test registration/execution or Clippy result is claimed. All eight
Copilot Rust tests, including both formerly nested cases, must actually
register and execute under the hosted locked-dependency gates.

[Bridge CI run 34983443655](https://github.com/Azure/kars/actions/runs/34983443655)
passed on MCP-only `a9a35bba`, including actual BFF and locked web execution.
It does not qualify the later AKS/Copilot composition. All current 31 branch
requirements, full combined-head component/native checks and matching-source
image qualification remain required.

The core review was source-only. Its reported live admitted-Pod comparison is
implementation-owner evidence, not independent live enrollment acceptance.
The two review contexts performed bounded correctness/compatibility reviews,
not exhaustive whole-application or kernel qualification.

## Separate deployment limits

Finishing the original private-root recovery does not make later controller
template changes supported. This beta still lacks a reviewed root-template
migration path; image/environment changes or a rollout restart must not bypass
the sealed binding. The public Helm guide records that limitation.

Managed-MCP credential restoration, actual orchestrator readiness, reproducible
node image-pull access, isolation prerequisites and the complete authenticated
Home-to-Team workflow require their own live evidence. This record does not
claim those outcomes or authorize infrastructure/GPU changes.

## Delegation and verdict

The independent core and application AI review contexts found no remaining
significant issue in these bounded changes after repair. The parent assembled
the source and added the CI registration guard and installation guide.

Author source attestation is exercised under the maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
These are independently separated AI contexts, not a second human review or a
claim of personal code review by the maintainer. That delegation does not waive
technical gates or separately authorize deployment.

Verdict: accept this bounded source repair, subject to actual combined-head
qualification and the deployment limits above.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
