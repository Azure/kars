# Managed MCP capability audit — 2026-09-08

Status: **Source audit approved under explicit maintainer delegation**.
Exact-head hosted and native qualification remain required before merge.

## Current delegated review and qualification

The maintainer authorized publication sign-offs after additional focused
reviews, recorded in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The second attestation below is delegated AI review, not an assertion that a
second human signed this audit.

The current source is `fbd01597ce1cb86fff6df71d21dc2bf07abaf528`, based on
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
