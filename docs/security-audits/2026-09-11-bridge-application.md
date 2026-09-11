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

### Content-digest adapter qualification

The standard SHA-256 content/receipt uses now route through a small BFF-local
`providers/signing.rs` adapter. Callers retain their original input bytes,
framing, case policy and output widths: full trace/objective/review/receipt
digests, eight-byte GitHub connection suffixes, sixteen-byte artifact addresses,
and existing engineering identifiers. The duplicated GitHub connection recipe
is shared by grant validation and the route surface.

Independent known-answer regressions cover standard SHA-256, raw subject case
and whitespace, sorted compact review JSON with null/absent distinctions,
decimal/pipe receipt chaining, and short artifact IDs. CI requires their actual
registration before running the complete suite. Hosted execution is still
pending in the initial extraction; syntax/metadata checks are not a substitute.
Corrected `1289a681` then passed actual Clippy, required known-answer registration
and the complete Rust suite in public run 34641158050.

After that qualification, only the 44-line standard digest adapter is registered
in the existing crypto-wrapper allowlist. File entries now match exactly rather
than granting accidental prefix access to lookalike paths; intentionally listed
directory prefixes retain their previous scope. Four actual-Git regressions
cover these boundaries. No directory-wide Bridge allowance is added.
The V1 secret-key derivation is not reclassified as content hashing, and
unreviewed application/provider paths remain rejected. Receipt signature
verification and outstanding source-review requirements are unchanged.

### Receipt primitive adapter candidate

The existing Ed25519 verification calls now share a separate
`providers/receipt.rs` wrapper, including calls previously indented inside route
functions. It retains the same standard-base64 decoding, exact 64-byte signature
length and `ed25519-dalek` verification operation. Anchor pins, signed payload,
DSSE framing, payload-binding checks, chain/checkpoint comparisons and advisory
witness semantics remain at the existing callers.

Signing used to construct test receipts is confined to a `cfg(test)` helper.
An RFC 8032 known-answer signature was independently verified with Node's crypto
implementation, and the Rust regression also rejects changed messages/keys,
invalid signature lengths, malformed encodings and trailing whitespace.
Existing fully re-signed attacker-anchor regressions remain required.
This new wrapper is not yet allowlisted or Rust-qualified; the V1 credential
key-derivation review remains separate and unresolved.
The remaining function-local skill-package and egress-approval content hashes
also use the qualified SHA adapter without changing their canonical input,
`host:port` framing or identifier widths. They are not left hidden from the
top-level import scanner.

### Explicit version-one credential-key compatibility boundary

The existing credential review key recipe is isolated in
`providers/credential_review.rs`, separate from the allowed content-digest
adapter. It remains the exact versioned SHA-256 derivation over the domain,
literal NUL and raw principal-secret bytes; it is not described as HKDF or as a
plain content hash. Independent compatibility vectors preserve whitespace and
the existing derived-key bytes.

This is an architectural extraction, not a claim of new cryptographic assurance.
HS256 algorithms, audiences, five-minute expiry, three-submission bounds,
operator identity and existing review/continuation/value-tag formats are
unchanged. An algorithm change would require a separately versioned migration
covering active continuation receipts and rolling upgrades, not silently
invalidating their tags. The new key adapter is not allowlisted or approved by
this record; explicit review and hosted compatibility proof remain required.

The imported application predates the core repository's file-size and copyright
header conventions. Several files exceed the unchanged 800-line new-file cap,
and the header gate reports missing Microsoft headers on imported files.
Existing author copyright notices are preserved, not reassigned by the import.
Neither a LOC exception nor an attribution exception is granted by this draft.
These gate failures and any crypto/stub findings must be resolved explicitly
before merge, along with genuine source-review sign-off.

No main-branch promotion, deployment, registry publication or change to the
original private repository's visibility follows from this draft assembly.
