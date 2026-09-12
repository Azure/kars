# Capability audit — Bounded keyless GitHub services

Date: 2026-09-08
Status: Bounded automated review/repair closure complete; human sign-offs and
cross-layer privacy qualification remain pending.

## Scope and provenance

Surgical service extraction from canonical
`ce9044077d6c2431aaad24d153f212e9d8aea0b3`, on public qualified baseline
`11f4224d7c830b2c57878e356d1b4b20b13751e0`. Existing configuration,
reconciliation, provider caches, credential source protections, immutable task
authorization, egress/content-safety, and standalone lifecycle are preserved.

The canonical PAT fallback, empty/all-installation repository scope, broad
credential injection, raw-token endpoint, and workflow review/merge policy are
not adopted. New production authentication uses existing `jsonwebtoken` RS256,
`reqwest`, and standard runtime libraries; no crypto implementation or dependency
manifest/lock change is introduced.

## Blocking deployment dependency

This baseline predates the separately reviewed SRE-authority repair. The
historical agent-held SRE Kubernetes credential can read cluster-wide Secrets.
Until that grant is removed through the qualified operator-authority migration,
router-private GitHub App custody is **not established against that principal**.
Mount isolation alone is not sufficient. That repair must be merged forward and
reviewed before deployment; this slice intentionally does not copy or redesign
SRE authority.

The prerequisite owner clarified the required integration contract:
`crate::sre_authority::privacy_epoch(client, target_namespace)` must gate new
GitHub credential issuance and reuse, using actual shared GET/LIST/WATCH denials
plus current v2 Ready/retired registration proof. `KARS_SERVICE_IDENTITY_JSON`
and task authorization digests establish attribution, **not credential privacy**.
This baseline does not include or call that gate. Consequently, merging #551's
ancestry alone is not sufficient: the GitHub issuance/reuse integration must
fail closed when privacy proof is missing or stale and be independently tested
before enabling the Secret. The upstream combined gate/rotation candidate was uncompiled at the original
clarification. Its later local `7dc72810` source has separate targeted Rust and
Clippy evidence, but full SRE lifecycle and integration review remain pending.
Neither its existence nor this slice's qualification proves App materialization
and privacy-gated issuance have been wired.

No cloud deployment, image/release publication, main promotion, public API
mutation, or live GitHub App installation was performed as qualification.

## Security contract

- Optional operator-owned Secret in the exactly owned Sandbox namespace, mounted
  only in the router, not the agent/init container or a generic provider source.
- Full managed service identity binding, including namespace/Sandbox UIDs and
  task authorization identity. Atomic configuration change removes previous
  caches; missing, invalid, or changed identity never authorizes old credentials.
- App/installation verification before minting; full returned repository and
  permission/expiry attestation before caching. Repository-specific, credential-
  incarnation-specific cache, with serialized refresh and no ambient fallback.
- Fixed GitHub credential recipients; same-pod peer check, path/method/query
  allowlist, explicit repository allowlist, existing egress check for every host,
  and no inbound credential/header forwarding.
- Exactly one permitted signed log redirect; GitHub authorization never crosses
  into the storage request. No second-hop redirect or signed URL response.
- Read-only default. No workflow scheduling, review, merge, repository transfer,
  arbitrary upload or administrative API. Opt-in git push still requires
  **external GitHub branch/ruleset enforcement**; repository scope is not branch
  ownership and is not a router-level no-main-push guarantee.
- Bounded body/response/log bytes, concurrency and deadlines. Error bodies,
  credentials and signed URLs are not logged or exposed by error responses.
  Requests are never automatically replayed after upstream acceptance.

## Verification evidence

### Independent review 649bb - qualified repairs

The independent read-only review identified two MEDIUM functional blockers:

1. Git POST forwarded compressed smart-HTTP negotiations unchanged while
   dropping `Content-Encoding`. The repair accepts only one validated `gzip`
   coding on Git POST, emits canonical `gzip`, and preserves the bounded wire
   bytes. Unsupported/repeated/comma-separated codings fail with 415 before
   authentication. No general header forwarding, decoding, retry or limits
   change was introduced.
2. Token profiles omitted `checks: read` and `statuses: read`, although the
   bounded API permits check-run and combined-status reads. Both permissions are
   now explicitly requested in read and write profiles and remain part of exact
   returned-permission verification. Missing or broader returned permissions,
   or an installation rejecting the required grants, fail closed without a
   narrower retry or credential fallback.

Six new Rust regressions cover actual gzip upload-pack negotiation bytes,
header/body/auth coherence, unsupported/multiple codings, compressed wire-byte
limits, exact read/write profiles, missing/overbroad permission attestations, and
installation-grant rejection without retry. They reuse existing `flate2`,
wiremock and RSA fixtures; no dependency changes were made.

The parent subsequently ran all 33 selected Rust cases successfully (27 authored
GitHub cases and six existing provider cases), plus strict paired all-target
Clippy and formatting. Only two new test layouts required formatting. The same
independent automated reviewer found no significant issues in the bounded repair
delta. This does not constitute a human sign-off. The privacy-epoch
issuance/reuse integration remains independently deployment-blocking.

Ready selector under the parent's prescribed combined-crate lease:

```sh
cargo test --offline --locked -p kars-controller -p kars-inference-router --lib --bins github
```

This includes the six new cases (`github_git_gzip` and `github_ci_` selectors)
and all existing GitHub/auth/config fixtures. The complete source contains
27 authored Rust tests.

Post-repair fast checks passed: seven cached OpenClaw HTTP/tool tests, full
runtime typecheck, tracked/new-file whitespace, new-source copyright headers
and the existing candidate LOC gate. The later parent Rust qualification used
the same existing target, both default-feature crates, offline/locked resolution,
two build jobs and an active 8.5 GiB floor. Minimum free space was 11.07 GiB;
the lease was released afterward. No dependency installation, manifest/lock
change, public publication or deployment occurred.

### Prior candidate qualification (before review repairs)

Completed before the repairs above:

- Seven OpenClaw tests using real local HTTP servers passed: exact URL and
  credential absence, real job-log content and truncation metadata, malicious
  inputs, redirect non-following, error-body privacy, byte bounds, interrupted
  response, loopback-only client and working tool registration. The actual plugin
  registration was exercised through its governance wrapper: a denied action
  caused no log request; an allowed action returned real bytes without logging
  them. CI log tool results are excluded from activity-memory checkpoints.
- Initial Node test attempt failed because the runner was absent. Qualification
  used an existing local cache (Vitest 4.1.10, TypeScript 5.9.3, Node declarations
  22.20.1); no network install or lock/manifest modification.
- Targeted TypeScript compilation and existing Oxlint passed for all three new
  runtime source/test modules (zero warnings or errors).
- Full runtime `npm run typecheck` passed after rebuilding the actual local
  `@kars/mesh` file dependency's declarations from this worktree. Missing
  `@noble/curves` and `@noble/hashes` 2.2.0 archives were restored from the
  existing npm mirror cache **after their SHA-512 digests matched the current
  lockfile**. Built declarations and the real package manifest were copied into
  the ignored runtime dependency directory; no placeholder, staged symlink,
  network install, manifest or lockfile change was used.
- All **21 authored Rust tests** passed (20 router, one controller).
  The `github` selector additionally passed six existing provider-detection
  tests. Four existing governed-service identity/projection tests and all
  21 existing credential-source tests passed.
- Strict all-target Clippy passed for controller and router together, with
  default features, locked/offline resolution and warnings denied. The initial
  compile found an owned `Blocklist` versus `Arc<Blocklist>` mismatch in the new
  route constructor; it was corrected, and the complete selectors rerun.
- Affected-crate formatting passed.
- Whitespace and existing A2A-isolation/copyright checks passed. The existing LOC
  checker, adapted in memory to inspect the uncommitted diff plus new files,
  passed without changing the gate or adding waivers. New Rust headers/module
  caps were also checked directly.

Pending:

- Independent reviewer assessment, supply-chain sign-off, forward-merged SRE
  boundary qualification, and real installation/operator acceptance are pending.

### Exact final qualification commands

Rust commands ran with:

```sh
export CARGO_TARGET_DIR=/Users/pallakatos/Private/Repos/kars/target
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
cargo test --offline --locked -p kars-controller -p kars-inference-router --lib --bins github
cargo test --offline --locked -p kars-controller -p kars-inference-router --lib --bins reconciler::governed_services
cargo test --offline --locked -p kars-controller -p kars-inference-router --lib --bins credential_sources::tests
cargo clippy --offline --locked -p kars-controller -p kars-inference-router --all-targets -- -D warnings
CARGO_NET_OFFLINE=true cargo fmt -p kars-controller -p kars-inference-router -- --check
```

An active one-second free-space guard surrounded compilation/tests/Clippy,
terminating only this process group if available disk fell below 8.5 GiB. It did
not trip. No alternate target, feature variant or target cleanup was used.
The exclusive lease was released after a process check found no Cargo/rustc/
rustfmt/Clippy processes; 14.69 GiB was free.

From the worktree root:

```sh
node runtimes/openclaw/node_modules/typescript/bin/tsc \
  -p mesh-plugin/tsconfig.json --emitDeclarationOnly --noEmitOnError \
  --typeRoots runtimes/openclaw/node_modules/@types
git diff --check
bash ci/a2a-module-isolation.sh
bash ci/check-copyright-headers.sh
```

From `runtimes/openclaw`:

```sh
npm run typecheck
npm test -- src/core/github-actions-logs.test.ts
node node_modules/oxlint/bin/oxlint \
  src/core/github-actions-logs.ts src/core/github-actions-logs.test.ts \
  src/core/agt-tools/github-actions.ts
```

The declaration-build command uses this worktree's source and verified local
dependency cache, not declarations from the immutable canonical worktree.
These tests exercise local fake upstreams; they do not claim live GitHub or
Kubernetes production acceptance.

## Residual operational constraints

Kubernetes Secret updates are eventually projected. Removal/rotation fences new
dispatches once observed, not already-accepted operations or projection delay.
Use GitHub revocation controls for immediate credential invalidation. Cached
permissions may persist until token expiry or a 401; the proxy repository scope
still applies to every request.

GitHub logs and successful API bodies are untrusted repository data; they are
not sanitized of secrets an upstream workflow may itself have printed. Upstream
log hygiene remains necessary. TLS trust and the configured signed egress policy
remain trust dependencies. Branch protections must deny App bypass before write
is enabled. This candidate provides no durable budget broker, workflow engine,
user approval ledger, or automatic worker enrollment.

## Sign-offs

| Role | Name | Date | Decision |
| --- | --- | --- | --- |
| Independent security reviewer | Pending | Pending | Pending |
| Runtime/controller maintainer | Pending | Pending | Pending |
| Supply-chain reviewer | Pending | Pending | Pending |
| Operator acceptance | Pending | Pending | Pending |

No reviewer identity or signature is asserted by this document.
