<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Core and Bridge beta publication (unsigned candidate)

Date: 2026-11-04
Scope: Core, Bridge, runtime adapters, packaging, dependency locks and CI in Azure/kars#593.
Gated paths: `controller/src/`, `inference-router/src/`, `bridge/`, `cli/src/`, `runtimes/openclaw/`, `sandbox-images/`, `shared/`, `deploy/helm/`.

## Summary

This is an **unsigned review record, not approval**. The PR base observed when
preparing this record is `b2ec2094cc663d5b747fec7f3c9d37cce667a24a`; the previous
published head is `b00670024b671ee04c0522888749666543dd8a45`. Reviewers must bind
approval to the actual subsequent PR head, including its repair delta, rather
than extending an older audit's signatures. This record covers the proposed
publication scope; AI-assisted repair work does not constitute a second human
review.

The follow-up repairs address hosted compilation/lint and fixture failures,
patched JavaScript dependency graphs, bounded filesystem access, GitHub URL
construction, and legacy budget staging. CI now installs the image-pinned,
patched Hermes source before native tests and tests the locked Headlamp build.
AL3 packaging and the selected runtime versions remain in place.

## T1: New capability / attack surface? (YES/NO)

YES for the full beta publication: customer-facing Core/Bridge installation,
runtime integration and administrative workflows require review together.
The repair delta reuses those implementations rather than introducing a second
runtime, transport or accounting path. Pinned-source installation and dependency
resolution execute in hosted CI; they do not authorize runtime package installs.

## T2: Security-control change? (YES/NO/NEUTRAL)

YES. Repairs constrain file reads and staging writes, retain private-file and
symlink protections, preserve the fixed GitHub API origin, and restrict IMDS
origins while disabling redirects, retries and proxies for that request.
Authentication precedence, tenant isolation, governed egress and calendar
accounting remain required.

Azure IMDS necessarily uses HTTP at `169.254.169.254`; replacing it with HTTPS
would break the protocol. Literal loopback origins exist for integration tests.
Any surviving non-HTTPS alert needs a narrow reviewed classification, not a
blanket suppression. Legacy persistence paths are operator configuration, not
sandbox request input; their parent directories must remain service/operator
owned. CodeQL path-taint results still require hosted verification.

Public run identifiers used to address ConfigMaps or label SSE frames are not
cryptographic nonces. Their wire fields and deterministic hash regression vectors
are retained; clearer parameter names do not establish analyzer clearance.

## T3: Availability / fail-open risk? (REDUCED/NEUTRAL/INCREASED)

REDUCED for the repaired predictable temporary-file truncation and malformed
request paths. Missing or unsupported Hermes runtime metadata still fails
closed in production. Adapter-only skips are explicitly not native execution
proof. Actual usage accounting and prior failed evidence remain intact; no
production funding policy or release gate is waived.

## Hosted successor correction — 2026-11-05

Published `6efe3a62e8899e791ed26a6fb069a87448ab61ba` failed the hosted Rust job
at `cargo fmt --all -- --check`, before compilation: the long rebind-test name
formatted differently on the hosted toolchain. Shortening that test name keeps
its complete default-worker lifecycle assertions unchanged. Whole-workspace
formatting now passes locally; Rust compilation and native acceptance remain
hosted gates, not inferred successes.

The retained prior Kind prepare phase lasted 257.292 seconds, so its 180-second
stage-command ceiling, not its 650-second phase budget, expired. The subsequent
workflow artifact contains API connection refusals after cluster teardown.
Staging now emits fixed phase labels before awaiting operations; the harness
retains only validated labels on timeout and filtered built-in Pod status in
`kars-system` and `headlamp` before teardown. It omits raw messages, logs,
container specifications and command output. These are diagnostics, not proof
of the underlying stall or a relaxation of Helm's wait/readiness/deadline rules.

Validation: 77 CLI staging/authority tests and 35 harness/diagnostic tests pass,
including actual child termination and private-output rejection. The first new
CLI assertion incorrectly expected Helm3 `--wait` from the existing Helm4
fixture; it was corrected to require `--wait=legacy`, with the original 8-minute
Helm timeout unchanged. The failed local test receipt is retained. New diagnostic
regressions are included in the hosted public-schema privacy job.

## Verification

- `python3 -B -m unittest discover -s ci/tests -p bridge_contracts_test.py`:
  16 passed for the current workflow contracts.
- `python -B -m pytest runtimes/hermes/tests -q --tb=short` in the local adapter
  environment: 1521 passed, 206 skipped, 29 subtests passed. This Python 3.14
  environment is not genuine native Hermes qualification.
- `ruff check runtimes/hermes`: passed. Scoped Rust formatting passed; Rust
  compilation, Clippy and new regression execution remain assigned to GitHub.
- Focused JS/TS repair suites and CLI/OpenClaw/mesh typechecks passed. These
  results do not validate every newly resolved dependency graph.
- Previous published-head Secret Scanning and Dependency Review succeeded.
  CI, Bridge CI, ci-gates and native qualification failed. CodeQL analysis
  completed but its findings check reported 61 annotations. Those results do
  not clear this newer candidate.
- Exact repair-candidate secret scanning, independent source review, hosted
  dependency/CodeQL/image checks and supported native execution must be bound
  to the publication head. Local advisory transport failures are retained;
  they are not a zero-advisory result.
- No beta release or fresh-cluster installation acceptance is claimed: 0/2.

### Follow-up: Headlamp pins, Dex versions and native schema harness (2026-11-05)

The frontend recipe now pins Vite 6.4.5 and fast-uri 3.1.9 instead of vulnerable
6.4.3/3.1.7. GitHub's advisory lookup returned no matching advisories for these
exact replacement versions. The generated production-build manifest now removes
unused Storybook/RSBuild development roots, and direct maintained Buffer,
process and stream polyfills replace the broad dependency path through crypto.
Global injection and builtin aliases remain; crypto imports fail rather than
being stubbed. The regenerated lock rejects deepmerge and elliptic at any depth.
This repairs the dependency paths instead of ignoring their LOW findings, but
fresh hosted lock resolution, image construction, scans and browser behavior
remain unqualified.

The image exports its generated manifest, canonical lock and Vite configuration,
bound by SHA-256 in frontend evidence. The existing Headlamp CI job now builds
the actual AL3 image without publishing, verifies those exact exported inputs,
and runs the maintained lock verifier. Pinned Trivy scans the image, generated
lock, frontend inventory and plugin at every severity, including unfixed issues.
Reports and build evidence are retained on failure. Existing plugin tests and
npm audit remain; a successful source test is not image or browser acceptance.

Hosted run `42013412886` on `9d255ad6` exposed stale harness expectations, not
permission to remove admission guards. The credential proof now includes all
four split GitHub-authority policies alongside the three-validation grant
policy. The shared SRE inventory includes report authority. Local regression
checks parse the real rendered Helm documents rather than substituting synthetic
policy documents after rendering. The obsolete `--json-params-candidate` fallback
was removed: production already preserves arbitrary JSON action parameters, and
the maintained production/native proof remains mandatory.

Hosted Bridge run `42013412900` exposed a separate test-only version parser
failure on a legitimate Go pseudoversion. The no-downgrade assertion now respects
semantic-version prerelease ordering, including pseudoversions and ignored build
metadata; exact reviewed/requested lock equality remains mandatory. No Dex source
or lock was changed by this repair.

- `PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=tests/e2e python3 -B -m unittest credential_policy_schema_test sre_authority.bootstrap_probe_test sre_authority.binding_probe_test sre_authority.harness_test`: 98 passed.
- `node --test sandbox-images/headlamp/build-frontend.test.mjs`: 9 passed, none skipped.
- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p '*_build_evidence_test.py'`: 7 exported-input binding tests passed.
- `KARS_DEX_PATCH_NETWORK=1 PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s bridge/idp/tests -p 'test_*.py'`: 63 passed against checksum-verified upstream source; no Go compilation.
- Scoped `git diff --check`: passed. No native execution or image qualification
  is implied by these local checks; the successor needs fresh hosted evidence.

### Follow-up: native stack failure and Rust scanner controls (2026-11-05)

Native run `42013412846` on `9d255ad6` passed the three cold API/admission
installs, but runtime bootstrap failed with BFF HTTP 502. Privacy-filtered
previous controller logs classify a stack overflow; the process exited 139 and
restarted twice. At failure, the paused Team's principal was Ready/Idle, while
the first legacy sandbox had no status or private namespace. These observations
narrow the failure to sandbox activation, not a successful installation.

The sandbox entry now uses the existing Team reconciler's heap boundary instead
of embedding its entire async state machine in the watcher and metrics futures.
A regression bounds both entry futures and actually polls reconciliation on a
default Tokio worker, retaining the existing finalization I/O assertions. No
stack size, pod memory limit, admission rule or reconciliation operation changed.
Rust execution and a fresh full native journey must still establish whether this
repair eliminates the observed crash; formatting is not that proof.

The four budget-persistence CodeQL alerts trace configured State through closure
captures. The maintained upstream correction is
[github/codeql#22917](https://github.com/github/codeql/pull/22917), at commit
`fdf5c8481ffef04e04c7376167fb5ce25aae8453`. No immutable released bundle containing
that fix was available when checked. The Rust matrix leg therefore temporarily
uses that commit's **complete security-and-quality suite**, not a suppressed query
or a newly declared sanitizer. JavaScript/TypeScript and Actions keep their
existing suites and all analysis categories remain unchanged.

Before initializing Kars analysis, the hosted job verifies the official 2.27.4
bundle's SHA-256, resolves the corrected source library, checks that every bundled
Rust query ID is retained, and runs the unchanged upstream CWE-022, lambda, local
and model expectations. These include request-derived path-injection positives
and configured-State negatives. Controls and their databases live outside the
Kars source root; only diagnostic evidence is uploaded as a separate artifact.
Failure stops analysis rather than silently reverting or dismissing alerts. The
control toolchains are the upstream revision's pins, not a moving nightly. A
separate Kars regression tests that request data cannot select persistence paths.

- `bash -n ci/codeql-rust-controls.sh`: passed.
- `shellcheck ci/codeql-rust-controls.sh`: passed.
- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p 'codeql_rust_controls_test.py'`: 7 coverage-guard tests passed, including missing queries and stale-library rejection.
- Workflow YAML parsed with unique-key validation. These local checks do not run
  CodeQL, compile the source suite, clear alerts or execute the Rust regression.

The unreleased source-suite/CLI combination is still subject to hosted
compatibility and behavioral qualification. Remove this temporary override and
its source-specific controls, restoring bundled defaults, when an immutable
official bundle containing the upstream fix passes the same coverage and
positive/negative controls. Do not mark the four alerts fixed until the corrected
full Kars analysis proves it.

## Follow-up: governed credentials and Headlamp scanner coverage

The published `c6a255af` revision passed Bridge CI, all three cold API/admission
installs and the six unchanged upstream CodeQL controls. Its native qualification
still failed: the first legitimate reviewed v2 sandbox did not acquire a
credential-bundle UID annotation before the controller entered stack-overflow
CrashLoopBackOff. Earlier v1 projection and invalid-v2 rejection cases passed.
This narrows the failing path but does not identify the exact overflowing frame.

The successor adds heap boundaries around shared credential-bundle preparation
and sandbox dispatch, without changing grant/source identity checks, bundle
ownership, UID/resourceVersion fencing or runtime isolation. Its new regression
uses the existing HTTP fixture on a default Tokio worker stack, checks bounded
public futures, and exercises suspended-to-active bundle reuse. Formatting has
passed; Rust compilation, the regression and elimination of the live controller
crash still require hosted proof. No stack-size increase or native build-profile
change is used to mask the failure.

Headlamp source repairs address the **67 advisory occurrences** retained across
image, frontend-lock, frontend-SBOM and build-tool example reports, including
CRITICAL, HIGH, MEDIUM, LOW and UNKNOWN findings. These are not 67 distinct
vulnerabilities and are not yet clean-scan results. Repairs remove unused lint
paths, update retained frontend dependencies and backend Go dependencies, and
apply a version/checksum-fenced patch to both RabbitMQ build-tool examples. The
CLI fixture now tests the maintained frontend preparation rather than a removed
upstream Vite configuration. The Helm upgrade also raises the Kubernetes client
floor; actual backend compilation and behavior remain required.

The retained plugin filesystem scan contained only two Go module inventories
and omitted npm development dependencies. Both filesystem scans now request
`--include-dev-deps`; a fail-closed check requires a populated npm
`package-lock.json` inventory. This checks inventory presence, not exact
package-by-package completeness. It accepts the retained frontend report and
rejects the retained Go-only plugin report. All severity levels, unfixed findings
and the scanner's nonzero failure status remain enforced. Expanded coverage may
expose additional findings that also require repair. CI now runs the patch
contracts and compiles both patched examples with `go test -mod=readonly ./...`.

CodeQL controls passed against the corrected source, but initialization failed
while attempting a second external clone. Analysis now selects a repository-local
suite importing the verified source pack; controls resolve that same suite,
require every selected query to reside in the exact source tree, preserve the
full baseline query set, and reject stale library resolution. Search-path options
bind action database initialization, query execution, interpretation and query
resolution to the verified checkout. The local suite avoids relying on an
unverified absolute external `queries.uses` path. Actual pinned-action execution
and full Kars analysis, including the four HIGH alerts, remain unproven.

Focused local evidence for these repairs:

- Headlamp frontend/build-tool contracts: 15 passed.
- Headlamp CLI fixture selectors: 2 passed; 64 intentionally unselected.
- Headlamp build-input and npm-inventory checks: 14 passed.
- CodeQL coverage and action-binding checks: 13 passed.
- SRE authority tests for the repaired historical schema assertion: 195 passed.
- Shell syntax, CodeQL ShellCheck, unique-key workflow/suite YAML parsing,
  targeted Rust formatting and scoped whitespace checks passed.

These checks do not establish clean rebuilt images, example/backend compilation,
Headlamp browser behavior, complete native qualification or fresh released Kind
installation. Heavy builds remain on GitHub. Beta installation acceptance remains
**0/2**, and genuine human audit signatures remain **0/2**.

## Follow-up: published runner failure and remaining checker dependency

Published `c197c722` passed secret scanning, dependency review, Bridge CI and the
IdP lock candidate. All three cold API/admission installs passed. Full CodeQL
analysis completed; the four prior Rust path-injection instances are marked
fixed. Of 27 new Rust unsafe-deserialization alerts, one identified a real
fail-open mesh identity boundary, nine participate in a shared chat-ingress
policy bypass, two have no established independent exploit (including an
unresolved Responses-provider differential), and 15 have no demonstrated
exploit at the reported sink. None of these
dispositions is a suppression or a clean-scan claim. Main CI's Rust and CLI
jobs passed; Headlamp failed before image building, examples or scans, and
Kind E2E also failed. Those failures remain retained.

The Headlamp `npm test` command ran all six Node patch tests and 84 component
tests successfully, then failed because Vitest also collected the Node-only
patch suite and attempted to bundle `node:test`. The runner repair excludes only
that file from Vitest while preserving Vitest's default exclusions. The same
suite remains mandatory through `node --test` before Vitest in `npm test`; its
manifest contract now asserts both runner commands. No test is removed.

Local `cd tools/headlamp-plugin && npm test` with this repair passed six Node
and 83 component tests but failed the middleware traversal regression. Installed
local Vitest4.1.9 and middleware6.1.3 differ from the candidate lock's4.1.12 and
7.4.6. This is a retained failed overall run, not a full-suite pass. No heavy local
reinstall was performed; fresh hosted execution with locked dependencies remains
necessary.

Five of six default-branch Dependabot alert records have fixed candidate
versions. LOW `GHSA-gcqx-w48m-w2h4` / `CVE-2026-93753` remains in deepmerge4.3.1,
requested by the Storybook Webpack TypeScript checker. No verified fixed upstream
checker/tool release was found. The user authorized preparation of a minimal,
attributable checker-consumer patch that preserves Storybook and TypeScript
checking. The prepared local checker archive replaces deepmerge with
`deepmerge-ts@8.0.2`, preserving upstream checker identity, MIT attribution,
worker code and immutable provenance. The repair's focused tests passed 26/26,
including constructor/schema, merge compatibility and prototype-poisoning
regressions; deterministic reproduction and integrity rejection also passed.
These are not installed compiler or shipping-image acceptance.

The required hosted command `node --test checker/compile.node.mjs` runs after
clean installation and tests real Webpack/TypeScript workers through valid
compilation, an expected TS2322 diagnostic, and recovery. Its local prerequisite
correctly rejected the stale unpatched installation before starting a compiler;
only syntax and CI wiring have been checked locally. Clean installation,
compiler execution, full plugin build and all-severity scans remain pending.
No scanner suppression, identity masking or capability removal is authorized.

## Follow-up: native Team credential rebind stack boundary

Hosted native run `42028650052` passed ten cases, including actual source
projection/runtime and selected-source revocation/UID fencing. The next case,
Team rebind continuity, failed awaiting current reattested credentials and
resumed execution. The controller restarted four times with exit139 and a
stack-overflow diagnostic. Task generation3 was governance-Ready, but execution
was Degraded; this does not qualify crash elimination or native acceptance.

The repair retains the existing Task reconciliation and credential-resume
state machines, placing their async state behind heap boundaries like the
existing Sandbox and Team entries. The full credential-rebind regression now
runs on a default Tokio worker stack and checks both entry-future sizes, while
retaining UID, namespace data, quiescence, attestation and resumed-deployment
assertions. Rust parsing passed; compilation, regression execution and the
unchanged hosted native journey remain pending. No credential fence or runtime
stack limit was relaxed. Released fresh-Kind acceptance remains 0/2.

## Follow-up: mesh identity parsing must fail closed

Rust alert 1073 identifies a HIGH-severity authorization bypass in registry
registration. `enforce_identity_claim` previously forwarded bodies when Rust
JSON parsing failed. A signed registration can include a deeply nested ignored
field that exceeds Rust's recursion budget while the pinned Python registry
accepts the request and drops that field. Proof of possession of the submitted
key does not authorize another sandbox's display name or discovery capability.
This is source tracing plus parser/schema/signature reproduction, not a live
end-to-end registry attack.

The prepared shared-router repair rejects parse errors with HTTP400 before
upstream dispatch and without echoing request contents. A regression constructs
a valid Ed25519 registration proof, adds a cross-name claim and a 140-level
ignored field, and exercises the actual router. It requires malformed and
over-depth requests to be rejected, ordinary cross-name claims to remain
forbidden, and only an allowed registration to reach the test registry. Both
Hermes and OpenClaw continue using the same registry boundary; no SDK or
cryptographic protocol is replaced. Rust syntax passed; compiled execution
remains pending on GitHub.

Follow-up identified a MEDIUM-severity chat-ingress policy bypass. With a
restrictive exact-model deny followed by `inference:*` allow and no lifetime
admission binding, an ignored 140-level field makes Rust authorize an empty
model while forwarding the original body. Supported-provider source evidence
at Ollama commit `9f3a37fd36bf1c46cc86a47bc5372535f8ee3547` shows its Go binder
accepts that depth, ignores the extra field, and preserves the submitted model.
This is not a claim that this provider revision is deployed or live-tested.
The same ingress bypasses the explicit requested-token cap and denied-tool
schema filtering; actual unauthorized tool execution is not established.

The prepared repair parses chat JSON before policy/model/token/tool decisions
and returns HTTP400 on failure. It retains the same parser limits and ordinary
missing-model behavior. An actual-router regression installs a real restrictive
AGT profile, disables no production gate, uses the supported absent-lifetime-
admission state, and requires zero capture-upstream requests for deep JSON,
malformed bodies, denied models and excessive token requests. Shallow allowed
requests still forward, and a shallow denied-tool schema is removed. Rust
syntax passed; compiled execution remains pending on GitHub.

Alerts 1063, 1064, 1067, 1068, 1069, 1072, 1078, 1079 and 1081 participate in
that shared chat path, not nine separate vulnerabilities. Enabled lifetime
contract and record-only admission already reject the payload before dispatch;
calendar-only accounting does not substitute for model authorization. Alert
1077 has no independently established usage-accounting bypass. Alert1084's
Responses-provider parser differential remains unresolved: that route rejects
Ollama, so the chat proof must not be transferred to Azure/OpenAI without
provider evidence. The other 15 alerts (1061, 1062, 1065, 1066, 1070,
1071, 1074, 1075, 1076, 1080, 1082, 1083, 1085, 1086 and 1087) target inert JSON
or string-vector deserialization with no demonstrated deserialization gadget
or control bypass. No verified upstream scanner-model correction is available.

The unpublished `eb619db6` candidate passed independent 20-path security review
and focused regressions, but failed copyright coverage and an archive-scanner
positive control. It was not published. The follow-up adds precise legal
coverage and the missing first-party README header without modifying upstream
license bytes; all 39 copyright tests pass. Explicit safe expansion and scanning
of all 171 checker-package files passed with zero findings and a detected
archive canary. The original failed control remains retained; archive coverage
is not inferred from scanning a Git binary patch. Exact successor validation
and review remain required.

The unpublished `636eb914` successor passed its focused Python, Headlamp and
CLI checks, copyright coverage and controlled secret scans, but review found
an undeclared `http` test import and the production crypto gate rejected the
signed mesh test. The successor uses Axum's existing HTTP re-export and makes
the exact mesh regression explicitly test-only. The gate permits that one
reviewed Ed25519 test path only with its module-level `cfg(test)` guard; other
paths and production signing remain blocked. All seven crypto-gate tests pass,
including missing-guard and neighboring-path rejection. Rust formatting passes;
compiled regressions remain pending. Both failed candidates and their receipts
are retained, not rewritten.

Hosted Kind E2E run `42028649943` also remains failed. Its historical SRE
migration fixture timed out in `authority stage` apply after the dry run passed.
The harness bounds each command by the smaller of its command timeout and the
remaining phase deadline; the log alone does not identify which expired.
Ready SRE pods do not establish stage completion. No timeout has been enlarged
and no migration gate has been bypassed; the stage failure still needs repair
and fresh hosted execution.

## Verdict

**Not accepted for release.** Required hosted verification and real author plus
independent-reviewer approval remain outstanding. No human signatures have been
supplied or inferred. Add genuine sign-offs only after the signers have reviewed
and approved the exact source scope and remaining protocol/taint classifications.
