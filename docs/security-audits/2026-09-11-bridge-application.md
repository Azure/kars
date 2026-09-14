<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Kars Bridge application publication record

Status: **Bounded source-approved under explicit maintainer delegation.**
Current-head technical/security gates and operational acceptance remain required;
this is not beta, deployment or release approval.

## Current delegated source attestation (2026-09-14)

Reviewed source: `aa4056358561a2e02928fe5346f54b733ab8eb1a`.
Integration base: `b5ad6791f9085e908cbf3d16b5de9021eb4b43a7`.

The maintainer authorized publication sign-offs after focused-agent review
rounds in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
Copilot exercises the author attestation under that delegation; this does not
claim personal code review by the maintainer. The original integration review
and three repair-closure rounds were performed in the separate read-only AI
context `bridge-publication-final-review`
(`b45996cb-cc53-443a-9c88-43112e10b551`), not by a second human or the
implementation contexts. The reviewer supplied unsigned source-review evidence;
the attestations below are recorded under the disclosed maintainer delegation.

### Reviewed boundaries and closure

The source-and-caller review covered authenticated Home intent, proposals and
launch; owner/admin authorization and credential-store boundaries; standing-Team
backlog and recurring intake; human review/revision; agent/run/activity/artifact/
PR attribution; receipt signatures, immutable pins and inclusion evidence;
web/BFF and Teams SDK contracts; optional Helm lifecycle and root additivity.
It was not exhaustive line-by-line assurance of every module.

The review found six concrete issues. Their repairs are committed in
`8f353535`, `366f02c8` and `aa405635`; none remains unresolved in the three
subsequent scoped closure reviews:

| Finding | Verified repair boundary |
| --- | --- |
| Active HTML/SVG artifacts inherited Bridge origin | Active/unknown content downloads unchanged; sandbox CSP, nosniff and no-store protect artifact responses and survive the same-origin proxy. Passive previews and ownership/missing-file denials remain. |
| Operators bypassed admin-only budget/retention controls | Web gates use verified session roles without an SSO development-cookie fallback. BFF middleware and handlers independently require admin; intended operator reads and other authorized operations remain. |
| Gateway initialization used incompatible positional SDK calls | Calls use the locked Kubernetes SDK request-object contract and SDK-derived types, without compatibility casts. Real HTTP fixtures exercise paths, bodies, errors, retries, bookmarks and restart/binding behavior. |
| Completed remediation suppressed later same-package advisories | Structured source evidence drives follow-up work while preserving completed and in-flight IDs, nonces, inputs and PR history. Pending refresh, withdrawal, partial scans, legacy metadata and final-merge races are covered. |
| Separate-namespace gateway targeted the wrong workspace | Commands/watches and bounded read authority use the configured core namespace; conversation storage and ServiceAccount remain add-on-local. Same/separate namespace and owned removal behavior are covered. |
| Filenames were presented as recorded producer evidence | Explicit matching producer metadata takes precedence. Filename-only evidence is inferred per file and cannot override a different recorded producer, assignment nonce or incomplete persistence. |

Production coverage transfers from the inspected worktree deltas to the published
commit on the parent's source-equivalence verification. The subsequent test-module
and pure `ArtifactSummary` extractions were notified as mechanical, with preserved
test/helper bodies and seven matching before/after rendering trees; they were not
independently re-reviewed as new functionality. Actual hosted registration,
compilation and execution below qualify their final wiring.

### Actual current-source execution

[Bridge CI 34897179126](https://github.com/Azure/kars/actions/runs/34897179126)
passed all eleven jobs at the exact source above:

- BFF job `104154078482` passed strict Clippy and all **259 library plus 4+2
  integration tests**, with none ignored. All **25 new regressions** actually
  executed successfully: five admin, four artifact and sixteen recurring-intake
  cases. All 37 explicitly required regression names were registered.
- Web job `104154078446` passed all **50 contracts**, typecheck/lint, the actual
  **Next 16.3.3** production build and immutable-root container startup.
- Gateway/add-on job `104154078031` used locked **Vitest 4.1.11**, passed all
  **84 normal cases**, then ran and passed all **three actual Kind lifecycle
  cases**. Real SDK HTTP and controlled Helm-removal regressions are included.
- Dependency/lockfile audits, secret/configuration scans and the required
  component aggregate passed.

The parent retrieved and checked these public logs, including each of the 25 new
Rust pass lines. The independent reviewer did not independently inspect the
hosted logs; its source conclusions are not being represented as test execution.
Earlier local Next/Vitest cache mismatches and local Kind skips were not counted
as locked/native proof. No local Rust build below the disk floor was used.

### Remaining conditions and scope limits

Exact `aa405635` subsequently passed
[full core CI 34897179070](https://github.com/Azure/kars/actions/runs/34897179070):
all 21 jobs, including **184/184 Kind cases**, actual historical schema migration
and the public API/CEL preflight. Its
[native run 34897179224](https://github.com/Azure/kars/actions/runs/34897179224)
passed all **18 runtime cases**, three cold API installs and the required
aggregate. Artifact `10370845677` binds both revisions to that source, reports
rotation in 85.68 seconds, and sets runtime/network-policy qualification true.
The lane remains controlled-no-LLM/no-active-SRE; active-SRE-combined qualification
is false. These results do not demonstrate the complete live standing-Team/
GitHub journey.

The final application assembly now includes actual protected integration
`e3d61351100e1091b0fc8b5220ac36e75a81848d`, which merged qualified Azure/kars#554.
Composition `74d959b7` retained the entire pre-merge application tree unchanged;
only duplicate CI insertion/test ordering and a documentation heading conflicted.
This audit update does not change production source. The final current-base PR
head must still establish its own required checks before application merge.
The earlier API/CEL public-CRD wait failure is retained; bounded diagnostics do
not establish its cause or turn it into a passing result.

CodeQL analysis `34897179154` completed, but its alert check remains failed on
IMDS alert 827. The fixed link-local HTTP request matches Azure's documented
host-local protocol and uses no proxy or redirects; its disposition remains
open. No absent-user answer was treated as authorization to dismiss it, and no
scanner suppression or unsupported HTTPS substitution was made.

Core credential/private-observation closure belongs to the separate review and
records attributed to context `7f37ca6e-6256-4dd1-8064-d8fe6ac2fc92`, not this
application review. Live LLM-driven standing-Team work, end-to-end GitHub
installation/review/revision, active-SRE combined operation and H100 acceptance
remain unproven here. Actual merging remains a human GitHub action; no automatic
merge authority or new storage/transport framework is introduced. `/sandbox`
remains ephemeral `emptyDir`.

This attestation does not waive checks, grant a GitHub review or merge bypass,
approve main/release/image promotion, or authorize customer/H100 deployment.
Historical failures and draft statements below describe their original candidates,
not retroactive approval of those revisions.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>

## Historical assembly and earlier qualification

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
invalidating their tags.

Exact `1289a681` through `a9a2d0da` source review found no new cryptographic or
v1 wire-compatibility defect in these extractions. Public run 34649106554 passed
Clippy, the required independent known-answer inventory and the complete Rust
suite. Only the two exact wrapper files are now registered: a standard Ed25519
adapter and an **explicit legacy v1 secret-derivation compatibility exception**.
This does not relabel the legacy recipe as HKDF, extend its scope to directories,
or imply whole-application approval.

The review also noted that the unchanged hand-written tag comparison lacked
a vetted constant-time implementation contract. It now uses `subtle::ConstantTimeEq`
through the key adapter, preserving exact byte/length equality and all tag wire
formats. `subtle` was already locked transitively at 2.6.1; only its direct
dependency edge was added, without version changes or network installation.
Every-byte, prefix and length regressions are required in hosted inventory.
That small follow-up still requires fresh Clippy and full-suite execution.

The imported application predates the core repository's file-size and copyright
header conventions. Several files exceed the unchanged 800-line new-file cap,
and the header gate reports missing Microsoft headers on imported files.
Existing author copyright notices are preserved, not reassigned by the import.
Neither a LOC exception nor an attribution exception is granted by this draft.
These gate failures and any crypto/stub findings must be resolved explicitly
before merge, along with genuine source-review sign-off.

No main-branch promotion, deployment, registry publication or change to the
original private repository's visibility follows from this draft assembly.
