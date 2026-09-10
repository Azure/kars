# Managed MCP capability audit — 2026-09-08

Status: **Source audit approved under explicit maintainer delegation**.
Exact-head hosted and native qualification remain required before merge.

## Current delegated review and qualification

The maintainer authorized publication sign-offs after additional focused
reviews, recorded in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The second attestation below is delegated AI review, not an assertion that a
second human signed this audit.

The approved production source was qualified at
`fbd01597ce1cb86fff6df71d21dc2bf07abaf528`, based on
signed governed-services source `6b34d5ea`. A focused AI security review of
that comparison reported no vulnerabilities in its reviewed scope. The
earlier caller-identity and lifecycle findings remain repaired as described
below. No source branch containing private SDK/security probes is included.

The additional tool-result compatibility fix preserves all five standard MCP
content kinds, embedded text/blob resources, metadata, annotations, structured
results and `isError`. Resource/icon URIs are not fetched. Unknown kinds and
malformed required fields remain protocol errors, and existing byte limits,
authentication, session isolation and policy checks are unchanged.

Actual local qualification at `fbd01597` passed 249 router MCP library cases,
17 managed-MCP/governed-telemetry integration cases, strict paired all-target
Clippy and formatting. The guarded shared target observed at least 10.01 GiB
free. Controller runtime tests were not rerun in this content-focused batch.
Earlier restack checks passed Helm, 108 Python cases, 22 CLI cases and type
checking with compatible cached tooling; these are not exact-lockfile or
native H100 deployment claims.

This branch has no MCP output Content Safety hook, and none is introduced or
claimed by the fix. Scanner regressions exercise existing AGT `scan_value`
visibility of serialized text, metadata and structured fields. Base64
validation establishes encoding only, not the safety of binary contents.
The focused reviewer did not independently rerun tests. Current-base hosted
Rust and native lifecycle/protocol acceptance remain mandatory, with no
technical gate, finding, identity fence or policy denial waived.

## Native client correction and current qualification (2026-09-10)

Published `211d0d4e209924b7cec72cdbfd00b24c96dfeb60` passed its hosted Rust
and other checks, but native job
[102915586372](https://github.com/Azure/kars/actions/runs/34487526141/job/102915586372)
failed with 168 passes and two reports of the same MCP gate failure. Both
same-named servers reached Ready with distinct UIDs and current generations;
the actual routed catalog did not converge. Legacy and fresh SRE acceptance
passed. That native result remains a failure.

The Python fixture advertised only `application/json`, whereas the real
Streamable HTTP router requires both JSON and `text/event-stream`. The
JSON-only request is deterministically rejected with 406. The old native
collector discarded HTTP status, so that original run itself did not retain
the 406; the exact fixture/header defect was subsequently reproduced.

Test-only repair `c56b90d4963f85df7442e3e6eafb8b99808f443e` corrects the
client header, retains bounded catalog-stage diagnostics and adds regression
coverage. Actual guarded execution passed 249 MCP library cases, 18 integration
cases, six Python cases, strict paired all-target Clippy and formatting.
The additional integration case executes the actual Python fixture client
against the real router and retains rejection of the unsupported Accept
header. Minimum free space was 9.04 GiB above the unchanged 8.5 GiB floor.

Current source `81ee4fe02f0a99f069ce636710e7308d30881a73` also incorporates
services `920f9f0e` and the actually landed SRE ancestry. That merge changes
only three CLI test-harness files; the 39 forwarded cases and typecheck pass
with the existing compatible cache. All production bytes remain identical
to source-approved `211d0d4e`. No authentication, transport guard, readiness
requirement, UID fence or native deadline was changed.

Fresh exact-head hosted/native lifecycle convergence is still required.
The passing Python-to-router regression is not substituted for the complete
Kind lifecycle, policy, image and cleanup acceptance.

## Transitive dependency repair after the guarded landing check

Current-base source `6f513c49d0be1b38b6464dbc6f5959b561d65a41` subsequently
passed full CI `34497050687`, including native lifecycle/protocol acceptance.
The landing guard still stopped before any temporary review allowance or
merge: ten unresolved GHAS/Trivy conversations identified genuinely unpatched
Everything image dependencies. Green high/critical gates were not treated as
permission to waive medium/low findings.

The compatible exact replacements are:

| Package | Previous | Patched |
|---|---|---|
| `@hono/node-server` | 1.19.14 | 1.19.15 |
| `hono` | 4.12.29 | 4.13.5 |
| `qs` | 6.15.3 | 6.16.0 |

These address alert743, alerts791-797 and alerts801-802. The server package
remains2026.7.4, fast-uri3.1.6 and ip-address10.3.1 remain pinned, and all
unrelated lock entries and dependency edges are byte-for-byte unchanged.
Only the version, canonical registry URL and integrity value changed in each
of the three reviewed lock entries.

The local offline cache lacked the required Hono metadata. No local network
workaround or manually invented integrity hash was used. A temporary,
read-only, fixed-branch workflow generated the lock normally on GitHub:
[run34506209770](https://github.com/Azure/kars/actions/runs/34506209770),
exact source `30d89dc23ddadf2afc9805251e7f74593ea92814`.
Node22.23.2/npm10.9.8 generated it, a clean script-disabled npm install
verified package integrity, npm audit reported zero vulnerabilities across
all severities, and the actual Everything Streamable HTTP server passed
initialization, listing13tools, echo and session-close/owned-process cleanup.

The importer independently verified source/run identity, the unchanged
reviewed manifest, all version pins, the three-entry-only diff and both file
hashes. Artifact10163921178's ZIP SHA256 is
`867fcda1f48c929a97f18a4977adad3009547717884c8b1947beb26c2d1394f9`;
the imported lock SHA256 is
`9f5fe73ea91d98215b8377ae4bd9ba0d30fc87d42b8f42b760a832718ba83f8c`.
Only manifest, lock and bounded provenance were present in the artifact.

The temporary generator and its one-off scripts are removed from the final
candidate. Their immutable generation commit and run retain the evidence.
No private source, SDK probe, credentials, deployment or image release was
involved. A fresh final-head pipeline and truthful resolution of the specific
review conversations remain required before the guarded integration merge.

## Scope

Closed managed Playwright/Everything presets; UID-bound namespace and resource
lifecycle; current rollout/protocol readiness; scoped router catalogs and
invocations; live AGT decisions; bounded session handling; OpenClaw/Hermes
bridges; optional image-build/apply assets.

Canonical source `ce9044077` is an extraction reference, not approval evidence.
Its forced ownership, name-only selectors, status-directed deletion, inline
hashing, argument/result logging and accepted-error retry behavior are not
qualification shortcuts.

## Required evidence

- Actual API rejection of stale/replaced source and namespace identities.
- No foreign-resource adoption, selector replacement or name-only cleanup.
- Exact owned Deployment/Service/NetworkPolicy creation and deletion.
- Ready binds a successful protocol probe to the current image/generation.
- Direct HTTP calls obey policy denial; scoped calls cannot cross servers.
- Accepted RPC failures and semantic `isError` are not false successes/replays.
- Bounded discovery, bodies, keepalive and bridge queues.
- Both runtime handlers execute the actual router protocol independently of
  Foundry-specific tool registration.
- Existing standalone/external MCP and publication guardrails remain intact.
- Qualified SRE-authority/governed-services ancestry, not merely copied source.

## Qualification state

Bounded automated review closed the guard type mismatch and incoming-caller
authorization findings. The unauthenticated lane requires actual loopback
socket identity; verified remote OAuth callers do not inherit managed Sandbox
catalogs or sessions. Automated review is not a human sign-off.

Local paired controller/router qualification passed 311 MCP-selected unit cases,
five managed-MCP HTTP cases and twelve existing governed-telemetry cases, plus
all-target type checking, strict Clippy and formatting. Two test fixtures were
corrected to send their claimed MIME/Accept headers; production protocol checks
were not relaxed. Minor Clippy simplifications preserve behavior.

Full OpenClaw type checking and five real MCP bridge HTTP tests passed using
the existing dependency cache after a missing-compiler failure. Its package
manifests, lockfile and mesh source match the previously qualified GitHub
candidate; no dependency installation, lockfile rewrite or placeholder
declaration was used. Earlier focused CLI and Hermes evidence is retained.

The candidate now includes the exact governed-service prerequisite `068ae160`.
Composition preserves both SRE privacy and MCP rollout annotations, runs SRE
mutation preflight before image application, and retains mandatory SRE migration
before the managed-resource acceptance phase. The composed source passed 315
selected unit cases, 17 HTTP cases, strict paired Clippy, CLI type checking and
53 image/application CLI cases.

The candidate still needs hosted prerequisite qualification and real Kind
lifecycle/protocol acceptance. Image availability and publishing are
separate operator actions; no image release or full deployment readiness is
asserted by these local results.

## Explicit limitations

No skill-package or memory acquisition, task delivery, aggregate budget broker,
new provider, outbound OAuth acquisition, attestation verification by reference
equality, or autonomous grant worker is included. Reconciliation-driven
revocation is not instantaneous cancellation of already accepted upstream work.

## Signatures

Signed-off-by: pallakatos (maintainer delegation recorded above) <lakatos.toth.pal@gmail.com>
Signed-off-by: GitHub Copilot (delegated AI audit, not an independent human) <223556219+Copilot@users.noreply.github.com>

The limited human-review waivers for earlier qualified publication slices do
not apply to this candidate. The separate, explicit delegation above supplies
the current source approval; it does not approve an unqualified integration
merge, customer/H100 deployment, image release or `main` promotion.
