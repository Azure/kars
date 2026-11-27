<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit - Qualified concierge successor (Azure/kars#638)

Date: 2026-11-26
Source: `a639732d65eacefa7c639afc266fa28aa640f57a`
Base: `754110953bf9cccbb7545a71782c38a59087ea2f`
PR: <https://github.com/Azure/kars/pull/638>

## Scope

This exact source retains the browser concierge and the separately authorized
minimal dependency repairs. It adds the authorized equivalent root Rust
kube/Quinn updates, the CLI Headlamp fixture correction, the guarded Headlamp
service-span formatter and the 15-line Helm CRD parity correction. The latter
removes only empty `categories: []` entries now omitted by kube 4.2.1.
Strict schema comparison and admission rules are unchanged.

Both earlier records remain immutable:

- [`2026-11-26-browser-concierge.md`](2026-11-26-browser-concierge.md)
- [`2026-11-26-browser-concierge-dependencies.md`](2026-11-26-browser-concierge-dependencies.md)

The approval-only successor changes only this record and the existing
gate/test/README plumbing. It does not change implementation, dependency locks,
workflow protections, advisory policy or image identity.

## T1: New attack surface? (NO)

No additional listener, endpoint, credential store or installation capability
is introduced beyond the retained concierge scope. AL3 runtime packaging,
Headlamp upstream source and builder pins remain unchanged. No runtime package
installation or private-repository repair is introduced.

## T2: Security-control change? (YES)

The root kube/Quinn updates patch dependency findings under the maintainer's
explicitly authorized minimal repair scope. Cluster authentication, TLS checks,
strict CRD validation and every dependency/image scan remain enabled.
Native TLS/CNI and governed API/admission qualification passed on this source.

Root audits still fail on URL 2.5.8 (RUSTSEC-2026-0592), required by AGT's exact
shared pin, and cargo-deny additionally rejects Chrono's RUSTSEC-2026-0543
maintenance advisory. The latter is not a demonstrated exploitable vulnerability.
The maintainer chose to keep these separately scoped in
[Azure/kars#639](https://github.com/Azure/kars/issues/639) and
[Azure/kars#634](https://github.com/Azure/kars/issues/634).
No new exception, AGT fork, pin relaxation or Chrono migration is approved here.

## T3: Availability or fail-open risk? (YES)

The dependency updates previously exposed two Headlamp tracing regressions
and 15 strict Rust/Helm representation mismatches. Those failed attempts are
retained. The existing Headlamp source patcher now explicitly preserves service
span names and rejects unexpected upstream source drift. Upstream tests remain
unchanged. Helm manifests now match generated Rust schemas without weakening
the drift comparison. All 4,107 root Rust tests and actual Kind E2E pass.

## Verification at the source

All linked runs below qualified implementation `a639732d`.

- [CI 45855025378](https://github.com/Azure/kars/actions/runs/45855025378):
  root formatting, clippy, release build and
  `cargo nextest run --workspace --release --no-fail-fast` **passed**:
  4,107 tests passed, zero skipped, including the unchanged strict CRD drift
  tests. Kind E2E, benchmark regression, chaos, CLI, OpenClaw, Hermes,
  governed inference API/CEL, AL3 add-ons and shipping Headlamp qualification
  **passed**. Root RustSec/cargo-deny checks **failed** on the separate blockers
  above; the overall workflow is not green.
- [Bridge CI 45855025566](https://github.com/Azure/kars/actions/runs/45855025566):
  **passed**, including BFF build/tests/audit/image scan, Dex runtime
  qualification and aggregate component acceptance.
- [Native qualification 45855025562](https://github.com/Azure/kars/actions/runs/45855025562):
  **passed** all three cold API/admission installations and native BFF grants,
  lifecycle, TLS, CNI, Team projection and writer lifecycle.
- [CodeQL 45855025403](https://github.com/Azure/kars/actions/runs/45855025403):
  **passed** Rust, JavaScript/TypeScript and Actions analysis.
- `helm lint deploy/helm/kars`: **passed** locally.
- `npm test --prefix cli -- src/commands/crd-helpers.test.ts src/lib/core-helm-schemas.test.ts src/testing/schema-hook-chart.test.ts`:
  **passed**, 95 tests across three suites.
- `git diff --check`: **passed** before implementation publication.

Source `7e95c9a3` previously passed all 4,092 non-failing root tests but failed
the 15 category comparisons. The unchanged tests all pass on `a639732d`.
An earlier Dex Trivy installation failure passed on an unchanged isolated
retry; the fresh Dex run linked above also passed without a retry.
No failed attempt has been removed or counted as a success.

The earlier automated concierge security review is not a new independent
review of this successor or a second human approval. Hosted disposable-cluster
qualification is not installed browser-concierge customer acceptance.
No customer installation, host-trust mutation or deployment occurred.
Beta acceptance remains 0/2.

After authorization, the approval-only regression command
`PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py`
**passed**, 392 tests, including rejection of changes to either historical
record, the Rust lock, Helm schemas or the tracing patch after approval.

## Verdict and authorization

Accept this exact source under the maintainer-authorized second-human exception.
This is not merge readiness, beta readiness or permission to merge.
Separately scoped dependency blockers remain enforced.

The maintainer was asked:

> The final concierge implementation, a639732d65eacefa7c639afc266fa28aa640f57a,
> now passes all 4,107 Rust tests, Kind E2E, native TLS/CNI/lifecycle
> qualification, Bridge, Headlamp, Dex and CodeQL. May I record your
> sole-maintainer sign-off for this exact PR638 source against Azure/kars base
> 754110953bf9cccbb7545a71782c38a59087ea2f, preserving both earlier approvals?
> This waives only the second-human requirement. URL #639 and Chrono #634 remain
> blocking, separately scoped issues; this is not merge authorization.

The maintainer selected **"Authorize final source-bound sign-off for PR638"**.
Pal Lakatos-Toth is the sole human approver; Copilot implemented the repair.
No second signer or independent review is claimed. Missing or altered source,
base, ancestry, PR context, either historical record or successor authorization
fails the existing gate. Further implementation changes invalidate this approval.

Beta-approval-exception: Azure/kars#638; source=a639732d65eacefa7c639afc266fa28aa640f57a; base=754110953bf9cccbb7545a71782c38a59087ea2f

Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
