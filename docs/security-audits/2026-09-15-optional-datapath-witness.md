<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Optional datapath witness - bounded delegated source review

Base: `3bc7ff58d8d2994e474be39fd2af201d750fc5c2`.
Reviewed producer/chart source: `15579b1cdf7adcc7bff34088631299c0a3973e84`.
Reviewed application repair: `f5dfe2c647667fb3fa02e4e17d4ba80db9271b9a`.

Status: **Scoped source-approved; hosted and kernel qualification remain separate.**
This record does not authorize live enablement, legacy adoption or deployment,
and does not waive required checks.

## Scope

The independent core context reviewed the optional Helm chart, Python capture
and publication implementation, pinned client/image packaging, associated tests
and runtime CI wiring. It independently checked the v0.53.2 CLI/wire contracts,
official release checksums, image digests, verification key and retained license
against public upstream sources.

A separate application context reviewed BFF classification, typed web consumers,
setup commands and evidence rendering. It identified an incomplete report
consistency check and closed the five-file corrective delta at `f5dfe2c6`,
including the shared matching fixtures and their CI step. The producer/chart
source did not change during that repair. The reviewers exchanged the actual
report identity, timing and unavailable-sample contracts.

## T1: New capability or attack surface?

This is an optional, default-off, operator-installed observer. Enabling installs
the privileged Inspektor Gadget DaemonSet and a bounded foreground DNS/TCP
aggregator. The web UI provides admin-only copyable commands, not a privileged
BFF installation endpoint.

Kernel collection spans namespaces on targeted Linux nodes, including Linux GPU
nodes if present; only explicitly selected sandbox aggregates are retained.
This distinction is not a claim that capture itself is sandbox-scoped. Gadget
host mounts, capabilities and node-proxy access remain elevated privileges.
No live H100 enablement or second observer is authorized by this review.

## T2: Security-control change?

The chart adds no core dependency, CRD or PVC. It uses fixed release/namespace
identities, collision checks, explicit sandbox-read permissions, retained
observer-namespace behavior and guarded disable before removal. Existing
recognized unowned installations are not adopted.

Helm lookup checks ownership metadata, not a recorded namespace UID. It cannot
make subsequent writes or deletes atomic. Serialized operator administration
and the documented guarded disable remain prerequisites.

The producer checks ready Gadget identities around capture, rereads declarations
and publishes only through an existing owned report ConfigMap. Publication pins
the ConfigMap UID for the process and uses resourceVersion-conditional updates;
there is no create fallback or stale-write retry.

The consumer requires matching settings/report identity, revision and digest.
Successful evidence must meet freshness, capture-window and complete-row
requirements. Unavailable samples intentionally carry no successful-capture
timing or sandbox evidence. Missing reports do not establish absent installation.

## T3: Availability and misleading evidence

The application review found that a Strict row with an empty declared baseline
and observed DNS could supply an empty `beyond_declared` set and a negative
verdict. Subset checking accepted that internally contradictory report.

The repair requires equality with the complete computed beyond-baseline host
set in Strict and Learn modes, using the producer's exact-string and
dot-boundary wildcard matching. Verdicts derive from the computed set. Invalid
rows cause rejection before any sandbox evidence is exposed, including when
an earlier row is valid. Unsupported Open mode remains rejected.

Empty, unavailable, stale, invalid, requested-off and unproven installation
states remain distinct. Evidence is partial DNS comparison against compiled
baseline hostnames, not complete traffic coverage, port/runtime-overlay
evaluation, successful TCP enforcement or cryptographic kernel proof.

## Verification and limits

The parent executed 13 producer and seven chart tests. The application reviewer
and parent each executed the three shared-contract Python tests, covering 14
matching cases, ten reports and six unsupported modes. The reviewer also
checked actual-producer normalization and deep-subdomain handling in memory.
The parent also executed all 54 web tests, full TypeScript checking and focused
lint after restoring the existing dependency cache in response to missing
dependencies. The temporary link was removed. Rust formatting passed.

Local Cargo build, test inventory/execution and Clippy remain unrun because the
available disk is below the agreed build floor. Local Docker is unavailable.
The local web cache uses Next 16.2.9 rather than locked 16.3.3, with TypeScript
5.9.3 and React 19.2.4; it does not replace locked hosted qualification.
The hosted workflow must execute locked Rust and web qualification, build the
pinned client image and exercise guarded Helm lifecycle in disposable Kind.
That lifecycle fixture does not demonstrate successful kernel capture.

Per-node BTF/gadget loading, traffic attribution, capture termination and crash
cleanup require separate native evidence. Neither independent context performed
live installation or kernel qualification; arm64 execution is also unverified.

This branch retains its existing 42 explicit Rust registration guards and adds
the producer/consumer Python parity step. Integration with the beta setup repair
must preserve its two additional Copilot guards, for at least 44 total, and the
new parity step. All final-head public branch requirements still apply.

## Delegation and verdict

Both independent AI contexts found no remaining high-confidence blocker in
their bounded source scopes after the consistency repair. The parent assembled
the commits and this record; it does not represent independent human review.

Author source attestation uses the maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
That delegation does not waive technical gates or authorize cluster mutations.

Verdict: accept the bounded source change for qualification, subject to actual
hosted evidence and the deployment/kernel limitations above.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
