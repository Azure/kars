# Security audit — registered SRE credential authority

Status: implemented and locally qualified candidate; pending real Kubernetes
admission/migration proof and independent review. **Not a sign-off.**

## Scope and trust root

This prerequisite introduces cluster-scoped `KarsSRERegistration/canonical`.
Only explicitly delegated registrars can author it; the controller can read,
use, and reconcile status. Namespace occupancy, SRE labels, account names, and
Helm-looking metadata are not privilege delegation.

## Boundaries implemented

- Exact source, controller/release, and runtime namespace UIDs are checked live.
- Reviewed legacy grants retire with UID/resourceVersion preconditions.
  Unrelated subjects/resources are preserved; custom/group ambiguity blocks
  before migration mutations.
- Shared live authorization reviews require Secret get/list/watch denial in
  namespace and cluster scope, including protected name-restricted grants,
  before issuance and proxy forwarding. Old status booleans are insufficient.
- The Pod's Azure identity remains unchanged. A separate private Kubernetes
  identity renews its short-lived Secret-bound token without ambient fallback.
- Old Hermes clients use a real loopback TLS endpoint through their standard
  token/CA/namespace paths, with an opaque non-Kubernetes credential.
- Secret projection omits values and annotation copies, retaining key names.
  Reads/logs/metrics and Pending-only proposals use a strict route/query/media
  allowlist; exec/proxy/token/write escapes are denied.
- Admission gates protect reserved sources, identities, grants, and material.
  Retained policies prevent insecure legacy grant restoration on rollback.
- Owned potentially exposed control credentials rotate and their consumers
  restart. No governed-service feature is disabled merely because SRE exists.
- Arbitrary-name legacy SA-token Secrets for the private identity are denied
  on CREATE and old/new UPDATE transitions, without a token-controller or
  registrar exception. Metadata-only prestage scans catch populated aliases,
  annotation changes and old/current SA UIDs before identity/grant issuance.
  Unsafe discovery after Ready revokes owned authority and retires owned
  credentials; unknown Secrets, replacement SA UIDs and unrelated subjects
  are preserved. Recovery rotates the bound Secret UID and consumer.
- Privacy revision `kars.azure.com/sre-privacy/v2` and the shared Rust wire
  helper are used by controller issuance, the privacy-epoch accessor, and the
  router's current-authority check. Bound TokenRequest renewal, pinned Hermes
  HTTPS clients, and the Pod's Azure ServiceAccount identity are unchanged.

## Qualification

Tests cover registrar/UID checks, reviewed grant CAS and unrelated subjects,
legacy denial, projection/WI/pinned-image invariants, real TLS API filtering,
private token renewal, direct agent-credential rejection, and the unchanged
Python Hermes client. CLI tests cover explicit enrollment and racing CREATE.

Measured local results:

- 37 controller SRE tests passed, including live UID/claim rejection,
  real SubjectAccessReview request handling, legacy binding CAS, private
  Secret-bound TokenRequests, TLS rotation/idempotency, owned retirement,
  admission-status rejection, terminating old control-token consumers despite
  converged rollout counters, metadata-only legacy-token alias scans,
  watch-only/wildcard grant rejection, post-Ready owned revocation, replacement
  UID preservation, token-anchor recovery, and existing writer/egress regressions.
- Nine router tests passed, including real loopback TLS, direct fake-token
  rejection by the test Kubernetes API, Secret/list redaction, logs/metrics/
  Pending proposals, token renewal, the unchanged `sre_kube.py` HTTPS client,
  and the actual unchanged `sre.py` proposal builder including its labels.
  The new negative matrix covers old Ready evidence, watch-only authorization,
  arbitrary-name/current-UID aliases, and metadata API errors before forwarding.
- The earlier baseline passed 158 CLI/Helm tests. The two HIGH closures reran
  116 affected CLI/Helm tests, all passing, plus TypeScript typecheck. CLI lint
  reports zero errors and 29 existing warnings outside the new helpers.
- CLI dependencies came from an existing verified local cache (Vitest 4.1.10);
  no npm install/ci. Standard TLS additions resolved `rcgen 0.13.2` and
  `yasna 0.5.2`; no custom cryptography.

- Final strict controller/router all-targets Clippy passed with `-D warnings`.
  All 46 Rust SRE tests passed after both HIGH closures, including the original
  readiness-capacity, proposal-builder compatibility, and spawner invariants.
  Scoped rustfmt, whitespace checks, current file caps, and source headers pass.

Parent dependency hygiene restored unrelated pre-existing `windows-sys` and
`oauth2`/`base64` resolution edges. The lockfile now adds only the required
direct TLS references and the new `rcgen`/`yasna` packages. Cargo accepted that
graph with `--offline --locked`; all 35 SRE tests and combined strict Clippy
passed again. An active disk guard protected that rerun; 18.35 GiB remained.

Reproducible final Clippy command (both packages together, default features;
no `--features` or `--no-default-features` flags):

```sh
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:?Set the existing shared Cargo target first}" \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
cargo clippy --offline --locked \
  -p kars-controller -p kars-inference-router --all-targets -- -D warnings
```

The final bounded Clippy run passed in 24.58 seconds with 17.91 GiB free before and
after. It used the same combined package/feature selection as the completed
Rust test command, without rerunning tests or changing feature variants:

```sh
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:?Set the existing shared Cargo target first}" \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
cargo test --offline --locked \
  -p kars-controller -p kars-inference-router sre_ --quiet
```

Resource note: an earlier combined-package Cargo feature-unification run
unexpectedly expanded the shared target from 8.3 to 6.0 GiB free. Cargo was
stopped after that completed command; no shared artifacts were deleted by
this implementation task. After disk capacity recovered to 19.92 GiB, final
qualification used the existing exclusive root target and an active 8.5 GiB
process-stop threshold. The HIGH follow-up finished with 17.91 GiB free and
no Cargo processes. Parent's lockfile minimization was preserved byte-for-byte
(SHA-256 `8be5db8d96793bf2aaa73f14ab51463fb922a07011b5cfb53c341ae46a1f1ccc`);
no dependency resolution or feature-selection changes were performed.
The final CLI/chart state also preserves live SRE source UID/resourceVersion
during explicit Helm staging and retains the registration CRD on uninstall.
The existing Helm legacy-floor suite passed another ten tests, enabled/default
Helm lint passed, and no new Rust/TypeScript module exceeds 800 lines.

The tests use real local TLS plus an HTTP test Kubernetes API, not a real
Kubernetes authorizer/admission server. Parent-owned API/E2E proof must verify
CEL type checking, the full staged/legacy migration and grant issuance,
tenant denial (including workload-template and connect escapes), token/TLS
rotation, and retirement on an actual supported Kubernetes release.
Later Azure/kars#550 must call the privacy-epoch helper before operator-secret issuance
and incorporate its epoch into cached-token rotation. No public image or
release is claimed qualified by this audit; no human signatures are supplied.

The completed Kind acceptance harness now stages immutable historical fixtures
before the new admission policies, then exercises delegated registration,
legacy token-controller aliases, watch-only grants, reviewed migration ordering,
filtered TLS access through the unchanged Hermes client, and retirement/fresh
re-enrollment. Ten pure harness tests and shell syntax pass; actual cluster
execution remains pending.

Harness construction exposed a CLI mismatch: a merge update could retain an
omitted retired `legacyConsumer`. Re-enrollment now tests the registration UID
and resourceVersion and replaces the complete reviewed spec with JSON Patch.
Absent and present consumer reviews plus stale registration identity are covered
by targeted regressions. The persisted-spec assertion in the real harness
remains strict.

The shared Rust privacy helper is included in capability-audit, no-stub,
no-custom-crypto and runtime-affecting Kind path classification. Its location
outside the individual crates is not a security-gate exception.

## Full-CI follow-up

The initial full run exposed omitted registration labels, printer columns and
standard conditions. The repair adds real Ready/Progressing/Degraded condition
updates using the existing transition-time helpers, along with the schema and
display metadata; it does not exempt this CRD from conformance. The existing
17-criterion conformance suite passes against the corrected schema.

Secret-type mutations are tested separately from validating admission:
Kubernetes rejects immutable `type` changes with a specific 422 cause before
the VAP runs. Schema-valid CREATE and annotation updates still require the
intended policy-specific 403. Neither arbitrary errors nor HTTP 200 watches
count as denial.

Four Rust CodeQL alerts were investigated without suppression or product
rewriting. Their reported flows start at the readiness handler's injected Axum
State. Current evidence identifies fixed credential filenames under the
production mount and a controller-configured Kubernetes origin, rather than an
HTTP-selected location. The independent assessment and the approved
per-alert dispositions are recorded below.

### Confirm-boundary-first evidence

Independent source review of SARIF analysis `1739818791` recommends classifying
alerts 780/781 (path injection) and 782/783 (request forgery) as false positives
for the reported HTTP-input flows. Each flow starts at `get(ready)` and treats
Axum `State<Proxy>` as request data. Pinned Axum 0.8.9 instead clones the supplied
server state and ignores request parts. The sole production constructor uses
`/etc/kars/sre-api`; filenames are literals, and Kubernetes origin/namespace
come from the controller-generated private configuration.

Regression-only coverage now sends eight hostile header/query/body scenarios
through the actual readiness handler. It forces projected-file rereads, token
renewal and metadata inventory: each scenario records 24 calls to the selected
Kubernetes endpoint, while alternate HTTP/HTTPS servers receive none. The
alternate credential files are not selected. Startup-constant provenance is
also checked. The flagged production files and lockfile remain unchanged.

All 50 focused SRE tests and strict combined controller/router Clippy pass,
including the now correctly registered condition tests. This evidence assumes
trusted controller/kubelet configuration and private-volume integrity; it does
not excuse privileged configuration tampering. The user explicitly approved
false-positive dispositions for only alerts 780, 781, 782 and 783; each GitHub
alert now carries its specific evidence comment. No query or source path was
excluded, and no other alert was dismissed. This is not author/reviewer audit
sign-off, PR approval, or permission to deploy.

The first full CI execution ran the new boundary case in an isolated process
and exposed missing test-local Rustls provider initialization. Other tests had
initialized it during the earlier grouped run. The case now explicitly selects
the existing AWS-LC provider before any TLS setup and passes when executed
alone. Production proxy code and the reported input boundaries remain unchanged;
hosted Kubernetes migration qualification is still required.
