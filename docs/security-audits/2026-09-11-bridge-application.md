# Kars Bridge application publication record

Status: **draft assembly; no source sign-off or release approval claimed**.

## Scope

The complete application snapshot is imported into `bridge/`: Rust BFF, Next.js
Workspace/Console/Audit, optional Teams gateway, additive Helm chart, development
entrypoints, documentation and acceptance fixtures. The existing Kars CLI
remains a core component; no new mandatory Bridge CLI dependency is invented.

The snapshot source tree is `0a10472ed7e2940235b714276e8def3c0d9190f4`.
Only selected tracked product files were copied. Private Git history, runtime
configuration, cluster-specific deployment overlays and private image-release
workflows were not imported.

## Additive boundary

Core has no dependency on Bridge. The BFF keeps its independent manifest and
lockfile outside the core Cargo workspace. Web and gateway retain separate npm
packages. Root Bridge make targets are opt-in. The separate chart retains its
namespace-ownership and uninstall-retention controls; no core resource is adopted
or deleted by this source move.

Bridge component and native workflows run in Azure/kars. Native qualification
checks out core and Bridge from the same immutable commit, with contents-read
permissions and no image publication. The required source/UID, real admission,
TLS, CNI and cleanup assertions are not replaced by successful compilation.

## Publication checks and limitations

An offline Gitleaks 8.30.1 directory scan of the imported product snapshot found
no leaks. The session-local scanner's official release checksum was verified
before execution. This is not a comprehensive security approval.

The historical preview image defaults are not a claim that public images have
been published. Operators must build and select their own repositories.
Development identity examples are not production authentication defaults.
The foreground BFF launcher does not kill an unrelated listener or silently
leave a detached process.

At public candidate `cf7e0ed1b821a10cd0b24515a579429410954bfd`, Bridge CI
34606482468 passed all ten component/audit/add-on jobs. Native run 34606482569
failed: the API lane reported undeclared `params` in credential-source-writes,
and the runtime lane's initial grant was denied by private-consumption-grant.
Lifecycle and TLS/CNI acceptance were not reached. Subsequent changes require
fresh same-candidate qualification. Core credential and evaluator prerequisites
remain separate reviewed PRs; full governed Team execution is not declared
qualified by this import.

The existing capability-audit, crypto, stub and null-provider gates now include
Bridge's relevant production paths. CodeQL retains repository-wide analysis
with no path exclusions. Importing source does not exempt it from these gates.

The stub gate now filters once per file instead of forking per source line.
That performance-only step reproduced all 130 prior public findings exactly.
The subsequent syntax-aware correction distinguishes actual JS/TS fields and
JSX/Tailwind form syntax from unfinished-code markers using the CLI's existing
locked TypeScript parser. Comments, string values and standalone unfinished
declarations remain checked, including on the same line as a form attribute;
parse or tool failures fail the gate. No production path or marker pattern was
removed. Eleven regression fixtures cover scope, genuine markers, diff position,
CSS variants and fail-closed parsing. Three comments describing example URLs
and input/number presentation were clarified without changing runtime code.
This is a scanner-correctness change, not application source sign-off.

## Focused crypto integration review

A bounded review of the flagged digest/receipt paths found that receipt detail
verification did not enforce configured out-of-band pins, and whole-log
key-ID-only pinning trusted a mutable label without binding it to the key.
Both paths now use one resolver: public-key pins compare decoded Ed25519 bytes,
and ID pins require the controller's full SHA-256 fingerprint of those bytes.
Unconfigured verification retains its explicitly weaker cluster-anchor trust.
Malformed or mismatched configured pins fail verification rather than silently
falling back. The existing wire payload, DSSE framing, chain and checkpoint
formats are unchanged.

The same review found a missing-witness branch that incorrectly invalidated
an otherwise verified checkpoint. Witness presence remains advisory and no
longer controls checkpoint validity. Added regressions cover both verification
paths, matching/mismatching/malformed pins, a fully re-signed replacement-anchor
fork retaining the pinned ID, the controller fingerprint vector, and a valid
checkpoint without witness metadata. Rust execution is pending hosted CI;
syntax checks are not represented as test execution.

Hosted Bridge CI at `132e1be5` stopped at Clippy before executing tests: an
unused task-module import and incorrectly nested anchor tests were rejected.
The correction removes the import, places the tests at module scope and
requires their exact registration in the hosted test inventory before running
the complete suite. No lint suppression or assertion removal is used.
The corrected `09954106` passed actual BFF Clippy, required regression
registration and the complete Cargo suite in Azure run 34614267072.

## Permanent integration CI

Core Rust, CLI and Kind checkout configurations now omit `bridge/` and assert
its absence. CLI validation uses its committed lockfile. A disposable sparse
checkout of public `09954106` also resolved all eight core workspace packages
with locked offline metadata and no Bridge directory; this is not local
compilation evidence.

Shared change classification is fail-closed, includes both sides of renames,
and covers CLI/runtime/mesh/shipped-skill and unknown source paths. Only root
documentation can bypass native execution. Reusable CI retains full non-PR
qualification, including release caller events. Core-only Kind can omit
Bridge-only changes while paired native qualification still requires them.

Both component and native workflows now report stable aggregates. Component
acceptance includes every build, dependency/lock audit, secret/configuration
scan and real add-on job. Native acceptance rejects failed scope selection,
missing outputs and failed/cancelled/unexpectedly skipped required lanes;
documentation-only skips explicitly do not claim runtime execution.

Local Git/scope/aggregate regressions and the core Python harness passed.
Structural CLI checks are provisional because the shared local Vitest/YAML
cache differs from the committed lock; the new hosted locked CLI job must
qualify them. Required branch-check policy integration, complete supported-pair
contracts and standing-Team acceptance remain open. No protection was weakened
or changed to accept these unqualified workflows.

Other findings remain open: credential-review V1 derives a secret key with a
custom versioned SHA-256 construction and cannot inherit a plain content-digest
exception. Case-normalization of remediation manifest paths also needs a
separately versioned identity correction that preserves existing work.
Standard digest/receipt adapter extraction and its exact byte-equivalence
vectors remain required; no blanket crypto allowance is granted.

The imported application predates the core repository's file-size and copyright
header conventions. Several files exceed the unchanged 800-line new-file cap,
and the header gate reports missing Microsoft headers on imported files.
Existing author copyright notices are preserved, not reassigned by the import.
Neither a LOC exception nor an attribution exception is granted by this draft.
These gate failures and any crypto/stub findings must be resolved explicitly
before merge, along with genuine source-review sign-off.

No main-branch promotion, deployment, registry publication or change to the
original private repository's visibility follows from this draft assembly.
