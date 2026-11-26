<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit - Browser concierge (Azure/kars#638)

Date: 2026-11-26
Scope: Browser-guided Kind setup and its directly required installer support.
Gated paths: `cli/src/commands/setup/`, changed `cli/src/commands/dev*` and
`cli/src/commands/up.ts`, `bridge/deploy/helm/kars-bridge/templates/`,
`deploy/helm/kars/files/schema-hook.mjs`.

## Summary

Reviewed implementation source:
`7a791e8b020cf29e68acb3f593f9cbb0cb547d92`.
Integration base: `754110953bf9cccbb7545a71782c38a59087ea2f`.
PR: <https://github.com/Azure/kars/pull/638>.

The port adds a loopback browser interface to the maintained installer, with
Core-only or Core + Bridge selection, provider verification and enrollment,
explicit consent, progress and retained-failure cleanup. Supporting changes
include local Dex ports, scoped gateway ingress, a dedicated Core-only provider
writer, record-only first-install SRE handling, sanitized schema-hook diagnostics
and the controller's named-provider endpoint check. Existing-cluster selection
is inspection-only, not a supported installation path.

No dependency locks, image recipes, CI workflows or unrelated historical
repairs were imported. Existing AL3 packaging remains unchanged. The subsequent
approval commit is limited to this record and the existing gate/test/README
approval mechanism.

## T1: New capability / attack surface? (YES)

- The local HTTP service can initiate a privileged installation. It binds to
  loopback and checks the exact Host, same-origin POSTs, Fetch Metadata,
  authenticated session cookie and CSRF token. A single-use bootstrap token
  arrives in a fragment, is removed by browser history replacement and expires.
  CSP hashes cover the generated static page; framing and browser caching are
  prohibited. External top-level navigation may open only the public landing
  page, not its APIs.
- Provider credentials, OAuth enrollment and administrator input are new browser
  inputs. Credentials are not persisted in browser storage or interpolated into
  generated HTML. Provider kind, endpoint and model are validated, official
  provider credentials remain bound to official endpoints, and custom remote
  endpoints must resolve to public addresses. Real provider verification is
  required; listing a model is not successful inference.
- Installation consumes approval bound to the reviewed draft. Changes invalidate
  that approval; consumed attempts cannot be replayed. OS authorization remains
  in the terminal or system dialog, never in the browser.
- Cleanup requires separate confirmation of this installer's recorded failed
  target. Ownership and receipt identity are rechecked, and retained failures
  are not represented as successful or resumable installations.

## T2: Security-control change? (YES)

- Core-only enrollment uses a newly created ServiceAccount and the existing
  credential-grant controller, not operator credentials as the provider writer.
  Grant, workspace, ServiceAccount and empty provider-store UIDs are checked;
  readiness must match the observed generation. The short-lived TokenRequest
  is bound to the provider Secret and limited to ten minutes. The isolated
  kubeconfig contains only loopback API/CA details and this identity.
  The writer permits one UID/resourceVersion compare-and-swap provider patch.
- Local Dex's alternate unprivileged port requires the managed local namespace,
  configured CA and exact local hostname form. Existing external HTTPS OIDC,
  Secret references, role mapping and chart CA validation remain enforced.
  Gateway ingress is scoped rather than globally opening the namespace.
- The SRE empty-catalog path is restricted to initial, unbounded record-only
  Kind beta setup; this is not production budget bypass or evidence of runtime
  readiness. Actual accounting and later enrollment checks remain.
- Progress uses owned read-only context and allowlisted diagnostics. It does
  not expose arbitrary Kubernetes specs, errors, subprocess output or secrets.
- Four same-line `ci:stub-ok` annotations in `page.ts` and `page-styles.ts`
  cover only the HTML input hint, its two DOM assignments and the CSS
  input-hint pseudo-element. These are implemented UI features, not incomplete
  behavior or default credentials/endpoints. The no-stubs gate is unchanged.

## T3: Availability / fail-open risk? (INCREASED)

An additional local server and orchestration surface introduces availability
risk. Request size, idle lifetime and operation concurrency are bounded.
Failures are retained and displayed as failures rather than silently retried.
The installer does not bypass image qualification, host safety or provider
capacity limits.

The original hosted run remains failed evidence: the isolated OIDC fixture
omitted the real chart's `$oidcCA` declaration, causing 13 CLI test failures.
The repair includes that source declaration and tests managed port limits,
missing CA, wrong namespace, foreign host and query rejection.

CodeQL's uppercase SCRIPT findings were in trusted static-page CSP extraction
and a test, not HTML sanitization. Extraction now honors case-insensitive HTML
tag names, with an HTTP/CSP regression test. The test-only resource matcher
now selects the exact kubectl `get` argument rather than using array
`includes`. The provider credential guard had already narrowed `apiKey` to a
nonempty string; its redundant check and assertion were removed, not its
validation. Hosted CodeQL confirmation of these repairs is still required.

## Verification

At the reviewed source, from `cli/`:

- `npm test -- src/commands/setup src/commands/dev/kind-beta-inference.test.ts src/testing/bridge-kind-beta-repair.test.ts`:
  **passed**, 277 tests across 11 suites.
- `npm run typecheck`: **passed**.
- `npm run lint`: **passed**, zero errors and 26 existing warnings.
- `npm run build`: **passed**.
- `npm run check:schema-hook`: **passed**.

From the repository root:

- `BASE_REF=754110953bf9cccbb7545a71782c38a59087ea2f /opt/homebrew/bin/bash ci/no-stubs.sh`:
  **passed**, evaluating the committed source.
- `git diff --check` and `git diff --cached --check`: **passed** before the
  implementation commit.

For the approval-only gate/test changes:

- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py`:
  **passed**, 338 tests, including rejection of changed implementation,
  wrong source/base/PR/repository, missing ancestry and modified prior records.

The original port also passed 905 targeted installer/setup tests, 231 provider,
schema and Dex compatibility tests, and seven controller inference tests.
Its live browser smoke covered authentication and Core/Bridge selection only.
An automated `security-review` agent reviewed the bounded concierge surfaces
and reported no vulnerabilities; it provided no independent human approval.
No fresh install, host-trust mutation, deployment or installed customer
acceptance was performed for this port. Beta acceptance remains 0/2.

Unresolved, separate hosted scan findings are **not waived**:

- Bridge BFF lock: `kube-client` 4.0.0 (RUSTSEC-2026-0600),
  `quinn-proto` 0.11.18 (RUSTSEC-2026-0524/0523),
  `quinn-udp` 0.5.14 (RUSTSEC-2026-0538), and `url` 2.5.8
  (RUSTSEC-2026-0592). The Azure base's run 43438840645 already failed on
  the Quinn findings. The two later advisories affect the same unchanged lock.
- Headlamp run 45819385751: CVE-2026-63402 in `i18next` 23.15.1
  (frontend) and 23.16.8 (plugin lock), plus CVE-2026-128085 in
  `go.opentelemetry.io/otel/sdk` and `/trace` v1.45.0.
  The recorded fixes are i18next 26.3.4 and OpenTelemetry 1.47.0.
  The Headlamp build inputs are unchanged from the Azure base; its earlier
  Headlamp job passed. That historical pass does not clear current findings.

## Verdict

Accept this exact source under the explicitly authorized single-maintainer
review exception; **not approved as dependency-clean, merge-ready or beta-ready**.
Technical gates and installed acceptance remain outstanding.

The maintainer was asked:

> May I record your sole-maintainer security sign-off for concierge source
> 7a791e8b020cf29e68acb3f593f9cbb0cb547d92 against Azure base 75411095,
> using the existing PR-specific approval mechanism for Azure/kars#638?
> This would waive only the second-human requirement--not dependency scans,
> other CI checks, or merge approval.

The maintainer selected **"Authorize my source-bound sign-off for PR638"**.
Pal Lakatos-Toth is the sole human approver. Copilot authored the port/repair
and used an automated security-review agent; neither is represented as a second
human. No historical approval is extended. Further implementation changes
invalidate this source binding.

Beta-approval-exception: Azure/kars#638; source=7a791e8b020cf29e68acb3f593f9cbb0cb547d92; base=754110953bf9cccbb7545a71782c38a59087ea2f

Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
