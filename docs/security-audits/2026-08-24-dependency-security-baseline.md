# Security Audit — dependency and CI security baseline recovery

Date: 2026-08-24
Scope: Rust and npm dependency manifests/locks, Sigstore verification, JWT
crypto-provider selection, secure temporary manifests, and Clippy-driven
internal refactors.
Gated paths: `inference-router/src/mcp/oauth.rs`,
`inference-router/src/mcp/oauth_layer.rs`,
`inference-router/src/routes/handoff/mod.rs`,
`inference-router/src/routes/handoff/succession.rs`,
`inference-router/src/routes/inference_translate.rs`,
`inference-router/src/routes/internal.rs`,
`inference-router/src/routes/mcp.rs`,
`inference-router/src/routes/signing_ops.rs`,
`inference-router/src/routes/spawn_policy.rs`,
`cli/src/commands/up/agentmesh_deploy.ts`, `cli/src/commands/up.ts`.

## Summary

The unchanged `main` commit stopped passing fresh CI because advisory databases,
package metadata, and the stable Rust toolchain moved after its original green
run. This change restores the security baseline without changing supported
customer-facing CLI commands, CRDs, Helm values, router APIs, or AGT mesh wire
formats.

- Rust dependencies move off current security findings: `h2` 0.4.16,
  `webbrowser` 1.2.2, `event-listener` 5.4.2, `serde_with` 3.21.0,
  non-yanked `spin` releases, and Sigstore 0.14.0. The Sigstore call site passes
  the original OCI image to the new API, which performs signature-reference
  triangulation internally.
- npm locks are refreshed across all seven workspaces. AGT remains on
  `js-yaml` 4.x semantics while moving to patched 4.3.1. Every changed lock
  entry resolves through `registry.npmjs.org`, has SHA-512 integrity, and was
  verified against the Microsoft public npm feed tarball before its integrity
  field was strengthened.
- Workspace feature unification now enables both `jsonwebtoken` crypto
  backends. The controller explicitly selects AWS-LC and the router explicitly
  selects RustCrypto before JWT operations, including concurrent tests.
- Stable-Clippy findings are resolved with behavior-neutral internal
  refactors. Large Axum error responses are boxed internally and dereferenced
  at every existing route boundary.
- The mesh plugin now declares its existing `oxlint` build dependency so a
  clean install no longer depends on stale or global tooling.
- Temporary Kubernetes/Bicep manifests now live in unpredictable private
  directories with mode `0600`, preventing shared-`/tmp` symlink races before
  privileged `kubectl` or `az` operations consume them.

## T1: New capability / attack surface? (NO)

- No endpoint, command, CRD field, Kubernetes permission, network destination,
  image contract, policy action, or mesh frame is added.
- Dependency changes replace vulnerable or yanked versions within existing
  code paths. The mesh and runtime continue using the same vendored AGT commit.
- The npm overrides affect only transitive patched versions. `js-yaml` stays
  within major version 4 to preserve the AGT SDK's parser behavior.

## T2: Security-control change? (IMPROVED)

- RustSec and high/critical npm findings are removed rather than ignored.
  `cargo audit --deny warnings` and `cargo deny check` remain fail-hard.
- Signed OCI policy verification remains fail-closed. Sigstore 0.14's
  `trusted_signature_layers` receives the original digest-pinned image, as
  required by the new API; passing a pre-triangulated signature reference was
  explicitly rejected during review because it would break verification.
- JWT provider selection is explicit instead of relying on feature-based
  auto-detection that becomes ambiguous in workspace builds.
- No advisory exceptions, audit suppressions, or weaker CI thresholds are
  introduced.
- npm lock provenance is not accepted from the workstation's SHA-1-only proxy:
  all changed entries use SHA-512 integrity and public registry URLs.
- Predictable shared temporary files are replaced with private directories and
  restrictive file modes, matching established secure CLI patterns.

## T3: Availability / fail-open risk? (REDUCED)

- The original `main` behavior remains the contract. Refactors preserve route
  status codes and response bodies.
- Sigstore errors still fail closed; there is no unsigned fallback.
- Explicit JWT providers remove a nondeterministic workspace-test panic and
  select the same crypto families already intended by each binary.
- Temporary file cleanup is unconditional through `finally`; deployment command
  behavior and manifest contents are unchanged.
- The change is isolated in a draft PR. It is not a release or deployment and
  must pass clean GitHub CI plus non-breaking contract review before merge.

## Verification

- Rust: `cargo clippy --all-targets --all-features -- -D warnings`; 2,128 tests
  passed, 0 failed, 3 ignored; `cargo audit --deny warnings`; `cargo deny check`.
- JWT workspace-provider tests: 25 focused tests passed in five consecutive
  parallel workspace runs.
- CLI: typecheck, lint, build, and 930 tests passed (2 skipped).
- Mesh plugin: typecheck, lint, build, and 68 tests passed (3 skipped).
- OpenClaw runtime: typecheck, lint, build, and 250 tests passed.
- npm audit: zero high or critical findings in all seven workspaces.
- CodeQL serious-alert triage verified 41 false positives and identified two
  instances of the shared temporary-manifest race fixed in this change.
- Lockfiles: zero SHA-1 integrity entries; changed tarballs verified by matching
  their existing SHA-1 before replacing it with locally computed SHA-512.
- Two independent diff reviews found and drove fixes for incorrect Sigstore
  double-triangulation, an unsafe `js-yaml` major override, weakened npm lock
  integrity, and two JWT-provider initialization races.

## Verdict

Accept for draft CI validation. The change removes current dependency risk and
CI drift without adding capabilities or intentionally changing supported
customer behavior. Merge and release remain blocked on clean remote CI and
maintainer review.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
