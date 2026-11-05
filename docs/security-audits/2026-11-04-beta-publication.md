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

## Verdict

**Not accepted for release.** Required hosted verification and real author plus
independent-reviewer approval remain outstanding. No human signatures have been
supplied or inferred. Add genuine sign-offs only after the signers have reviewed
and approved the exact source scope and remaining protocol/taint classifications.
