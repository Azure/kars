# Capability audit - Bounded receipt inclusion logs

Date: 2026-09-11
Status: **Bounded source approval under explicit maintainer delegation**.
Exact-head hosted and native qualification remain required before merge.

## Scope and authorization

Reviewed source: `91484ed2ef6fd6cbfab45f5d4af8fdc09cb8915a`, based on
actual integration `fe29941f80d6b4c1ce015749f6a825b214a43e07`.

The maintainer authorized publication sign-offs after focused review rounds in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The attestation identifies Copilot as delegated AI review, not a second human.
No technical check, signature, native result or review identity is fabricated.
The repository's general two-person review policy is not rewritten.

This restores existing canonical receipt-log segmentation and complete CLI
reads without copying weaker canonical signed-predicate checks or witness
claims. It introduces no SDK, transport, image, storage class, PVC, endpoint,
RBAC grant or independent witness. Core receipts remain usable without Bridge.

## Storage and integrity boundaries

The legacy `kars-receipt-log` ConfigMap remains segment zero. Sequence numbers
and the existing SHA-256 hash-link recipe continue across numbered overflow
ConfigMaps. Parsing rejects missing or duplicate segments, malformed chains,
foreign component/owner metadata, missing namespace/UID/resourceVersion and
incorrect segment-index or previous-root bindings. Corrupt state is never
silently replaced by an empty log.

The controller reads the head and overflow from one complete API list
snapshot. Legacy unlabeled heads remain supported: the prior writer could
drop labels during replacement. A partial list cannot establish a complete
log. Current objects retain their metadata and data through UID/RV-fenced
replacement; only actual API conflicts receive bounded retries.

Rotation depends on committed JSON bytes, not competing entry sizes. At
700 KiB the current segment is sealed using Kubernetes immutability before
the next segment is created. A crash after sealing resumes from that state.
The 64 KiB individual-entry limit leaves room below the ConfigMap limit.
Unsealed older overflow prefixes are sealed before further extension.
No segment is automatically deleted or adopted from a foreign owner.

Individual signed receipts remain available when inclusion logging fails;
the existing caller reports that failure. This source does not expand the
receipt's completeness or regulatory claims, change signing keys, or approve
unrelated checkpoint publication behavior.

## CLI and compatibility

The receipt verifier retains its existing signed task, subject, digest, issuer,
scheme and claims binding. All three log consumers (`verify`, `log`,
`checkpoint`) read the complete chain in the configured receipt namespace.
The actual `kubectl get --raw` response preserves the collection's
resourceVersion; normal kubectl JSON printing does not. Namespace validation
prevents path construction from an invalid Kubernetes namespace.

API denials, malformed JSON and corrupt/incomplete snapshots are explicit
errors, never successful absence. ConfigMap list permission is now necessary
for complete-log verification. The supported existing single-map format and
configured namespace remain readable. After rotation, older head-only writers
and verifiers are not suitable for downgrade: the sealed prefix is immutable
and an old reader cannot verify later entries. This limit is documented rather
than hidden behind a fallback that claims a truncated log is complete.

Private Bridge's receipt endpoint already understands overflow. Its head-only
summary readers are a separately tracked, required private-app parity follow-up;
this core slice does not claim those summaries or full Bridge publication ready.

## Qualification and retained failures

- Exact `166f57991cd09068c89203fc1334afa6d52d8127` passed 18 storage/log tests
  and 44 receipt-filter controller-binary tests; the 44 include those 18.
  Actual client/API tests cover the committed threshold, sealed-prefix restart,
  idempotence, UID/RV replacement, a winning concurrent append, malformed
  history, incomplete reads and bounded conflict-only retries.
- Strict controller all-target/all-feature Clippy and formatting passed.
  Cargo used the existing shared target, offline/locked dependencies, two jobs
  and incremental compilation disabled. Minimum free space was 8.78 GiB,
  above the unchanged 8.5 GiB floor. Later commits do not change Rust source.
- An earlier run compiled but failed six API fixtures because the test client
  lacked the existing explicit rustls provider initialization. That failure
  is retained; the one-line fixture repair preceded the passing run.
- The real packaged CLI plus actual kubectl reproduced the JSON-printer
  resourceVersion loss before the raw-read repair. The corrected source passes
  52 CLI cases, including real-tool positive and redacted API-denial cases,
  typecheck, package build and focused lint.
- Local CLI validation used the existing compatible cache: Vitest 4.1.10
  versus locked 4.1.8 and YAML 2.9.0 versus locked 2.8.3. No local network
  installation or lockfile change was made.
- The existing CI Python selection passes 90 cases, including 16 bounded
  rotation/provenance/cleanup/convergence fixture tests. Unchanged LOC,
  custom-crypto, whitespace and shell-syntax gates pass.

Focused review found and then closed the actual kubectl compatibility defect.
It subsequently identified append-before-checkpoint publication as a race in
the native probe. The final test-only repair was re-reviewed with no significant
issues reported. Review did not independently execute a native cluster; repeat
review rounds are not represented as additional human reviewers.

## Prepared native proof - not yet executed

The existing CI Kind Task lifecycle now runs an additional bounded proof.
It accepts only CI, the harness-owned kubeconfig path, the exact Kind context
and its verified loopback API. It changes only whitespace around existing
real chain JSON to reach 700 KiB, preserving every actual entry and applying
the change with the observed head UID/RV. It never synthesizes receipt history.

A uniquely created governed-but-idle Task must cause the installed controller
to emit a real receipt into overflow. The packaged CLI must verify full
inclusion and the controller-published checkpoint. Append and checkpoint are
separate writes: only a sole, authenticated older-checkpoint/live-log mismatch
may consume the remainder of the original 90-second deadline. Identity changes,
API denials, signature failures, other failed checks and equal-size divergence
are fatal. The probe never writes or manufactures a checkpoint.

A dry-run mutation of the sealed head must receive the precise Kubernetes
immutable-data 422 denial, not an arbitrary failure. Cleanup deletes only the
created Task after UID, generation and full-spec checks with fresh UID/RV
preconditions, then observes removal. Existing log history is retained.

This preparation is not native acceptance. The candidate may enter its normal
draft-PR qualification; all required exact-head checks and actual native proof
remain necessary before integration merge. No customer/H100 deployment, image
release, private visibility change or promotion to public main is authorized.

Signed-off-by: pallakatos (maintainer delegation recorded above) <lakatos.toth.pal@gmail.com>
Signed-off-by: GitHub Copilot (delegated AI review, not an independent human) <223556219+Copilot@users.noreply.github.com>
