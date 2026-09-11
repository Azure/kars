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

The imported application predates the core repository's file-size and copyright
header conventions. Several files exceed the unchanged 800-line new-file cap,
and the header gate reports missing Microsoft headers on imported files.
Existing author copyright notices are preserved, not reassigned by the import.
Neither a LOC exception nor an attribution exception is granted by this draft.
These gate failures and any crypto/stub findings must be resolved explicitly
before merge, along with genuine source-review sign-off.

No main-branch promotion, deployment, registry publication or change to the
original private repository's visibility follows from this draft assembly.
